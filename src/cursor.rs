//! Immutable raw cursors over structurally shared green trees.

use std::fmt;
use std::hash::Hash;
use std::hash::Hasher;
use std::iter;

use thiserror::Error;
use triomphe::Arc;

use crate::Direction;
use crate::GreenElement;
use crate::GreenNode;
use crate::GreenToken;
use crate::NodeOrToken;
use crate::SyntaxText;
use crate::TextRange;
use crate::TextSize;
use crate::TokenAtOffset;
use crate::WalkEvent;
use crate::cursor_data::SyntaxNodeData;
use crate::cursor_data::SyntaxTokenData;
pub use crate::cursor_editor::EditError;
pub use crate::cursor_editor::EditorElementId;
pub use crate::cursor_editor::EditorNodeId;
pub use crate::cursor_editor::EditorTokenId;
pub use crate::cursor_editor::SpliceOutcome;
pub use crate::cursor_editor::SyntaxEditor;
pub use crate::cursor_traversal::Preorder;
pub use crate::cursor_traversal::PreorderWithTokens;
pub use crate::cursor_traversal::SyntaxElementChildren;
pub use crate::cursor_traversal::SyntaxElementChildrenByKind;
pub use crate::cursor_traversal::SyntaxNodeChildren;
pub use crate::cursor_traversal::SyntaxNodeChildrenByKind;
pub use crate::cursor_traversal::TraversalError;
use crate::green::GreenChild;
use crate::green::GreenError;
use crate::green::SyntaxKind;

/// A rejected source offset or text range.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RangeError {
  /// An offset lies outside the valid closed query interval.
  #[error("offset {offset:?} is outside {valid:?}")]
  OffsetOutOfBounds {
    /// Rejected offset.
    offset: TextSize,
    /// Valid containing range.
    valid:  TextRange,
  },
  /// A range lies outside the valid containing range.
  #[error("range {requested:?} is outside {valid:?}")]
  RangeOutOfBounds {
    /// Rejected range.
    requested: TextRange,
    /// Valid containing range.
    valid:     TextRange,
  },
  /// Caller-provided range endpoints are reversed.
  #[error("range start {start:?} is after end {end:?}")]
  ReversedRange {
    /// Rejected start offset.
    start: TextSize,
    /// Rejected end offset.
    end:   TextSize,
  },
  /// A byte offset falls inside a UTF-8 code point.
  #[error("offset {offset:?} is not a UTF-8 character boundary")]
  NotCharBoundary {
    /// Rejected relative offset.
    offset: TextSize,
  },
  /// Translating a relative offset into an absolute offset overflowed.
  #[error("offset arithmetic overflow while adding {delta:?} to {base:?}")]
  OffsetArithmeticOverflow {
    /// Base offset.
    base:  TextSize,
    /// Relative delta.
    delta: TextSize,
  },
}

/// A raw node or token cursor.
pub type SyntaxElement = NodeOrToken<SyntaxNode, SyntaxToken>;

/// Immutable raw cursor pointing at a green node and its absolute source position.
#[derive(Clone)]
#[repr(transparent)]
pub struct SyntaxNode {
  /// Shared immutable red payload.
  pub(crate) payload: Arc<SyntaxNodeData>,
}

/// Immutable raw cursor pointing at a green token and its absolute source position.
#[derive(Clone)]
#[repr(transparent)]
pub struct SyntaxToken {
  /// Shared immutable red payload.
  pub(crate) payload: Arc<SyntaxTokenData>,
}

/// Fully owned description of one red child cursor.
struct ElementDescriptor {
  /// Parent node payload.
  parent: Arc<SyntaxNodeData>,
  /// Owned immutable green child.
  green:  GreenElement,
  /// Child index.
  index:  usize,
  /// Checked absolute offset.
  offset: TextSize,
}

/// Fully owned description of one red node cursor.
struct NodeDescriptor {
  /// Parent node payload.
  parent: Arc<SyntaxNodeData>,
  /// Owned immutable green node.
  green:  GreenNode,
  /// Child index.
  index:  usize,
  /// Checked absolute offset.
  offset: TextSize,
}

/// Construct a valid text range without exposing an overflow panic path.
fn validated_text_range(offset: TextSize, text_len: TextSize) -> TextRange {
  match offset.checked_add(text_len) {
    Some(end) => TextRange::new(offset, end),
    None => TextRange::empty(offset),
  }
}

/// Add a parent offset to a validated child-relative offset.
fn child_offset(parent: &SyntaxNodeData, child: &GreenChild) -> Option<TextSize> {
  parent.offset.checked_add(child.rel_offset())
}

/// Convert an offset-bearing green child into a red element descriptor.
fn describe_child(parent: &Arc<SyntaxNodeData>, index: usize, child: &GreenChild) -> Option<ElementDescriptor> {
  let offset = child_offset(parent, child)?;
  Some(ElementDescriptor {
    parent: parent.clone(),
    green: child.as_ref().to_owned(),
    index,
    offset,
  })
}

/// Convert one green child into a typed red-node descriptor when it is a node.
fn describe_node_child(parent: &Arc<SyntaxNodeData>, index: usize, child: &GreenChild) -> Option<NodeDescriptor> {
  let NodeOrToken::Node(green) = child.as_ref() else {
    return None;
  };
  Some(NodeDescriptor {
    parent: parent.clone(),
    green: green.clone(),
    index,
    offset: child_offset(parent, child)?,
  })
}

/// Find the first node child from one edge without allocating intermediate cursors.
fn boundary_node_descriptor(parent: &Arc<SyntaxNodeData>, direction: Direction) -> Option<NodeDescriptor> {
  let child_count = parent.green.child_records().len();
  let mut index = match direction {
    Direction::Next => 0,
    Direction::Prev => child_count.checked_sub(1)?,
  };
  loop {
    let child = parent.green.child_records().get(index)?;
    if let Some(descriptor) = describe_node_child(parent, index, child) {
      return Some(descriptor);
    }
    index = match direction {
      Direction::Next => index.checked_add(1)?,
      Direction::Prev => index.checked_sub(1)?,
    };
  }
}

/// Describe one immediately adjacent element from shared node/token location metadata.
fn sibling_element_descriptor(parent: Option<Arc<SyntaxNodeData>>, index: usize, direction: Direction) -> Option<ElementDescriptor> {
  let parent = parent?;
  let sibling_index = match direction {
    Direction::Next => index.checked_add(1)?,
    Direction::Prev => index.checked_sub(1)?,
  };
  let child = parent.green.child_records().get(sibling_index)?;
  describe_child(&parent, sibling_index, child)
}

/// Iterate over an element and all sibling elements in one direction.
fn sibling_elements(start: SyntaxElement, direction: Direction) -> impl Iterator<Item = SyntaxElement> {
  iter::successors(Some(start), move |element| match direction {
    Direction::Next => element.next_sibling_or_token(),
    Direction::Prev => element.prev_sibling_or_token(),
  })
}

/// Test whether one direct child satisfies the deterministic range-selection rule.
fn query_selects_child(child_range: TextRange, query: TextRange) -> bool {
  if query.is_empty() {
    let offset = query.start();
    child_range.start() == offset || (child_range.start() < offset && offset < child_range.end())
  } else {
    child_range.contains_range(query)
  }
}

/// Validate a caller-provided absolute range before any query arithmetic.
fn validate_query_range(valid: TextRange, requested: TextRange) -> Result<(), RangeError> {
  if requested.start() > requested.end() {
    return Err(RangeError::ReversedRange {
      start: requested.start(),
      end:   requested.end(),
    });
  }
  if valid.contains_range(requested) {
    Ok(())
  } else {
    Err(RangeError::RangeOutOfBounds {
      requested,
      valid,
    })
  }
}

/// Find the next token in one source-order direction, skipping empty subtrees.
fn adjacent_token(mut current: SyntaxElement, direction: Direction) -> Option<SyntaxToken> {
  loop {
    let sibling = match direction {
      Direction::Next => current.next_sibling_or_token(),
      Direction::Prev => current.prev_sibling_or_token(),
    };
    let Some(sibling) = sibling else {
      current = SyntaxElement::from(current.parent()?);
      continue;
    };
    let token = match direction {
      Direction::Next => sibling.first_token(),
      Direction::Prev => sibling.last_token(),
    };
    if token.is_some() {
      return token;
    }
    current = sibling;
  }
}

/// Descend to the token nearest one edge of a subtree.
fn boundary_token(mut current: Option<SyntaxElement>, direction: Direction) -> Option<SyntaxToken> {
  loop {
    current = match current? {
      NodeOrToken::Node(node) => match direction {
        Direction::Next => node.first_child_or_token(),
        Direction::Prev => node.last_child_or_token(),
      },
      NodeOrToken::Token(token) => return Some(token),
    };
  }
}

/// Define the element-sibling navigation shared by node and token cursors.
macro_rules! element_sibling_navigation {
  () => {
    /// Return the next direct sibling element.
    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
      self.next_element_descriptor().map(ElementDescriptor::into_element)
    }

    /// Return the next sibling element whose kind satisfies `matcher`.
    pub fn next_sibling_or_token_by_kind(&self, matcher: &impl Fn(SyntaxKind) -> bool) -> Option<SyntaxElement> {
      iter::successors(self.next_sibling_or_token(), SyntaxElement::next_sibling_or_token).find(|element| matcher(element.kind()))
    }

    /// Return the previous direct sibling element.
    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
      sibling_element_descriptor(self.payload.parent.clone(), self.payload.index, Direction::Prev).map(ElementDescriptor::into_element)
    }
  };
}

impl ElementDescriptor {
  /// Allocate the variant-specific immutable red payload.
  fn into_element(self) -> SyntaxElement {
    match self.green {
      NodeOrToken::Node(green) => SyntaxElement::Node(SyntaxNode {
        payload: Arc::new(SyntaxNodeData {
          parent: Some(self.parent),
          green,
          index: self.index,
          offset: self.offset,
        }),
      }),
      NodeOrToken::Token(green) => SyntaxElement::Token(SyntaxToken {
        payload: Arc::new(SyntaxTokenData {
          parent: Some(self.parent),
          green,
          index: self.index,
          offset: self.offset,
        }),
      }),
    }
  }
}

impl NodeDescriptor {
  /// Allocate the immutable red-node payload described by this value.
  fn into_node(self) -> SyntaxNode {
    SyntaxNode {
      payload: Arc::new(SyntaxNodeData {
        parent: Some(self.parent),
        green:  self.green,
        index:  self.index,
        offset: self.offset,
      }),
    }
  }
}

/// Variant-specific access required to reuse a unique red payload allocation.
trait ReusableCursor: Sized {
  /// Immutable green handle stored by the payload.
  type Green;
  /// Variant-specific immutable red payload.
  type Payload;

  /// Mutably borrow the shared payload handle.
  fn payload_mut(&mut self) -> &mut Arc<Self::Payload>;

  /// Replace uniquely borrowed payload fields with an adjacent location.
  fn update_payload(payload: &mut Self::Payload, parent: Arc<SyntaxNodeData>, green: Self::Green, index: usize, offset: TextSize);

  /// Convert the cursor into the corresponding element variant.
  fn into_element(self) -> SyntaxElement;

  /// Allocate the corresponding element variant when the payload is shared.
  fn allocate_element(parent: Arc<SyntaxNodeData>, green: Self::Green, index: usize, offset: TextSize) -> SyntaxElement;
}

/// Implement the identical safe allocation-reuse protocol for both cursor variants.
macro_rules! implement_reusable_cursor {
  ($cursor:ident, $payload:ident, $green:ty, $variant:ident) => {
    impl ReusableCursor for $cursor {
      type Green = $green;
      type Payload = $payload;

      fn payload_mut(&mut self) -> &mut Arc<Self::Payload> {
        &mut self.payload
      }

      fn update_payload(payload: &mut Self::Payload, parent: Arc<SyntaxNodeData>, green: Self::Green, index: usize, offset: TextSize) {
        payload.parent = Some(parent);
        payload.green = green;
        payload.index = index;
        payload.offset = offset;
      }

      fn into_element(self) -> SyntaxElement {
        NodeOrToken::$variant(self)
      }

      fn allocate_element(parent: Arc<SyntaxNodeData>, green: Self::Green, index: usize, offset: TextSize) -> SyntaxElement {
        NodeOrToken::$variant(Self {
          payload: Arc::new($payload {
            parent: Some(parent),
            green,
            index,
            offset,
          }),
        })
      }
    }
  };
}

implement_reusable_cursor!(SyntaxNode, SyntaxNodeData, GreenNode, Node);
implement_reusable_cursor!(SyntaxToken, SyntaxTokenData, GreenToken, Token);

/// Advance one same-variant cursor while reusing unique storage when possible.
fn reuse_cursor<Cursor>(
  mut cursor: Cursor,
  parent: Arc<SyntaxNodeData>,
  green: Cursor::Green,
  index: usize,
  offset: TextSize,
) -> SyntaxElement
where
  Cursor: ReusableCursor,
{
  match Arc::get_mut(cursor.payload_mut()) {
    Some(payload) => {
      Cursor::update_payload(payload, parent, green, index, offset);
      cursor.into_element()
    }
    None => Cursor::allocate_element(parent, green, index, offset),
  }
}

/// Advance an element cursor, retaining allocation reuse only within the same variant.
fn advance_element(current: SyntaxElement, descriptor: ElementDescriptor) -> SyntaxElement {
  let ElementDescriptor {
    parent,
    green,
    index,
    offset,
  } = descriptor;
  match (current, green) {
    (NodeOrToken::Node(node), NodeOrToken::Node(green)) => reuse_cursor(node, parent, green, index, offset),
    (NodeOrToken::Token(token), NodeOrToken::Token(green)) => reuse_cursor(token, parent, green, index, offset),
    (previous, green) => {
      drop(previous);
      ElementDescriptor {
        parent,
        green,
        index,
        offset,
      }
      .into_element()
    }
  }
}

impl SyntaxNode {
  /// Create a root cursor at absolute offset zero.
  pub fn new_root(green: GreenNode) -> Self {
    Self {
      payload: Arc::new(SyntaxNodeData {
        parent: None,
        green,
        index: 0,
        offset: TextSize::default(),
      }),
    }
  }

  /// Return this node's raw syntax kind.
  pub fn kind(&self) -> SyntaxKind {
    self.payload.green.kind()
  }

  /// Return this node's absolute source range.
  pub fn text_range(&self) -> TextRange {
    validated_text_range(self.payload.offset, self.payload.green.text_len())
  }

  /// Return this node's index in its parent, or zero for a root.
  pub fn index(&self) -> usize {
    self.payload.index
  }

  /// Return a chunked text view over this subtree.
  pub fn text(&self) -> SyntaxText {
    SyntaxText::new(self.clone())
  }

  /// Borrow the immutable green node represented by this cursor.
  pub fn green(&self) -> &GreenNode {
    &self.payload.green
  }

  /// Return this node's parent.
  pub fn parent(&self) -> Option<Self> {
    self.payload.parent.clone().map(|payload| Self {
      payload,
    })
  }

  /// Iterate over this node and all of its ancestors.
  pub fn ancestors(&self) -> impl Iterator<Item = Self> + use<> {
    iter::successors(Some(self.clone()), Self::parent)
  }

  /// Iterate over direct child nodes.
  pub fn children(&self) -> SyntaxNodeChildren {
    SyntaxNodeChildren::new(self.clone())
  }

  /// Iterate over direct child nodes and tokens.
  pub fn children_with_tokens(&self) -> SyntaxElementChildren {
    SyntaxElementChildren::new(self.clone())
  }

  /// Return the first direct child node.
  pub fn first_child(&self) -> Option<Self> {
    boundary_node_descriptor(&self.payload, Direction::Next).map(NodeDescriptor::into_node)
  }

  /// Return the first direct child node whose kind satisfies `matcher`.
  pub fn first_child_by_kind(&self, matcher: &impl Fn(SyntaxKind) -> bool) -> Option<Self> {
    self.children().find(|node| matcher(node.kind()))
  }

  /// Return the last direct child node.
  pub fn last_child(&self) -> Option<Self> {
    boundary_node_descriptor(&self.payload, Direction::Prev).map(NodeDescriptor::into_node)
  }

  /// Return the first direct child element.
  pub fn first_child_or_token(&self) -> Option<SyntaxElement> {
    self
      .payload
      .green
      .child_records()
      .first()
      .and_then(|child| describe_child(&self.payload, 0, child))
      .map(ElementDescriptor::into_element)
  }

  /// Return the first direct child element whose kind satisfies `matcher`.
  pub fn first_child_or_token_by_kind(&self, matcher: &impl Fn(SyntaxKind) -> bool) -> Option<SyntaxElement> {
    self.children_with_tokens().find(|element| matcher(element.kind()))
  }

  /// Return the last direct child element.
  pub fn last_child_or_token(&self) -> Option<SyntaxElement> {
    self
      .payload
      .green
      .child_records()
      .iter()
      .enumerate()
      .next_back()
      .and_then(|(index, child)| describe_child(&self.payload, index, child))
      .map(ElementDescriptor::into_element)
  }

  /// Describe the next direct sibling element.
  fn next_element_descriptor(&self) -> Option<ElementDescriptor> {
    sibling_element_descriptor(self.payload.parent.clone(), self.payload.index, Direction::Next)
  }

  /// Describe the next sibling node, skipping tokens.
  fn next_node_descriptor(&self) -> Option<NodeDescriptor> {
    let parent = self.payload.parent.clone()?;
    let next_index = self.payload.index.checked_add(1)?;
    parent
      .green
      .child_records()
      .iter()
      .enumerate()
      .skip(next_index)
      .find_map(|(index, child)| describe_node_child(&parent, index, child))
  }

  /// Consume and advance this cursor, reusing a uniquely owned payload when possible.
  pub(crate) fn into_next_sibling(mut self) -> Option<Self> {
    let descriptor = self.next_node_descriptor()?;
    if let Some(payload) = Arc::get_mut(&mut self.payload) {
      payload.parent = Some(descriptor.parent);
      payload.green = descriptor.green;
      payload.index = descriptor.index;
      payload.offset = descriptor.offset;
      Some(self)
    } else {
      Some(descriptor.into_node())
    }
  }

  /// Return the next sibling node, skipping tokens.
  pub fn next_sibling(&self) -> Option<Self> {
    self.next_node_descriptor().map(NodeDescriptor::into_node)
  }

  /// Return the next sibling node whose kind satisfies `matcher`.
  pub fn next_sibling_by_kind(&self, matcher: &impl Fn(SyntaxKind) -> bool) -> Option<Self> {
    iter::successors(self.next_sibling(), Self::next_sibling).find(|node| matcher(node.kind()))
  }

  /// Return the previous sibling node, skipping tokens.
  pub fn prev_sibling(&self) -> Option<Self> {
    let parent = self.payload.parent.clone()?;
    parent
      .green
      .child_records()
      .iter()
      .enumerate()
      .take(self.payload.index)
      .rev()
      .find_map(|(index, child)| describe_node_child(&parent, index, child))
      .map(NodeDescriptor::into_node)
  }

  element_sibling_navigation!();

  /// Return the leftmost token in this subtree.
  pub fn first_token(&self) -> Option<SyntaxToken> {
    boundary_token(self.first_child_or_token(), Direction::Next)
  }

  /// Return the rightmost token in this subtree.
  pub fn last_token(&self) -> Option<SyntaxToken> {
    boundary_token(self.last_child_or_token(), Direction::Prev)
  }

  /// Iterate over this node and its siblings in `direction`.
  pub fn siblings(&self, direction: Direction) -> impl Iterator<Item = Self> + use<> {
    iter::successors(Some(self.clone()), move |node| match direction {
      Direction::Next => node.next_sibling(),
      Direction::Prev => node.prev_sibling(),
    })
  }

  /// Iterate over this element and its sibling elements in `direction`.
  pub fn siblings_with_tokens(&self, direction: Direction) -> impl Iterator<Item = SyntaxElement> + use<> {
    sibling_elements(SyntaxElement::from(self.clone()), direction)
  }

  /// Iterate over this node and every descendant node in preorder.
  pub fn descendants(&self) -> impl Iterator<Item = Self> + use<> {
    self.preorder().filter_map(|event| match event {
      WalkEvent::Enter(node) => Some(node),
      WalkEvent::Leave(_) => None,
    })
  }

  /// Iterate over this node and every descendant node or token in preorder.
  pub fn descendants_with_tokens(&self) -> impl Iterator<Item = SyntaxElement> + use<> {
    self.preorder_with_tokens().filter_map(|event| match event {
      WalkEvent::Enter(element) => Some(element),
      WalkEvent::Leave(_) => None,
    })
  }

  /// Traverse this subtree in node-only preorder.
  pub fn preorder(&self) -> Preorder {
    Preorder::new(self.clone())
  }

  /// Traverse this subtree in preorder including tokens.
  pub fn preorder_with_tokens(&self) -> PreorderWithTokens {
    PreorderWithTokens::new(self.clone())
  }

  /// Find the token or adjacent token pair touching an absolute offset.
  ///
  /// # Errors
  ///
  /// Returns [`RangeError::OffsetOutOfBounds`] when `offset` is outside this node's closed range.
  pub fn token_at_offset(&self, offset: TextSize) -> Result<TokenAtOffset<SyntaxToken>, RangeError> {
    let valid = self.text_range();
    if !valid.contains_inclusive(offset) {
      return Err(RangeError::OffsetOutOfBounds {
        offset,
        valid,
      });
    }
    if valid.is_empty() {
      return Ok(TokenAtOffset::None);
    }

    let mut touching = self
      .descendants_with_tokens()
      .filter_map(NodeOrToken::into_token)
      .filter(|token| {
        let token_range = token.text_range();
        !token_range.is_empty() && token_range.contains_inclusive(offset)
      });
    let first = touching.next();
    let second = touching.next();
    Ok(match (first, second) {
      (None, _) => TokenAtOffset::None,
      (Some(token), None) => TokenAtOffset::Single(token),
      (Some(left), Some(right)) => TokenAtOffset::Between(left, right),
    })
  }

  /// Return the deepest descendant that fully contains an absolute range.
  ///
  /// # Errors
  ///
  /// Returns [`RangeError::RangeOutOfBounds`] when `range` is not contained by this node.
  pub fn covering_element(&self, range: TextRange) -> Result<SyntaxElement, RangeError> {
    let valid = self.text_range();
    validate_query_range(valid, range)?;
    let mut current = SyntaxElement::from(self.clone());
    loop {
      current = match current {
        NodeOrToken::Token(token) => return Ok(NodeOrToken::Token(token)),
        NodeOrToken::Node(node) => match node.child_or_token_at_range(range)? {
          Some(child) => child,
          None => return Ok(NodeOrToken::Node(node)),
        },
      };
    }
  }

  /// Return the direct child that fully contains an absolute range.
  ///
  /// Valid ranges spanning multiple children return `Ok(None)`. Empty internal boundaries are
  /// right-biased; an empty range at this node's end selects its last child.
  ///
  /// # Errors
  ///
  /// Returns [`RangeError::RangeOutOfBounds`] when `range` is not contained by this node.
  pub fn child_or_token_at_range(&self, range: TextRange) -> Result<Option<SyntaxElement>, RangeError> {
    let valid = self.text_range();
    validate_query_range(valid, range)?;

    if range.is_empty() && range.start() == valid.end() {
      return Ok(self.last_child_or_token());
    }
    for child in self.children_with_tokens() {
      if query_selects_child(child.text_range(), range) {
        return Ok(Some(child));
      }
    }
    Ok(None)
  }

  /// Return an independent root cursor sharing this subtree's green allocation.
  pub fn clone_subtree(&self) -> Self {
    Self::new_root(self.payload.green.clone())
  }

  /// Functionally replace this node and rebuild its ancestor spine.
  ///
  /// # Errors
  ///
  /// Returns [`GreenError::KindMismatch`] before rebuilding when `replacement` changes this node's
  /// kind, or propagates a green construction failure.
  pub fn replace_with(&self, replacement: GreenNode) -> Result<GreenNode, GreenError> {
    if replacement.kind() != self.kind() {
      return Err(GreenError::KindMismatch {
        expected: self.kind(),
        actual:   replacement.kind(),
      });
    }
    let mut current = self.clone();
    let mut rebuilt = replacement;
    while let Some(parent) = current.parent() {
      rebuilt = parent.green().replace_child(current.index(), GreenElement::from(rebuilt))?;
      current = parent;
    }
    Ok(rebuilt)
  }

  /// Return the top green root allocation and complete child-index path for editor lookup.
  pub(crate) fn root_locator(&self) -> (GreenNode, Vec<usize>) {
    let mut current = self.clone();
    let mut reverse_path = Vec::new();
    while let Some(parent) = current.parent() {
      reverse_path.push(current.index());
      current = parent;
    }
    reverse_path.reverse();
    (current.green().clone(), reverse_path)
  }
}

impl SyntaxToken {
  /// Return this token's raw syntax kind.
  pub fn kind(&self) -> SyntaxKind {
    self.payload.green.kind()
  }

  /// Return this token's absolute source range.
  pub fn text_range(&self) -> TextRange {
    validated_text_range(self.payload.offset, self.payload.green.text_len())
  }

  /// Return this token's index in its parent.
  pub fn index(&self) -> usize {
    self.payload.index
  }

  /// Borrow this token's exact UTF-8 text.
  pub fn text(&self) -> &str {
    self.payload.green.text()
  }

  /// Borrow the immutable green token represented by this cursor.
  pub fn green(&self) -> &GreenToken {
    &self.payload.green
  }

  /// Return this token's parent node.
  pub fn parent(&self) -> Option<SyntaxNode> {
    self.payload.parent.clone().map(|payload| SyntaxNode {
      payload,
    })
  }

  /// Iterate over this token's parent and all higher ancestors.
  pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
    iter::successors(self.parent(), SyntaxNode::parent)
  }

  /// Describe the next direct sibling element.
  fn next_element_descriptor(&self) -> Option<ElementDescriptor> {
    sibling_element_descriptor(self.payload.parent.clone(), self.payload.index, Direction::Next)
  }

  element_sibling_navigation!();

  /// Iterate over this token and its sibling elements in `direction`.
  pub fn siblings_with_tokens(&self, direction: Direction) -> impl Iterator<Item = SyntaxElement> + use<> {
    sibling_elements(SyntaxElement::from(self.clone()), direction)
  }

  /// Return the next token in source order.
  pub fn next_token(&self) -> Option<Self> {
    adjacent_token(SyntaxElement::from(self.clone()), Direction::Next)
  }

  /// Return the previous token in source order.
  pub fn prev_token(&self) -> Option<Self> {
    adjacent_token(SyntaxElement::from(self.clone()), Direction::Prev)
  }

  /// Functionally replace this token and rebuild its ancestor spine.
  ///
  /// # Errors
  ///
  /// Returns [`GreenError::KindMismatch`] before rebuilding when `replacement` changes this
  /// token's kind, or propagates a green construction failure.
  pub fn replace_with(&self, replacement: GreenToken) -> Result<GreenNode, GreenError> {
    if replacement.kind() != self.kind() {
      return Err(GreenError::KindMismatch {
        expected: self.kind(),
        actual:   replacement.kind(),
      });
    }
    let parent = self.parent().ok_or(GreenError::ChildIndexOutOfBounds {
      index:       self.index(),
      child_count: 0,
    })?;
    let rebuilt = parent.green().replace_child(self.index(), GreenElement::from(replacement))?;
    parent.replace_with(rebuilt)
  }

  /// Return the top green root allocation and complete child-index path for editor lookup.
  pub(crate) fn root_locator(&self) -> Option<(GreenNode, Vec<usize>)> {
    let mut current = self.parent()?;
    let mut reverse_path = vec![self.index()];
    while let Some(parent) = current.parent() {
      reverse_path.push(current.index());
      current = parent;
    }
    reverse_path.reverse();
    Some((current.green().clone(), reverse_path))
  }
}

impl SyntaxElement {
  /// Return this element's absolute source range.
  pub fn text_range(&self) -> TextRange {
    self.either(SyntaxNode::text_range, SyntaxToken::text_range)
  }

  /// Return this element's index in its parent, or zero for a root node.
  pub fn index(&self) -> usize {
    self.either(SyntaxNode::index, SyntaxToken::index)
  }

  /// Return this element's raw syntax kind.
  pub fn kind(&self) -> SyntaxKind {
    self.either(SyntaxNode::kind, SyntaxToken::kind)
  }

  /// Return this element's parent node.
  pub fn parent(&self) -> Option<SyntaxNode> {
    self.either(SyntaxNode::parent, SyntaxToken::parent)
  }

  /// Iterate over this node and its ancestors, or a token's parent ancestors.
  pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> + use<> {
    let first = match self {
      NodeOrToken::Node(node) => Some(node.clone()),
      NodeOrToken::Token(token) => token.parent(),
    };
    iter::successors(first, SyntaxNode::parent)
  }

  /// Return this element's leftmost token.
  pub fn first_token(&self) -> Option<SyntaxToken> {
    self.either(SyntaxNode::first_token, |token| Some(token.clone()))
  }

  /// Return this element's rightmost token.
  pub fn last_token(&self) -> Option<SyntaxToken> {
    self.either(SyntaxNode::last_token, |token| Some(token.clone()))
  }

  /// Return the next direct sibling element.
  pub fn next_sibling_or_token(&self) -> Option<Self> {
    self.either(SyntaxNode::next_sibling_or_token, SyntaxToken::next_sibling_or_token)
  }

  /// Consume and advance this element, reusing a same-variant uniquely owned payload when possible.
  pub(crate) fn into_next_sibling_or_token(self) -> Option<Self> {
    let descriptor = match &self {
      NodeOrToken::Node(node) => node.next_element_descriptor(),
      NodeOrToken::Token(token) => token.next_element_descriptor(),
    }?;
    Some(advance_element(self, descriptor))
  }

  /// Return the next sibling element whose kind satisfies `matcher`.
  pub fn next_sibling_or_token_by_kind(&self, matcher: &impl Fn(SyntaxKind) -> bool) -> Option<Self> {
    iter::successors(self.next_sibling_or_token(), Self::next_sibling_or_token).find(|element| matcher(element.kind()))
  }

  /// Return the previous direct sibling element.
  pub fn prev_sibling_or_token(&self) -> Option<Self> {
    self.either(SyntaxNode::prev_sibling_or_token, SyntaxToken::prev_sibling_or_token)
  }

  /// Clone the represented immutable green handle.
  pub(crate) fn green_owned(&self) -> GreenElement {
    self.either(
      |node| GreenElement::from(node.green().clone()),
      |token| GreenElement::from(token.green().clone()),
    )
  }
}

impl PartialEq for SyntaxNode {
  fn eq(&self, other: &Self) -> bool {
    self.payload.offset == other.payload.offset && self.payload.green.ptr_eq(&other.payload.green)
  }
}

impl Eq for SyntaxNode {}

impl Hash for SyntaxNode {
  fn hash<HasherType: Hasher>(&self, state: &mut HasherType) {
    self.payload.green.hash_identity(state);
    self.payload.offset.hash(state);
  }
}

impl fmt::Debug for SyntaxNode {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    crate::debug::write(crate::debug::DebugTarget::SyntaxNode(self), formatter)
  }
}

impl fmt::Display for SyntaxNode {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    for event in self.preorder_with_tokens() {
      if let WalkEvent::Enter(NodeOrToken::Token(token)) = event {
        fmt::Display::fmt(&token, formatter)?;
      }
    }
    Ok(())
  }
}

impl PartialEq for SyntaxToken {
  fn eq(&self, other: &Self) -> bool {
    self.payload.offset == other.payload.offset && self.payload.green.ptr_eq(&other.payload.green)
  }
}

impl Eq for SyntaxToken {}

impl Hash for SyntaxToken {
  fn hash<HasherType: Hasher>(&self, state: &mut HasherType) {
    self.payload.green.hash_identity(state);
    self.payload.offset.hash(state);
  }
}

impl fmt::Debug for SyntaxToken {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    crate::debug::write(crate::debug::DebugTarget::SyntaxToken(self), formatter)
  }
}

impl fmt::Display for SyntaxToken {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Display::fmt(self.text(), formatter)
  }
}

impl From<SyntaxNode> for SyntaxElement {
  fn from(node: SyntaxNode) -> Self {
    NodeOrToken::Node(node)
  }
}

impl From<SyntaxToken> for SyntaxElement {
  fn from(token: SyntaxToken) -> Self {
    NodeOrToken::Token(token)
  }
}

#[cfg(test)]
mod tests {
  use std::hash::Hash;
  use std::iter;

  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;
  use strict_test_support::ensure_ne;
  use strict_test_support::ensure_ok;
  use strict_test_support::ensure_some;

  use super::RangeError;
  use super::SyntaxElement;
  use super::SyntaxNode;
  use super::SyntaxToken;
  use super::query_selects_child;
  use crate::Direction;
  use crate::GreenElement;
  use crate::GreenError;
  use crate::GreenNode;
  use crate::GreenToken;
  use crate::NodeOrToken;
  use crate::SyntaxKind;
  use crate::TextRange;
  use crate::TextSize;
  use crate::TokenAtOffset;
  use crate::test_support::ensure_one_word;
  use crate::test_support::ensure_same_hash;
  use crate::test_support::event_trace;

  /// Build a token fixture.
  fn token(kind: u16, text: &str) -> Result<GreenToken, TestFailure> {
    ensure_ok(GreenToken::new(SyntaxKind(kind), text), "the cursor token fixture must allocate")
  }

  /// Build a node fixture.
  fn node(kind: u16, children: impl IntoIterator<Item = GreenElement>) -> Result<GreenNode, TestFailure> {
    ensure_ok(GreenNode::new(SyntaxKind(kind), children), "the cursor node fixture must allocate")
  }

  /// Build the shared mixed tree used by cursor behavior tests.
  fn mixed_tree() -> Result<SyntaxNode, TestFailure> {
    let empty = node(4, std::iter::empty())?;
    let branch = node(2, [
      GreenElement::from(token(3, "β")?),
      GreenElement::from(empty.clone()),
      GreenElement::from(empty),
      GreenElement::from(token(1, "c")?),
    ])?;
    let root = node(0, [
      GreenElement::from(token(1, "a")?),
      GreenElement::from(branch),
      GreenElement::from(token(1, "d")?),
    ])?;
    Ok(SyntaxNode::new_root(root))
  }

  /// Return source-order kind labels for elements.
  fn element_kinds(elements: impl IntoIterator<Item = SyntaxElement>) -> Vec<SyntaxKind> {
    elements.into_iter().map(|element| element.kind()).collect()
  }

  #[test]
  fn raw_navigation_preserves_order_in_both_directions() -> Result<(), TestFailure> {
    let root = mixed_tree()?;
    let branch = ensure_some(root.first_child(), "the root must contain its branch node")?;
    ensure(branch.kind() == SyntaxKind(2), "node-only navigation must skip the first token")?;
    ensure(
      ensure_some(root.last_child(), "the root must have a last child node")?.kind() == SyntaxKind(2),
      "the branch must be both the first and last direct node",
    )?;
    ensure(
      root.first_child_by_kind(&|kind| kind == SyntaxKind(9)).is_none(),
      "a matcher miss must return None",
    )?;
    ensure(
      ensure_some(
        root.first_child_by_kind(&|kind| kind == SyntaxKind(2)),
        "the matcher must find the branch",
      )?
      .kind()
        == SyntaxKind(2),
      "a matcher hit must preserve the matched node",
    )?;

    let direct = element_kinds(root.children_with_tokens());
    ensure(
      direct == vec![SyntaxKind(1), SyntaxKind(2), SyntaxKind(1)],
      "direct element iteration must preserve token-node-token order",
    )?;
    let first = ensure_some(root.first_child_or_token(), "the root must have a first element")?;
    let forward = element_kinds(iter::successors(Some(first), SyntaxElement::next_sibling_or_token));
    ensure(forward == direct, "forward sibling iteration must include the starting element")?;
    let last = ensure_some(root.last_child_or_token(), "the root must have a final element")?;
    let reverse = element_kinds(iter::successors(Some(last), SyntaxElement::prev_sibling_or_token));
    ensure(
      reverse == vec![SyntaxKind(1), SyntaxKind(2), SyntaxKind(1)],
      "reverse sibling iteration must retain reverse source positions even when kinds repeat",
    )?;

    let branch_elements = branch.children_with_tokens().collect::<Vec<_>>();
    ensure_eq(&branch_elements.len(), &4, "the branch must expose both repeated empty nodes")?;
    let beta = ensure_some(
      branch_elements.first().cloned().and_then(NodeOrToken::into_token),
      "the branch must begin with beta",
    )?;
    let c = ensure_some(
      branch_elements.last().cloned().and_then(NodeOrToken::into_token),
      "the branch must end with c",
    )?;
    ensure_eq(
      &ensure_some(beta.next_token(), "beta must have a next token")?.text(),
      &"c",
      "next_token must cross zero-width nodes",
    )?;
    ensure_eq(
      &ensure_some(c.prev_token(), "c must have a previous token")?.text(),
      &"β",
      "prev_token must cross zero-width nodes",
    )?;
    ensure_eq(
      &ensure_some(c.next_token(), "c must advance out of its branch")?.text(),
      &"d",
      "next_token must cross an ancestor boundary",
    )?;
    ensure_eq(
      &ensure_some(beta.prev_token(), "beta must retreat out of its branch")?.text(),
      &"a",
      "prev_token must cross an ancestor boundary",
    )?;
    ensure_eq(&branch.ancestors().count(), &2, "branch ancestors must include branch and root")?;
    ensure(
      root.descendants().map(|node| node.kind()).collect::<Vec<_>>() == vec![SyntaxKind(0), SyntaxKind(2), SyntaxKind(4), SyntaxKind(4)],
      "node descendants must use preorder and omit tokens",
    )?;
    ensure(
      element_kinds(root.descendants_with_tokens())
        == vec![
          SyntaxKind(0),
          SyntaxKind(1),
          SyntaxKind(2),
          SyntaxKind(3),
          SyntaxKind(4),
          SyntaxKind(4),
          SyntaxKind(1),
          SyntaxKind(1),
        ],
      "element descendants must preserve complete preorder",
    )
  }

  /// Extract token text from a token-at-offset result.
  fn touching_texts(result: TokenAtOffset<SyntaxToken>) -> Vec<String> {
    result.map(|token| token.text().to_owned()).collect()
  }

  #[test]
  fn range_queries_enforce_closed_offsets_and_fixed_boundary_bias() -> Result<(), TestFailure> {
    let root = mixed_tree()?;
    ensure(
      touching_texts(ensure_ok(
        root.token_at_offset(TextSize::from(0)),
        "the start offset must be valid",
      )?)
        == vec!["a".to_owned()],
      "the start offset must touch only the first token",
    )?;
    ensure(
      touching_texts(ensure_ok(
        root.token_at_offset(TextSize::from(1)),
        "the first boundary must be valid",
      )?)
        == vec!["a".to_owned(), "β".to_owned()],
      "an inter-token boundary must return left then right",
    )?;
    ensure(
      touching_texts(ensure_ok(
        root.token_at_offset(TextSize::from(3)),
        "the inner boundary must be valid",
      )?)
        == vec!["β".to_owned(), "c".to_owned()],
      "zero-width nodes must not displace adjacent non-empty tokens",
    )?;
    ensure(
      touching_texts(ensure_ok(root.token_at_offset(TextSize::from(5)), "the end offset must be valid")?) == vec!["d".to_owned()],
      "the closed end offset must touch the final token",
    )?;
    ensure(
      root.token_at_offset(TextSize::from(6))
        == Err(RangeError::OffsetOutOfBounds {
          offset: TextSize::from(6),
          valid:  TextRange::new(TextSize::from(0), TextSize::from(5)),
        }),
      "an offset beyond the closed interval must be rejected",
    )?;

    let right_biased = ensure_some(
      ensure_ok(
        root.child_or_token_at_range(TextRange::empty(TextSize::from(1))),
        "the internal empty range must be valid",
      )?,
      "the internal empty range must select a direct child",
    )?;
    ensure(
      right_biased.kind() == SyntaxKind(2),
      "an internal empty range must prefer the right child",
    )?;
    let end_biased = ensure_some(
      ensure_ok(
        root.child_or_token_at_range(TextRange::empty(TextSize::from(5))),
        "the end empty range must be valid",
      )?,
      "the end empty range must select the final child",
    )?;
    ensure(end_biased.kind() == SyntaxKind(1), "the container end must select its final child")?;
    ensure(
      ensure_ok(
        root.child_or_token_at_range(TextRange::new(TextSize::from(0), TextSize::from(2))),
        "the spanning range must still be valid",
      )?
      .is_none(),
      "a range spanning direct children must return None",
    )?;
    let covered = ensure_ok(
      root.covering_element(TextRange::new(TextSize::from(1), TextSize::from(3))),
      "the beta range must be coverable",
    )?;
    ensure(
      covered.kind() == SyntaxKind(3),
      "covering_element must descend to the containing token",
    )?;
    ensure(
      root.child_or_token_at_range(TextRange::new(TextSize::from(0), TextSize::from(6)))
        == Err(RangeError::RangeOutOfBounds {
          requested: TextRange::new(TextSize::from(0), TextSize::from(6)),
          valid:     root.text_range(),
        }),
      "out-of-bounds ranges must be rejected",
    )?;

    let empty = SyntaxNode::new_root(node(8, std::iter::empty())?);
    ensure(
      ensure_ok(
        empty.token_at_offset(TextSize::from(0)),
        "an empty root's sole offset must be valid",
      )? == TokenAtOffset::None,
      "an empty root must not invent a token",
    )
  }

  #[test]
  fn cursor_identity_uses_green_allocation_and_absolute_offset() -> Result<(), TestFailure> {
    let root = mixed_tree()?;
    let reconstructed = SyntaxNode::new_root(root.green().clone());
    let separate = SyntaxNode::new_root(node(0, [
      GreenElement::from(token(1, "a")?),
      GreenElement::from(node(2, [
        GreenElement::from(token(3, "β")?),
        GreenElement::from(node(4, std::iter::empty())?),
        GreenElement::from(node(4, std::iter::empty())?),
        GreenElement::from(token(1, "c")?),
      ])?),
      GreenElement::from(token(1, "d")?),
    ])?);
    ensure_eq(
      &root,
      &reconstructed,
      "reconstruction from the same green root must preserve cursor identity",
    )?;
    ensure_ne(
      &root,
      &separate,
      "a structurally equal but separate green root must have different cursor identity",
    )?;

    let branch = ensure_some(root.first_child(), "the branch must exist")?;
    let empties = branch.children().collect::<Vec<_>>();
    let first_empty = ensure_some(empties.first(), "the first empty node must exist")?;
    let second_empty = ensure_some(empties.get(1), "the second empty node must exist")?;
    ensure_eq(
      first_empty,
      second_empty,
      "repeated zero-width nodes with shared green allocation and offset intentionally share public cursor identity",
    )?;

    let shared_token = token(1, "x")?;
    let repeated = SyntaxNode::new_root(node(9, [
      GreenElement::from(shared_token.clone()),
      GreenElement::from(shared_token),
    ])?);
    let repeated_tokens = repeated
      .children_with_tokens()
      .filter_map(NodeOrToken::into_token)
      .collect::<Vec<_>>();
    ensure_ne(
      ensure_some(repeated_tokens.first(), "the first repeated token must exist")?,
      ensure_some(repeated_tokens.get(1), "the second repeated token must exist")?,
      "the same token allocation at different absolute offsets must have different cursor identity",
    )?;

    ensure_same_hash(
      &root,
      &reconstructed,
      Hash::hash,
      "equal cursors must produce equal identity hashes",
    )
  }

  /// Assert thread-transfer traits through a generic bound.
  fn send_sync<Value: Send + Sync>() {}

  #[test]
  fn raw_cursor_handles_are_one_word_send_and_sync() -> Result<(), TestFailure> {
    send_sync::<SyntaxNode>();
    send_sync::<SyntaxToken>();
    ensure_one_word::<SyntaxNode>("a raw syntax node must remain one machine word")?;
    ensure_one_word::<SyntaxToken>("a raw syntax token must remain one machine word")
  }

  #[test]
  fn functional_replacement_rebuilds_only_the_ancestor_spine() -> Result<(), TestFailure> {
    let root = mixed_tree()?;
    let branch = ensure_some(root.first_child(), "the replaceable branch must exist")?;
    let replacement_branch = node(2, [GreenElement::from(token(3, "z")?)])?;
    let rebuilt = ensure_ok(
      branch.replace_with(replacement_branch.clone()),
      "a same-kind node replacement must rebuild the root",
    )?;
    ensure_eq(
      &rebuilt.to_string(),
      &"azd".to_owned(),
      "node replacement must preserve the unaffected context",
    )?;
    ensure(
      ensure_some(rebuilt.children().next(), "the rebuilt root's first child must exist")?
        .to_owned()
        .ptr_eq(&GreenElement::from(token_from_root(&root, "a")?.green().clone())),
      "the unchanged prefix token must retain allocation identity",
    )?;
    ensure_eq(
      &root.to_string(),
      &"aβcd".to_owned(),
      "replacement must leave the immutable source unchanged",
    )?;
    ensure(
      branch.replace_with(node(9, std::iter::empty())?)
        == Err(GreenError::KindMismatch {
          expected: SyntaxKind(2),
          actual:   SyntaxKind(9),
        }),
      "a node kind mismatch must fail before ancestor rebuilding",
    )?;

    let c = token_from_root(&root, "c")?;
    let replaced_token_root = ensure_ok(
      c.replace_with(token(1, "C")?),
      "a same-kind token replacement must rebuild every ancestor",
    )?;
    ensure_eq(
      &replaced_token_root.to_string(),
      &"aβCd".to_owned(),
      "token replacement must preserve all unaffected source text",
    )?;
    ensure(
      c.replace_with(token(9, "C")?)
        == Err(GreenError::KindMismatch {
          expected: SyntaxKind(1),
          actual:   SyntaxKind(9),
        }),
      "a token kind mismatch must fail before ancestor rebuilding",
    )
  }

  /// Find a token with exact text in one cursor tree.
  fn token_from_root(root: &SyntaxNode, text: &str) -> Result<SyntaxToken, TestFailure> {
    ensure_some(
      root
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|token| token.text() == text),
      "the requested token fixture must exist",
    )
  }

  #[test]
  fn preorder_emits_balanced_enter_leave_events() -> Result<(), TestFailure> {
    let root = mixed_tree()?;
    let events = event_trace(root.preorder(), |node| node.kind());
    ensure(
      events
        == vec![
          (true, SyntaxKind(0)),
          (true, SyntaxKind(2)),
          (true, SyntaxKind(4)),
          (false, SyntaxKind(4)),
          (true, SyntaxKind(4)),
          (false, SyntaxKind(4)),
          (false, SyntaxKind(2)),
          (false, SyntaxKind(0)),
        ],
      "node preorder must emit every balanced event in source order",
    )
  }

  /// Raw cursor fixture spanning node/token sibling boundaries.
  struct RawFixture {
    /// Root cursor.
    root:      SyntaxNode,
    /// First direct child node.
    left:      SyntaxNode,
    /// Last direct child node.
    right:     SyntaxNode,
    /// Intervening separator token.
    separator: SyntaxToken,
  }

  /// Build the shared raw cursor fixture.
  fn raw_fixture() -> Result<RawFixture, TestFailure> {
    let left_green = node(2, [GreenElement::from(token(1, "left")?)])?;
    let right_green = node(3, std::iter::empty())?;
    let root_green = node(0, [
      GreenElement::from(left_green),
      GreenElement::from(token(4, ":")?),
      GreenElement::from(right_green),
      GreenElement::from(token(5, "tail")?),
    ])?;
    let root = SyntaxNode::new_root(root_green);
    let left = ensure_some(root.first_child(), "the left child node must exist")?;
    let right = ensure_some(root.last_child(), "the right child node must exist")?;
    let separator = ensure_some(
      root
        .first_child_or_token_by_kind(&|kind| kind == SyntaxKind(4))
        .and_then(NodeOrToken::into_token),
      "the direct-element matcher must find the separator token",
    )?;
    Ok(RawFixture {
      root,
      left,
      right,
      separator,
    })
  }

  #[test]
  fn raw_node_sibling_navigation_covers_both_directions_and_misses() -> Result<(), TestFailure> {
    let RawFixture {
      left,
      right,
      ..
    } = raw_fixture()?;
    ensure(
      ensure_some(left.next_sibling(), "the left node must have a later node sibling")? == right,
      "next_sibling must skip the intervening token",
    )?;
    ensure(
      ensure_some(right.prev_sibling(), "the right node must have an earlier node sibling")? == left,
      "prev_sibling must skip the intervening token",
    )?;
    ensure(left.prev_sibling().is_none(), "the first node sibling must have no previous node")?;
    ensure(right.next_sibling().is_none(), "the last node sibling must have no next node")?;
    ensure(
      left.next_sibling_by_kind(&|kind| kind == SyntaxKind(3)) == Some(right.clone()),
      "kind-filtered node navigation must find a later matching sibling",
    )?;
    ensure(
      left.next_sibling_by_kind(&|kind| kind == SyntaxKind(9)).is_none(),
      "kind-filtered node navigation must return None on a miss",
    )?;
    ensure(
      left.siblings(Direction::Next).map(|node| node.kind()).collect::<Vec<_>>() == vec![SyntaxKind(2), SyntaxKind(3)],
      "forward node siblings must include the starting node and skip tokens",
    )?;
    ensure(
      right.siblings(Direction::Prev).map(|node| node.kind()).collect::<Vec<_>>() == vec![SyntaxKind(3), SyntaxKind(2)],
      "reverse node siblings must include the starting node in reverse order",
    )
  }

  #[test]
  fn raw_element_sibling_navigation_crosses_variants_and_preserves_order() -> Result<(), TestFailure> {
    let RawFixture {
      root,
      left,
      right,
      separator,
    } = raw_fixture()?;
    ensure(
      root.first_child_or_token_by_kind(&|kind| kind == SyntaxKind(9)).is_none(),
      "the direct-element matcher must return None on a miss",
    )?;
    ensure(
      left.next_sibling_or_token_by_kind(&|kind| kind == SyntaxKind(3)) == Some(SyntaxElement::from(right.clone())),
      "node element navigation must skip to a later matching variant",
    )?;
    ensure(
      separator.next_sibling_or_token_by_kind(&|kind| kind == SyntaxKind(5)).is_some(),
      "token element navigation must find a later matching sibling",
    )?;
    ensure(
      separator.next_sibling_or_token_by_kind(&|kind| kind == SyntaxKind(9)).is_none(),
      "token element navigation must return None on a miss",
    )?;
    ensure(
      separator
        .siblings_with_tokens(Direction::Prev)
        .map(|element| element.kind())
        .collect::<Vec<_>>()
        == vec![SyntaxKind(4), SyntaxKind(2)],
      "reverse token siblings must preserve direct reverse source order",
    )?;
    ensure(
      left
        .siblings_with_tokens(Direction::Next)
        .map(|element| element.kind())
        .collect::<Vec<_>>()
        == vec![SyntaxKind(2), SyntaxKind(4), SyntaxKind(3), SyntaxKind(5)],
      "forward node element siblings must cross variants in source order",
    )
  }

  #[test]
  fn raw_element_metadata_and_boundaries_preserve_variant_contracts() -> Result<(), TestFailure> {
    let RawFixture {
      root,
      left,
      separator,
      ..
    } = raw_fixture()?;
    let node_element = SyntaxElement::from(left.clone());
    let token_element = SyntaxElement::from(separator.clone());
    ensure(node_element.index() == 0, "node elements must expose their parent index")?;
    ensure(token_element.index() == 1, "token elements must expose their parent index")?;
    ensure(
      node_element.text_range() == left.text_range(),
      "node elements must preserve node ranges",
    )?;
    ensure(
      token_element.text_range() == separator.text_range(),
      "token elements must preserve token ranges",
    )?;
    ensure(
      node_element.parent() == Some(root.clone()),
      "node elements must preserve their parent",
    )?;
    ensure(
      token_element.parent() == Some(root.clone()),
      "token elements must preserve their parent",
    )?;
    ensure_eq(
      &node_element.ancestors().count(),
      &2,
      "node-element ancestors must begin with the node itself",
    )?;
    ensure_eq(
      &token_element.ancestors().count(),
      &1,
      "token-element ancestors must begin with the token parent",
    )?;
    ensure(
      ensure_some(node_element.first_token(), "the left node element must have a first token")?.text() == "left",
      "node-element first_token must descend into the node",
    )?;
    ensure(
      ensure_some(node_element.last_token(), "the left node element must have a last token")?.text() == "left",
      "node-element last_token must descend into the node",
    )?;
    ensure(
      (token_element.first_token(), token_element.last_token()) == (Some(separator.clone()), Some(separator.clone())),
      "token elements must return themselves as both boundary tokens",
    )?;
    ensure(
      token_element
        .green_owned()
        .ptr_eq(&GreenElement::from(separator.green().clone())),
      "token-element green cloning must preserve allocation identity",
    )?;
    ensure(
      node_element.green_owned().ptr_eq(&GreenElement::from(left.green().clone())),
      "node-element green cloning must preserve allocation identity",
    )?;
    ensure(
      token_element
        .next_sibling_or_token_by_kind(&|kind| kind == SyntaxKind(3))
        .is_some(),
      "element-level matching must delegate across the token-to-node boundary",
    )?;
    ensure(
      node_element.prev_sibling_or_token().is_none(),
      "the first node element must have no previous direct element",
    )
  }

  #[test]
  fn raw_cursor_formatting_hashing_and_empty_navigation_are_total() -> Result<(), TestFailure> {
    let RawFixture {
      root,
      separator,
      ..
    } = raw_fixture()?;
    ensure_eq(
      &separator.to_string(),
      &":".to_owned(),
      "raw token display must preserve exact text",
    )?;
    ensure(
      format!("{separator:?}").contains("SyntaxToken"),
      "raw token debug output must expose token metadata",
    )?;
    ensure(
      format!("{root:?}").contains("SyntaxNode"),
      "raw node debug output must expose node metadata",
    )?;
    let reconstructed_separator = ensure_some(
      SyntaxNode::new_root(root.green().clone())
        .children_with_tokens()
        .find_map(NodeOrToken::into_token),
      "the reconstructed separator token must exist",
    )?;
    ensure_same_hash(
      &separator,
      &reconstructed_separator,
      Hash::hash,
      "equal token cursors reconstructed from the same green root must hash equally",
    )?;

    let empty = SyntaxNode::new_root(node(8, std::iter::empty())?);
    ensure(empty.first_child().is_none(), "an empty node must have no first child node")?;
    ensure(empty.last_child().is_none(), "an empty node must have no last child node")?;
    ensure(empty.first_child_or_token().is_none(), "an empty node must have no first element")?;
    ensure(empty.last_child_or_token().is_none(), "an empty node must have no last element")?;
    ensure(empty.first_token().is_none(), "an empty node must have no first token")?;
    ensure(empty.last_token().is_none(), "an empty node must have no last token")
  }

  #[test]
  fn replacing_a_root_reuses_the_validated_replacement_without_an_ancestor_spine() -> Result<(), TestFailure> {
    let source = SyntaxNode::new_root(node(7, [GreenElement::from(token(1, "old")?)])?);
    let replacement = node(7, [GreenElement::from(token(1, "new")?)])?;
    let rebuilt = ensure_ok(
      source.replace_with(replacement.clone()),
      "a same-kind root replacement must succeed without requiring a parent",
    )?;
    ensure(
      rebuilt.ptr_eq(&replacement),
      "root replacement must return the validated replacement allocation directly",
    )?;
    ensure_eq(
      &rebuilt.to_string(),
      &"new".to_owned(),
      "root replacement must preserve replacement text",
    )?;
    ensure_eq(
      &source.to_string(),
      &"old".to_owned(),
      "root replacement must leave the immutable source reusable",
    )
  }

  #[test]
  fn zero_width_tokens_and_consuming_navigation_preserve_boundary_identity() -> Result<(), TestFailure> {
    let root = SyntaxNode::new_root(node(0, [
      GreenElement::from(token(1, "")?),
      GreenElement::from(token(1, "a")?),
      GreenElement::from(token(1, "b")?),
    ])?);
    let first = ensure_some(root.first_child_or_token(), "the zero-width leading token must exist")?;
    let second = ensure_some(
      first.into_next_sibling_or_token(),
      "consuming navigation must advance from the zero-width token",
    )?;
    let second_range = second.text_range();
    let second_index = second.index();
    let third = ensure_some(
      second.into_next_sibling_or_token(),
      "consuming navigation must advance between non-empty tokens",
    )?;

    let touching_start = touching_texts(ensure_ok(
      root.token_at_offset(TextSize::default()),
      "the root start must remain a valid closed-boundary query",
    )?);
    let touching_end = touching_texts(ensure_ok(
      root.token_at_offset(TextSize::from(2)),
      "the root end must remain a valid closed-boundary query",
    )?);
    let spanning = ensure_ok(
      root.covering_element(TextRange::new(TextSize::default(), TextSize::from(2))),
      "a range spanning both non-empty tokens must remain valid",
    )?;
    let outside = root.covering_element(TextRange::new(TextSize::default(), TextSize::from(3)));
    let selection_polarities = [
      query_selects_child(
        TextRange::new(TextSize::from(1), TextSize::from(3)),
        TextRange::empty(TextSize::from(1)),
      ),
      query_selects_child(
        TextRange::new(TextSize::from(5), TextSize::from(7)),
        TextRange::empty(TextSize::from(4)),
      ),
      query_selects_child(
        TextRange::new(TextSize::from(1), TextSize::from(5)),
        TextRange::empty(TextSize::from(3)),
      ),
      query_selects_child(
        TextRange::new(TextSize::from(1), TextSize::from(3)),
        TextRange::empty(TextSize::from(3)),
      ),
    ];

    let distinct_root = SyntaxNode::new_root(node(0, [GreenElement::from(token(1, "")?)])?);
    let distinct = ensure_some(distinct_root.first_token(), "the separately allocated zero-width token must exist")?;
    let original = ensure_some(root.first_token(), "the original zero-width token must remain reachable")?;

    ensure(
      (
        second_range,
        second_index,
        third.text_range(),
        third.index(),
        touching_start,
        touching_end,
        original == distinct,
        spanning.kind(),
        outside,
        selection_polarities,
      ) == (
        TextRange::new(TextSize::default(), TextSize::from(1)),
        1,
        TextRange::new(TextSize::from(1), TextSize::from(2)),
        2,
        vec!["a".to_owned()],
        vec!["b".to_owned()],
        false,
        SyntaxKind(0),
        Err(RangeError::RangeOutOfBounds {
          requested: TextRange::new(TextSize::default(), TextSize::from(3)),
          valid:     TextRange::new(TextSize::default(), TextSize::from(2)),
        }),
        [true, false, true, false],
      ),
      "zero-width filtering, consuming reuse, closed boundaries, and token allocation identity must remain independent",
    )
  }

  #[test]
  fn deep_cursor_traversal_and_formatting_do_not_recurse() -> Result<(), TestFailure> {
    let mut green = node(7, [GreenElement::from(token(1, "x")?)])?;
    for _ in 0..4_096 {
      green = node(7, [GreenElement::from(green)])?;
    }
    let root = SyntaxNode::new_root(green);
    ensure_eq(
      &root.descendants().count(),
      &4_097,
      "deep node traversal must visit every nested node",
    )?;
    ensure_eq(
      &root.to_string(),
      &"x".to_owned(),
      "deep cursor display must reach the token iteratively",
    )?;
    let leaf = ensure_some(root.first_token(), "the deep leaf token must be reachable")?;
    drop(root);
    drop(leaf);
    Ok(())
  }
}
