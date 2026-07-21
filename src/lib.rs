//! Generic, immutable, lossless syntax trees.
//!
//! Rowan separates compact structurally shared green trees from transient immutable red cursors,
//! and provides an exclusively borrowed [`SyntaxEditor`] for transactional mutation.
//!
//! See the [0.17 migration guide](../docs/migration-0.17.md) for the breaking safe-ownership and
//! fallible-API changes.
#![forbid(unsafe_code)]

pub mod api;
pub mod ast;
pub mod cursor;
mod cursor_data;
mod cursor_editor;
mod cursor_traversal;
mod debug;
mod green;
#[cfg(feature = "serde")]
mod serde_impls;
mod syntax_text;
#[cfg(test)]
mod test_support;
mod utility_types;

pub use text_size::TextLen;
pub use text_size::TextRange;
pub use text_size::TextSize;

pub use crate::api::Language;
pub use crate::api::Preorder;
pub use crate::api::PreorderWithTokens;
pub use crate::api::SyntaxEditor;
pub use crate::api::SyntaxElement;
pub use crate::api::SyntaxElementChildren;
pub use crate::api::SyntaxNode;
pub use crate::api::SyntaxNodeChildren;
pub use crate::api::SyntaxToken;
pub use crate::ast::AstError;
pub use crate::ast::ResolveError;
pub use crate::cursor::RangeError;
pub use crate::cursor::TraversalError;
pub use crate::cursor_editor::EditError;
pub use crate::cursor_editor::EditorElementId;
pub use crate::cursor_editor::EditorNodeId;
pub use crate::cursor_editor::EditorTokenId;
pub use crate::cursor_editor::SpliceOutcome;
pub use crate::green::BuildError;
pub use crate::green::Checkpoint;
pub use crate::green::Children;
pub use crate::green::GreenElement;
pub use crate::green::GreenElementRef;
pub use crate::green::GreenError;
pub use crate::green::GreenNode;
pub use crate::green::GreenNodeBuilder;
pub use crate::green::GreenToken;
pub use crate::green::NodeCache;
pub use crate::green::SyntaxKind;
pub use crate::syntax_text::ChunkError;
pub use crate::syntax_text::SyntaxText;
pub use crate::syntax_text::SyntaxTextRange;
pub use crate::utility_types::Direction;
pub use crate::utility_types::NodeOrToken;
pub use crate::utility_types::TokenAtOffset;
pub use crate::utility_types::WalkEvent;
