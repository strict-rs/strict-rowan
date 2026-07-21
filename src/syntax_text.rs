//! Validated chunked text views over syntax subtrees.

use std::convert::Infallible;
use std::fmt;
use std::ops;

use thiserror::Error;

use crate::TextRange;
use crate::TextSize;
use crate::cursor::RangeError;
use crate::cursor::SyntaxNode;

/// A range-error or caller callback failure while visiting text chunks.
#[derive(Debug, Error)]
pub enum ChunkError<CallbackError> {
  /// Rowan rejected an internal or caller-provided range.
  #[error(transparent)]
  Range(#[from] RangeError),
  /// The caller's callback returned an error.
  #[error("text chunk callback failed")]
  Callback(CallbackError),
}

/// Public range vocabulary accepted by [`SyntaxText::slice`].
pub trait SyntaxTextRange {
  /// Optional inclusive start offset relative to the text view.
  fn start(&self) -> Option<TextSize>;

  /// Optional exclusive end offset relative to the text view.
  fn end(&self) -> Option<TextSize>;
}

/// A validated, possibly cross-token text view.
#[derive(Clone)]
pub struct SyntaxText {
  /// Subtree owning every token referenced by this view.
  node:  SyntaxNode,
  /// Absolute validated source range.
  range: TextRange,
}

impl SyntaxText {
  /// Create a whole-subtree text view.
  pub(crate) fn new(node: SyntaxNode) -> Self {
    let range = node.text_range();
    Self {
      node,
      range,
    }
  }

  /// Return this view's UTF-8 byte length.
  pub fn len(&self) -> TextSize {
    self.range.len()
  }

  /// Test whether this view is empty.
  pub fn is_empty(&self) -> bool {
    self.range.is_empty()
  }

  /// Test whether this view contains `character`.
  ///
  /// # Errors
  ///
  /// Returns a [`RangeError`] if an internal chunk boundary is not representable. Valid
  /// `SyntaxText` values do not produce that state.
  pub fn contains_char(&self, character: char) -> Result<bool, RangeError> {
    Ok(self.validated_string()?.contains(character))
  }

  /// Find the first relative UTF-8 byte offset of `character`.
  ///
  /// # Errors
  ///
  /// Returns a [`RangeError`] if an internal chunk boundary or result offset is not representable.
  pub fn find_char(&self, character: char) -> Result<Option<TextSize>, RangeError> {
    let text = self.validated_string()?;
    text
      .find(character)
      .map(|byte_index| {
        TextSize::try_from(byte_index).map_err(|_| RangeError::OffsetArithmeticOverflow {
          base:  TextSize::default(),
          delta: self.len(),
        })
      })
      .transpose()
  }

  /// Return the character beginning at a relative UTF-8 byte offset.
  ///
  /// `offset == self.len()` returns `Ok(None)`.
  ///
  /// # Errors
  ///
  /// Returns [`RangeError::OffsetOutOfBounds`] beyond the end and
  /// [`RangeError::NotCharBoundary`] inside a code point.
  pub fn char_at(&self, offset: TextSize) -> Result<Option<char>, RangeError> {
    let valid = TextRange::up_to(self.len());
    if offset > self.len() {
      return Err(RangeError::OffsetOutOfBounds {
        offset,
        valid,
      });
    }
    if offset == self.len() {
      return Ok(None);
    }
    let text = self.validated_string()?;
    let byte_index = usize::from(offset);
    if !text.is_char_boundary(byte_index) {
      return Err(RangeError::NotCharBoundary {
        offset,
      });
    }
    Ok(text.get(byte_index..).and_then(|suffix| suffix.chars().next()))
  }

  /// Create a validated subview using offsets relative to this view.
  ///
  /// # Errors
  ///
  /// Returns [`RangeError::ReversedRange`], [`RangeError::RangeOutOfBounds`],
  /// [`RangeError::NotCharBoundary`], or [`RangeError::OffsetArithmeticOverflow`] for invalid
  /// caller input.
  pub fn slice<RangeType: SyntaxTextRange>(&self, range: RangeType) -> Result<Self, RangeError> {
    let start = range.start().map_or(TextSize::default(), std::convert::identity);
    let end = match range.end() {
      Some(end) => end,
      None => self.len(),
    };
    if start > end {
      return Err(RangeError::ReversedRange {
        start,
        end,
      });
    }
    let valid = TextRange::up_to(self.len());
    if end > self.len() {
      return Err(RangeError::RangeOutOfBounds {
        requested: TextRange::new(start, end),
        valid,
      });
    }
    let text = self.validated_string()?;
    for endpoint in [start, end] {
      if !text.is_char_boundary(usize::from(endpoint)) {
        return Err(RangeError::NotCharBoundary {
          offset: endpoint
        });
      }
    }
    let absolute_start = self
      .range
      .start()
      .checked_add(start)
      .ok_or(RangeError::OffsetArithmeticOverflow {
        base:  self.range.start(),
        delta: start,
      })?;
    let absolute_end = self
      .range
      .start()
      .checked_add(end)
      .ok_or(RangeError::OffsetArithmeticOverflow {
        base:  self.range.start(),
        delta: end,
      })?;
    Ok(Self {
      node:  self.node.clone(),
      range: TextRange::new(absolute_start, absolute_end),
    })
  }

  /// Fold every non-overlapping token chunk intersecting this view.
  ///
  /// # Errors
  ///
  /// Returns [`ChunkError::Range`] for an invalid internal UTF-8/range boundary and
  /// [`ChunkError::Callback`] when `callback` fails.
  pub fn try_fold_chunks<Accumulator, CallbackError>(
    &self,
    initial: Accumulator,
    mut callback: impl FnMut(Accumulator, &str) -> Result<Accumulator, CallbackError>,
  ) -> Result<Accumulator, ChunkError<CallbackError>> {
    self
      .node
      .descendants_with_tokens()
      .filter_map(|element| element.into_token())
      .try_fold(initial, |accumulator, token| {
        match intersecting_chunk(self.range, token.text_range(), token.text())? {
          Some(chunk) => callback(accumulator, chunk).map_err(ChunkError::Callback),
          None => Ok(accumulator),
        }
      })
  }

  /// Visit every non-overlapping token chunk intersecting this view.
  ///
  /// # Errors
  ///
  /// Returns [`ChunkError::Range`] for an invalid internal UTF-8/range boundary and
  /// [`ChunkError::Callback`] when `callback` fails.
  pub fn try_for_each_chunk<CallbackError>(
    &self,
    mut callback: impl FnMut(&str) -> Result<(), CallbackError>,
  ) -> Result<(), ChunkError<CallbackError>> {
    self.try_fold_chunks((), |(), chunk| callback(chunk))
  }

  /// Visit every non-overlapping token chunk intersecting this view.
  ///
  /// # Errors
  ///
  /// Returns a [`RangeError`] for an invalid internal UTF-8/range boundary.
  pub fn for_each_chunk(&self, mut callback: impl FnMut(&str)) -> Result<(), RangeError> {
    match self.try_for_each_chunk(|chunk| {
      callback(chunk);
      Ok::<(), Infallible>(())
    }) {
      Ok(()) => Ok(()),
      Err(ChunkError::Range(error)) => Err(error),
      Err(ChunkError::Callback(impossible)) => match impossible {},
    }
  }

  /// Materialize a validated string for character-oriented operations.
  fn validated_string(&self) -> Result<String, RangeError> {
    let mut text = String::new();
    self.for_each_chunk(|chunk| text.push_str(chunk))?;
    Ok(text)
  }
}

/// Borrow the validated token substring intersecting a syntax-text view.
fn intersecting_chunk(view_range: TextRange, token_range: TextRange, token_text: &str) -> Result<Option<&str>, RangeError> {
  let Some(intersection) = view_range.intersect(token_range) else {
    return Ok(None);
  };
  let relative = intersection
    .checked_sub(token_range.start())
    .ok_or(RangeError::OffsetArithmeticOverflow {
      base:  token_range.start(),
      delta: intersection.start(),
    })?;
  let start = usize::from(relative.start());
  let end = usize::from(relative.end());
  token_text
    .get(start..end)
    .map(Some)
    .ok_or_else(|| invalid_chunk_boundary(view_range, token_range, token_text, relative))
}

/// Describe an invalid token substring boundary relative to its syntax-text view.
fn invalid_chunk_boundary(view_range: TextRange, token_range: TextRange, token_text: &str, relative: TextRange) -> RangeError {
  let start = usize::from(relative.start());
  let rejected = if token_text.is_char_boundary(start) {
    relative.end()
  } else {
    relative.start()
  };
  let absolute = match token_range.start().checked_add(rejected) {
    Some(absolute) => absolute,
    None => {
      return RangeError::OffsetArithmeticOverflow {
        base:  token_range.start(),
        delta: rejected,
      };
    }
  };
  match absolute.checked_sub(view_range.start()) {
    Some(offset) => RangeError::NotCharBoundary {
      offset,
    },
    None => RangeError::OffsetArithmeticOverflow {
      base:  view_range.start(),
      delta: absolute,
    },
  }
}

impl fmt::Debug for SyntaxText {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Debug::fmt(&self.to_string(), formatter)
  }
}

impl fmt::Display for SyntaxText {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self.try_for_each_chunk(|chunk| fmt::Display::fmt(chunk, formatter)) {
      Ok(()) => Ok(()),
      Err(ChunkError::Range(_)) | Err(ChunkError::Callback(_)) => Err(fmt::Error),
    }
  }
}

impl From<SyntaxText> for String {
  fn from(text: SyntaxText) -> Self {
    text.to_string()
  }
}

impl PartialEq<str> for SyntaxText {
  fn eq(&self, other: &str) -> bool {
    self.validated_string().is_ok_and(|text| text == other)
  }
}

impl PartialEq<SyntaxText> for str {
  fn eq(&self, other: &SyntaxText) -> bool {
    other == self
  }
}

impl PartialEq<&'_ str> for SyntaxText {
  fn eq(&self, other: &&str) -> bool {
    self == *other
  }
}

impl PartialEq<SyntaxText> for &'_ str {
  fn eq(&self, other: &SyntaxText) -> bool {
    other == self
  }
}

impl PartialEq for SyntaxText {
  fn eq(&self, other: &Self) -> bool {
    match (self.validated_string(), other.validated_string()) {
      (Ok(left), Ok(right)) => left == right,
      (Ok(_), Err(_)) | (Err(_), Ok(_)) | (Err(_), Err(_)) => false,
    }
  }
}

impl Eq for SyntaxText {}

impl SyntaxTextRange for TextRange {
  fn start(&self) -> Option<TextSize> {
    Some(TextRange::start(*self))
  }

  fn end(&self) -> Option<TextSize> {
    Some(TextRange::end(*self))
  }
}

impl SyntaxTextRange for ops::Range<TextSize> {
  fn start(&self) -> Option<TextSize> {
    Some(self.start)
  }

  fn end(&self) -> Option<TextSize> {
    Some(self.end)
  }
}

impl SyntaxTextRange for ops::RangeFrom<TextSize> {
  fn start(&self) -> Option<TextSize> {
    Some(self.start)
  }

  fn end(&self) -> Option<TextSize> {
    None
  }
}

impl SyntaxTextRange for ops::RangeTo<TextSize> {
  fn start(&self) -> Option<TextSize> {
    None
  }

  fn end(&self) -> Option<TextSize> {
    Some(self.end)
  }
}

impl SyntaxTextRange for ops::RangeFull {
  fn start(&self) -> Option<TextSize> {
    None
  }

  fn end(&self) -> Option<TextSize> {
    None
  }
}

#[cfg(test)]
mod tests {
  use std::convert::Infallible;

  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;
  use strict_test_support::ensure_ok;
  use strict_test_support::ensure_some;

  use super::ChunkError;
  use super::SyntaxText;
  use super::SyntaxTextRange;
  use super::invalid_chunk_boundary;
  use crate::GreenElement;
  use crate::GreenNode;
  use crate::GreenToken;
  use crate::RangeError;
  use crate::SyntaxKind;
  use crate::TextRange;
  use crate::TextSize;
  use crate::cursor::SyntaxNode;

  /// Build a chunked text fixture with one token per supplied string.
  fn chunked(chunks: &[&str]) -> Result<SyntaxText, TestFailure> {
    let mut children = Vec::with_capacity(chunks.len());
    for chunk in chunks {
      let token = ensure_ok(GreenToken::new(SyntaxKind(1), chunk), "the syntax-text token fixture must allocate")?;
      children.push(GreenElement::from(token));
    }
    let green = ensure_ok(
      GreenNode::new(SyntaxKind(0), children),
      "the syntax-text root fixture must allocate",
    )?;
    Ok(SyntaxNode::new_root(green).text())
  }

  #[test]
  fn character_queries_use_relative_unicode_byte_offsets() -> Result<(), TestFailure> {
    let text = chunked(&["a", "β", "cd"])?;
    ensure(text.len() == TextSize::from(5), "text length must count UTF-8 bytes")?;
    ensure(!text.is_empty(), "the fixture text must be non-empty")?;
    ensure(
      ensure_ok(text.contains_char('β'), "contains_char must validate the view")?,
      "beta must be present",
    )?;
    ensure(
      !ensure_ok(text.contains_char('z'), "contains_char must validate an absent query")?,
      "an absent character must remain absent",
    )?;
    ensure(
      ensure_some(
        ensure_ok(text.find_char('β'), "find_char must validate the view")?,
        "beta must have an offset",
      )? == TextSize::from(1),
      "find_char must report a relative UTF-8 byte offset",
    )?;
    ensure(
      ensure_ok(text.find_char('z'), "an absent find query must still validate")?.is_none(),
      "an absent character must return None",
    )?;
    ensure_eq(
      &ensure_some(
        ensure_ok(text.char_at(TextSize::from(1)), "a character boundary must validate")?,
        "beta must exist",
      )?,
      &'β',
      "char_at must decode a complete multibyte character",
    )?;
    ensure(
      text.char_at(TextSize::from(2))
        == Err(RangeError::NotCharBoundary {
          offset: TextSize::from(2)
        }),
      "an in-range byte inside beta must be rejected",
    )?;
    ensure(
      ensure_ok(text.char_at(TextSize::from(5)), "the end offset must validate")?.is_none(),
      "char_at(len) must return None",
    )?;
    ensure(
      text.char_at(TextSize::from(6))
        == Err(RangeError::OffsetOutOfBounds {
          offset: TextSize::from(6),
          valid:  TextRange::up_to(TextSize::from(5)),
        }),
      "an offset beyond the text must be rejected",
    )
  }

  /// Caller-defined range used to exercise reversed relative endpoints safely.
  struct RelativeRange {
    /// Inclusive start.
    start: TextSize,
    /// Exclusive end.
    end:   TextSize,
  }

  impl SyntaxTextRange for RelativeRange {
    fn start(&self) -> Option<TextSize> {
      Some(self.start)
    }

    fn end(&self) -> Option<TextSize> {
      Some(self.end)
    }
  }

  #[test]
  fn slices_validate_bounds_and_cross_token_boundaries() -> Result<(), TestFailure> {
    let text = chunked(&["a", "β", "cd"])?;
    let cross_token = ensure_ok(
      text.slice(TextSize::from(1)..TextSize::from(4)),
      "a Unicode-aligned cross-token slice must succeed",
    )?;
    ensure_eq(
      &cross_token.to_string(),
      &"βc".to_owned(),
      "cross-token slices must preserve exact text",
    )?;
    ensure_eq(
      &ensure_ok(cross_token.slice(..), "a full subview slice must succeed")?.to_string(),
      &"βc".to_owned(),
      "slice offsets must be relative to the current text view",
    )?;
    ensure(
      text.slice(RelativeRange {
        start: TextSize::from(4),
        end:   TextSize::from(3),
      }) == Err(RangeError::ReversedRange {
        start: TextSize::from(4),
        end:   TextSize::from(3),
      }),
      "reversed bounds must be rejected before containment",
    )?;
    ensure(
      text.slice(TextSize::from(0)..TextSize::from(6))
        == Err(RangeError::RangeOutOfBounds {
          requested: TextRange::new(TextSize::from(0), TextSize::from(6)),
          valid:     TextRange::up_to(TextSize::from(5)),
        }),
      "an end beyond the view must be rejected",
    )?;
    ensure(
      text.slice(TextSize::from(2)..TextSize::from(3))
        == Err(RangeError::NotCharBoundary {
          offset: TextSize::from(2)
        }),
      "a slice endpoint inside beta must be rejected",
    )
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  enum CallbackFailure {
    /// Deliberate caller failure.
    Stop,
  }

  #[test]
  fn chunk_visitors_preserve_segmentation_and_callback_errors() -> Result<(), TestFailure> {
    let text = chunked(&["a", "β", "cd"])?;
    let mut chunks = Vec::new();
    ensure_ok(
      text.for_each_chunk(|chunk| chunks.push(chunk.to_owned())),
      "infallible chunk visitation must succeed",
    )?;
    ensure(
      chunks == vec!["a".to_owned(), "β".to_owned(), "cd".to_owned()],
      "whole-view visitation must retain token segmentation",
    )?;

    let folded = ensure_ok(
      text.try_fold_chunks(String::new(), |mut accumulated, chunk| {
        accumulated.push_str(chunk);
        Ok::<String, Infallible>(accumulated)
      }),
      "a successful callback fold must preserve text",
    )?;
    ensure_eq(&folded, &"aβcd".to_owned(), "folded chunks must reproduce display text")?;

    let callback_error = text.try_for_each_chunk(|chunk| {
      if chunk == "β" {
        Err(CallbackFailure::Stop)
      } else {
        Ok(())
      }
    });
    ensure(
      matches!(callback_error, Err(ChunkError::Callback(CallbackFailure::Stop))),
      "caller failure must remain distinguishable from a Rowan range error",
    )
  }

  #[test]
  fn text_equality_depends_on_content_not_token_segmentation() -> Result<(), TestFailure> {
    let segmented = chunked(&["a", "β", "cd"])?;
    let combined = chunked(&["aβcd"])?;
    let different = chunked(&["aβce"])?;
    ensure_eq(
      &segmented,
      &combined,
      "equal text with different token segmentation must compare equal",
    )?;
    ensure(segmented == "aβcd", "SyntaxText must compare with a matching str")?;
    ensure("aβcd" == segmented, "str must compare symmetrically with SyntaxText")?;
    ensure(segmented != different, "different text must compare unequal")?;

    let empty = chunked(&[])?;
    ensure(empty.is_empty(), "an empty root must produce an empty text view")?;
    ensure_eq(&empty.to_string(), &String::new(), "empty text display must be empty")
  }

  #[test]
  fn text_conversions_and_range_vocabularies_preserve_relative_views() -> Result<(), TestFailure> {
    let text = chunked(&["a", "β", "cd"])?;
    ensure_eq(
      &format!("{text:?}"),
      &"\"aβcd\"".to_owned(),
      "syntax-text debug output must render the validated complete string",
    )?;
    let owned: String = text.clone().into();
    ensure_eq(&owned, &"aβcd".to_owned(), "String conversion must preserve validated text")?;
    ensure(
      <SyntaxText as PartialEq<&str>>::eq(&text, &"aβcd"),
      "SyntaxText must compare with a borrowed matching str",
    )?;
    ensure(
      <&str as PartialEq<SyntaxText>>::eq(&"aβcd", &text),
      "a borrowed str must compare symmetrically with SyntaxText",
    )?;
    ensure(
      !<SyntaxText as PartialEq<&str>>::eq(&text, &"aβce"),
      "borrowed unequal text must remain unequal",
    )?;

    let by_text_range = ensure_ok(
      text.slice(TextRange::new(TextSize::from(1), TextSize::from(3))),
      "TextRange bounds must be accepted as relative syntax-text offsets",
    )?;
    ensure_eq(
      &by_text_range.to_string(),
      &"β".to_owned(),
      "TextRange slicing must preserve a complete multibyte character",
    )?;
    ensure_eq(
      &ensure_ok(text.slice(TextSize::from(3)..), "RangeFrom bounds must extend through the view end")?.to_string(),
      &"cd".to_owned(),
      "RangeFrom slicing must remain relative to the complete view",
    )?;
    ensure_eq(
      &ensure_ok(text.slice(..TextSize::from(3)), "RangeTo bounds must begin at the view start")?.to_string(),
      &"aβ".to_owned(),
      "RangeTo slicing must retain the aligned prefix",
    )?;
    let empty = ensure_ok(
      text.slice(TextSize::from(3)..TextSize::from(3)),
      "an aligned empty range must be valid",
    )?;
    ensure(empty.is_empty(), "an empty relative range must produce an empty text view")?;
    ensure(
      ensure_ok(text.char_at(TextSize::default()), "the start character must validate")? == Some('a'),
      "char_at at zero must return the first character",
    )?;
    ensure(
      ensure_ok(text.char_at(TextSize::from(3)), "the post-beta boundary must validate")? == Some('c'),
      "char_at must continue correctly after a multibyte character",
    )?;
    ensure(
      text.slice(TextSize::default()..TextSize::from(2))
        == Err(RangeError::NotCharBoundary {
          offset: TextSize::from(2)
        }),
      "a slice end inside a code point must be rejected independently of its start",
    )
  }

  #[test]
  fn invalid_chunk_boundaries_preserve_endpoint_and_arithmetic_polarity() -> Result<(), TestFailure> {
    let maximum = TextSize::from(u32::MAX);
    let observations = [
      invalid_chunk_boundary(
        TextRange::new(TextSize::from(5), TextSize::from(7)),
        TextRange::new(TextSize::from(5), TextSize::from(7)),
        "β",
        TextRange::new(TextSize::from(1), TextSize::from(2)),
      ),
      invalid_chunk_boundary(
        TextRange::new(TextSize::from(5), TextSize::from(7)),
        TextRange::new(TextSize::from(5), TextSize::from(7)),
        "β",
        TextRange::new(TextSize::default(), TextSize::from(1)),
      ),
      invalid_chunk_boundary(
        TextRange::empty(maximum),
        TextRange::empty(maximum),
        "",
        TextRange::new(TextSize::default(), TextSize::from(1)),
      ),
      invalid_chunk_boundary(
        TextRange::empty(TextSize::from(3)),
        TextRange::new(TextSize::from(1), TextSize::from(3)),
        "ab",
        TextRange::new(TextSize::default(), TextSize::from(1)),
      ),
    ];
    ensure(
      observations
        == [
          RangeError::NotCharBoundary {
            offset: TextSize::from(1)
          },
          RangeError::NotCharBoundary {
            offset: TextSize::from(1)
          },
          RangeError::OffsetArithmeticOverflow {
            base:  maximum,
            delta: TextSize::from(1),
          },
          RangeError::OffsetArithmeticOverflow {
            base:  TextSize::from(3),
            delta: TextSize::from(2),
          },
        ],
      "the production boundary classifier must distinguish start, end, addition, and rebasing failures",
    )
  }
}
