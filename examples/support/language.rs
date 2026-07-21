//! Shared example-language conversion declarations and tests.

/// Resolve one raw kind through an ordered, zero-based declaration.
pub(crate) fn kind_from_raw<Kind: Copy>(raw: rowan::SyntaxKind, declared: &[Kind], fallback: Kind) -> Kind {
  match declared.get(usize::from(raw.0)) {
    Some(kind) => *kind,
    None => fallback,
  }
}

/// Resolve one declared kind to its zero-based raw ordinal.
///
/// The declaration macro emits both the enum and `declared` from the same repetition, so a missing
/// kind is impossible. The sentinel branch keeps the conversion total if a declaration ever exceeds
/// the representable raw-kind space.
pub(crate) fn kind_to_raw<Kind: Copy + Eq>(kind: Kind, declared: &[Kind]) -> rowan::SyntaxKind {
  let raw = declared
    .iter()
    .position(|candidate| *candidate == kind)
    .and_then(|index| u16::try_from(index).ok());
  match raw {
    Some(raw) => rowan::SyntaxKind(raw),
    None => rowan::SyntaxKind(u16::MAX),
  }
}

/// Define one example kind enum and language from a single ordered declaration.
macro_rules! define_language {
  ($language:ident, $kind:ident, $fallback:ident, [$($variant:ident),+ $(,)?]) => {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    enum $kind {
      $($variant),+
    }

    impl From<$kind> for rowan::SyntaxKind {
      fn from(kind: $kind) -> Self {
        $crate::language_support::kind_to_raw(kind, &[$($kind::$variant),+])
      }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    enum $language {}

    impl rowan::Language for $language {
      type Kind = $kind;

      fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        $crate::language_support::kind_from_raw(raw, &[$($kind::$variant),+], $kind::$fallback)
      }

      fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        kind.into()
      }
    }

    #[cfg(test)]
    impl $crate::language_support::DeclaredLanguage for $language {
      const DECLARED_KINDS: &'static [Self::Kind] = &[$($kind::$variant),+];
      const FALLBACK: Self::Kind = $kind::$fallback;
    }
  };
}

pub(crate) use define_language;

/// Test metadata generated from the same declaration as an example language.
#[cfg(test)]
pub(crate) trait DeclaredLanguage: rowan::Language
where
  Self::Kind: 'static,
{
  /// Complete declared kind set.
  const DECLARED_KINDS: &'static [Self::Kind];
  /// Recovery kind for unknown raw values.
  const FALLBACK: Self::Kind;
}

/// Validate unknown-kind recovery and round trips for one example language.
#[cfg(test)]
pub(crate) fn verify_language<LanguageType>() -> Result<(), strict_test_support::TestFailure>
where
  LanguageType: DeclaredLanguage,
  LanguageType::Kind: 'static,
{
  strict_test_support::ensure(
    LanguageType::kind_from_raw(rowan::SyntaxKind(u16::MAX)) == LanguageType::FALLBACK,
    "unknown raw syntax kinds must recover through the declared fallback",
  )?;
  for kind in LanguageType::DECLARED_KINDS.iter().copied() {
    strict_test_support::ensure(
      LanguageType::kind_from_raw(LanguageType::kind_to_raw(kind)) == kind,
      "every declared example syntax kind must round-trip",
    )?;
  }
  Ok(())
}
