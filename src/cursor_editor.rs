//! Transactional, exclusively borrowed syntax-tree editing.

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::hash::Hasher;
use std::ops::Range;
use std::sync::Arc;

use thiserror::Error;

use crate::GreenElement;
use crate::GreenNode;
use crate::GreenToken;
use crate::NodeOrToken;
use crate::TextRange;
use crate::TextSize;
use crate::cursor::SyntaxElement;
use crate::cursor::SyntaxNode;
use crate::cursor::SyntaxToken;
use crate::green::GreenError;
use crate::green::SyntaxKind;
use crate::green::checked_text_add;

/// Pointer-identity token that scopes all editor IDs to one editor.
#[derive(Clone)]
struct EditorOwner(Arc<()>);

impl EditorOwner {
  /// Allocate a new editor owner identity.
  fn new() -> Self {
    Self(Arc::new(()))
  }
}

impl PartialEq for EditorOwner {
  fn eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.0, &other.0)
  }
}

impl Eq for EditorOwner {}

impl Hash for EditorOwner {
  fn hash<HasherType: Hasher>(&self, state: &mut HasherType) {
    std::ptr::hash(Arc::as_ptr(&self.0), state);
  }
}

impl fmt::Debug for EditorOwner {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.debug_tuple("EditorOwner").finish()
  }
}

/// Define one stable editor ID variant with the shared ownership and slot contract.
macro_rules! define_editor_id {
  ($(#[$metadata:meta])* $name:ident) => {
    $(#[$metadata])*
    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub struct $name {
      /// Editor that owns this ID.
      owner: EditorOwner,
      /// Monotonic arena slot; slots are never reused.
      slot: usize,
    }
  };
}

define_editor_id!(/// Stable opaque ID for a node inside one editor.
EditorNodeId);
define_editor_id!(/// Stable opaque ID for a token inside one editor.
EditorTokenId);

/// Stable opaque ID for either editor element variant.
pub type EditorElementId = NodeOrToken<EditorNodeId, EditorTokenId>;

/// Elements detached from the target range by a successful splice.
#[derive(Debug)]
pub struct SpliceOutcome {
  /// Stable IDs of detached component roots in original target order.
  detached: Vec<EditorElementId>,
}

impl SpliceOutcome {
  /// Borrow the detached component IDs.
  pub fn detached(&self) -> &[EditorElementId] {
    &self.detached
  }

  /// Consume the outcome and return its detached component IDs.
  pub fn into_detached(self) -> Vec<EditorElementId> {
    self.detached
  }
}

/// A rejected editor lookup or transactional edit.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EditError {
  /// Green-tree validation or allocation failed.
  #[error(transparent)]
  Green(#[from] GreenError),
  /// An ID belongs to another editor or does not name the requested variant.
  #[error("editor ID is foreign")]
  ForeignId,
  /// A source cursor is outside the original subtree supplied to this editor.
  #[error("source element does not belong to this editor's original subtree")]
  UnknownSourceElement,
  /// A target child range is reversed or out of bounds.
  #[error("child range {start}..{end} is invalid for {child_count} children")]
  InvalidChildRange {
    /// Inclusive range start.
    start:       usize,
    /// Exclusive range end.
    end:         usize,
    /// Number of target children.
    child_count: usize,
  },
  /// The same insertion ID appears more than once.
  #[error("insertion at position {duplicate_position} duplicates position {first_position}")]
  DuplicateInsertion {
    /// First insertion position.
    first_position:     usize,
    /// Repeated insertion position.
    duplicate_position: usize,
  },
  /// An edit would make a node its own ancestor.
  #[error("edit would create a syntax-tree cycle")]
  Cycle,
  /// `into_syntax` was asked to finish a node that still has a parent.
  #[error("syntax root is still attached")]
  RootStillAttached,
}

/// Mutable editor record for a node.
#[derive(Clone)]
struct EditorNodeRecord {
  /// Current parent ID.
  parent:   Option<EditorNodeId>,
  /// Current index, or `None` for a component root.
  index:    Option<usize>,
  /// Component-relative checked offset.
  offset:   TextSize,
  /// Current subtree byte length.
  text_len: TextSize,
  /// Original or most recently materialized immutable green node.
  green:    GreenNode,
  /// Current child IDs in source order.
  children: Vec<EditorElementId>,
  /// Whether this node must be rebuilt during completion.
  dirty:    bool,
}

/// Mutable editor record for a token.
#[derive(Clone)]
struct EditorTokenRecord {
  /// Current parent ID.
  parent: Option<EditorNodeId>,
  /// Current index, or `None` for a detached imported token.
  index:  Option<usize>,
  /// Component-relative checked offset.
  offset: TextSize,
  /// Immutable green token allocation.
  green:  GreenToken,
}

/// One stable editor arena record.
#[derive(Clone)]
enum EditorRecord {
  /// Node record.
  Node(EditorNodeRecord),
  /// Token record.
  Token(EditorTokenRecord),
}

/// Node/token discriminator in a source cursor locator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SourceVariant {
  /// Node cursor.
  Node,
  /// Token cursor.
  Token,
}

/// Allocation identity plus a complete child-index path from the top cursor root.
#[derive(Clone)]
struct SourceLocator {
  /// Top immutable green root allocation.
  root:    GreenNode,
  /// Complete child-index path from `root`.
  path:    Vec<usize>,
  /// Cursor variant at the path.
  variant: SourceVariant,
}

impl PartialEq for SourceLocator {
  fn eq(&self, other: &Self) -> bool {
    self.variant == other.variant && self.path == other.path && self.root.ptr_eq(&other.root)
  }
}

impl Eq for SourceLocator {}

impl Hash for SourceLocator {
  fn hash<HasherType: Hasher>(&self, state: &mut HasherType) {
    self.root.hash_identity(state);
    self.path.hash(state);
    self.variant.hash(state);
  }
}

/// One pending iterative materialization task.
struct MaterializeTask {
  /// Green element to materialize.
  green:  GreenElement,
  /// New parent ID.
  parent: Option<EditorNodeId>,
  /// New index in the parent.
  index:  Option<usize>,
  /// Component-relative offset.
  offset: TextSize,
  /// Optional original-source locator.
  source: Option<SourceLocator>,
}

/// Transactional mutable syntax-tree editor.
pub struct SyntaxEditor {
  /// Owner identity shared by every ID.
  owner:      EditorOwner,
  /// Stable, append-only arena.
  records:    Vec<EditorRecord>,
  /// ID of the original source subtree root.
  root:       EditorNodeId,
  /// Original source locators used by `node_id` and `token_id`.
  source_ids: HashMap<SourceLocator, EditorElementId>,
}

impl fmt::Debug for SyntaxEditor {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter
      .debug_struct("SyntaxEditor")
      .field("record_count", &self.records.len())
      .field("root", &self.root)
      .finish()
  }
}

/// Mutable metadata snapshot used to validate an edit before committing it.
#[derive(Clone)]
struct PlannedRecord {
  /// Planned parent.
  parent:   Option<EditorNodeId>,
  /// Planned child index.
  index:    Option<usize>,
  /// Planned component-relative offset.
  offset:   TextSize,
  /// Planned subtree or token length.
  text_len: TextSize,
  /// Variant-specific metadata.
  content:  PlannedContent,
}

/// Variant-specific portion of a planned arena record.
#[derive(Clone)]
enum PlannedContent {
  /// Metadata needed to validate and rebuild a node.
  Node(PlannedNode),
  /// Token marker; immutable token storage stays in the committed arena.
  Token,
}

/// Mutable node-only metadata in a transaction plan.
#[derive(Clone)]
struct PlannedNode {
  /// Planned child order.
  children: Vec<EditorElementId>,
  /// Existing or newly propagated dirty state.
  dirty:    bool,
}

impl PlannedRecord {
  /// Construct a node plan from current arena metadata.
  fn for_node(node: &EditorNodeRecord) -> Self {
    Self {
      parent:   node.parent.clone(),
      index:    node.index,
      offset:   node.offset,
      text_len: node.text_len,
      content:  PlannedContent::Node(PlannedNode {
        children: node.children.clone(),
        dirty:    node.dirty,
      }),
    }
  }

  /// Construct a token plan from current arena metadata.
  fn for_token(token: &EditorTokenRecord) -> Self {
    Self {
      parent:   token.parent.clone(),
      index:    token.index,
      offset:   token.offset,
      text_len: token_text_len(token),
      content:  PlannedContent::Token,
    }
  }

  /// Return the planned parent.
  fn parent(&self) -> Option<&EditorNodeId> {
    self.parent.as_ref()
  }

  /// Return the planned text length.
  fn text_len(&self) -> TextSize {
    self.text_len
  }

  /// Borrow node-only plan metadata.
  fn node(&self) -> Option<&PlannedNode> {
    let PlannedContent::Node(node) = &self.content else {
      return None;
    };
    Some(node)
  }

  /// Mutably borrow node-only plan metadata.
  fn node_mut(&mut self) -> Option<&mut PlannedNode> {
    let PlannedContent::Node(node) = &mut self.content else {
      return None;
    };
    Some(node)
  }

  /// Mutably borrow a planned node's children.
  fn children_mut(&mut self) -> Option<&mut Vec<EditorElementId>> {
    self.node_mut().map(|node| &mut node.children)
  }

  /// Borrow a planned node's children.
  fn children(&self) -> Option<&[EditorElementId]> {
    self.node().map(|node| node.children.as_slice())
  }
}

impl SyntaxEditor {
  /// Materialize an immutable source subtree into a new editor.
  pub fn new(root: &SyntaxNode) -> Self {
    let owner = EditorOwner::new();
    let placeholder_root = EditorNodeId {
      owner: owner.clone(),
      slot:  0,
    };
    let mut editor = Self {
      owner,
      records: Vec::new(),
      root: placeholder_root,
      source_ids: HashMap::new(),
    };
    let (top_root, path) = root.root_locator();
    let source = SourceLocator {
      root: top_root,
      path,
      variant: SourceVariant::Node,
    };
    let materialized = editor.materialize(GreenElement::from(root.green().clone()), Some(source));
    if let NodeOrToken::Node(root_id) = materialized {
      editor.root = root_id;
    }
    editor
  }

  /// Return the original source subtree root ID.
  pub fn root_id(&self) -> EditorNodeId {
    self.root.clone()
  }

  /// Resolve an original source node to its stable editor ID.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::UnknownSourceElement`] when `source` lies outside the subtree supplied to
  /// [`SyntaxEditor::new`].
  pub fn node_id(&self, source: &SyntaxNode) -> Result<EditorNodeId, EditError> {
    let (root, path) = source.root_locator();
    let locator = SourceLocator {
      root,
      path,
      variant: SourceVariant::Node,
    };
    self
      .source_ids
      .get(&locator)
      .and_then(|element| element.as_node().cloned())
      .ok_or(EditError::UnknownSourceElement)
  }

  /// Resolve an original source token to its stable editor ID.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::UnknownSourceElement`] when `source` lies outside the subtree supplied to
  /// [`SyntaxEditor::new`].
  pub fn token_id(&self, source: &SyntaxToken) -> Result<EditorTokenId, EditError> {
    let (root, path) = source.root_locator().ok_or(EditError::UnknownSourceElement)?;
    let locator = SourceLocator {
      root,
      path,
      variant: SourceVariant::Token,
    };
    self
      .source_ids
      .get(&locator)
      .and_then(|element| element.as_token().cloned())
      .ok_or(EditError::UnknownSourceElement)
  }

  /// Import an immutable cursor subtree as a detached editor component.
  ///
  /// # Errors
  ///
  /// Returns an editor error if materializing the validated green subtree fails.
  pub fn import(&mut self, element: SyntaxElement) -> Result<EditorElementId, EditError> {
    Ok(self.materialize(element.green_owned(), None))
  }

  /// Import an immutable green subtree as a detached editor component.
  ///
  /// # Errors
  ///
  /// Returns an editor error if materializing the validated green subtree fails.
  pub fn import_green(&mut self, element: GreenElement) -> Result<EditorElementId, EditError> {
    Ok(self.materialize(element, None))
  }

  /// Return an editor element's raw syntax kind.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID not owned by this editor.
  pub fn kind(&self, element: &EditorElementId) -> Result<SyntaxKind, EditError> {
    match element {
      NodeOrToken::Node(node) => Ok(self.node_record(node)?.green.kind()),
      NodeOrToken::Token(token) => Ok(self.token_record(token)?.green.kind()),
    }
  }

  /// Return an editor element's current parent.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID not owned by this editor.
  pub fn parent(&self, element: &EditorElementId) -> Result<Option<EditorNodeId>, EditError> {
    match element {
      NodeOrToken::Node(node) => Ok(self.node_record(node)?.parent.clone()),
      NodeOrToken::Token(token) => Ok(self.token_record(token)?.parent.clone()),
    }
  }

  /// Return an editor element's current index, or `None` for a component root.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID not owned by this editor.
  pub fn index(&self, element: &EditorElementId) -> Result<Option<usize>, EditError> {
    match element {
      NodeOrToken::Node(node) => Ok(self.node_record(node)?.index),
      NodeOrToken::Token(token) => Ok(self.token_record(token)?.index),
    }
  }

  /// Clone a node's current child IDs in source order.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID not owned by this editor.
  pub fn children(&self, node: &EditorNodeId) -> Result<Vec<EditorElementId>, EditError> {
    Ok(self.node_record(node)?.children.clone())
  }

  /// Return an editor element's current UTF-8 byte length.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID not owned by this editor.
  pub fn text_len(&self, element: &EditorElementId) -> Result<TextSize, EditError> {
    match element {
      NodeOrToken::Node(node) => Ok(self.node_record(node)?.text_len),
      NodeOrToken::Token(token) => Ok(token_text_len(self.token_record(token)?)),
    }
  }

  /// Return an editor element's component-relative text range.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID not owned by this editor, or a green overflow error
  /// if the stored validated range cannot be represented.
  pub fn text_range(&self, element: &EditorElementId) -> Result<TextRange, EditError> {
    let (offset, text_len) = match element {
      NodeOrToken::Node(node) => {
        let record = self.node_record(node)?;
        (record.offset, record.text_len)
      }
      NodeOrToken::Token(token) => {
        let record = self.token_record(token)?;
        (record.offset, token_text_len(record))
      }
    };
    let end = checked_text_add(offset, text_len)?;
    Ok(TextRange::new(offset, end))
  }

  /// Borrow an editor token's exact text.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID not owned by this editor.
  pub fn token_text(&self, token: &EditorTokenId) -> Result<&str, EditError> {
    Ok(self.token_record(token)?.green.text())
  }

  /// Detach an attached element from its parent.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID not owned by this editor, or propagates
  /// transactional validation failures. A rejected edit leaves the editor unchanged.
  pub fn detach(&mut self, element: &EditorElementId) -> Result<bool, EditError> {
    let parent = match self.parent(element)? {
      Some(parent) => parent,
      None => return Ok(false),
    };
    let index = self.index(element)?.ok_or(EditError::ForeignId)?;
    let end = index.checked_add(1).ok_or(EditError::InvalidChildRange {
      start:       index,
      end:         index,
      child_count: self.node_record(&parent)?.children.len(),
    })?;
    let _ = self.splice_children(&parent, index..end, std::iter::empty())?;
    Ok(true)
  }

  /// Transactionally splice a node's original child sequence.
  ///
  /// The committed sequence is the original prefix excluding insertion IDs, followed by insertion
  /// IDs in caller order, followed by the original suffix excluding insertion IDs.
  ///
  /// # Errors
  ///
  /// Returns a typed ownership, range, duplicate, cycle, or checked-length error. Every validation
  /// and derived metadata update completes before any arena record is committed.
  pub fn splice_children(
    &mut self,
    target: &EditorNodeId,
    range: Range<usize>,
    insertions: impl IntoIterator<Item = EditorElementId>,
  ) -> Result<SpliceOutcome, EditError> {
    let target_record = self.node_record(target)?;
    let original = target_record.children.clone();
    let child_count = original.len();
    if range.start > range.end || range.end > child_count {
      return Err(EditError::InvalidChildRange {
        start: range.start,
        end: range.end,
        child_count,
      });
    }
    let insertions: Vec<EditorElementId> = insertions.into_iter().collect();
    for insertion in &insertions {
      self.validate_element(insertion)?;
    }
    validate_duplicates(&insertions)?;
    self.validate_cycles(target, &insertions)?;

    let mut planned = self.planned_records();
    let range_len = range.end.checked_sub(range.start).ok_or(EditError::InvalidChildRange {
      start: range.start,
      end: range.end,
      child_count,
    })?;
    let detached = original
      .iter()
      .skip(range.start)
      .take(range_len)
      .filter(|element| !insertions.contains(element))
      .cloned()
      .collect::<Vec<_>>();

    for insertion in &insertions {
      let source_parent = planned_parent(&planned, insertion)?;
      if let Some(parent) = source_parent.filter(|parent| parent != target) {
        let source = planned_node_mut(&mut planned, &parent)?;
        source.retain(|child| child != insertion);
      }
    }

    let mut final_children = Vec::with_capacity(original.len().saturating_add(insertions.len()));
    final_children.extend(
      original
        .iter()
        .take(range.start)
        .filter(|child| !insertions.contains(child))
        .cloned(),
    );
    final_children.extend(insertions.iter().cloned());
    final_children.extend(
      original
        .iter()
        .skip(range.end)
        .filter(|child| !insertions.contains(child))
        .cloned(),
    );
    *planned_node_mut(&mut planned, target)? = final_children;

    normalize_parent_links(&self.owner, &mut planned)?;
    recompute_lengths(&self.owner, &mut planned)?;
    recompute_offsets(&self.owner, &mut planned)?;
    mark_dirty_ancestors(&self.records, &mut planned)?;
    self.commit_planned(planned)?;

    Ok(SpliceOutcome {
      detached,
    })
  }

  /// Consume the editor and materialize one detached node component.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an invalid root, [`EditError::RootStillAttached`] for an
  /// attached node, or propagates a green reconstruction failure.
  pub fn into_syntax(self, root: &EditorNodeId) -> Result<SyntaxNode, EditError> {
    let root_record = self.node_record(root)?;
    if root_record.parent.is_some() {
      return Err(EditError::RootStillAttached);
    }
    let mut built: Vec<Option<GreenElement>> = vec![None; self.records.len()];
    let mut stack = vec![(EditorElementId::Node(root.clone()), false)];
    while let Some((element, visited)) = stack.pop() {
      process_completion_task(&self.records, element, visited, &mut built, &mut stack)?;
    }
    let root_green = built
      .get_mut(root.slot)
      .and_then(Option::take)
      .and_then(NodeOrToken::into_node)
      .ok_or(EditError::ForeignId)?;
    Ok(SyntaxNode::new_root(root_green))
  }

  /// Append a complete green component to the stable arena.
  fn materialize(&mut self, root: GreenElement, source: Option<SourceLocator>) -> EditorElementId {
    let mut stack = vec![MaterializeTask {
      green: root,
      parent: None,
      index: None,
      offset: TextSize::default(),
      source,
    }];
    let mut root_id = None;
    while let Some(task) = stack.pop() {
      let slot = self.records.len();
      let (element_id, parent, source) = self.materialize_one(task, slot, &mut stack);
      self.attach_materialized(&element_id, parent.as_ref());
      if parent.is_none() && root_id.is_none() {
        root_id = Some(element_id.clone());
      }
      if let Some(source) = source {
        self.source_ids.insert(source, element_id);
      }
    }
    match root_id {
      Some(element) => element,
      None => EditorElementId::Node(self.root.clone()),
    }
  }

  /// Materialize one task and queue its children in source order.
  fn materialize_one(
    &mut self,
    task: MaterializeTask,
    slot: usize,
    stack: &mut Vec<MaterializeTask>,
  ) -> (EditorElementId, Option<EditorNodeId>, Option<SourceLocator>) {
    let MaterializeTask {
      green,
      parent,
      index,
      offset,
      source,
    } = task;
    let element = match green {
      NodeOrToken::Node(green) => {
        let node_id = EditorNodeId {
          owner: self.owner.clone(),
          slot,
        };
        let child_tasks = materialize_child_tasks(&green, &node_id, offset, source.as_ref());
        self.records.push(EditorRecord::Node(EditorNodeRecord {
          parent: parent.clone(),
          index,
          offset,
          text_len: green.text_len(),
          green,
          children: Vec::new(),
          dirty: false,
        }));
        stack.extend(child_tasks.into_iter().rev());
        EditorElementId::Node(node_id)
      }
      NodeOrToken::Token(green) => {
        let token_id = EditorTokenId {
          owner: self.owner.clone(),
          slot,
        };
        self.records.push(EditorRecord::Token(EditorTokenRecord {
          parent: parent.clone(),
          index,
          offset,
          green,
        }));
        EditorElementId::Token(token_id)
      }
    };
    (element, parent, source)
  }

  /// Append one materialized child to its already-created parent record.
  fn attach_materialized(&mut self, element: &EditorElementId, parent: Option<&EditorNodeId>) {
    let Some(parent) = parent else {
      return;
    };
    if let Some(EditorRecord::Node(parent_record)) = self.records.get_mut(parent.slot) {
      parent_record.children.push(element.clone());
    }
  }

  /// Validate and borrow a node record.
  fn node_record(&self, node: &EditorNodeId) -> Result<&EditorNodeRecord, EditError> {
    match self.record(&node.owner, node.slot)? {
      Some(EditorRecord::Node(record)) => Ok(record),
      Some(EditorRecord::Token(_)) | None => Err(EditError::ForeignId),
    }
  }

  /// Validate and borrow a token record.
  fn token_record(&self, token: &EditorTokenId) -> Result<&EditorTokenRecord, EditError> {
    match self.record(&token.owner, token.slot)? {
      Some(EditorRecord::Token(record)) => Ok(record),
      Some(EditorRecord::Node(_)) | None => Err(EditError::ForeignId),
    }
  }

  /// Validate editor ownership before looking up one arena slot.
  fn record(&self, owner: &EditorOwner, slot: usize) -> Result<Option<&EditorRecord>, EditError> {
    if owner != &self.owner {
      return Err(EditError::ForeignId);
    }
    Ok(self.records.get(slot))
  }

  /// Validate an arbitrary editor element ID.
  fn validate_element(&self, element: &EditorElementId) -> Result<(), EditError> {
    match element {
      NodeOrToken::Node(node) => self.node_record(node).map(|_| ()),
      NodeOrToken::Token(token) => self.token_record(token).map(|_| ()),
    }
  }

  /// Reject insertion of the target or one of its ancestors.
  fn validate_cycles(&self, target: &EditorNodeId, insertions: &[EditorElementId]) -> Result<(), EditError> {
    let mut ancestor = Some(target.clone());
    while let Some(node) = ancestor {
      if insertions.iter().any(|insertion| insertion.as_node() == Some(&node)) {
        return Err(EditError::Cycle);
      }
      ancestor = self.node_record(&node)?.parent.clone();
    }
    Ok(())
  }

  /// Clone only mutable arena metadata into a pre-commit validation plan.
  fn planned_records(&self) -> Vec<PlannedRecord> {
    self
      .records
      .iter()
      .map(|record| match record {
        EditorRecord::Node(node) => PlannedRecord::for_node(node),
        EditorRecord::Token(token) => PlannedRecord::for_token(token),
      })
      .collect()
  }

  /// Commit a fully validated metadata plan without further fallible work.
  fn commit_planned(&mut self, planned: Vec<PlannedRecord>) -> Result<(), EditError> {
    if planned.len() != self.records.len() {
      return Err(EditError::ForeignId);
    }
    for (record, update) in self.records.iter_mut().zip(planned) {
      let PlannedRecord {
        parent,
        index,
        offset,
        text_len,
        content,
      } = update;
      match (record, content) {
        (EditorRecord::Node(node), PlannedContent::Node(planned_node)) => {
          node.parent = parent;
          node.index = index;
          node.offset = offset;
          node.text_len = text_len;
          node.children = planned_node.children;
          node.dirty = planned_node.dirty;
        }
        (EditorRecord::Token(token), PlannedContent::Token) => {
          token.parent = parent;
          token.index = index;
          token.offset = offset;
        }
        (EditorRecord::Node(_), PlannedContent::Token) | (EditorRecord::Token(_), PlannedContent::Node(_)) => {
          return Err(EditError::ForeignId);
        }
      }
    }
    Ok(())
  }
}

/// Queue one node's children for source-order iterative arena materialization.
fn materialize_child_tasks(
  green: &GreenNode,
  parent: &EditorNodeId,
  parent_offset: TextSize,
  source: Option<&SourceLocator>,
) -> Vec<MaterializeTask> {
  green
    .child_records()
    .iter()
    .enumerate()
    .filter_map(|(index, child)| {
      let offset = parent_offset.checked_add(child.rel_offset())?;
      let child_source = source.map(|locator| {
        let mut path = locator.path.clone();
        path.push(index);
        SourceLocator {
          root: locator.root.clone(),
          path,
          variant: match child.as_ref() {
            NodeOrToken::Node(_) => SourceVariant::Node,
            NodeOrToken::Token(_) => SourceVariant::Token,
          },
        }
      });
      Some(MaterializeTask {
        green: child.as_ref().to_owned(),
        parent: Some(parent.clone()),
        index: Some(index),
        offset,
        source: child_source,
      })
    })
    .collect()
}

/// Store one completed immutable element in its stable arena slot.
fn store_built(built: &mut [Option<GreenElement>], element: &EditorElementId, green: GreenElement) -> Result<(), EditError> {
  let destination = built.get_mut(element_slot(element)).ok_or(EditError::ForeignId)?;
  *destination = Some(green);
  Ok(())
}

/// Take every already-completed child for a dirty node in source order.
fn take_built_children(record: &EditorNodeRecord, built: &mut [Option<GreenElement>]) -> Result<Vec<GreenElement>, EditError> {
  let mut children = Vec::with_capacity(record.children.len());
  for child in &record.children {
    let green = built
      .get_mut(element_slot(child))
      .and_then(Option::take)
      .ok_or(EditError::ForeignId)?;
    children.push(green);
  }
  Ok(children)
}

/// Process one post-order immutable completion task.
fn process_completion_task(
  records: &[EditorRecord],
  element: EditorElementId,
  visited: bool,
  built: &mut [Option<GreenElement>],
  stack: &mut Vec<(EditorElementId, bool)>,
) -> Result<(), EditError> {
  let record = records.get(element_slot(&element)).ok_or(EditError::ForeignId)?;
  match (record, &element) {
    (EditorRecord::Token(token), NodeOrToken::Token(_)) => store_built(built, &element, GreenElement::from(token.green.clone())),
    (EditorRecord::Node(node), NodeOrToken::Node(_)) if !node.dirty => store_built(built, &element, GreenElement::from(node.green.clone())),
    (EditorRecord::Node(node), NodeOrToken::Node(_)) if visited => {
      let children = take_built_children(node, built)?;
      let green = GreenNode::new(node.green.kind(), children)?;
      store_built(built, &element, GreenElement::from(green))
    }
    (EditorRecord::Node(node), NodeOrToken::Node(_)) => {
      stack.push((element, true));
      stack.extend(node.children.iter().rev().cloned().map(|child| (child, false)));
      Ok(())
    }
    (EditorRecord::Node(_), NodeOrToken::Token(_)) | (EditorRecord::Token(_), NodeOrToken::Node(_)) => Err(EditError::ForeignId),
  }
}

/// Return a token record's already-validated byte length.
fn token_text_len(record: &EditorTokenRecord) -> TextSize {
  record.green.text_len()
}

/// Return an editor element's arena slot.
fn element_slot(element: &EditorElementId) -> usize {
  match element {
    NodeOrToken::Node(node) => node.slot,
    NodeOrToken::Token(token) => token.slot,
  }
}

/// Construct an element ID for a planned slot and variant.
fn planned_id(owner: &EditorOwner, slot: usize, record: &PlannedRecord) -> EditorElementId {
  match &record.content {
    PlannedContent::Node(_) => EditorElementId::Node(EditorNodeId {
      owner: owner.clone(),
      slot,
    }),
    PlannedContent::Token => EditorElementId::Token(EditorTokenId {
      owner: owner.clone(),
      slot,
    }),
  }
}

/// Validate that insertion IDs are pairwise distinct.
fn validate_duplicates(insertions: &[EditorElementId]) -> Result<(), EditError> {
  for (duplicate_position, insertion) in insertions.iter().enumerate() {
    if let Some(first_position) = insertions
      .iter()
      .take(duplicate_position)
      .position(|earlier| earlier == insertion)
    {
      return Err(EditError::DuplicateInsertion {
        first_position,
        duplicate_position,
      });
    }
  }
  Ok(())
}

/// Borrow a planned element's parent.
fn planned_parent(planned: &[PlannedRecord], element: &EditorElementId) -> Result<Option<EditorNodeId>, EditError> {
  planned
    .get(element_slot(element))
    .map(|record| record.parent().cloned())
    .ok_or(EditError::ForeignId)
}

/// Mutably borrow a planned node's child sequence.
fn planned_node_mut<'a>(planned: &'a mut [PlannedRecord], node: &EditorNodeId) -> Result<&'a mut Vec<EditorElementId>, EditError> {
  planned
    .get_mut(node.slot)
    .and_then(PlannedRecord::children_mut)
    .ok_or(EditError::ForeignId)
}

/// Rebuild every parent and index from the planned forest's child sequences.
fn normalize_parent_links(owner: &EditorOwner, planned: &mut [PlannedRecord]) -> Result<(), EditError> {
  for record in planned.iter_mut() {
    record.parent = None;
    record.index = None;
  }
  let parent_children = planned
    .iter()
    .enumerate()
    .filter_map(|(slot, record)| record.children().map(|children| (slot, children.to_vec())))
    .collect::<Vec<_>>();
  for (parent_slot, children) in parent_children {
    let parent_record = planned.get(parent_slot).ok_or(EditError::ForeignId)?;
    let parent = match planned_id(owner, parent_slot, parent_record) {
      NodeOrToken::Node(node) => node,
      NodeOrToken::Token(_) => return Err(EditError::ForeignId),
    };
    for (index, child) in children.iter().enumerate() {
      let child_record = planned.get_mut(element_slot(child)).ok_or(EditError::ForeignId)?;
      if child_record.parent.is_some() {
        return Err(EditError::Cycle);
      }
      child_record.parent = Some(parent.clone());
      child_record.index = Some(index);
    }
  }
  Ok(())
}

/// Recompute all node lengths bottom-up and reject cycles or overflow.
fn recompute_lengths(owner: &EditorOwner, planned: &mut [PlannedRecord]) -> Result<(), EditError> {
  let roots = planned
    .iter()
    .enumerate()
    .filter(|(_, record)| record.parent().is_none() && record.children().is_some())
    .map(|(slot, record)| planned_id(owner, slot, record))
    .collect::<Vec<_>>();
  let mut states = vec![0_u8; planned.len()];
  for root in roots {
    let mut stack = vec![(root, false)];
    while let Some((element, visited)) = stack.pop() {
      process_length_task(planned, &mut states, element, visited, &mut stack)?;
    }
  }
  if planned
    .iter()
    .enumerate()
    .any(|(slot, record)| record.children().is_some() && states.get(slot).copied() != Some(2))
  {
    return Err(EditError::Cycle);
  }
  Ok(())
}

/// Sum one planned node's direct child lengths with checked text arithmetic.
fn planned_children_text_len(planned: &[PlannedRecord], slot: usize) -> Result<TextSize, EditError> {
  let children = planned
    .get(slot)
    .and_then(PlannedRecord::children)
    .ok_or(EditError::ForeignId)?;
  children.iter().try_fold(TextSize::default(), |text_len, child| {
    checked_text_add(text_len, planned_text_len(planned, child)?).map_err(EditError::from)
  })
}

/// Process one cycle-checked post-order node-length task.
fn process_length_task(
  planned: &mut [PlannedRecord],
  states: &mut [u8],
  element: EditorElementId,
  visited: bool,
  stack: &mut Vec<(EditorElementId, bool)>,
) -> Result<(), EditError> {
  let slot = element_slot(&element);
  if visited {
    let text_len = planned_children_text_len(planned, slot)?;
    let record = planned.get_mut(slot).ok_or(EditError::ForeignId)?;
    if record.node().is_none() {
      return Err(EditError::ForeignId);
    }
    record.text_len = text_len;
    *states.get_mut(slot).ok_or(EditError::ForeignId)? = 2;
    return Ok(());
  }

  match states.get(slot).copied().ok_or(EditError::ForeignId)? {
    1 => return Err(EditError::Cycle),
    2 => return Ok(()),
    _ => {}
  }
  *states.get_mut(slot).ok_or(EditError::ForeignId)? = 1;
  stack.push((element, true));
  let children = planned
    .get(slot)
    .and_then(PlannedRecord::children)
    .ok_or(EditError::ForeignId)?;
  stack.extend(
    children
      .iter()
      .rev()
      .filter(|child| child.as_node().is_some())
      .cloned()
      .map(|child| (child, false)),
  );
  Ok(())
}

/// Recompute every component-relative offset top-down.
fn recompute_offsets(owner: &EditorOwner, planned: &mut [PlannedRecord]) -> Result<(), EditError> {
  let roots = planned
    .iter()
    .enumerate()
    .filter(|(_, record)| record.parent().is_none())
    .map(|(slot, record)| planned_id(owner, slot, record))
    .collect::<Vec<_>>();
  for root in roots {
    set_planned_offset(planned, &root, TextSize::default())?;
    let mut stack = vec![root];
    while let Some(element) = stack.pop() {
      process_offset_task(planned, &element, &mut stack)?;
    }
  }
  Ok(())
}

/// Assign direct child offsets and queue child nodes for top-down traversal.
fn process_offset_task(
  planned: &mut [PlannedRecord],
  element: &EditorElementId,
  stack: &mut Vec<EditorElementId>,
) -> Result<(), EditError> {
  let parent_offset = planned_offset(planned, element)?;
  let children = match planned.get(element_slot(element)).and_then(PlannedRecord::children) {
    Some(children) => children.to_vec(),
    None => Vec::new(),
  };
  let mut rel_offset = TextSize::default();
  for child in &children {
    set_planned_offset(planned, child, checked_text_add(parent_offset, rel_offset)?)?;
    rel_offset = checked_text_add(rel_offset, planned_text_len(planned, child)?)?;
  }
  stack.extend(children.into_iter().rev().filter(|child| child.as_node().is_some()));
  Ok(())
}

/// Return a planned element's offset.
fn planned_offset(planned: &[PlannedRecord], element: &EditorElementId) -> Result<TextSize, EditError> {
  planned
    .get(element_slot(element))
    .map(|record| record.offset)
    .ok_or(EditError::ForeignId)
}

/// Set a planned element's offset.
fn set_planned_offset(planned: &mut [PlannedRecord], element: &EditorElementId, new_offset: TextSize) -> Result<(), EditError> {
  let record = planned.get_mut(element_slot(element)).ok_or(EditError::ForeignId)?;
  record.offset = new_offset;
  Ok(())
}

/// Return a planned element's text length.
fn planned_text_len(planned: &[PlannedRecord], element: &EditorElementId) -> Result<TextSize, EditError> {
  planned
    .get(element_slot(element))
    .map(PlannedRecord::text_len)
    .ok_or(EditError::ForeignId)
}

/// Mark exactly changed parents and their current ancestors dirty.
fn mark_dirty_ancestors(original: &[EditorRecord], planned: &mut [PlannedRecord]) -> Result<(), EditError> {
  let changed = original
    .iter()
    .zip(planned.iter())
    .enumerate()
    .filter_map(|(slot, (record, update))| match (record, update.children()) {
      (EditorRecord::Node(node), Some(children)) if node.children != children => Some(slot),
      (EditorRecord::Node(_) | EditorRecord::Token(_), _) => None,
    })
    .collect::<Vec<_>>();
  for slot in changed {
    let mut current = Some(slot);
    while let Some(node_slot) = current {
      let record = planned.get_mut(node_slot).ok_or(EditError::ForeignId)?;
      let parent = record.parent.clone();
      let node = record.node_mut().ok_or(EditError::ForeignId)?;
      node.dirty = true;
      current = parent.map(|parent| parent.slot);
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use std::collections::HashSet;

  use proptest::collection::vec as strategy_vec;
  use proptest::strict::ensure_property;
  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;
  use strict_test_support::ensure_ok;
  use strict_test_support::ensure_some;

  use super::EditError;
  use super::EditorElementId;
  use super::EditorNodeId;
  use super::EditorOwner;
  use super::PlannedContent;
  use super::PlannedNode;
  use super::PlannedRecord;
  use super::SyntaxEditor;
  use super::recompute_lengths;
  use crate::GreenElement;
  use crate::GreenError;
  use crate::GreenNode;
  use crate::GreenToken;
  use crate::NodeOrToken;
  use crate::SyntaxKind;
  use crate::TextRange;
  use crate::TextSize;
  use crate::cursor::SyntaxNode;

  /// Build a green token fixture with the shared editor token kind.
  fn token(text: &str) -> Result<GreenToken, TestFailure> {
    ensure_ok(GreenToken::new(SyntaxKind(1), text), "the editor token fixture must allocate")
  }

  /// Build a green node fixture.
  fn node(kind: u16, children: impl IntoIterator<Item = GreenElement>) -> Result<GreenNode, TestFailure> {
    ensure_ok(GreenNode::new(SyntaxKind(kind), children), "the editor node fixture must allocate")
  }

  /// Build a flat token root from borrowed or owned text values.
  fn flat_root<Text>(texts: &[Text]) -> Result<SyntaxNode, TestFailure>
  where
    Text: AsRef<str>,
  {
    let mut children = Vec::with_capacity(texts.len());
    for text in texts {
      children.push(GreenElement::from(token(text.as_ref())?));
    }
    Ok(SyntaxNode::new_root(node(0, children)?))
  }

  /// Build a root containing `left(a, b)` and `right(c)`.
  fn nested_root() -> Result<SyntaxNode, TestFailure> {
    let left = node(2, [GreenElement::from(token("a")?), GreenElement::from(token("b")?)])?;
    let right = node(3, [GreenElement::from(token("c")?)])?;
    Ok(SyntaxNode::new_root(node(0, [
      GreenElement::from(left),
      GreenElement::from(right),
    ])?))
  }

  /// Extract a token ID from an editor element ID.
  fn token_id(element: &EditorElementId) -> Result<super::EditorTokenId, TestFailure> {
    ensure_some(element.as_token().cloned(), "the editor element must be a token ID")
  }

  /// Extract a node ID from an editor element ID.
  fn node_id(element: &EditorElementId) -> Result<EditorNodeId, TestFailure> {
    ensure_some(element.as_node().cloned(), "the editor element must be a node ID")
  }

  /// Build one synthetic node plan for checked-arithmetic and cycle tests.
  fn planned_node(parent: Option<EditorNodeId>, children: Vec<EditorElementId>) -> PlannedRecord {
    PlannedRecord {
      parent,
      index: None,
      offset: TextSize::default(),
      text_len: TextSize::default(),
      content: PlannedContent::Node(PlannedNode {
        children,
        dirty: false,
      }),
    }
  }

  /// Build one synthetic token plan with a chosen validated length.
  fn planned_token(text_len: TextSize) -> PlannedRecord {
    PlannedRecord {
      parent: None,
      index: None,
      offset: TextSize::default(),
      text_len,
      content: PlannedContent::Token,
    }
  }

  /// Observe every public metadata field for a stable set of IDs.
  fn observe(editor: &SyntaxEditor, elements: &[EditorElementId]) -> Result<Vec<String>, TestFailure> {
    let mut observations = Vec::with_capacity(elements.len());
    for element in elements {
      let kind = ensure_ok(editor.kind(element), "the observed ID kind must resolve")?;
      let parent = ensure_ok(editor.parent(element), "the observed ID parent must resolve")?;
      let index = ensure_ok(editor.index(element), "the observed ID index must resolve")?;
      let text_len = ensure_ok(editor.text_len(element), "the observed ID length must resolve")?;
      let range = ensure_ok(editor.text_range(element), "the observed ID range must resolve")?;
      let child_count = match element {
        NodeOrToken::Node(node) => ensure_ok(editor.children(node), "observed node children must resolve")?.len(),
        NodeOrToken::Token(_) => 0,
      };
      let token_text = match element {
        NodeOrToken::Node(_) => String::new(),
        NodeOrToken::Token(token) => ensure_ok(editor.token_text(token), "observed token text must resolve")?.to_owned(),
      };
      observations.push(format!(
        "{kind:?}|{parent:?}|{index:?}|{text_len:?}|{range:?}|{child_count}|{token_text}"
      ));
    }
    Ok(observations)
  }

  /// Editor fixture whose complete public view is observed around rejected transactions.
  struct RejectionFixture {
    /// Editor under test.
    editor:       SyntaxEditor,
    /// Original source node retained for locator validation.
    left_source:  SyntaxNode,
    /// Root target ID.
    root:         EditorNodeId,
    /// Left child target ID.
    left:         EditorNodeId,
    /// Root ID expressed as an insertion element.
    root_element: EditorElementId,
    /// One valid child used for duplicate attempts.
    duplicate:    EditorElementId,
    /// Stable ID set covering every affected public field.
    observed_ids: Vec<EditorElementId>,
  }

  impl RejectionFixture {
    /// Build the nested source and its complete observation set.
    fn new() -> Result<Self, TestFailure> {
      let source = nested_root()?;
      let left_source = ensure_some(source.children().next(), "the left source node must exist")?;
      let editor = SyntaxEditor::new(&source);
      let root = editor.root_id();
      let root_element = EditorElementId::Node(root.clone());
      let root_children = ensure_ok(editor.children(&root), "the root children must resolve")?;
      let left = node_id(ensure_some(root_children.first(), "the left ID must exist")?)?;
      let left_children = ensure_ok(editor.children(&left), "the left children must resolve")?;
      let duplicate = ensure_some(left_children.first().cloned(), "the duplicate insertion ID must exist")?;
      let mut observed_ids = vec![root_element.clone()];
      observed_ids.extend(root_children);
      observed_ids.extend(left_children);
      Ok(Self {
        editor,
        left_source,
        root,
        left,
        root_element,
        duplicate,
        observed_ids,
      })
    }

    /// Run one rejected operation and compare every public field before and after it.
    fn rejects_without_change(
      &mut self,
      expected: EditError,
      operation: impl FnOnce(&mut SyntaxEditor) -> Result<(), EditError>,
      message: &'static str,
    ) -> Result<(), TestFailure> {
      let before = observe(&self.editor, &self.observed_ids)?;
      let actual = operation(&mut self.editor);
      ensure(actual == Err(expected), message)?;
      ensure(
        observe(&self.editor, &self.observed_ids)? == before,
        "a rejected transaction must preserve every public editor field",
      )
    }
  }

  #[test]
  fn source_lookup_uses_complete_paths_and_imports_are_independent() -> Result<(), TestFailure> {
    let empty = node(4, std::iter::empty())?;
    let source = SyntaxNode::new_root(node(0, [GreenElement::from(empty.clone()), GreenElement::from(empty)])?);
    let source_children = source.children().collect::<Vec<_>>();
    let first_source = ensure_some(source_children.first(), "the first repeated source node must exist")?;
    let second_source = ensure_some(source_children.get(1), "the second repeated source node must exist")?;
    ensure_eq(
      first_source,
      second_source,
      "public cursor identity intentionally coincides for repeated zero-width allocations",
    )?;

    let mut editor = SyntaxEditor::new(&source);
    let first_id = ensure_ok(editor.node_id(first_source), "the first complete source path must resolve")?;
    let second_id = ensure_ok(editor.node_id(second_source), "the second complete source path must resolve")?;
    ensure(
      first_id != second_id,
      "complete child-index paths must distinguish repeated zero-width nodes",
    )?;
    let root_id = editor.root_id();
    ensure(
      ensure_ok(editor.children(&root_id), "the source root children must resolve")?
        == vec![
          EditorElementId::Node(first_id.clone()),
          EditorElementId::Node(second_id.clone()),
        ],
      "source IDs must follow source child order",
    )?;

    let imported_first = ensure_ok(
      editor.import(GreenCursorElement::from(first_source.clone())),
      "the first syntax import must succeed",
    )?;
    let imported_second = ensure_ok(
      editor.import_green(GreenElement::from(first_source.green().clone())),
      "the green import must succeed",
    )?;
    ensure(
      imported_first != imported_second,
      "repeated imports must create independent stable components",
    )?;
    for imported in [&imported_first, &imported_second] {
      ensure(
        ensure_ok(editor.parent(imported), "an imported parent query must resolve")?.is_none(),
        "every imported component must begin detached",
      )?;
      ensure(
        ensure_ok(editor.text_range(imported), "an imported range query must resolve")? == TextRange::empty(TextSize::from(0)),
        "every imported component root must be rebased to zero",
      )?;
    }

    let outside = flat_root(&["outside"])?;
    ensure(
      editor.node_id(&outside) == Err(EditError::UnknownSourceElement),
      "node lookup must reject a source outside the selected subtree",
    )
  }

  /// Local alias used to make raw syntax-element conversion explicit.
  type GreenCursorElement = crate::cursor::SyntaxElement;

  #[test]
  fn detach_updates_complete_metadata_and_preserves_the_source() -> Result<(), TestFailure> {
    let source = flat_root(&["a", "b", "c"])?;
    let source_tokens = source
      .children_with_tokens()
      .filter_map(NodeOrToken::into_token)
      .collect::<Vec<_>>();
    let mut editor = SyntaxEditor::new(&source);
    let root = editor.root_id();
    let children = ensure_ok(editor.children(&root), "the flat root children must resolve")?;
    let middle = ensure_some(children.get(1).cloned(), "the middle token ID must exist")?;

    ensure(
      ensure_ok(editor.detach(&middle), "the attached token must detach")?,
      "the first detach must report true",
    )?;
    ensure(
      ensure_ok(editor.parent(&middle), "the detached parent query must resolve")?.is_none(),
      "the detached token must become a component root",
    )?;
    ensure(
      ensure_ok(editor.index(&middle), "the detached index query must resolve")?.is_none(),
      "the detached token must have no parent index",
    )?;
    ensure(
      ensure_ok(editor.text_range(&middle), "the detached range must resolve")? == TextRange::new(TextSize::from(0), TextSize::from(1)),
      "the detached token range must rebase to zero",
    )?;
    ensure(
      ensure_ok(
        editor.text_len(&EditorElementId::Node(root.clone())),
        "the edited root length must resolve",
      )? == TextSize::from(2),
      "the old parent length must shrink",
    )?;
    ensure(
      ensure_ok(editor.children(&root), "the edited child order must resolve")?
        == vec![
          ensure_some(children.first().cloned(), "the first child must exist")?,
          ensure_some(children.get(2).cloned(), "the final child must exist")?,
        ],
      "the old parent must reindex its unaffected children",
    )?;
    ensure(
      !ensure_ok(editor.detach(&middle), "detaching a component root must succeed as a no-op")?,
      "a second detach must report false",
    )?;
    ensure(
      !ensure_ok(
        editor.detach(&EditorElementId::Node(root)),
        "detaching the source component root must succeed as a no-op",
      )?,
      "the source root is already detached",
    )?;
    ensure_eq(
      &source.to_string(),
      &"abc".to_owned(),
      "editing must never modify the immutable source",
    )?;
    ensure(
      source_tokens.get(1).map(|token| token.text()) == Some("b"),
      "pre-existing source cursors must remain readable",
    )
  }

  #[test]
  fn splice_defines_reorder_reinsertion_and_detachment_without_index_ambiguity() -> Result<(), TestFailure> {
    let source = flat_root(&["a", "b", "c"])?;
    let mut editor = SyntaxEditor::new(&source);
    let root = editor.root_id();
    let original = ensure_ok(editor.children(&root), "the original child sequence must resolve")?;
    let c = ensure_some(original.get(2).cloned(), "the c ID must exist")?;
    let outcome = ensure_ok(editor.splice_children(&root, 0..1, [c]), "same-parent reordering must succeed")?;
    ensure(
      outcome.detached() == &original[..1],
      "the removed non-reinserted prefix must be reported detached",
    )?;
    let reordered = ensure_ok(editor.children(&root), "the reordered sequence must resolve")?;
    let reordered_text = reordered
      .iter()
      .map(token_id)
      .collect::<Result<Vec<_>, _>>()?
      .iter()
      .map(|token| ensure_ok(editor.token_text(token), "reordered token text must resolve"))
      .collect::<Result<Vec<_>, _>>()?
      .join("");
    ensure_eq(
      &reordered_text,
      &"cb".to_owned(),
      "same-parent movement must use the locked original-sequence rule",
    )?;

    let source = flat_root(&["a", "b", "c"])?;
    let mut editor = SyntaxEditor::new(&source);
    let root = editor.root_id();
    let original = ensure_ok(editor.children(&root), "the reinsertion source must resolve")?;
    let b = ensure_some(original.get(1).cloned(), "the b ID must exist")?;
    let outcome = ensure_ok(
      editor.splice_children(&root, 1..2, [b]),
      "reinsertion from the deletion range must succeed",
    )?;
    ensure(
      outcome.detached().is_empty(),
      "a reinserted range element must not be reported detached",
    )?;
    ensure(
      ensure_ok(editor.children(&root), "the reinserted sequence must resolve")? == original,
      "reinserting the sole deleted element in place must preserve exact order",
    )
  }

  #[test]
  fn splice_moves_across_parents_and_accepts_both_import_surfaces() -> Result<(), TestFailure> {
    let source = nested_root()?;
    let mut source_nodes = source.children();
    let left_source = ensure_some(source_nodes.next(), "the source left node must exist")?;
    let right_source = ensure_some(source_nodes.next(), "the source right node must exist")?;
    let mut left_tokens = left_source.children_with_tokens().filter_map(NodeOrToken::into_token);
    let _a_source = ensure_some(left_tokens.next(), "the source a token must exist")?;
    let b_source = ensure_some(left_tokens.next(), "the source b token must exist")?;

    let mut editor = SyntaxEditor::new(&source);
    let root = editor.root_id();
    let left = ensure_ok(editor.node_id(&left_source), "the left source path must resolve")?;
    let right = ensure_ok(editor.node_id(&right_source), "the right source path must resolve")?;
    let b = ensure_ok(editor.token_id(&b_source), "the b source path must resolve")?;
    let outcome = ensure_ok(
      editor.splice_children(&right, 1..1, [EditorElementId::Token(b.clone())]),
      "a cross-parent move must succeed",
    )?;
    ensure(
      outcome.detached().is_empty(),
      "a moved insertion remains attached and must not be reported detached",
    )?;
    ensure(
      ensure_ok(editor.parent(&EditorElementId::Token(b.clone())), "the moved parent must resolve")? == Some(right.clone()),
      "the moved token must adopt its target parent",
    )?;
    ensure_eq(
      &ensure_ok(editor.children(&left), "the source parent children must resolve")?.len(),
      &1,
      "the source parent must shrink",
    )?;

    let imported_token = ensure_ok(
      editor.import_green(GreenElement::from(token("x")?)),
      "a green token import must succeed",
    )?;
    let external = flat_root(&["y"])?;
    let external_token = ensure_some(external.first_token(), "the external syntax token must exist")?;
    let imported_syntax = ensure_ok(
      editor.import(GreenCursorElement::from(external_token)),
      "a syntax token import must succeed",
    )?;
    let _outcome = ensure_ok(
      editor.splice_children(&left, 1..1, [imported_token, imported_syntax]),
      "green and syntax imports must be insertable together",
    )?;
    let completed = ensure_ok(editor.into_syntax(&root), "the detached source root must complete")?;
    ensure_eq(
      &completed.to_string(),
      &"axycb".to_owned(),
      "cross-parent and imported insertions must preserve target source order",
    )?;
    ensure_eq(
      &source.to_string(),
      &"abc".to_owned(),
      "completion must leave the original tree unchanged",
    )
  }

  #[test]
  fn rejected_splices_preserve_the_complete_public_editor_view() -> Result<(), TestFailure> {
    let mut fixture = RejectionFixture::new()?;
    let root = fixture.root.clone();
    fixture.rejects_without_change(
      EditError::InvalidChildRange {
        start:       2,
        end:         1,
        child_count: 2,
      },
      |editor| {
        editor
          .splice_children(
            &root,
            std::ops::Range {
              start: 2, end: 1
            },
            std::iter::empty(),
          )
          .map(|_| ())
      },
      "a reversed range must be rejected",
    )?;
    let root = fixture.root.clone();
    fixture.rejects_without_change(
      EditError::InvalidChildRange {
        start:       0,
        end:         3,
        child_count: 2,
      },
      |editor| editor.splice_children(&root, 0..3, std::iter::empty()).map(|_| ()),
      "an end beyond the child sequence must be rejected",
    )?;
    let left = fixture.left.clone();
    let duplicate = fixture.duplicate.clone();
    fixture.rejects_without_change(
      EditError::DuplicateInsertion {
        first_position:     0,
        duplicate_position: 1,
      },
      |editor| editor.splice_children(&left, 0..0, [duplicate.clone(), duplicate]).map(|_| ()),
      "duplicate insertion IDs must report both positions",
    )?;
    let left = fixture.left.clone();
    let root_element = fixture.root_element.clone();
    fixture.rejects_without_change(
      EditError::Cycle,
      |editor| editor.splice_children(&left, 0..0, [root_element]).map(|_| ()),
      "inserting an ancestor below its descendant must be rejected",
    )?;
    let foreign_source = flat_root(&["foreign"])?;
    let foreign_editor = SyntaxEditor::new(&foreign_source);
    let foreign_id = ensure_some(
      ensure_ok(foreign_editor.children(&foreign_editor.root_id()), "foreign children must resolve")?
        .first()
        .cloned(),
      "the foreign token ID must exist",
    )?;
    let left = fixture.left.clone();
    fixture.rejects_without_change(
      EditError::ForeignId,
      |editor| editor.splice_children(&left, 0..0, [foreign_id]).map(|_| ()),
      "an insertion owned by another editor must be rejected",
    )?;
    ensure_ok(
      fixture.editor.node_id(&fixture.left_source),
      "the editor must remain queryable after all rejections",
    )?;
    Ok(())
  }

  #[test]
  fn completion_requires_a_component_root_and_reuses_unchanged_green_allocations() -> Result<(), TestFailure> {
    let source = nested_root()?;
    let mut source_children = source.children();
    let left_source = ensure_some(source_children.next(), "the source left node must exist")?;
    let right_source = ensure_some(source_children.next(), "the source right node must exist")?;
    let attached_editor = SyntaxEditor::new(&source);
    let attached_left = ensure_ok(attached_editor.node_id(&left_source), "the attached left path must resolve")?;
    ensure(
      attached_editor.into_syntax(&attached_left) == Err(EditError::RootStillAttached),
      "completion must reject an attached node",
    )?;

    let mut editor = SyntaxEditor::new(&source);
    let root = editor.root_id();
    let left = ensure_ok(editor.node_id(&left_source), "the editable left path must resolve")?;
    let replacement = ensure_ok(
      editor.import_green(GreenElement::from(token("z")?)),
      "the replacement token import must succeed",
    )?;
    let _outcome = ensure_ok(
      editor.splice_children(&left, 0..1, [replacement]),
      "the left prefix replacement must succeed",
    )?;
    let completed = ensure_ok(editor.into_syntax(&root), "the detached root must complete")?;
    ensure_eq(
      &completed.to_string(),
      &"zbc".to_owned(),
      "completion must materialize the committed editor state",
    )?;
    ensure(
      !completed.green().ptr_eq(source.green()),
      "a dirty root must receive a new green allocation",
    )?;
    let completed_right = ensure_some(completed.children().nth(1), "the completed right node must exist")?;
    ensure(
      completed_right.green().ptr_eq(right_source.green()),
      "an unrelated unchanged branch must retain exact green allocation identity",
    )?;
    ensure_eq(
      &source.to_string(),
      &"abc".to_owned(),
      "completion must preserve the immutable source",
    )?;

    let mut detached_editor = SyntaxEditor::new(&source);
    let detached_left = ensure_ok(detached_editor.node_id(&left_source), "the detachable left path must resolve")?;
    ensure(
      ensure_ok(
        detached_editor.detach(&EditorElementId::Node(detached_left.clone())),
        "the attached left node must detach",
      )?,
      "detaching the left node must report true",
    )?;
    let detached = ensure_ok(
      detached_editor.into_syntax(&detached_left),
      "a detached source node must complete independently",
    )?;
    ensure(
      detached.text_range() == TextRange::new(TextSize::from(0), TextSize::from(2)),
      "completion must rebase a detached component",
    )?;
    ensure(
      detached.green().ptr_eq(left_source.green()),
      "an unchanged detached component must reuse its original green allocation",
    )
  }

  #[test]
  fn checked_length_planning_rejects_overflow_before_commit() -> Result<(), TestFailure> {
    let owner = EditorOwner::new();
    let first = super::EditorTokenId {
      owner: owner.clone(),
      slot:  1,
    };
    let second = super::EditorTokenId {
      owner: owner.clone(),
      slot:  2,
    };
    let mut planned = vec![
      planned_node(None, vec![EditorElementId::Token(first), EditorElementId::Token(second)]),
      planned_token(TextSize::from(u32::MAX)),
      planned_token(TextSize::from(1)),
    ];
    ensure(
      recompute_lengths(&owner, &mut planned)
        == Err(EditError::Green(GreenError::TextLengthOverflow {
          accumulated: TextSize::from(u32::MAX),
          next:        TextSize::from(1),
        })),
      "the production bottom-up plan must reject aggregate text overflow",
    )?;

    let first_node = EditorNodeId {
      owner: owner.clone(),
      slot:  0,
    };
    let second_node = EditorNodeId {
      owner: owner.clone(),
      slot:  1,
    };
    let mut cyclic = vec![
      planned_node(Some(second_node.clone()), vec![EditorElementId::Node(second_node)]),
      planned_node(Some(first_node.clone()), vec![EditorElementId::Node(first_node)]),
    ];
    ensure(
      recompute_lengths(&owner, &mut cyclic) == Err(EditError::Cycle),
      "the production bottom-up plan must reject a disconnected staged cycle",
    )
  }

  #[test]
  fn foreign_ownership_rejects_every_lookup_without_changing_local_state() -> Result<(), TestFailure> {
    let source = flat_root(&["local"])?;
    let foreign_source = flat_root(&["foreign"])?;
    let mut editor = SyntaxEditor::new(&source);
    let local_root = editor.root_id();
    let before = ensure_ok(editor.children(&local_root), "the local child sequence must resolve")?;

    let foreign_editor = SyntaxEditor::new(&foreign_source);
    let foreign_root = foreign_editor.root_id();
    let foreign_child = ensure_some(
      ensure_ok(foreign_editor.children(&foreign_root), "the foreign child sequence must resolve")?
        .first()
        .cloned(),
      "the foreign token ID must exist",
    )?;
    let foreign_token = token_id(&foreign_child)?;
    let foreign_root_element = EditorElementId::Node(foreign_root.clone());

    let mut hashed_ids = HashSet::new();
    let first_hash_insert = hashed_ids.insert(foreign_root.clone());
    let duplicate_hash_insert = hashed_ids.insert(foreign_root.clone());
    let outside_token = ensure_some(foreign_source.first_token(), "the foreign source token must exist")?;
    let errors = vec![
      editor.node_id(&foreign_source).err(),
      editor.token_id(&outside_token).err(),
      editor.kind(&foreign_root_element).err(),
      editor.parent(&foreign_child).err(),
      editor.index(&foreign_child).err(),
      editor.children(&foreign_root).err(),
      editor.text_len(&foreign_child).err(),
      editor.text_range(&foreign_child).err(),
      editor.token_text(&foreign_token).err(),
      editor.detach(&foreign_child).err(),
    ];
    let after = ensure_ok(
      editor.children(&local_root),
      "the local child sequence must survive rejected foreign operations",
    )?;
    let completion_error = editor.into_syntax(&foreign_root).err();

    ensure(
      (
        first_hash_insert,
        duplicate_hash_insert,
        errors,
        before.clone(),
        after,
        completion_error,
      ) == (
        true,
        false,
        vec![
          Some(EditError::UnknownSourceElement),
          Some(EditError::UnknownSourceElement),
          Some(EditError::ForeignId),
          Some(EditError::ForeignId),
          Some(EditError::ForeignId),
          Some(EditError::ForeignId),
          Some(EditError::ForeignId),
          Some(EditError::ForeignId),
          Some(EditError::ForeignId),
          Some(EditError::ForeignId),
        ],
        before.clone(),
        before,
        Some(EditError::ForeignId),
      ),
      "foreign IDs must preserve hash identity, reject every public lookup, and leave local state unchanged",
    )
  }

  /// Convert a generated selector into an index below a known non-zero bound.
  fn bounded_index(selector: usize, exclusive_bound: usize) -> Result<usize, TestFailure> {
    ensure_some(
      selector.checked_rem(exclusive_bound),
      "the generated model bound must remain non-zero",
    )
  }

  /// Insert one value at a validated reference-model boundary.
  fn reference_insert<Value: Clone>(values: &[Value], boundary: usize, insertion: Value) -> Vec<Value> {
    values
      .iter()
      .take(boundary)
      .cloned()
      .chain(std::iter::once(insertion))
      .chain(values.iter().skip(boundary).cloned())
      .collect()
  }

  /// Remove one value at a validated reference-model index.
  fn reference_remove<Value: Clone>(values: &[Value], removed_index: usize) -> Vec<Value> {
    values
      .iter()
      .enumerate()
      .filter(|(index, _)| *index != removed_index)
      .map(|(_, value)| value.clone())
      .collect()
  }

  /// Apply the editor's locked same-parent movement rule to a plain reference sequence.
  fn reference_move<Value: Clone>(values: &[Value], source_index: usize, boundary: usize) -> Result<Vec<Value>, TestFailure> {
    let moved = ensure_some(values.get(source_index).cloned(), "the generated move source must exist")?;
    let mut reordered = Vec::with_capacity(values.len());
    reordered.extend(
      values
        .iter()
        .enumerate()
        .take(boundary)
        .filter(|(index, _)| *index != source_index)
        .map(|(_, value)| value.clone()),
    );
    reordered.push(moved);
    reordered.extend(
      values
        .iter()
        .enumerate()
        .skip(boundary)
        .filter(|(index, _)| *index != source_index)
        .map(|(_, value)| value.clone()),
    );
    Ok(reordered)
  }

  /// Compare every attached flat-tree field with an independent plain-text model.
  fn validate_flat_model(
    editor: &SyntaxEditor,
    root: &EditorNodeId,
    expected_ids: &[EditorElementId],
    expected_texts: &[String],
  ) -> Result<(), TestFailure> {
    let attached = ensure_ok(editor.children(root), "the generated attached sequence must resolve")?;
    ensure(
      attached == expected_ids,
      "the generated attached ID order must match the reference sequence",
    )?;
    ensure_eq(
      &expected_ids.len(),
      &expected_texts.len(),
      "the generated identity and text models must remain aligned",
    )?;

    let mut actual = Vec::with_capacity(expected_ids.len());
    let mut expected = Vec::with_capacity(expected_ids.len());
    let mut offset = TextSize::default();
    for (index, (element, text)) in expected_ids.iter().zip(expected_texts).enumerate() {
      let token = token_id(element)?;
      let text_len = ensure_ok(
        crate::green::text_size_from_usize(text.len()),
        "the bounded generated token text must fit TextSize",
      )?;
      let end = ensure_ok(
        crate::green::checked_text_add(offset, text_len),
        "the bounded generated model offset must fit TextSize",
      )?;
      actual.push(format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{}",
        ensure_ok(editor.kind(element), "the generated kind must resolve")?,
        ensure_ok(editor.parent(element), "the generated parent must resolve")?,
        ensure_ok(editor.index(element), "the generated index must resolve")?,
        ensure_ok(editor.text_len(element), "the generated text length must resolve")?,
        ensure_ok(editor.text_range(element), "the generated text range must resolve")?,
        ensure_ok(editor.token_text(&token), "the generated token text must resolve")?,
      ));
      expected.push(format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{}",
        SyntaxKind(1),
        Some(root.clone()),
        Some(index),
        text_len,
        TextRange::new(offset, end),
        text,
      ));
      offset = end;
    }
    ensure_eq(
      &actual.join("\n"),
      &expected.join("\n"),
      "generated kinds, parents, indices, lengths, ranges, and token text must match the plain model",
    )?;
    ensure(
      (
        ensure_ok(
          editor.text_len(&EditorElementId::Node(root.clone())),
          "the generated root length must resolve",
        )?,
        ensure_ok(
          editor.text_range(&EditorElementId::Node(root.clone())),
          "the generated root range must resolve",
        )?,
      ) == (offset, TextRange::new(TextSize::default(), offset)),
      "the generated root metadata must equal the accumulated plain-model extent",
    )
  }

  /// Mutable editor and independent flat-tree model advanced as one unit.
  struct FlatEditModel<'model> {
    /// Editor under test.
    editor:         &'model mut SyntaxEditor,
    /// Stable edited root ID.
    root:           &'model EditorNodeId,
    /// Expected attached element identities.
    expected_ids:   &'model mut Vec<EditorElementId>,
    /// Expected attached token text.
    expected_texts: &'model mut Vec<String>,
  }

  impl FlatEditModel<'_> {
    /// Append one newly imported token.
    fn append(&mut self, inserted_text: String) -> Result<(), TestFailure> {
      let insertion = ensure_ok(
        self.editor.import_green(GreenElement::from(token(&inserted_text)?)),
        "a generated append token must import",
      )?;
      let boundary = self.expected_ids.len();
      let outcome = ensure_ok(
        self.editor.splice_children(self.root, boundary..boundary, [insertion.clone()]),
        "a generated append must splice",
      )?;
      ensure(outcome.detached().is_empty(), "a generated append must detach nothing")?;
      self.expected_ids.push(insertion);
      self.expected_texts.push(inserted_text);
      Ok(())
    }

    /// Move one attached token to a selected same-parent boundary.
    fn move_attached(&mut self, source_selector: usize, boundary_selector: usize) -> Result<(), TestFailure> {
      let source_index = bounded_index(source_selector, self.expected_ids.len())?;
      let boundary_bound = ensure_some(
        self.expected_ids.len().checked_add(1),
        "the bounded generated sequence must admit an insertion boundary",
      )?;
      let boundary = bounded_index(boundary_selector, boundary_bound)?;
      let moved = ensure_some(self.expected_ids.get(source_index).cloned(), "the generated moved ID must exist")?;
      let outcome = ensure_ok(
        self.editor.splice_children(self.root, boundary..boundary, [moved]),
        "a generated same-parent move must splice",
      )?;
      ensure(outcome.detached().is_empty(), "a generated same-parent move must detach nothing")?;
      *self.expected_ids = reference_move(self.expected_ids, source_index, boundary)?;
      *self.expected_texts = reference_move(self.expected_texts, source_index, boundary)?;
      Ok(())
    }

    /// Replace one attached token with a newly imported token.
    fn replace(&mut self, selector: usize, inserted_text: String) -> Result<(), TestFailure> {
      let replaced_index = bounded_index(selector, self.expected_ids.len())?;
      let range_end = ensure_some(
        replaced_index.checked_add(1),
        "the generated replacement range must have a representable end",
      )?;
      let replaced = ensure_some(
        self.expected_ids.get(replaced_index).cloned(),
        "the generated replaced ID must exist",
      )?;
      let insertion = ensure_ok(
        self.editor.import_green(GreenElement::from(token(&inserted_text)?)),
        "a generated replacement token must import",
      )?;
      let outcome = ensure_ok(
        self
          .editor
          .splice_children(self.root, replaced_index..range_end, [insertion.clone()]),
        "a generated replacement must splice",
      )?;
      ensure(
        outcome.detached() == std::slice::from_ref(&replaced),
        "a generated replacement must return exactly its old component",
      )?;
      *ensure_some(
        self.expected_ids.get_mut(replaced_index),
        "the generated identity replacement position must exist",
      )? = insertion;
      *ensure_some(
        self.expected_texts.get_mut(replaced_index),
        "the generated text replacement position must exist",
      )? = inserted_text;
      Ok(())
    }

    /// Detach one token and reinsert its stable ID at another boundary.
    fn detach_and_reinsert(&mut self, source_selector: usize, boundary_selector: usize) -> Result<(), TestFailure> {
      let detached_index = bounded_index(source_selector, self.expected_ids.len())?;
      let detached = ensure_some(
        self.expected_ids.get(detached_index).cloned(),
        "the generated detached ID must exist",
      )?;
      ensure(
        ensure_ok(self.editor.detach(&detached), "a generated attached element must detach")?,
        "a generated detach must report a state change",
      )?;
      let remaining_ids = reference_remove(self.expected_ids, detached_index);
      let remaining_texts = reference_remove(self.expected_texts, detached_index);
      let detached_text = ensure_some(
        self.expected_texts.get(detached_index).cloned(),
        "the generated detached text must exist",
      )?;
      let boundary_bound = ensure_some(
        remaining_ids.len().checked_add(1),
        "the bounded detached sequence must admit a reinsertion boundary",
      )?;
      let boundary = bounded_index(boundary_selector, boundary_bound)?;
      let outcome = ensure_ok(
        self.editor.splice_children(self.root, boundary..boundary, [detached.clone()]),
        "a generated detached component must reinsert",
      )?;
      ensure(outcome.detached().is_empty(), "generated reinsertion must detach nothing else")?;
      *self.expected_ids = reference_insert(&remaining_ids, boundary, detached);
      *self.expected_texts = reference_insert(&remaining_texts, boundary, detached_text);
      Ok(())
    }
  }

  /// Apply one valid generated command through its behavior-specific model transition.
  fn apply_valid_command(
    editor: &mut SyntaxEditor,
    root: &EditorNodeId,
    expected_ids: &mut Vec<EditorElementId>,
    expected_texts: &mut Vec<String>,
    generated: (u8, usize, usize, String),
  ) -> Result<(), TestFailure> {
    let (command, first_selector, second_selector, inserted_text) = generated;
    let mut model = FlatEditModel {
      editor,
      root,
      expected_ids,
      expected_texts,
    };
    match command {
      0 => model.append(inserted_text),
      1 => model.move_attached(first_selector, second_selector),
      2 => model.replace(first_selector, inserted_text),
      _ => model.detach_and_reinsert(first_selector, second_selector),
    }
  }

  #[test]
  fn valid_edit_sequences_match_an_independent_plain_tree_model() -> Result<(), TestFailure> {
    let strategy = (
      strategy_vec("[a-z]{1,3}", 1..7),
      strategy_vec((0_u8..4, 0_usize..32, 0_usize..32, "[a-z]{1,3}"), 1..12),
    );
    ensure_property(
      &strategy,
      "bounded valid edit sequences match a plain ordering and metadata model",
      |(texts, commands)| {
        let source = flat_root(&texts)?;
        let mut editor = SyntaxEditor::new(&source);
        let root = editor.root_id();
        let mut expected_ids = ensure_ok(editor.children(&root), "the generated initial child sequence must resolve")?;
        let mut expected_texts = texts.clone();

        for command in commands {
          apply_valid_command(&mut editor, &root, &mut expected_ids, &mut expected_texts, command)?;
        }

        validate_flat_model(&editor, &root, &expected_ids, &expected_texts)?;
        let completed = ensure_ok(editor.into_syntax(&root), "the generated edited root must complete")?;
        let expected = expected_texts.concat();
        ensure_eq(
          &completed.to_string(),
          &expected,
          "generated editor completion must match the independent plain-tree text",
        )?;
        ensure_eq(&source.to_string(), &texts.concat(), "the generated source must remain immutable")
      },
    )
  }

  #[test]
  fn invalid_generated_edits_preserve_the_complete_editor_model() -> Result<(), TestFailure> {
    let strategy = (strategy_vec("[a-z]{1,3}", 1..7), 0_u8..5);
    ensure_property(
      &strategy,
      "invalid ranges, duplicates, foreign IDs, and cycles preserve complete editor state",
      |(texts, invalid_case)| {
        let source = flat_root(&texts)?;
        let mut editor = SyntaxEditor::new(&source);
        let root = editor.root_id();
        let children = ensure_ok(editor.children(&root), "the invalid-model child sequence must resolve")?;
        let first = ensure_some(children.first().cloned(), "the invalid-model first child must exist")?;
        let mut observed_ids = vec![EditorElementId::Node(root.clone())];
        observed_ids.extend(children.clone());
        let before = observe(&editor, &observed_ids)?;

        let (actual, expected) = match invalid_case {
          0 => (
            editor
              .splice_children(
                &root,
                std::ops::Range {
                  start: 1, end: 0
                },
                std::iter::empty(),
              )
              .map(|_| ()),
            EditError::InvalidChildRange {
              start:       1,
              end:         0,
              child_count: children.len(),
            },
          ),
          1 => {
            let end = ensure_some(
              children.len().checked_add(1),
              "the bounded invalid range end must remain representable",
            )?;
            (
              editor.splice_children(&root, 0..end, std::iter::empty()).map(|_| ()),
              EditError::InvalidChildRange {
                start: 0,
                end,
                child_count: children.len(),
              },
            )
          }
          2 => (
            editor.splice_children(&root, 0..0, [first.clone(), first]).map(|_| ()),
            EditError::DuplicateInsertion {
              first_position:     0,
              duplicate_position: 1,
            },
          ),
          3 => {
            let foreign_source = flat_root(&["foreign"])?;
            let foreign_editor = SyntaxEditor::new(&foreign_source);
            let foreign = ensure_some(
              ensure_ok(
                foreign_editor.children(&foreign_editor.root_id()),
                "the generated foreign sequence must resolve",
              )?
              .first()
              .cloned(),
              "the generated foreign ID must exist",
            )?;
            (editor.splice_children(&root, 0..0, [foreign]).map(|_| ()), EditError::ForeignId)
          }
          _ => (
            editor
              .splice_children(&root, 0..0, [EditorElementId::Node(root.clone())])
              .map(|_| ()),
            EditError::Cycle,
          ),
        };
        let after = observe(&editor, &observed_ids)?;
        ensure(
          (actual, after) == (Err(expected), before),
          "every generated invalid edit must return its exact error and roll back every observable field",
        )
      },
    )
  }
}
