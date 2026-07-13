#![cfg(feature = "serde")]

use rowan::GreenNodeBuilder;
use rowan::Language;
use rowan::SyntaxKind as RawSyntaxKind;
use rowan::SyntaxNode;
use serde_json::json;
use strict_test_support::TestFailure;
use strict_test_support::ensure_eq;
use strict_test_support::ensure_ok;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum TestKind {
  Root,
  Branch,
  Token,
  Empty,
  Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum TestLanguage {}

impl Language for TestLanguage {
  type Kind = TestKind;

  fn kind_from_raw(raw: RawSyntaxKind) -> Self::Kind {
    match raw.0 {
      0 => TestKind::Root,
      1 => TestKind::Branch,
      2 => TestKind::Token,
      3 => TestKind::Empty,
      4 => TestKind::Error,
      5.. => TestKind::Error,
    }
  }

  fn kind_to_raw(kind: Self::Kind) -> RawSyntaxKind {
    RawSyntaxKind(match kind {
      TestKind::Root => 0,
      TestKind::Branch => 1,
      TestKind::Token => 2,
      TestKind::Empty => 3,
      TestKind::Error => 4,
    })
  }
}

#[test]
fn serde_feature_serializes_nested_nodes_tokens_and_empty_nodes() -> Result<(), TestFailure> {
  let mut builder = GreenNodeBuilder::new();
  builder.start_node(TestLanguage::kind_to_raw(TestKind::Root));
  builder.token(TestLanguage::kind_to_raw(TestKind::Token), "a");
  builder.start_node(TestLanguage::kind_to_raw(TestKind::Branch));
  builder.token(TestLanguage::kind_to_raw(TestKind::Token), "β");
  builder.finish_node();
  builder.start_node(TestLanguage::kind_to_raw(TestKind::Empty));
  builder.finish_node();
  builder.finish_node();
  let root = SyntaxNode::<TestLanguage>::new_root(builder.finish());

  let actual = ensure_ok(serde_json::to_value(root), "the typed syntax root must serialize")?;
  let expected = json!({
    "kind": "Root",
    "text_range": [0, 3],
    "children": [
      {
        "kind": "Token",
        "text_range": [0, 1],
        "text": "a"
      },
      {
        "kind": "Branch",
        "text_range": [1, 3],
        "children": [
          {
            "kind": "Token",
            "text_range": [1, 3],
            "text": "β"
          }
        ]
      },
      {
        "kind": "Empty",
        "text_range": [3, 3],
        "children": []
      }
    ]
  });

  ensure_eq(
    &actual,
    &expected,
    "serialization must preserve the complete node and token wire contract",
  )
}
