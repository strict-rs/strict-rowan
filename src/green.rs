//! Immutable green-tree storage, construction, and interning.

mod builder;
mod element;
mod node;
mod node_cache;
mod token;

use thiserror::Error;

pub use self::builder::BuildError;
pub use self::builder::Checkpoint;
pub use self::builder::GreenNodeBuilder;
pub use self::element::GreenElement;
pub use self::element::GreenElementRef;
pub use self::node::Children;
pub(crate) use self::node::GreenChild;
pub use self::node::GreenNode;
pub use self::node_cache::NodeCache;
pub use self::token::GreenToken;
use crate::TextSize;

/// A raw kind tag shared by green and red syntax elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SyntaxKind(pub u16);

/// A validated green-tree construction or transformation failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GreenError {
  /// Token text does not fit Rowan's `TextSize` coordinate space.
  #[error("token text length {byte_len} does not fit in TextSize")]
  TokenTextTooLong {
    /// UTF-8 byte length supplied by the caller.
    byte_len: usize,
  },
  /// Adding a child would overflow Rowan's text coordinate space.
  #[error("text length overflow while adding {next:?} to {accumulated:?}")]
  TextLengthOverflow {
    /// Length accumulated before the failing child.
    accumulated: TextSize,
    /// Length of the child that could not be added.
    next:        TextSize,
  },
  /// A child index does not name an existing child.
  #[error("child index {index} is out of bounds for {child_count} children")]
  ChildIndexOutOfBounds {
    /// Rejected child index.
    index:       usize,
    /// Number of children in the node.
    child_count: usize,
  },
  /// A child splice range is reversed or extends beyond the node.
  #[error("child range {start}..{end} is invalid for {child_count} children")]
  InvalidChildRange {
    /// Inclusive range start.
    start:       usize,
    /// Exclusive range end.
    end:         usize,
    /// Number of children in the node.
    child_count: usize,
  },
  /// A replacement changes the kind of the element it replaces.
  #[error("replacement kind {actual:?} does not match expected kind {expected:?}")]
  KindMismatch {
    /// Kind of the existing element.
    expected: SyntaxKind,
    /// Kind of the replacement element.
    actual:   SyntaxKind,
  },
  /// The backing shared allocation could not be created.
  #[error("green-tree allocation failed")]
  AllocationFailed,
}

/// Convert a UTF-8 byte length into Rowan's coordinate type.
pub(crate) fn text_size_from_usize(byte_len: usize) -> Result<TextSize, GreenError> {
  TextSize::try_from(byte_len).map_err(|_| GreenError::TokenTextTooLong {
    byte_len,
  })
}

/// Add two validated text lengths without overflow.
pub(crate) fn checked_text_add(accumulated: TextSize, next: TextSize) -> Result<TextSize, GreenError> {
  accumulated.checked_add(next).ok_or(GreenError::TextLengthOverflow {
    accumulated,
    next,
  })
}

#[cfg(test)]
mod tests {
  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_ok;

  use super::GreenError;
  use super::checked_text_add;
  use super::text_size_from_usize;
  use crate::TextSize;

  #[test]
  fn checked_text_helpers_accept_bounds_and_report_overflow() -> Result<(), TestFailure> {
    let converted = ensure_ok(text_size_from_usize(3), "a small byte length must fit")?;
    ensure(converted == TextSize::from(3), "the conversion must preserve the byte length")?;
    let sum = ensure_ok(
      checked_text_add(TextSize::from(2), TextSize::from(3)),
      "representable text lengths must add",
    )?;
    ensure(sum == TextSize::from(5), "checked addition must preserve the exact sum")?;

    ensure(
      checked_text_add(TextSize::from(u32::MAX), TextSize::from(1))
        == Err(GreenError::TextLengthOverflow {
          accumulated: TextSize::from(u32::MAX),
          next:        TextSize::from(1),
        }),
      "overflow must retain both operands in the typed error",
    )
  }

  #[cfg(target_pointer_width = "64")]
  #[test]
  fn checked_text_conversion_rejects_lengths_above_text_size() -> Result<(), TestFailure> {
    ensure(
      text_size_from_usize(usize::MAX)
        == Err(GreenError::TokenTextTooLong {
          byte_len: usize::MAX
        }),
      "the production conversion helper must reject an unrepresentable byte length",
    )
  }
}
