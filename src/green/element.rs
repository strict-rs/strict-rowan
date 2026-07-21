//! Owned and borrowed green-element unions.

use crate::NodeOrToken;
use crate::TextSize;
use crate::green::GreenNode;
use crate::green::GreenToken;
use crate::green::SyntaxKind;

/// An owned immutable green node or token.
pub type GreenElement = NodeOrToken<GreenNode, GreenToken>;

/// A borrowed immutable green node or token handle.
pub type GreenElementRef<'a> = NodeOrToken<&'a GreenNode, &'a GreenToken>;

impl From<GreenNode> for GreenElement {
  fn from(node: GreenNode) -> Self {
    NodeOrToken::Node(node)
  }
}

impl From<GreenToken> for GreenElement {
  fn from(token: GreenToken) -> Self {
    NodeOrToken::Token(token)
  }
}

impl<'a> From<&'a GreenNode> for GreenElementRef<'a> {
  fn from(node: &'a GreenNode) -> Self {
    NodeOrToken::Node(node)
  }
}

impl<'a> From<&'a GreenToken> for GreenElementRef<'a> {
  fn from(token: &'a GreenToken) -> Self {
    NodeOrToken::Token(token)
  }
}

impl GreenElement {
  /// Return this element's raw syntax kind.
  pub fn kind(&self) -> SyntaxKind {
    self.either(GreenNode::kind, GreenToken::kind)
  }

  /// Return this element's UTF-8 byte length.
  pub fn text_len(&self) -> TextSize {
    self.either(GreenNode::text_len, GreenToken::text_len)
  }

  /// Test whether two elements name the same backing allocation and variant.
  pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
    match (self, other) {
      (NodeOrToken::Node(left), NodeOrToken::Node(right)) => left.ptr_eq(right),
      (NodeOrToken::Token(left), NodeOrToken::Token(right)) => left.ptr_eq(right),
      (NodeOrToken::Node(_), NodeOrToken::Token(_)) | (NodeOrToken::Token(_), NodeOrToken::Node(_)) => false,
    }
  }
}

impl GreenElementRef<'_> {
  /// Clone the referenced shared handle into an owned green element.
  pub fn to_owned(self) -> GreenElement {
    match self {
      NodeOrToken::Node(node) => NodeOrToken::Node(node.clone()),
      NodeOrToken::Token(token) => NodeOrToken::Token(token.clone()),
    }
  }

  /// Return this element's raw syntax kind.
  pub fn kind(self) -> SyntaxKind {
    match self {
      NodeOrToken::Node(node) => node.kind(),
      NodeOrToken::Token(token) => token.kind(),
    }
  }

  /// Return this element's UTF-8 byte length.
  pub fn text_len(self) -> TextSize {
    match self {
      NodeOrToken::Node(node) => node.text_len(),
      NodeOrToken::Token(token) => token.text_len(),
    }
  }
}

#[cfg(test)]
mod tests {
  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_ok;
  use strict_test_support::ensure_some;

  use crate::GreenElement;
  use crate::GreenElementRef;
  use crate::GreenNode;
  use crate::GreenToken;
  use crate::NodeOrToken;
  use crate::SyntaxKind;
  use crate::TextSize;

  #[test]
  fn owned_and_borrowed_elements_preserve_variant_metadata_and_identity() -> Result<(), TestFailure> {
    let token = ensure_ok(GreenToken::new(SyntaxKind(1), "text"), "the element token fixture must allocate")?;
    let node = ensure_ok(
      GreenNode::new(SyntaxKind(2), [GreenElement::from(token.clone())]),
      "the element node fixture must allocate",
    )?;
    let owned_node = GreenElement::from(node.clone());
    let owned_token = GreenElement::from(token.clone());

    ensure(owned_node.kind() == SyntaxKind(2), "owned node metadata must retain its kind")?;
    ensure(
      owned_node.text_len() == TextSize::from(4),
      "owned node metadata must retain its aggregate byte length",
    )?;
    ensure(owned_token.kind() == SyntaxKind(1), "owned token metadata must retain its kind")?;
    ensure(
      owned_token.text_len() == TextSize::from(4),
      "owned token metadata must retain its byte length",
    )?;
    ensure(
      !owned_node.ptr_eq(&owned_token),
      "allocation identity must never cross node and token variants",
    )?;

    let borrowed_node = GreenElementRef::from(&node);
    ensure(borrowed_node.kind() == SyntaxKind(2), "borrowed node metadata must retain its kind")?;
    ensure(
      borrowed_node.text_len() == TextSize::from(4),
      "borrowed node metadata must retain its byte length",
    )?;
    ensure(
      borrowed_node.to_owned().ptr_eq(&owned_node),
      "owning a borrowed node must clone its shared handle",
    )?;

    let borrowed_token = GreenElementRef::from(&token);
    ensure(
      borrowed_token.kind() == SyntaxKind(1),
      "borrowed token metadata must retain its kind",
    )?;
    ensure(
      borrowed_token.text_len() == TextSize::from(4),
      "borrowed token metadata must retain its byte length",
    )?;
    let cloned_token = ensure_some(
      borrowed_token.to_owned().into_token(),
      "owning a borrowed token must preserve the token variant",
    )?;
    ensure(cloned_token.ptr_eq(&token), "owning a borrowed token must clone its shared handle")?;
    ensure(
      matches!(borrowed_node, NodeOrToken::Node(_)) && matches!(borrowed_token, NodeOrToken::Token(_)),
      "borrowed conversion must preserve both discriminants",
    )
  }
}
