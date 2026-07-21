//! Strongly typed language wrappers over Rowan's raw cursor API.

use std::fmt;
use std::marker::PhantomData;
use std::ops::Range;

use crate::Direction;
use crate::EditError;
use crate::EditorElementId;
use crate::EditorNodeId;
use crate::EditorTokenId;
use crate::GreenElement;
use crate::GreenError;
use crate::GreenNode;
use crate::GreenToken;
use crate::NodeOrToken;
use crate::RangeError;
use crate::SpliceOutcome;
use crate::SyntaxKind;
use crate::SyntaxText;
use crate::TextRange;
use crate::TextSize;
use crate::TokenAtOffset;
use crate::TraversalError;
use crate::WalkEvent;
use crate::cursor;

/// Write one indentation level without nested formatting control flow.
fn write_indentation(formatter: &mut fmt::Formatter<'_>, level: usize) -> fmt::Result {
  for _ in 0..level {
    formatter.write_str("  ")?;
  }
  Ok(())
}

/// Language-specific conversion between raw and typed syntax kinds.
pub trait Language: Sized + Copy + fmt::Debug + Eq + Ord + std::hash::Hash {
  /// Language-specific syntax kind.
  type Kind: Sized + Copy + fmt::Debug + Eq + Ord + std::hash::Hash;

  /// Convert a raw Rowan kind into the language's kind.
  fn kind_from_raw(raw: SyntaxKind) -> Self::Kind;

  /// Convert the language's kind into a raw Rowan kind.
  fn kind_to_raw(kind: Self::Kind) -> SyntaxKind;
}

/// Define one typed one-word cursor over its raw storage variant.
macro_rules! define_typed_cursor {
  ($(#[$metadata:meta])* $name:ident => $raw:path) => {
    $(#[$metadata])*
    #[derive(Clone, PartialEq, Eq, Hash)]
    #[repr(transparent)]
    pub struct $name<L: Language> {
      /// Raw cursor carrying the storage and location identity.
      raw: $raw,
      /// Compile-time language marker.
      language: PhantomData<L>,
    }
  };
}

define_typed_cursor!(/// Typed immutable syntax-node cursor.
SyntaxNode => cursor::SyntaxNode);
define_typed_cursor!(/// Typed immutable syntax-token cursor.
SyntaxToken => cursor::SyntaxToken);

/// Typed syntax node or token.
pub type SyntaxElement<L> = NodeOrToken<SyntaxNode<L>, SyntaxToken<L>>;

/// Typed, exclusively borrowed transactional syntax editor.
pub struct SyntaxEditor<L: Language> {
  /// Raw editor containing all transactional state.
  raw:      cursor::SyntaxEditor,
  /// Compile-time language marker.
  language: PhantomData<L>,
}

impl<L: Language> fmt::Debug for SyntaxEditor<L> {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.debug_tuple("SyntaxEditor").field(&self.raw).finish()
  }
}

impl<L: Language> fmt::Debug for SyntaxNode<L> {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    if !formatter.alternate() {
      return write!(formatter, "{:?}@{:?}", self.kind(), self.text_range());
    }
    let mut level = 0_usize;
    for event in self.preorder_with_tokens() {
      match event {
        WalkEvent::Enter(element) => {
          write_indentation(formatter, level)?;
          match element {
            NodeOrToken::Node(node) => writeln!(formatter, "{:?}", node)?,
            NodeOrToken::Token(token) => writeln!(formatter, "{:?}", token)?,
          }
          level = level.checked_add(1).ok_or(fmt::Error)?;
        }
        WalkEvent::Leave(_) => {
          level = level.checked_sub(1).ok_or(fmt::Error)?;
        }
      }
    }
    if level == 0 { Ok(()) } else { Err(fmt::Error) }
  }
}

impl<L: Language> fmt::Display for SyntaxNode<L> {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Display::fmt(&self.raw, formatter)
  }
}

impl<L: Language> fmt::Debug for SyntaxToken<L> {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "{:?}@{:?}", self.kind(), self.text_range())?;
    let text = self.text();
    if text.len() < 25 {
      return write!(formatter, " {text:?}");
    }
    let boundary = text
      .char_indices()
      .map(|(byte_index, _)| byte_index)
      .take_while(|byte_index| *byte_index <= 21)
      .last()
      .map_or(0, std::convert::identity);
    match text.get(..boundary) {
      Some(prefix) => write!(formatter, " {prefix:?} ..."),
      None => Err(fmt::Error),
    }
  }
}

impl<L: Language> fmt::Display for SyntaxToken<L> {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Display::fmt(&self.raw, formatter)
  }
}

impl<L: Language> SyntaxNode<L> {
  /// Create a typed root cursor at absolute offset zero.
  pub fn new_root(green: GreenNode) -> Self {
    cursor::SyntaxNode::new_root(green).into()
  }

  /// Functionally replace this node and rebuild its ancestor spine.
  ///
  /// # Errors
  ///
  /// Returns a [`GreenError`] for a kind mismatch, checked length overflow, or allocation failure.
  pub fn replace_with(&self, replacement: GreenNode) -> Result<GreenNode, GreenError> {
    self.raw.replace_with(replacement)
  }

  /// Return this node's typed syntax kind.
  pub fn kind(&self) -> L::Kind {
    L::kind_from_raw(self.raw.kind())
  }

  /// Return this node's absolute source range.
  pub fn text_range(&self) -> TextRange {
    self.raw.text_range()
  }

  /// Return this node's index in its parent, or zero for a root.
  pub fn index(&self) -> usize {
    self.raw.index()
  }

  /// Return a chunked text view over this subtree.
  pub fn text(&self) -> SyntaxText {
    self.raw.text()
  }

  /// Borrow this cursor's immutable green node handle.
  pub fn green(&self) -> &GreenNode {
    self.raw.green()
  }

  /// Return this node's parent.
  pub fn parent(&self) -> Option<Self> {
    self.raw.parent().map(Self::from)
  }

  /// Iterate over this node and all ancestors.
  pub fn ancestors(&self) -> impl Iterator<Item = Self> + use<L> {
    self.raw.ancestors().map(Self::from)
  }

  /// Iterate over direct child nodes.
  pub fn children(&self) -> SyntaxNodeChildren<L> {
    SyntaxNodeChildren {
      raw:      self.raw.children(),
      language: PhantomData,
    }
  }

  /// Iterate over direct child nodes and tokens.
  pub fn children_with_tokens(&self) -> SyntaxElementChildren<L> {
    SyntaxElementChildren {
      raw:      self.raw.children_with_tokens(),
      language: PhantomData,
    }
  }

  /// Return the first direct child node.
  pub fn first_child(&self) -> Option<Self> {
    self.raw.first_child().map(Self::from)
  }

  /// Return the first direct child node whose kind satisfies `matcher`.
  pub fn first_child_by_kind(&self, matcher: &impl Fn(L::Kind) -> bool) -> Option<Self> {
    self
      .raw
      .first_child_by_kind(&|kind| matcher(L::kind_from_raw(kind)))
      .map(Self::from)
  }

  /// Return the last direct child node.
  pub fn last_child(&self) -> Option<Self> {
    self.raw.last_child().map(Self::from)
  }

  /// Return the first direct child element.
  pub fn first_child_or_token(&self) -> Option<SyntaxElement<L>> {
    self.raw.first_child_or_token().map(SyntaxElement::from)
  }

  /// Return the first direct child element whose kind satisfies `matcher`.
  pub fn first_child_or_token_by_kind(&self, matcher: &impl Fn(L::Kind) -> bool) -> Option<SyntaxElement<L>> {
    self
      .raw
      .first_child_or_token_by_kind(&|kind| matcher(L::kind_from_raw(kind)))
      .map(SyntaxElement::from)
  }

  /// Return the last direct child element.
  pub fn last_child_or_token(&self) -> Option<SyntaxElement<L>> {
    self.raw.last_child_or_token().map(SyntaxElement::from)
  }

  /// Return the next sibling node, skipping tokens.
  pub fn next_sibling(&self) -> Option<Self> {
    self.raw.next_sibling().map(Self::from)
  }

  /// Return the next sibling node whose kind satisfies `matcher`.
  pub fn next_sibling_by_kind(&self, matcher: &impl Fn(L::Kind) -> bool) -> Option<Self> {
    self
      .raw
      .next_sibling_by_kind(&|kind| matcher(L::kind_from_raw(kind)))
      .map(Self::from)
  }

  /// Return the previous sibling node, skipping tokens.
  pub fn prev_sibling(&self) -> Option<Self> {
    self.raw.prev_sibling().map(Self::from)
  }

  /// Return the next direct sibling element.
  pub fn next_sibling_or_token(&self) -> Option<SyntaxElement<L>> {
    self.raw.next_sibling_or_token().map(SyntaxElement::from)
  }

  /// Return the next sibling element whose kind satisfies `matcher`.
  pub fn next_sibling_or_token_by_kind(&self, matcher: &impl Fn(L::Kind) -> bool) -> Option<SyntaxElement<L>> {
    self
      .raw
      .next_sibling_or_token_by_kind(&|kind| matcher(L::kind_from_raw(kind)))
      .map(SyntaxElement::from)
  }

  /// Return the previous direct sibling element.
  pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement<L>> {
    self.raw.prev_sibling_or_token().map(SyntaxElement::from)
  }

  /// Return the leftmost token in this subtree.
  pub fn first_token(&self) -> Option<SyntaxToken<L>> {
    self.raw.first_token().map(SyntaxToken::from)
  }

  /// Return the rightmost token in this subtree.
  pub fn last_token(&self) -> Option<SyntaxToken<L>> {
    self.raw.last_token().map(SyntaxToken::from)
  }

  /// Iterate over this node and its sibling nodes in `direction`.
  pub fn siblings(&self, direction: Direction) -> impl Iterator<Item = Self> + use<L> {
    self.raw.siblings(direction).map(Self::from)
  }

  /// Iterate over this element and its sibling elements in `direction`.
  pub fn siblings_with_tokens(&self, direction: Direction) -> impl Iterator<Item = SyntaxElement<L>> + use<L> {
    self.raw.siblings_with_tokens(direction).map(SyntaxElement::from)
  }

  /// Iterate over this node and every descendant node in preorder.
  pub fn descendants(&self) -> impl Iterator<Item = Self> + use<L> {
    self.raw.descendants().map(Self::from)
  }

  /// Iterate over this node and every descendant element in preorder.
  pub fn descendants_with_tokens(&self) -> impl Iterator<Item = SyntaxElement<L>> + use<L> {
    self.raw.descendants_with_tokens().map(SyntaxElement::from)
  }

  /// Traverse this subtree in node-only preorder.
  pub fn preorder(&self) -> Preorder<L> {
    Preorder {
      raw:      self.raw.preorder(),
      language: PhantomData,
    }
  }

  /// Traverse this subtree in preorder including tokens.
  pub fn preorder_with_tokens(&self) -> PreorderWithTokens<L> {
    PreorderWithTokens {
      raw:      self.raw.preorder_with_tokens(),
      language: PhantomData,
    }
  }

  /// Find the token or adjacent token pair touching an absolute offset.
  ///
  /// # Errors
  ///
  /// Returns [`RangeError::OffsetOutOfBounds`] when `offset` is outside this node's closed range.
  pub fn token_at_offset(&self, offset: TextSize) -> Result<TokenAtOffset<SyntaxToken<L>>, RangeError> {
    self.raw.token_at_offset(offset).map(|tokens| tokens.map(SyntaxToken::from))
  }

  /// Return the deepest descendant fully containing an absolute range.
  ///
  /// # Errors
  ///
  /// Returns a [`RangeError`] when `range` is invalid for this subtree.
  pub fn covering_element(&self, range: TextRange) -> Result<SyntaxElement<L>, RangeError> {
    self.raw.covering_element(range).map(SyntaxElement::from)
  }

  /// Return the direct child fully containing an absolute range.
  ///
  /// # Errors
  ///
  /// Returns a [`RangeError`] when `range` is invalid for this subtree.
  pub fn child_or_token_at_range(&self, range: TextRange) -> Result<Option<SyntaxElement<L>>, RangeError> {
    self
      .raw
      .child_or_token_at_range(range)
      .map(|element| element.map(SyntaxElement::from))
  }

  /// Return an independent root cursor sharing this subtree's green allocation.
  pub fn clone_subtree(&self) -> Self {
    self.raw.clone_subtree().into()
  }
}

impl<L: Language> SyntaxToken<L> {
  /// Functionally replace this token and rebuild its ancestor spine.
  ///
  /// # Errors
  ///
  /// Returns a [`GreenError`] for a kind mismatch, checked length overflow, or allocation failure.
  pub fn replace_with(&self, replacement: GreenToken) -> Result<GreenNode, GreenError> {
    self.raw.replace_with(replacement)
  }

  /// Return this token's typed syntax kind.
  pub fn kind(&self) -> L::Kind {
    L::kind_from_raw(self.raw.kind())
  }

  /// Return this token's absolute source range.
  pub fn text_range(&self) -> TextRange {
    self.raw.text_range()
  }

  /// Return this token's index in its parent.
  pub fn index(&self) -> usize {
    self.raw.index()
  }

  /// Borrow this token's exact UTF-8 text.
  pub fn text(&self) -> &str {
    self.raw.text()
  }

  /// Borrow this cursor's immutable green token handle.
  pub fn green(&self) -> &GreenToken {
    self.raw.green()
  }

  /// Return this token's parent node.
  pub fn parent(&self) -> Option<SyntaxNode<L>> {
    self.raw.parent().map(SyntaxNode::from)
  }

  /// Iterate over this token's parent and all higher ancestors.
  pub fn parent_ancestors(&self) -> impl Iterator<Item = SyntaxNode<L>> + use<L> {
    self.raw.ancestors().map(SyntaxNode::from)
  }

  /// Return the next direct sibling element.
  pub fn next_sibling_or_token(&self) -> Option<SyntaxElement<L>> {
    self.raw.next_sibling_or_token().map(SyntaxElement::from)
  }

  /// Return the previous direct sibling element.
  pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement<L>> {
    self.raw.prev_sibling_or_token().map(SyntaxElement::from)
  }

  /// Iterate over this token and its sibling elements in `direction`.
  pub fn siblings_with_tokens(&self, direction: Direction) -> impl Iterator<Item = SyntaxElement<L>> + use<L> {
    self.raw.siblings_with_tokens(direction).map(SyntaxElement::from)
  }

  /// Return the next token in source order.
  pub fn next_token(&self) -> Option<Self> {
    self.raw.next_token().map(Self::from)
  }

  /// Return the previous token in source order.
  pub fn prev_token(&self) -> Option<Self> {
    self.raw.prev_token().map(Self::from)
  }
}

impl<L: Language> SyntaxElement<L> {
  /// Clone the one-word raw handle behind either typed element variant.
  fn raw_clone(&self) -> cursor::SyntaxElement {
    match self {
      NodeOrToken::Node(node) => cursor::SyntaxElement::Node(node.raw.clone()),
      NodeOrToken::Token(token) => cursor::SyntaxElement::Token(token.raw.clone()),
    }
  }

  /// Return this element's absolute source range.
  pub fn text_range(&self) -> TextRange {
    self.raw_clone().text_range()
  }

  /// Return this element's index in its parent, or zero for a root node.
  pub fn index(&self) -> usize {
    self.raw_clone().index()
  }

  /// Return this element's typed syntax kind.
  pub fn kind(&self) -> L::Kind {
    L::kind_from_raw(self.raw_clone().kind())
  }

  /// Return this element's parent node.
  pub fn parent(&self) -> Option<SyntaxNode<L>> {
    self.raw_clone().parent().map(SyntaxNode::from)
  }

  /// Iterate over this node and ancestors, or a token's parent ancestors.
  pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode<L>> + use<L> {
    self.raw_clone().ancestors().map(SyntaxNode::from)
  }

  /// Return this element's leftmost token.
  pub fn first_token(&self) -> Option<SyntaxToken<L>> {
    self.raw_clone().first_token().map(SyntaxToken::from)
  }

  /// Return this element's rightmost token.
  pub fn last_token(&self) -> Option<SyntaxToken<L>> {
    self.raw_clone().last_token().map(SyntaxToken::from)
  }

  /// Return the next direct sibling element.
  pub fn next_sibling_or_token(&self) -> Option<Self> {
    self.raw_clone().next_sibling_or_token().map(Self::from)
  }

  /// Return the previous direct sibling element.
  pub fn prev_sibling_or_token(&self) -> Option<Self> {
    self.raw_clone().prev_sibling_or_token().map(Self::from)
  }
}

/// Typed direct child-node iterator.
#[derive(Debug, Clone)]
pub struct SyntaxNodeChildren<L: Language> {
  /// Raw child iterator.
  raw:      cursor::SyntaxNodeChildren,
  /// Compile-time language marker.
  language: PhantomData<L>,
}

impl<L: Language> Iterator for SyntaxNodeChildren<L> {
  type Item = SyntaxNode<L>;

  fn next(&mut self) -> Option<Self::Item> {
    self.raw.next().map(SyntaxNode::from)
  }
}

impl<L: Language> SyntaxNodeChildren<L> {
  /// Keep only nodes whose typed kind satisfies `matcher`.
  pub fn by_kind(self, matcher: impl Fn(L::Kind) -> bool) -> impl Iterator<Item = SyntaxNode<L>> {
    self.filter(move |node| matcher(node.kind()))
  }
}

/// Typed direct child-element iterator.
#[derive(Debug, Clone)]
pub struct SyntaxElementChildren<L: Language> {
  /// Raw child iterator.
  raw:      cursor::SyntaxElementChildren,
  /// Compile-time language marker.
  language: PhantomData<L>,
}

impl<L: Language> Iterator for SyntaxElementChildren<L> {
  type Item = SyntaxElement<L>;

  fn next(&mut self) -> Option<Self::Item> {
    self.raw.next().map(SyntaxElement::from)
  }
}

impl<L: Language> SyntaxElementChildren<L> {
  /// Keep only elements whose typed kind satisfies `matcher`.
  pub fn by_kind(self, matcher: impl Fn(L::Kind) -> bool) -> impl Iterator<Item = SyntaxElement<L>> {
    self.filter(move |element| matcher(element.kind()))
  }
}

/// Define one typed preorder wrapper with shared skip delegation.
macro_rules! define_typed_preorder {
  ($(#[$metadata:meta])* $name:ident => $raw:path) => {
    $(#[$metadata])*
    #[derive(Debug, Clone)]
    pub struct $name<L: Language> {
      /// Raw preorder iterator.
      raw: $raw,
      /// Compile-time language marker.
      language: PhantomData<L>,
    }

    impl<L: Language> $name<L> {
      /// Skip descendants of the node most recently entered.
      ///
      /// # Errors
      ///
      /// Returns a [`TraversalError`] when no node-entry window is active or a skip was already
      /// requested.
      pub fn skip_subtree(&mut self) -> Result<(), TraversalError> {
        self.raw.skip_subtree()
      }
    }
  };
}

define_typed_preorder!(/// Typed node-only preorder traversal.
Preorder => cursor::Preorder);

impl<L: Language> Iterator for Preorder<L> {
  type Item = WalkEvent<SyntaxNode<L>>;

  fn next(&mut self) -> Option<Self::Item> {
    self.raw.next().map(|event| event.map(SyntaxNode::from))
  }
}

define_typed_preorder!(/// Typed preorder traversal including tokens.
PreorderWithTokens => cursor::PreorderWithTokens);

impl<L: Language> Iterator for PreorderWithTokens<L> {
  type Item = WalkEvent<SyntaxElement<L>>;

  fn next(&mut self) -> Option<Self::Item> {
    self.raw.next().map(|event| event.map(SyntaxElement::from))
  }
}

impl<L: Language> SyntaxEditor<L> {
  /// Materialize a typed immutable source subtree into a transactional editor.
  pub fn new(root: &SyntaxNode<L>) -> Self {
    Self {
      raw:      cursor::SyntaxEditor::new(&root.raw),
      language: PhantomData,
    }
  }

  /// Return the original source subtree root ID.
  pub fn root_id(&self) -> EditorNodeId {
    self.raw.root_id()
  }

  /// Resolve an original source node.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::UnknownSourceElement`] outside the original source subtree.
  pub fn node_id(&self, source: &SyntaxNode<L>) -> Result<EditorNodeId, EditError> {
    self.raw.node_id(&source.raw)
  }

  /// Resolve an original source token.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::UnknownSourceElement`] outside the original source subtree.
  pub fn token_id(&self, source: &SyntaxToken<L>) -> Result<EditorTokenId, EditError> {
    self.raw.token_id(&source.raw)
  }

  /// Import an immutable cursor subtree as a detached component.
  ///
  /// # Errors
  ///
  /// Returns an [`EditError`] if materialization fails.
  pub fn import(&mut self, element: SyntaxElement<L>) -> Result<EditorElementId, EditError> {
    self.raw.import(cursor::SyntaxElement::from(element))
  }

  /// Import an immutable green subtree as a detached component.
  ///
  /// # Errors
  ///
  /// Returns an [`EditError`] if materialization fails.
  pub fn import_green(&mut self, element: GreenElement) -> Result<EditorElementId, EditError> {
    self.raw.import_green(element)
  }

  /// Return an element's typed syntax kind.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID from another editor.
  pub fn kind(&self, element: &EditorElementId) -> Result<L::Kind, EditError> {
    self.raw.kind(element).map(L::kind_from_raw)
  }

  /// Return an element's current parent.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID from another editor.
  pub fn parent(&self, element: &EditorElementId) -> Result<Option<EditorNodeId>, EditError> {
    self.raw.parent(element)
  }

  /// Return an element's current index, or `None` for a component root.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID from another editor.
  pub fn index(&self, element: &EditorElementId) -> Result<Option<usize>, EditError> {
    self.raw.index(element)
  }

  /// Clone a node's current child IDs.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID from another editor.
  pub fn children(&self, node: &EditorNodeId) -> Result<Vec<EditorElementId>, EditError> {
    self.raw.children(node)
  }

  /// Return an element's current UTF-8 byte length.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID from another editor.
  pub fn text_len(&self, element: &EditorElementId) -> Result<TextSize, EditError> {
    self.raw.text_len(element)
  }

  /// Return an element's component-relative text range.
  ///
  /// # Errors
  ///
  /// Returns a typed ownership or checked-range error.
  pub fn text_range(&self, element: &EditorElementId) -> Result<TextRange, EditError> {
    self.raw.text_range(element)
  }

  /// Borrow an editor token's exact text.
  ///
  /// # Errors
  ///
  /// Returns [`EditError::ForeignId`] for an ID from another editor.
  pub fn token_text(&self, token: &EditorTokenId) -> Result<&str, EditError> {
    self.raw.token_text(token)
  }

  /// Detach an attached element.
  ///
  /// # Errors
  ///
  /// Returns a typed ownership or transactional validation error without partial mutation.
  pub fn detach(&mut self, element: &EditorElementId) -> Result<bool, EditError> {
    self.raw.detach(element)
  }

  /// Transactionally splice a node's original child sequence.
  ///
  /// # Errors
  ///
  /// Returns a typed ownership, range, duplicate, cycle, or checked-length error without partial
  /// mutation.
  pub fn splice_children(
    &mut self,
    target: &EditorNodeId,
    range: Range<usize>,
    insertions: impl IntoIterator<Item = EditorElementId>,
  ) -> Result<SpliceOutcome, EditError> {
    self.raw.splice_children(target, range, insertions)
  }

  /// Consume the editor and materialize one detached node component.
  ///
  /// # Errors
  ///
  /// Returns a typed ownership, attachment, or green reconstruction error.
  pub fn into_syntax(self, root: &EditorNodeId) -> Result<SyntaxNode<L>, EditError> {
    self.raw.into_syntax(root).map(SyntaxNode::from)
  }
}

impl<L: Language> From<cursor::SyntaxNode> for SyntaxNode<L> {
  fn from(raw: cursor::SyntaxNode) -> Self {
    Self {
      raw,
      language: PhantomData,
    }
  }
}

impl<L: Language> From<SyntaxNode<L>> for cursor::SyntaxNode {
  fn from(node: SyntaxNode<L>) -> Self {
    node.raw
  }
}

impl<L: Language> From<cursor::SyntaxToken> for SyntaxToken<L> {
  fn from(raw: cursor::SyntaxToken) -> Self {
    Self {
      raw,
      language: PhantomData,
    }
  }
}

impl<L: Language> From<SyntaxToken<L>> for cursor::SyntaxToken {
  fn from(token: SyntaxToken<L>) -> Self {
    token.raw
  }
}

impl<L: Language> From<cursor::SyntaxElement> for SyntaxElement<L> {
  fn from(element: cursor::SyntaxElement) -> Self {
    element.map(SyntaxNode::from, SyntaxToken::from)
  }
}

impl<L: Language> From<SyntaxElement<L>> for cursor::SyntaxElement {
  fn from(element: SyntaxElement<L>) -> Self {
    element.map(cursor::SyntaxNode::from, cursor::SyntaxToken::from)
  }
}

impl<L: Language> From<SyntaxNode<L>> for SyntaxElement<L> {
  fn from(node: SyntaxNode<L>) -> Self {
    NodeOrToken::Node(node)
  }
}

impl<L: Language> From<SyntaxToken<L>> for SyntaxElement<L> {
  fn from(token: SyntaxToken<L>) -> Self {
    NodeOrToken::Token(token)
  }
}

#[cfg(test)]
mod tests {
  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;
  use strict_test_support::ensure_ok;
  use strict_test_support::ensure_some;

  use super::Language;
  use super::SyntaxEditor;
  use super::SyntaxElement;
  use super::SyntaxNode;
  use super::SyntaxToken;
  use crate::Direction;
  use crate::GreenElement;
  use crate::GreenError;
  use crate::GreenNode;
  use crate::GreenToken;
  use crate::SyntaxKind;
  use crate::TextRange;
  use crate::TextSize;
  use crate::TokenAtOffset;
  use crate::TraversalError;
  use crate::WalkEvent;
  use crate::cursor;
  use crate::test_support::TestLanguage;
  use crate::test_support::ensure_one_word;
  use crate::test_support::typed_token_root;

  /// Build the typed tree `root(token a, branch(token b), token c)`.
  fn typed_tree() -> Result<SyntaxNode<TestLanguage>, TestFailure> {
    let a = ensure_ok(GreenToken::new(SyntaxKind(1), "a"), "typed token a must allocate")?;
    let b = ensure_ok(GreenToken::new(SyntaxKind(1), "b"), "typed token b must allocate")?;
    let c = ensure_ok(GreenToken::new(SyntaxKind(1), "c"), "typed token c must allocate")?;
    let branch = ensure_ok(
      GreenNode::new(SyntaxKind(2), [GreenElement::from(b)]),
      "the typed branch must allocate",
    )?;
    let root = ensure_ok(
      GreenNode::new(SyntaxKind(0), [
        GreenElement::from(a),
        GreenElement::from(branch),
        GreenElement::from(c),
      ]),
      "the typed root must allocate",
    )?;
    Ok(SyntaxNode::new_root(root))
  }

  /// Extract a token from a typed element.
  fn typed_token(element: SyntaxElement<TestLanguage>) -> Result<SyntaxToken<TestLanguage>, TestFailure> {
    ensure_some(element.into_token(), "the typed element must be a token")
  }

  #[test]
  fn typed_cursor_root_and_child_navigation_match_raw_semantics() -> Result<(), TestFailure> {
    let root = typed_tree()?;
    ensure(root.kind() == SyntaxKind(0), "typed kind conversion must preserve the root kind")?;
    ensure(
      root.text_range() == TextRange::new(TextSize::from(0), TextSize::from(3)),
      "typed ranges must match raw ranges",
    )?;
    ensure_eq(&root.index(), &0, "a typed root index must be zero")?;
    ensure_eq(
      &root.text().to_string(),
      &"abc".to_owned(),
      "typed text must retain lossless source",
    )?;
    ensure(
      root.green().kind() == SyntaxKind(0),
      "typed green access must borrow the root handle",
    )?;
    ensure(root.parent().is_none(), "a typed root must have no parent")?;
    ensure_eq(&root.ancestors().count(), &1, "typed root ancestors must include the root")?;

    let branch = ensure_some(root.first_child(), "typed node navigation must find the branch")?;
    ensure(branch.kind() == SyntaxKind(2), "typed node navigation must skip direct tokens")?;
    ensure_eq(
      &ensure_some(root.last_child(), "the typed last child must exist")?,
      &branch,
      "the sole child node must be both first and last",
    )?;
    ensure(
      root.first_child_by_kind(&|kind| kind == SyntaxKind(9)).is_none(),
      "typed matcher misses must return None",
    )?;
    ensure(
      root.first_child_by_kind(&|kind| kind == SyntaxKind(2)).is_some(),
      "typed matcher hits must return the node",
    )?;
    ensure_eq(&root.children().count(), &1, "typed child iteration must expose direct nodes")?;
    ensure_eq(
      &root.children().by_kind(|kind| kind == SyntaxKind(2)).count(),
      &1,
      "typed child filtering must use typed kinds",
    )?;
    ensure_eq(
      &root.children_with_tokens().count(),
      &3,
      "typed element iteration must preserve all children",
    )?;
    ensure_eq(
      &root.children_with_tokens().by_kind(|kind| kind == SyntaxKind(1)).count(),
      &2,
      "typed element filtering must preserve matching tokens",
    )
  }

  #[test]
  fn typed_token_navigation_matches_raw_semantics() -> Result<(), TestFailure> {
    let root = typed_tree()?;
    let first = typed_token(ensure_some(root.first_child_or_token(), "the typed first element must exist")?)?;
    let last = typed_token(ensure_some(root.last_child_or_token(), "the typed final element must exist")?)?;
    ensure_eq(&first.text(), &"a", "the typed first token text must be exact")?;
    ensure_eq(&last.text(), &"c", "the typed final token text must be exact")?;
    ensure(first.kind() == SyntaxKind(1), "typed token kinds must convert from raw")?;
    ensure_eq(&first.index(), &0, "typed token indices must preserve source position")?;
    ensure_eq(&first.green().text(), &"a", "typed token green access must borrow the handle")?;
    ensure(first.parent().is_some(), "a direct typed token must have a parent")?;
    ensure_eq(
      &first.parent_ancestors().count(),
      &1,
      "typed token ancestors must begin at its parent",
    )?;
    ensure(
      ensure_some(first.next_sibling_or_token(), "the typed first token must have a sibling")?.kind() == SyntaxKind(2),
      "typed next-element navigation must cross variants",
    )?;
    ensure(
      ensure_some(last.prev_sibling_or_token(), "the typed final token must have a sibling")?.kind() == SyntaxKind(2),
      "typed previous-element navigation must cross variants",
    )?;
    ensure_eq(
      &first.siblings_with_tokens(Direction::Next).count(),
      &3,
      "typed sibling iteration must include its start",
    )?;
    ensure_eq(
      &ensure_some(first.next_token(), "the first typed token must advance")?.text(),
      &"b",
      "typed token navigation must enter nodes",
    )?;
    ensure_eq(
      &ensure_some(last.prev_token(), "the final typed token must retreat")?.text(),
      &"b",
      "typed token navigation must leave nodes",
    )?;
    ensure(
      root.first_token() == Some(first.clone()),
      "typed first_token must find the leftmost token",
    )?;
    ensure(
      root.last_token() == Some(last.clone()),
      "typed last_token must find the rightmost token",
    )?;
    ensure_eq(&root.descendants().count(), &2, "typed descendants must include root and branch")?;
    ensure_eq(
      &root.descendants_with_tokens().count(),
      &5,
      "typed element descendants must include nodes and tokens",
    )
  }

  #[test]
  fn typed_queries_formatting_and_traversal_match_raw_semantics() -> Result<(), TestFailure> {
    let root = typed_tree()?;
    let touching = ensure_ok(root.token_at_offset(TextSize::from(1)), "typed boundary lookup must validate")?;
    ensure(
      matches!(touching, TokenAtOffset::Between(_, _)),
      "typed boundary lookup must preserve left-to-right polarity",
    )?;
    ensure(
      ensure_some(
        ensure_ok(
          root.child_or_token_at_range(TextRange::new(TextSize::from(1), TextSize::from(2))),
          "typed direct-child range lookup must validate",
        )?,
        "the branch range must select a child",
      )?
      .kind()
        == SyntaxKind(2),
      "typed direct-child queries must preserve raw selection",
    )?;
    ensure(
      ensure_ok(
        root.covering_element(TextRange::new(TextSize::from(1), TextSize::from(2))),
        "typed covering lookup must validate",
      )?
      .kind()
        == SyntaxKind(1),
      "typed covering queries must descend to the token",
    )?;
    ensure(root.clone_subtree().parent().is_none(), "typed subtree cloning must produce a root")?;
    ensure_eq(
      &format!("{root:#?}"),
      &concat!(
        "SyntaxKind(0)@0..3\n",
        "  SyntaxKind(1)@0..1 \"a\"\n",
        "  SyntaxKind(2)@1..2\n",
        "    SyntaxKind(1)@1..2 \"b\"\n",
        "  SyntaxKind(1)@2..3 \"c\"\n",
      )
      .to_owned(),
      "typed alternate debug must render the complete tree with checked indentation",
    )?;

    let events = root.preorder().collect::<Vec<_>>();
    ensure(
      matches!(events.first(), Some(WalkEvent::Enter(node)) if node.kind() == SyntaxKind(0)),
      "typed preorder must enter the root first",
    )?;
    let token_events = root.preorder_with_tokens().count();
    ensure_eq(
      &token_events,
      &10,
      "typed element preorder must emit balanced events for five elements",
    )
  }

  /// Assert thread-transfer traits through a generic bound.
  fn send_sync<Value: Send + Sync>() {}

  #[test]
  fn typed_handles_remain_one_word_send_and_sync() -> Result<(), TestFailure> {
    send_sync::<SyntaxNode<TestLanguage>>();
    send_sync::<SyntaxToken<TestLanguage>>();
    ensure_one_word::<SyntaxNode<TestLanguage>>("a typed node handle must remain one machine word")?;
    ensure_one_word::<SyntaxToken<TestLanguage>>("a typed token handle must remain one machine word")
  }

  #[test]
  fn typed_node_wrappers_cover_matching_and_sibling_polarities() -> Result<(), TestFailure> {
    ensure(
      TestLanguage::kind_to_raw(SyntaxKind(7)) == SyntaxKind(7),
      "language conversion must preserve typed kinds in both directions",
    )?;
    let root = typed_tree()?;
    ensure_eq(&root.to_string(), &"abc".to_owned(), "typed node display must preserve source text")?;
    let branch = ensure_some(root.first_child(), "the typed branch must exist")?;
    ensure(
      branch.next_sibling().is_none(),
      "the sole direct child node must have no next node sibling",
    )?;
    ensure(
      branch.prev_sibling().is_none(),
      "the sole direct child node must have no previous node sibling",
    )?;
    ensure(
      branch.next_sibling_by_kind(&|kind| kind == SyntaxKind(9)).is_none(),
      "typed node matcher navigation must return None when no later node matches",
    )?;
    ensure_eq(
      &branch.siblings(Direction::Next).count(),
      &1,
      "typed node sibling iteration must include its starting node",
    )?;
    ensure_eq(
      &branch.siblings_with_tokens(Direction::Prev).count(),
      &2,
      "typed reverse element siblings must cross from the branch to the leading token",
    )?;
    ensure(
      ensure_some(branch.next_sibling_or_token(), "the branch must have a trailing token")?.kind() == SyntaxKind(1),
      "typed next-element navigation must cross from a node to a token",
    )?;
    ensure(
      ensure_some(branch.prev_sibling_or_token(), "the branch must have a leading token")?.kind() == SyntaxKind(1),
      "typed previous-element navigation must cross from a node to a token",
    )?;
    ensure(
      branch.next_sibling_or_token_by_kind(&|kind| kind == SyntaxKind(1)).is_some(),
      "typed element matcher navigation must find the trailing token",
    )?;
    ensure(
      root.first_child_or_token_by_kind(&|kind| kind == SyntaxKind(2)).is_some(),
      "typed direct-element matching must find a node across a leading token",
    )?;
    ensure(
      root.first_child_or_token_by_kind(&|kind| kind == SyntaxKind(9)).is_none(),
      "typed direct-element matching must return None on a miss",
    )
  }

  #[test]
  fn typed_element_wrappers_preserve_navigation_and_conversion_identity() -> Result<(), TestFailure> {
    let root = typed_tree()?;
    let branch = ensure_some(root.first_child(), "the typed branch must exist")?;
    let first = typed_token(ensure_some(root.first_child_or_token(), "the first typed element must exist")?)?;
    let node_element = SyntaxElement::from(branch.clone());
    let token_element = SyntaxElement::from(first.clone());
    ensure(
      node_element.text_range() == branch.text_range(),
      "typed node elements must preserve ranges",
    )?;
    ensure(
      token_element.text_range() == first.text_range(),
      "typed token elements must preserve ranges",
    )?;
    ensure(node_element.index() == 1, "typed node elements must preserve indices")?;
    ensure(token_element.index() == 0, "typed token elements must preserve indices")?;
    ensure(
      node_element.parent() == Some(root.clone()),
      "typed node elements must preserve parents",
    )?;
    ensure(
      token_element.parent() == Some(root.clone()),
      "typed token elements must preserve parents",
    )?;
    ensure_eq(
      &node_element.ancestors().count(),
      &2,
      "typed node-element ancestors must include the node and root",
    )?;
    ensure_eq(
      &token_element.ancestors().count(),
      &1,
      "typed token-element ancestors must begin at the parent",
    )?;
    ensure(
      ensure_some(node_element.first_token(), "the node element must have a first token")?.text() == "b",
      "typed node-element first_token must descend",
    )?;
    ensure(
      ensure_some(node_element.last_token(), "the node element must have a last token")?.text() == "b",
      "typed node-element last_token must descend",
    )?;
    ensure(
      (token_element.first_token(), token_element.last_token()) == (Some(first.clone()), Some(first.clone())),
      "typed token elements must return themselves as boundary tokens",
    )?;
    ensure(
      token_element.prev_sibling_or_token().is_none(),
      "the first typed token element must have no previous sibling",
    )?;
    ensure(
      token_element.next_sibling_or_token() == Some(node_element.clone()),
      "typed token-element navigation must cross to the branch",
    )?;
    ensure(
      node_element.prev_sibling_or_token() == Some(token_element.clone()),
      "typed node-element navigation must cross to the leading token",
    )?;
    ensure(
      node_element.next_sibling_or_token().is_some(),
      "typed node-element navigation must cross to the trailing token",
    )?;

    let raw_root: cursor::SyntaxNode = root.clone().into();
    let reconstructed = SyntaxNode::<TestLanguage>::from(raw_root.clone());
    ensure(
      reconstructed == root,
      "typed-to-raw-to-typed node conversion must preserve identity",
    )?;
    let raw_element: cursor::SyntaxElement = node_element.clone().into();
    ensure(
      SyntaxElement::<TestLanguage>::from(raw_element) == node_element,
      "typed-to-raw-to-typed element conversion must preserve variant and identity",
    )?;
    let raw_token: cursor::SyntaxToken = first.clone().into();
    ensure(
      SyntaxToken::<TestLanguage>::from(raw_token) == first,
      "typed-to-raw-to-typed token conversion must preserve identity",
    )
  }

  #[test]
  fn typed_replacements_preserve_success_and_kind_failure() -> Result<(), TestFailure> {
    let root = typed_tree()?;
    let branch = ensure_some(root.first_child(), "the typed branch must exist")?;
    let first = typed_token(ensure_some(root.first_child_or_token(), "the first typed element must exist")?)?;
    let replacement = ensure_ok(
      GreenNode::new(SyntaxKind(2), [GreenElement::from(ensure_ok(
        GreenToken::new(SyntaxKind(1), "B"),
        "the typed node replacement token must allocate",
      )?)]),
      "the typed node replacement must allocate",
    )?;
    let rebuilt = ensure_ok(branch.replace_with(replacement), "typed node replacement must delegate")?;
    ensure_eq(
      &rebuilt.to_string(),
      &"aBc".to_owned(),
      "typed node replacement must rebuild its context",
    )?;
    ensure(
      branch.replace_with(ensure_ok(
        GreenNode::new(SyntaxKind(9), std::iter::empty()),
        "the mismatched typed node must allocate",
      )?)
        == Err(GreenError::KindMismatch {
          expected: SyntaxKind(2),
          actual:   SyntaxKind(9),
        }),
      "typed node replacement must preserve kind-mismatch errors",
    )?;
    let token_rebuilt = ensure_ok(
      first.replace_with(ensure_ok(
        GreenToken::new(SyntaxKind(1), "A"),
        "the typed token replacement must allocate",
      )?),
      "typed token replacement must delegate",
    )?;
    ensure_eq(
      &token_rebuilt.to_string(),
      &"Abc".to_owned(),
      "typed token replacement must rebuild its context",
    )
  }

  #[test]
  fn typed_preorder_delegates_valid_and_invalid_skip_transitions() -> Result<(), TestFailure> {
    let root = typed_tree()?;
    let mut node_preorder = root.preorder();
    let _root_enter = ensure_some(node_preorder.next(), "typed node preorder must enter the root")?;
    ensure_ok(node_preorder.skip_subtree(), "typed node preorder must delegate a valid skip")?;
    let _root_leave = ensure_some(node_preorder.next(), "a typed root skip must retain the leave event")?;
    ensure(node_preorder.next().is_none(), "a typed root skip must omit descendants")?;

    let mut element_preorder = root.preorder_with_tokens();
    let _element_root_enter = ensure_some(element_preorder.next(), "typed element preorder must enter the root")?;
    let _token_enter = ensure_some(element_preorder.next(), "typed element preorder must enter the first token")?;
    ensure(
      element_preorder.skip_subtree() == Err(TraversalError::NoNodeSubtree),
      "typed element preorder must reject a skip after entering a token",
    )?;
    ensure(
      element_preorder.next().is_some(),
      "a rejected typed skip must leave traversal state able to advance",
    )
  }

  #[test]
  fn typed_token_debug_truncates_only_at_unicode_boundaries() -> Result<(), TestFailure> {
    let long_token = ensure_some(
      typed_token_root(SyntaxKind(0), SyntaxKind(1), "abcdefghijklmnopqrstu🙂tail")?.first_token(),
      "the long typed debug token must be reachable",
    )?;
    ensure(
      format!("{long_token:?}").ends_with(" ..."),
      "typed token debug truncation must stop at a valid UTF-8 boundary",
    )?;
    ensure_eq(
      &long_token.to_string(),
      &"abcdefghijklmnopqrstu🙂tail".to_owned(),
      "typed token display must remain untruncated",
    )
  }

  #[test]
  fn typed_editor_delegates_identity_and_metadata_queries() -> Result<(), TestFailure> {
    let source = typed_tree()?;
    let branch_source = ensure_some(source.first_child(), "the typed source branch must exist")?;
    let b_source = ensure_some(branch_source.first_token(), "the typed source b token must exist")?;
    let editor = SyntaxEditor::new(&source);
    ensure(
      format!("{editor:?}").contains("SyntaxEditor"),
      "typed editor diagnostics must identify the transactional editor state",
    )?;
    let root = editor.root_id();
    let branch = ensure_ok(editor.node_id(&branch_source), "typed node source lookup must resolve")?;
    let b = ensure_ok(editor.token_id(&b_source), "typed token source lookup must resolve")?;
    let branch_element = crate::EditorElementId::Node(branch.clone());
    let b_element = crate::EditorElementId::Token(b.clone());
    ensure(
      ensure_ok(editor.kind(&branch_element), "typed editor kind must resolve")? == SyntaxKind(2),
      "typed editor kind must convert",
    )?;
    ensure(
      ensure_ok(editor.parent(&branch_element), "typed editor parent must resolve")? == Some(root.clone()),
      "typed editor parent must preserve IDs",
    )?;
    ensure(
      ensure_ok(editor.index(&branch_element), "typed editor index must resolve")? == Some(1),
      "typed editor index must be exact",
    )?;
    ensure(
      ensure_ok(editor.children(&branch), "typed editor children must resolve")? == vec![b_element.clone()],
      "typed editor children must preserve order",
    )?;
    ensure(
      ensure_ok(editor.text_len(&branch_element), "typed editor length must resolve")? == TextSize::from(1),
      "typed editor length must be exact",
    )?;
    ensure(
      ensure_ok(editor.text_range(&branch_element), "typed editor range must resolve")?
        == TextRange::new(TextSize::from(1), TextSize::from(2)),
      "typed editor range must be exact",
    )?;
    ensure_eq(
      &ensure_ok(editor.token_text(&b), "typed editor token text must resolve")?,
      &"b",
      "typed editor text must be exact",
    )
  }

  #[test]
  fn typed_editor_delegates_import_splice_and_completion() -> Result<(), TestFailure> {
    let source = typed_tree()?;
    let branch_source = ensure_some(source.first_child(), "the typed source branch must exist")?;
    let b_source = ensure_some(branch_source.first_token(), "the typed source b token must exist")?;
    let mut editor = SyntaxEditor::new(&source);
    let root = editor.root_id();
    let branch = ensure_ok(editor.node_id(&branch_source), "typed node source lookup must resolve")?;
    let b = ensure_ok(editor.token_id(&b_source), "typed token source lookup must resolve")?;
    let b_element = crate::EditorElementId::Token(b);
    let imported_green = ensure_ok(
      editor.import_green(GreenElement::from(ensure_ok(
        GreenToken::new(SyntaxKind(1), "x"),
        "the typed imported green token must allocate",
      )?)),
      "typed green import must succeed",
    )?;
    let external = typed_tree()?;
    let external_token = ensure_some(external.first_token(), "the typed external token must exist")?;
    let imported_syntax = ensure_ok(
      editor.import(SyntaxElement::from(external_token)),
      "typed syntax import must succeed",
    )?;
    let outcome = ensure_ok(
      editor.splice_children(&branch, 0..1, [imported_green, imported_syntax]),
      "typed splice must delegate transactionally",
    )?;
    ensure(
      outcome.detached() == std::slice::from_ref(&b_element),
      "typed splice outcomes must expose detached IDs",
    )?;
    ensure(
      outcome.into_detached() == [b_element.clone()],
      "consuming a typed splice outcome must preserve the detached ID order",
    )?;
    let completed = ensure_ok(editor.into_syntax(&root), "typed completion must succeed")?;
    ensure_eq(
      &completed.to_string(),
      &"axac".to_owned(),
      "typed completion must preserve delegated edit order",
    )
  }

  #[test]
  fn typed_editor_delegates_detachment() -> Result<(), TestFailure> {
    let source = typed_tree()?;
    let branch_source = ensure_some(source.first_child(), "the typed source branch must exist")?;
    let mut detach_editor = SyntaxEditor::new(&source);
    let branch = ensure_ok(detach_editor.node_id(&branch_source), "typed detach source lookup must resolve")?;
    ensure(
      ensure_ok(
        detach_editor.detach(&crate::EditorElementId::Node(branch)),
        "typed detach must delegate",
      )?,
      "typed detach must report removal",
    )
  }
}
