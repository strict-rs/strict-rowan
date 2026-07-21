//! Compact immutable green tokens.

use std::fmt;

use countme::Count;
use ecow::EcoString;
use triomphe::Arc;

use crate::TextSize;
use crate::green::GreenError;
use crate::green::SyntaxKind;
use crate::green::text_size_from_usize;

/// Shared token allocation contents.
#[derive(PartialEq, Eq, Hash)]
struct GreenTokenPayload {
  /// Raw token kind.
  kind:   SyntaxKind,
  /// Compact UTF-8 token text.
  text:   EcoString,
  /// Allocation accounting marker.
  _count: Count<GreenToken>,
}

/// A leaf in an immutable green syntax tree.
#[derive(Clone, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct GreenToken {
  /// Shared token payload.
  payload: Arc<GreenTokenPayload>,
}

impl GreenToken {
  /// Create a token after validating its UTF-8 byte length.
  ///
  /// # Errors
  ///
  /// Returns [`GreenError::TokenTextTooLong`] when `text` cannot be represented by `TextSize`, or
  /// [`GreenError::AllocationFailed`] when the shared payload allocation fails.
  pub fn new(kind: SyntaxKind, text: &str) -> Result<Self, GreenError> {
    let _ = text_size_from_usize(text.len())?;
    let payload = GreenTokenPayload {
      kind,
      text: EcoString::from(text),
      _count: Count::new(),
    };
    Arc::try_new(payload)
      .map(|payload| Self {
        payload,
      })
      .map_err(|_| GreenError::AllocationFailed)
  }

  /// Return this token's raw syntax kind.
  pub fn kind(&self) -> SyntaxKind {
    self.payload.kind
  }

  /// Borrow this token's exact UTF-8 text.
  pub fn text(&self) -> &str {
    &self.payload.text
  }

  /// Return this token's UTF-8 byte length.
  pub fn text_len(&self) -> TextSize {
    match text_size_from_usize(self.payload.text.len()) {
      Ok(text_len) => text_len,
      Err(_) => TextSize::from(u32::MAX),
    }
  }

  /// Test whether two tokens share the same allocation.
  pub fn ptr_eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.payload, &other.payload)
  }

  /// Hash the token allocation identity rather than its contents.
  pub(crate) fn hash_identity<HasherType: std::hash::Hasher>(&self, state: &mut HasherType) {
    std::ptr::hash(Arc::as_ptr(&self.payload), state);
  }
}

impl fmt::Debug for GreenToken {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    crate::debug::write(crate::debug::DebugTarget::GreenToken(self), formatter)
  }
}

impl fmt::Display for GreenToken {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Display::fmt(self.text(), formatter)
  }
}

#[cfg(test)]
mod tests {
  use std::hash::Hash;

  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;
  use strict_test_support::ensure_ne;
  use strict_test_support::ensure_ok;

  use super::GreenToken;
  use crate::SyntaxKind;
  use crate::TextSize;
  use crate::test_support::ensure_one_word;
  use crate::test_support::ensure_same_hash;

  /// Construct a green token through its real fallible API.
  fn token(kind: u16, text: &str) -> Result<GreenToken, TestFailure> {
    ensure_ok(GreenToken::new(SyntaxKind(kind), text), "the green token must allocate")
  }

  #[test]
  fn token_handles_preserve_compact_and_shared_text_contracts() -> Result<(), TestFailure> {
    for text in ["", "short", "a token text long enough to spill", "aβ🙂"] {
      let green = token(7, text)?;
      ensure(green.kind() == SyntaxKind(7), "the token kind must remain exact")?;
      ensure_eq(&green.text(), &text, "the token text must remain exact")?;
      let expected_len = ensure_ok(TextSize::try_from(text.len()), "the fixture length must fit")?;
      ensure(green.text_len() == expected_len, "the token length must count UTF-8 bytes")?;
      ensure_eq(&green.to_string(), &text.to_owned(), "token display must preserve text")?;
    }
    Ok(())
  }

  #[test]
  fn token_identity_is_distinct_from_structural_equality() -> Result<(), TestFailure> {
    let first = token(1, "same")?;
    let shared = first.clone();
    let separate = token(1, "same")?;
    let different_kind = token(2, "same")?;
    let different_text = token(1, "different")?;

    ensure(first.ptr_eq(&shared), "cloning a token handle must retain allocation identity")?;
    ensure(!first.ptr_eq(&separate), "separate equal tokens must retain distinct allocations")?;
    ensure_eq(&first, &separate, "separate equal tokens must compare structurally")?;
    ensure_ne(&first, &different_kind, "different kinds must compare unequal")?;
    ensure_ne(&first, &different_text, "different text must compare unequal")?;
    ensure_eq(
      &format!("{first:?}"),
      &"GreenToken { kind: SyntaxKind(1), text: \"same\" }".to_owned(),
      "token debug output must expose stable semantic metadata",
    )?;

    ensure_same_hash(
      &first,
      &separate,
      Hash::hash,
      "structurally equal tokens must produce equal structural hashes",
    )?;
    ensure_same_hash(
      &first,
      &shared,
      GreenToken::hash_identity,
      "cloned token handles must produce equal allocation-identity hashes",
    )
  }

  #[test]
  fn token_handle_remains_one_machine_word() -> Result<(), TestFailure> {
    ensure_one_word::<GreenToken>("a green token handle must remain one machine word")
  }
}
