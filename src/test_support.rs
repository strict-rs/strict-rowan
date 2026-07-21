//! Shared language fixture for typed API tests.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::mem;

use strict_test_support::TestFailure;
use strict_test_support::ensure_eq;
use strict_test_support::ensure_ok;

use crate::GreenElement;
use crate::GreenNode;
use crate::GreenToken;
use crate::Language;
use crate::SyntaxKind;
use crate::SyntaxNode;
use crate::WalkEvent;

/// Identity language used to compare raw and typed behavior without conversion noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TestLanguage {}

impl Language for TestLanguage {
  type Kind = SyntaxKind;

  fn kind_from_raw(raw: SyntaxKind) -> Self::Kind {
    raw
  }

  fn kind_to_raw(kind: Self::Kind) -> SyntaxKind {
    kind
  }
}

/// Assert a locked one-machine-word representation for one handle type.
pub(crate) fn ensure_one_word<Value>(message: &'static str) -> Result<(), TestFailure> {
  ensure_eq(&mem::size_of::<Value>(), &mem::size_of::<usize>(), message)
}

/// Compare two hashes produced by a caller-selected semantic or identity hash function.
pub(crate) fn ensure_same_hash<Value>(
  left: &Value,
  right: &Value,
  hash: impl Fn(&Value, &mut DefaultHasher),
  message: &'static str,
) -> Result<(), TestFailure> {
  let mut left_hash = DefaultHasher::new();
  hash(left, &mut left_hash);
  let mut right_hash = DefaultHasher::new();
  hash(right, &mut right_hash);
  ensure_eq(&left_hash.finish(), &right_hash.finish(), message)
}

/// Build a typed root containing one direct token for cross-module typed tests.
pub(crate) fn typed_token_root(root_kind: SyntaxKind, token_kind: SyntaxKind, text: &str) -> Result<SyntaxNode<TestLanguage>, TestFailure> {
  let token = ensure_ok(GreenToken::new(token_kind, text), "the shared typed token must allocate")?;
  let root = ensure_ok(
    GreenNode::new(root_kind, [GreenElement::from(token)]),
    "the shared typed root must allocate",
  )?;
  Ok(SyntaxNode::new_root(root))
}

/// Convert preorder events into an observable enter/leave and kind trace.
pub(crate) fn event_trace<Element>(
  events: impl IntoIterator<Item = WalkEvent<Element>>,
  kind: impl Fn(Element) -> SyntaxKind,
) -> Vec<(bool, SyntaxKind)> {
  events
    .into_iter()
    .map(|event| match event {
      WalkEvent::Enter(element) => (true, kind(element)),
      WalkEvent::Leave(element) => (false, kind(element)),
    })
    .collect()
}
