//! Centralized semantic debug formatting for immutable green and red handles.

use std::fmt;

use crate::GreenNode;
use crate::GreenToken;
use crate::cursor::SyntaxNode;
use crate::cursor::SyntaxToken;

/// One handle whose stable semantic metadata is being formatted.
pub(crate) enum DebugTarget<'a> {
  /// Immutable green node handle.
  GreenNode(&'a GreenNode),
  /// Immutable green token handle.
  GreenToken(&'a GreenToken),
  /// Immutable raw node cursor.
  SyntaxNode(&'a SyntaxNode),
  /// Immutable raw token cursor.
  SyntaxToken(&'a SyntaxToken),
}

/// Render the stable semantic debug schema for one handle.
pub(crate) fn write(target: DebugTarget<'_>, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
  match target {
    DebugTarget::GreenNode(node) => formatter
      .debug_struct("GreenNode")
      .field("kind", &node.kind())
      .field("text_len", &node.text_len())
      .field("child_count", &node.children().len())
      .finish(),
    DebugTarget::GreenToken(token) => formatter
      .debug_struct("GreenToken")
      .field("kind", &token.kind())
      .field("text", &token.text())
      .finish(),
    DebugTarget::SyntaxNode(node) => formatter
      .debug_struct("SyntaxNode")
      .field("kind", &node.kind())
      .field("text_range", &node.text_range())
      .finish(),
    DebugTarget::SyntaxToken(token) => formatter
      .debug_struct("SyntaxToken")
      .field("kind", &token.kind())
      .field("text_range", &token.text_range())
      .field("text", &token.text())
      .finish(),
  }
}
