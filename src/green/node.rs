//! Thin, immutable green-node allocations.

use std::fmt;
use std::iter::FusedIterator;
use std::ops::Range;
use std::slice;

use countme::Count;
use triomphe::ThinArc;

use crate::GreenToken;
use crate::NodeOrToken;
use crate::TextSize;
use crate::green::GreenElement;
use crate::green::GreenElementRef;
use crate::green::GreenError;
use crate::green::SyntaxKind;
use crate::green::checked_text_add;

/// Fixed header stored alongside a green node's variable-length child slice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct GreenNodeHead {
  /// Raw node kind.
  kind:     SyntaxKind,
  /// Total UTF-8 byte length of all children.
  text_len: TextSize,
  /// Allocation accounting marker.
  _count:   Count<GreenNode>,
}

/// A green child paired with its offset relative to its parent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[repr(u8)]
pub(crate) enum GreenChild {
  /// An immutable child node.
  Node {
    /// Child offset relative to its parent.
    rel_offset: TextSize,
    /// Shared child node handle.
    node:       GreenNode,
  },
  /// An immutable child token.
  Token {
    /// Child offset relative to its parent.
    rel_offset: TextSize,
    /// Shared child token handle.
    token:      GreenToken,
  },
}

/// An internal node in an immutable green syntax tree.
#[derive(Clone)]
#[repr(transparent)]
pub struct GreenNode {
  /// Thin shared allocation containing the header and children.
  children: ThinArc<GreenNodeHead, GreenChild>,
}

/// Compare one pair of borrowed green children and queue matching node pairs.
fn children_match<'a>(left: GreenElementRef<'a>, right: GreenElementRef<'a>, pending: &mut Vec<(&'a GreenNode, &'a GreenNode)>) -> bool {
  match (left, right) {
    (NodeOrToken::Node(left_node), NodeOrToken::Node(right_node)) => {
      pending.push((left_node, right_node));
      true
    }
    (NodeOrToken::Token(left_token), NodeOrToken::Token(right_token)) => left_token == right_token,
    (NodeOrToken::Node(_), NodeOrToken::Token(_)) | (NodeOrToken::Token(_), NodeOrToken::Node(_)) => false,
  }
}

/// Compare one node level and queue each matching child-node pair.
fn node_level_matches<'a>(left: &'a GreenNode, right: &'a GreenNode, pending: &mut Vec<(&'a GreenNode, &'a GreenNode)>) -> bool {
  left.kind() == right.kind()
    && left.children().len() == right.children().len()
    && left
      .children()
      .rev()
      .zip(right.children().rev())
      .all(|(left_child, right_child)| children_match(left_child, right_child, pending))
}

impl GreenNode {
  /// Create an immutable node from an arbitrary child iterator.
  ///
  /// # Errors
  ///
  /// Returns [`GreenError::TextLengthOverflow`] when the aggregate child length exceeds
  /// `TextSize`, or [`GreenError::AllocationFailed`] when the thin shared allocation fails.
  pub fn new(kind: SyntaxKind, children: impl IntoIterator<Item = GreenElement>) -> Result<Self, GreenError> {
    let elements: Vec<GreenElement> = children.into_iter().collect();
    let mut text_len = TextSize::default();
    let mut child_records = Vec::with_capacity(elements.len());

    for element in elements {
      let rel_offset = text_len;
      text_len = checked_text_add(text_len, element.text_len())?;
      let child_record = match element {
        NodeOrToken::Node(node) => GreenChild::Node {
          rel_offset,
          node,
        },
        NodeOrToken::Token(token) => GreenChild::Token {
          rel_offset,
          token,
        },
      };
      child_records.push(child_record);
    }

    let header = GreenNodeHead {
      kind,
      text_len,
      _count: Count::new(),
    };
    ThinArc::try_from_header_and_iter(header, child_records.into_iter())
      .map(|children| Self {
        children,
      })
      .map_err(|_| GreenError::AllocationFailed)
  }

  /// Return this node's raw syntax kind.
  pub fn kind(&self) -> SyntaxKind {
    self.children.header.header.kind
  }

  /// Return the total UTF-8 byte length covered by this node.
  pub fn text_len(&self) -> TextSize {
    self.children.header.header.text_len
  }

  /// Iterate over borrowed child handles.
  pub fn children(&self) -> Children<'_> {
    Children {
      inner: self.children.slice.iter(),
    }
  }

  /// Test whether two nodes share the same thin allocation.
  pub fn ptr_eq(&self, other: &Self) -> bool {
    std::ptr::eq(self.children.as_ptr(), other.children.as_ptr())
  }

  /// Compare complete green subtrees without recursive application stack use.
  pub(crate) fn structurally_eq(&self, other: &Self) -> bool {
    let mut pending = vec![(self, other)];
    while let Some((left, right)) = pending.pop() {
      if !node_level_matches(left, right, &mut pending) {
        return false;
      }
    }
    true
  }

  /// Replace one child without changing its syntax kind.
  ///
  /// # Errors
  ///
  /// Returns [`GreenError::ChildIndexOutOfBounds`] for an absent child,
  /// [`GreenError::KindMismatch`] when the replacement has a different kind, or propagates a
  /// construction error from rebuilding the node.
  pub fn replace_child(&self, index: usize, replacement: GreenElement) -> Result<Self, GreenError> {
    let child_count = self.children.slice.len();
    let existing = self.children.slice.get(index).ok_or(GreenError::ChildIndexOutOfBounds {
      index,
      child_count,
    })?;
    let expected = existing.as_ref().kind();
    let actual = replacement.kind();
    if expected != actual {
      return Err(GreenError::KindMismatch {
        expected,
        actual,
      });
    }

    let rebuilt = self.children().enumerate().map(|(child_index, child)| {
      if child_index == index {
        replacement.clone()
      } else {
        child.to_owned()
      }
    });
    Self::new(self.kind(), rebuilt)
  }

  /// Insert a child before `index`, permitting insertion at the end.
  ///
  /// # Errors
  ///
  /// Returns [`GreenError::ChildIndexOutOfBounds`] when `index` exceeds the child count, or
  /// propagates a construction error from rebuilding the node.
  pub fn insert_child(&self, index: usize, insertion: GreenElement) -> Result<Self, GreenError> {
    let child_count = self.children.slice.len();
    if index > child_count {
      return Err(GreenError::ChildIndexOutOfBounds {
        index,
        child_count,
      });
    }
    self.splice_children(index..index, std::iter::once(insertion))
  }

  /// Remove an existing child.
  ///
  /// # Errors
  ///
  /// Returns [`GreenError::ChildIndexOutOfBounds`] when `index` does not name a child, or
  /// propagates a construction error from rebuilding the node.
  pub fn remove_child(&self, index: usize) -> Result<Self, GreenError> {
    let child_count = self.children.slice.len();
    if index >= child_count {
      return Err(GreenError::ChildIndexOutOfBounds {
        index,
        child_count,
      });
    }
    let range_end = index.checked_add(1).ok_or(GreenError::ChildIndexOutOfBounds {
      index,
      child_count,
    })?;
    self.splice_children(index..range_end, std::iter::empty())
  }

  /// Replace an exclusive child range with new children.
  ///
  /// # Errors
  ///
  /// Returns [`GreenError::InvalidChildRange`] for a reversed or out-of-bounds range, or
  /// propagates a construction error from rebuilding the node.
  pub fn splice_children(&self, range: Range<usize>, replacements: impl IntoIterator<Item = GreenElement>) -> Result<Self, GreenError> {
    let child_count = self.children.slice.len();
    if range.start > range.end || range.end > child_count {
      return Err(GreenError::InvalidChildRange {
        start: range.start,
        end: range.end,
        child_count,
      });
    }

    let replacements: Vec<GreenElement> = replacements.into_iter().collect();
    let rebuilt = self
      .children()
      .take(range.start)
      .map(GreenElementRef::to_owned)
      .chain(replacements)
      .chain(self.children().skip(range.end).map(GreenElementRef::to_owned));
    Self::new(self.kind(), rebuilt)
  }

  /// Borrow the offset-bearing child records for red cursor construction.
  pub(crate) fn child_records(&self) -> &[GreenChild] {
    &self.children.slice
  }

  /// Hash the node allocation identity rather than its contents.
  pub(crate) fn hash_identity<HasherType: std::hash::Hasher>(&self, state: &mut HasherType) {
    std::ptr::hash(self.children.as_ptr(), state);
  }
}

impl GreenChild {
  /// Borrow the child handle without its relative offset.
  pub(crate) fn as_ref(&self) -> GreenElementRef<'_> {
    match self {
      Self::Node {
        node, ..
      } => NodeOrToken::Node(node),
      Self::Token {
        token, ..
      } => NodeOrToken::Token(token),
    }
  }

  /// Return this child's offset relative to its parent.
  pub(crate) fn rel_offset(&self) -> TextSize {
    match self {
      Self::Node {
        rel_offset, ..
      }
      | Self::Token {
        rel_offset, ..
      } => *rel_offset,
    }
  }
}

impl fmt::Debug for GreenNode {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    crate::debug::write(crate::debug::DebugTarget::GreenNode(self), formatter)
  }
}

impl fmt::Display for GreenNode {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut pending = self.children().rev().collect::<Vec<_>>();
    while let Some(element) = pending.pop() {
      match element {
        NodeOrToken::Node(node) => pending.extend(node.children().rev()),
        NodeOrToken::Token(token) => fmt::Display::fmt(token, formatter)?,
      }
    }
    Ok(())
  }
}

impl PartialEq for GreenNode {
  fn eq(&self, other: &Self) -> bool {
    self.structurally_eq(other)
  }
}

impl Eq for GreenNode {}

impl std::hash::Hash for GreenNode {
  fn hash<HasherType: std::hash::Hasher>(&self, state: &mut HasherType) {
    let mut pending = vec![GreenElementRef::from(self)];
    while let Some(element) = pending.pop() {
      match element {
        NodeOrToken::Node(node) => {
          0_u8.hash(state);
          node.kind().hash(state);
          node.children().len().hash(state);
          pending.extend(node.children().rev());
        }
        NodeOrToken::Token(token) => {
          1_u8.hash(state);
          token.kind().hash(state);
          token.text().hash(state);
        }
      }
    }
  }
}

/// A fused, double-ended, exact-size iterator over borrowed green children.
#[derive(Debug, Clone)]
pub struct Children<'a> {
  /// Backing child-record iterator.
  inner: slice::Iter<'a, GreenChild>,
}

impl ExactSizeIterator for Children<'_> {
  fn len(&self) -> usize {
    self.inner.len()
  }
}

impl<'a> Iterator for Children<'a> {
  type Item = GreenElementRef<'a>;

  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next().map(GreenChild::as_ref)
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    self.inner.size_hint()
  }

  fn count(self) -> usize {
    self.inner.count()
  }

  fn nth(&mut self, index: usize) -> Option<Self::Item> {
    self.inner.nth(index).map(GreenChild::as_ref)
  }

  fn last(mut self) -> Option<Self::Item> {
    self.next_back()
  }
}

impl DoubleEndedIterator for Children<'_> {
  fn next_back(&mut self) -> Option<Self::Item> {
    self.inner.next_back().map(GreenChild::as_ref)
  }

  fn nth_back(&mut self, index: usize) -> Option<Self::Item> {
    self.inner.nth_back(index).map(GreenChild::as_ref)
  }
}

impl FusedIterator for Children<'_> {}

#[cfg(test)]
mod tests {
  use std::collections::hash_map::DefaultHasher;
  use std::hash::Hash;
  use std::hash::Hasher;
  use std::mem;

  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;
  use strict_test_support::ensure_ne;
  use strict_test_support::ensure_ok;
  use strict_test_support::ensure_some;

  use super::GreenChild;
  use super::GreenNode;
  use crate::GreenElement;
  use crate::GreenError;
  use crate::GreenToken;
  use crate::SyntaxKind;
  use crate::TextSize;
  use crate::test_support::ensure_one_word;

  /// Construct a token fixture through the public fallible constructor.
  fn token(kind: u16, text: &str) -> Result<GreenToken, TestFailure> {
    ensure_ok(GreenToken::new(SyntaxKind(kind), text), "the token fixture must allocate")
  }

  /// Construct a node fixture through the public fallible constructor.
  fn node(kind: u16, children: impl IntoIterator<Item = GreenElement>) -> Result<GreenNode, TestFailure> {
    ensure_ok(GreenNode::new(SyntaxKind(kind), children), "the node fixture must allocate")
  }

  #[test]
  fn nodes_preserve_offsets_iteration_and_mixed_child_order() -> Result<(), TestFailure> {
    let empty = node(3, std::iter::empty())?;
    let first = token(1, "a")?;
    let unicode = token(2, "β")?;
    let root = node(0, [
      GreenElement::from(first.clone()),
      GreenElement::from(empty.clone()),
      GreenElement::from(unicode.clone()),
    ])?;

    ensure(root.kind() == SyntaxKind(0), "the root kind must remain exact")?;
    ensure(root.text_len() == TextSize::from(3), "node length must sum UTF-8 child bytes")?;
    ensure_eq(&root.to_string(), &"aβ".to_owned(), "node display must preserve source order")?;
    let offsets = root.child_records().iter().map(GreenChild::rel_offset).collect::<Vec<_>>();
    ensure(
      offsets == vec![TextSize::from(0), TextSize::from(1), TextSize::from(1)],
      "child records must retain exact relative offsets, including zero-width nodes",
    )?;

    let mut children = root.children();
    ensure_eq(&children.len(), &3, "the exact-size iterator must report all children")?;
    ensure(
      children.size_hint() == (3, Some(3)),
      "the child iterator must expose an exact initial size hint",
    )?;
    ensure_eq(
      &children.clone().count(),
      &3,
      "counting a cloned iterator must consume exactly its remaining children",
    )?;
    ensure(
      ensure_some(children.clone().nth(1), "the middle child must be addressable by nth")?.kind() == SyntaxKind(3),
      "nth must preserve source-order child selection",
    )?;
    ensure(
      ensure_some(children.clone().last(), "the final child must be addressable by last")?.kind() == SyntaxKind(2),
      "last must select the source-order final child",
    )?;
    ensure(
      ensure_some(children.clone().nth_back(1), "the middle child must be addressable from the back")?.kind() == SyntaxKind(3),
      "nth_back must preserve reverse source-order selection",
    )?;
    ensure(
      ensure_some(children.next(), "the first child must exist")?.kind() == SyntaxKind(1),
      "forward iteration must begin with the first token",
    )?;
    ensure(
      ensure_some(children.next_back(), "the final child must exist")?.kind() == SyntaxKind(2),
      "reverse iteration must begin with the final token",
    )?;
    ensure_eq(&children.len(), &1, "mixed-direction iteration must update the exact length")?;
    let _middle = ensure_some(children.next(), "the middle child must remain")?;
    ensure(children.next().is_none(), "an exhausted iterator must stay empty")?;
    ensure(children.next_back().is_none(), "a fused iterator must stay empty from either end")?;
    ensure_eq(
      &format!("{root:?}"),
      &"GreenNode { kind: SyntaxKind(0), text_len: 3, child_count: 3 }".to_owned(),
      "node debug output must expose kind, length, and arity",
    )
  }

  #[test]
  fn functional_child_edits_validate_before_rebuilding() -> Result<(), TestFailure> {
    let a = token(1, "a")?;
    let b = token(1, "b")?;
    let c = token(1, "c")?;
    let root = node(0, [a.clone().into(), b.clone().into()])?;

    let replaced = ensure_ok(
      root.replace_child(1, GreenElement::from(c.clone())),
      "a same-kind replacement must succeed",
    )?;
    ensure_eq(&replaced.to_string(), &"ac".to_owned(), "replacement must preserve order")?;
    ensure(
      ensure_some(replaced.children().next(), "the unchanged first child must exist")?
        .to_owned()
        .ptr_eq(&GreenElement::from(a.clone())),
      "replacement must retain the unchanged child's allocation",
    )?;

    let inserted = ensure_ok(
      root.insert_child(2, GreenElement::from(c.clone())),
      "insertion at the child-count boundary must succeed",
    )?;
    ensure_eq(&inserted.to_string(), &"abc".to_owned(), "insertion must preserve caller order")?;
    let removed = ensure_ok(root.remove_child(0), "an existing child must be removable")?;
    ensure_eq(&removed.to_string(), &"b".to_owned(), "removal must retain the unaffected suffix")?;
    let spliced = ensure_ok(
      root.splice_children(0..2, [GreenElement::from(c.clone())]),
      "a complete range replacement must succeed",
    )?;
    ensure_eq(&spliced.to_string(), &"c".to_owned(), "splice must replace the complete range")?;
    ensure_eq(
      &root.to_string(),
      &"ab".to_owned(),
      "functional edits must leave the source reusable",
    )?;
    ensure(!root.ptr_eq(&replaced), "a changed node must receive a new allocation")?;

    ensure(
      root.replace_child(0, GreenElement::from(token(9, "x")?))
        == Err(GreenError::KindMismatch {
          expected: SyntaxKind(1),
          actual:   SyntaxKind(9),
        }),
      "replacement kind mismatch must be rejected before rebuilding",
    )?;
    ensure(
      root.replace_child(2, GreenElement::from(c.clone()))
        == Err(GreenError::ChildIndexOutOfBounds {
          index:       2,
          child_count: 2,
        }),
      "replacement must reject the end index",
    )?;
    ensure(
      root.insert_child(3, GreenElement::from(c.clone()))
        == Err(GreenError::ChildIndexOutOfBounds {
          index:       3,
          child_count: 2,
        }),
      "insertion must reject indices beyond the end",
    )?;
    ensure(
      root.remove_child(2)
        == Err(GreenError::ChildIndexOutOfBounds {
          index:       2,
          child_count: 2,
        }),
      "removal must reject the end index",
    )?;
    ensure(
      root.splice_children(
        std::ops::Range {
          start: 2, end: 1
        },
        std::iter::empty(),
      ) == Err(GreenError::InvalidChildRange {
        start:       2,
        end:         1,
        child_count: 2,
      }),
      "a reversed splice must be rejected first",
    )?;
    ensure(
      root.splice_children(0..3, std::iter::empty())
        == Err(GreenError::InvalidChildRange {
          start:       0,
          end:         3,
          child_count: 2,
        }),
      "an out-of-bounds splice must be rejected",
    )?;
    ensure_eq(
      &root.to_string(),
      &"ab".to_owned(),
      "all rejected edits must leave the source unchanged",
    )
  }

  /// Build a deeply nested tree without recursive fixture construction.
  fn deep_tree(depth: usize) -> Result<GreenNode, TestFailure> {
    let mut current = node(4, [GreenElement::from(token(1, "x")?)])?;
    for _ in 0..depth {
      current = node(4, [GreenElement::from(current)])?;
    }
    Ok(current)
  }

  #[test]
  fn structural_equality_hashing_and_display_are_iterative() -> Result<(), TestFailure> {
    let first = deep_tree(2_048)?;
    let second = deep_tree(2_048)?;
    ensure(
      !first.ptr_eq(&second),
      "independent deep trees must not share their root allocation",
    )?;
    ensure_eq(&first, &second, "independent equivalent deep trees must compare structurally")?;
    ensure_eq(&first.to_string(), &"x".to_owned(), "deep display must reach the leaf iteratively")?;
    let mut first_hash = DefaultHasher::new();
    first.hash(&mut first_hash);
    let mut second_hash = DefaultHasher::new();
    second.hash(&mut second_hash);
    ensure_eq(
      &first_hash.finish(),
      &second_hash.finish(),
      "equivalent deep trees must produce equal iterative structural hashes",
    )?;
    ensure_ne(
      &first,
      &node(4, [GreenElement::from(token(1, "y")?)])?,
      "different deep-tree text must compare unequal",
    )?;
    ensure_ne(
      &node(4, [GreenElement::from(token(1, "x")?)])?,
      &node(5, [GreenElement::from(token(1, "x")?)])?,
      "different node kinds must compare unequal before descending",
    )?;
    ensure_ne(
      &node(4, [GreenElement::from(token(1, "x")?)])?,
      &node(4, std::iter::empty())?,
      "different child counts must compare unequal before pairing children",
    )?;
    ensure_ne(
      &node(4, [GreenElement::from(token(1, "x")?)])?,
      &node(4, [GreenElement::from(node(1, std::iter::empty())?)])?,
      "a node and token at the same child position must compare unequal",
    )
  }

  #[test]
  fn green_representation_stays_within_locked_word_sizes() -> Result<(), TestFailure> {
    ensure_one_word::<GreenNode>("a green node handle must remain one machine word")?;
    let two_words = ensure_some(mem::size_of::<usize>().checked_mul(2), "two machine words must be representable")?;
    ensure(
      mem::size_of::<GreenChild>() <= two_words,
      "an offset-bearing green child must fit within two machine words",
    )
  }
}
