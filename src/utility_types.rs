//! Small public enums shared by the green, cursor, and typed APIs.

use std::fmt;

/// A value that is either a syntax node or a syntax token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeOrToken<Node, Token> {
  /// Node variant.
  Node(Node),
  /// Token variant.
  Token(Token),
}

impl<Node, Token> NodeOrToken<Node, Token> {
  /// Extract the node variant.
  pub fn into_node(self) -> Option<Node> {
    match self {
      Self::Node(node) => Some(node),
      Self::Token(_) => None,
    }
  }

  /// Extract the token variant.
  pub fn into_token(self) -> Option<Token> {
    match self {
      Self::Node(_) => None,
      Self::Token(token) => Some(token),
    }
  }

  /// Borrow the node variant.
  pub fn as_node(&self) -> Option<&Node> {
    match self {
      Self::Node(node) => Some(node),
      Self::Token(_) => None,
    }
  }

  /// Borrow the token variant.
  pub fn as_token(&self) -> Option<&Token> {
    match self {
      Self::Node(_) => None,
      Self::Token(token) => Some(token),
    }
  }

  /// Borrow either contained value while preserving its variant.
  pub fn as_ref(&self) -> NodeOrToken<&Node, &Token> {
    match self {
      Self::Node(node) => NodeOrToken::Node(node),
      Self::Token(token) => NodeOrToken::Token(token),
    }
  }

  /// Map both variants independently.
  pub fn map<NewNode, NewToken>(
    self,
    map_node: impl FnOnce(Node) -> NewNode,
    map_token: impl FnOnce(Token) -> NewToken,
  ) -> NodeOrToken<NewNode, NewToken> {
    match self {
      Self::Node(node) => NodeOrToken::Node(map_node(node)),
      Self::Token(token) => NodeOrToken::Token(map_token(token)),
    }
  }

  /// Dispatch borrowed variants into one common output type.
  pub fn either<Output>(&self, node: impl FnOnce(&Node) -> Output, token: impl FnOnce(&Token) -> Output) -> Output {
    match self {
      Self::Node(node_value) => node(node_value),
      Self::Token(token_value) => token(token_value),
    }
  }
}

impl<Node: fmt::Display, Token: fmt::Display> fmt::Display for NodeOrToken<Node, Token> {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Node(node) => fmt::Display::fmt(node, formatter),
      Self::Token(token) => fmt::Display::fmt(token, formatter),
    }
  }
}

/// Direction used by sibling iterators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Direction {
  /// Move toward later siblings.
  Next,
  /// Move toward earlier siblings.
  Prev,
}

/// An event emitted while walking a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkEvent<Element> {
  /// Emitted before traversing an element's descendants.
  Enter(Element),
  /// Emitted after traversing an element's descendants.
  Leave(Element),
}

impl<Element> WalkEvent<Element> {
  /// Map the event's element without changing enter/leave polarity.
  pub fn map<Mapped>(self, map_element: impl FnOnce(Element) -> Mapped) -> WalkEvent<Mapped> {
    match self {
      Self::Enter(element) => WalkEvent::Enter(map_element(element)),
      Self::Leave(element) => WalkEvent::Leave(map_element(element)),
    }
  }
}

/// Zero, one, or two tokens touching a source offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenAtOffset<Token> {
  /// No token touches the offset, as in an empty tree.
  None,
  /// Exactly one token touches the offset.
  Single(Token),
  /// The offset is a boundary between a left and right token.
  Between(Token, Token),
}

impl<Token> TokenAtOffset<Token> {
  /// Map every contained token.
  pub fn map<Mapped>(self, map_token: impl Fn(Token) -> Mapped) -> TokenAtOffset<Mapped> {
    match self {
      Self::None => TokenAtOffset::None,
      Self::Single(token) => TokenAtOffset::Single(map_token(token)),
      Self::Between(left, right) => TokenAtOffset::Between(map_token(left), map_token(right)),
    }
  }

  /// Convert to an option, preferring the right token at a boundary.
  pub fn right_biased(self) -> Option<Token> {
    match self {
      Self::None => None,
      Self::Single(token) | Self::Between(_, token) => Some(token),
    }
  }

  /// Convert to an option, preferring the left token at a boundary.
  pub fn left_biased(self) -> Option<Token> {
    match self {
      Self::None => None,
      Self::Single(token) | Self::Between(token, _) => Some(token),
    }
  }
}

impl<Token> Iterator for TokenAtOffset<Token> {
  type Item = Token;

  fn next(&mut self) -> Option<Self::Item> {
    match std::mem::replace(self, Self::None) {
      Self::None => None,
      Self::Single(token) => Some(token),
      Self::Between(left, right) => {
        *self = Self::Single(right);
        Some(left)
      }
    }
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    match self {
      Self::None => (0, Some(0)),
      Self::Single(_) => (1, Some(1)),
      Self::Between(..) => (2, Some(2)),
    }
  }
}

impl<Token> ExactSizeIterator for TokenAtOffset<Token> {}

#[cfg(test)]
mod tests {
  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;

  use super::NodeOrToken;
  use super::TokenAtOffset;
  use super::WalkEvent;

  #[test]
  fn node_or_token_preserves_variant_through_access_and_mapping() -> Result<(), TestFailure> {
    let node = NodeOrToken::<u8, &str>::Node(3);
    ensure(node.as_node() == Some(&3), "node borrowing must preserve the node value")?;
    ensure(node.as_token().is_none(), "node borrowing must not invent a token")?;
    ensure(
      node.as_ref() == NodeOrToken::Node(&3),
      "borrowing the union must preserve the node variant",
    )?;
    ensure_eq(
      &node.map(u16::from, str::len),
      &NodeOrToken::<u16, usize>::Node(3),
      "node mapping must use only the node mapper",
    )?;
    ensure(node.into_node() == Some(3), "node extraction must return the node")?;
    ensure(
      node.into_token().is_none(),
      "extracting a token from the node variant must return None",
    )?;

    let token = NodeOrToken::<u8, &str>::Token("text");
    ensure(token.as_token() == Some(&"text"), "token borrowing must preserve the token value")?;
    ensure(token.as_node().is_none(), "token borrowing must not invent a node")?;
    ensure(
      token.as_ref() == NodeOrToken::Token(&"text"),
      "borrowing the union must preserve the token variant",
    )?;
    ensure_eq(
      &token.map(u16::from, str::len),
      &NodeOrToken::<u16, usize>::Token(4),
      "token mapping must use only the token mapper",
    )?;
    ensure(token.into_token() == Some("text"), "token extraction must return the token")?;
    ensure(
      token.into_node().is_none(),
      "extracting a node from the token variant must return None",
    )?;
    ensure_eq(
      &format!("{}", NodeOrToken::<u8, &str>::Node(7)),
      &"7".to_owned(),
      "node display must delegate to the contained node",
    )?;
    ensure_eq(
      &format!("{}", NodeOrToken::<u8, &str>::Token("shown")),
      &"shown".to_owned(),
      "display must delegate to the active variant",
    )
  }

  #[test]
  fn token_at_offset_bias_and_iteration_preserve_left_to_right_order() -> Result<(), TestFailure> {
    ensure(
      TokenAtOffset::<u8>::Between(1, 2).left_biased() == Some(1),
      "left bias must select the left boundary token",
    )?;
    ensure(
      TokenAtOffset::<u8>::Between(1, 2).right_biased() == Some(2),
      "right bias must select the right boundary token",
    )?;
    ensure(
      TokenAtOffset::<u8>::Single(3).left_biased() == Some(3),
      "either bias must preserve a single token",
    )?;
    ensure(
      TokenAtOffset::<u8>::Single(3).right_biased() == Some(3),
      "right bias must preserve a single token",
    )?;
    ensure(
      TokenAtOffset::<u8>::None.right_biased().is_none(),
      "biasing no token must remain None",
    )?;
    ensure(
      TokenAtOffset::<u8>::None.left_biased().is_none(),
      "left-biasing no token must remain None",
    )?;
    ensure(
      TokenAtOffset::<u8>::None.map(u16::from) == TokenAtOffset::None,
      "mapping an empty result must remain empty",
    )?;
    ensure(
      TokenAtOffset::Single(4_u8).map(u16::from) == TokenAtOffset::Single(4_u16),
      "mapping a single result must preserve its value",
    )?;
    let none = TokenAtOffset::<u8>::None;
    ensure(
      none.size_hint() == (0, Some(0)),
      "an empty result must report an exact zero iterator size",
    )?;
    let single = TokenAtOffset::Single(3_u8);
    ensure(
      single.size_hint() == (1, Some(1)),
      "a single result must report an exact one-token iterator size",
    )?;
    let mut between = TokenAtOffset::Between("left", "right");
    ensure(
      between.size_hint() == (2, Some(2)),
      "a boundary result must report an exact two-token iterator size",
    )?;
    ensure_eq(&between.len(), &2, "a boundary iterator must begin with two tokens")?;
    ensure(between.next() == Some("left"), "boundary iteration must yield the left token first")?;
    ensure_eq(&between.len(), &1, "yielding left must retain one token")?;
    ensure(
      between.next() == Some("right"),
      "boundary iteration must yield the right token second",
    )?;
    ensure(between.next().is_none(), "an exhausted boundary iterator must stay empty")?;
    let mapped = TokenAtOffset::Between(1_u8, 2_u8).map(u16::from).collect::<Vec<_>>();
    ensure(mapped == vec![1_u16, 2_u16], "mapping must preserve boundary order")
  }

  #[test]
  fn walk_event_mapping_preserves_enter_leave_polarity() -> Result<(), TestFailure> {
    ensure(
      WalkEvent::Enter(2_u8).map(u16::from) == WalkEvent::Enter(2_u16),
      "enter mapping must remain Enter",
    )?;
    ensure(
      WalkEvent::Leave(3_u8).map(u16::from) == WalkEvent::Leave(3_u16),
      "leave mapping must remain Leave",
    )
  }
}
