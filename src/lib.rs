//! A generic library for lossless syntax trees.
//! See `examples/s_expressions.rs` for a tutorial.
#![forbid(
    // missing_debug_implementations,
    unconditional_recursion,
    future_incompatible,
    // missing_docs,
)]
#![deny(unsafe_code)]

#[allow(unsafe_code)]
pub mod cursor;
#[allow(unsafe_code)]
mod green;

pub mod api;
mod syntax_text;
mod utility_types;

#[allow(unsafe_code)]
mod arc;
pub mod ast;
mod cow_mut;
#[cfg(feature = "serde1")]
mod serde_impls;
#[allow(unsafe_code)]
mod sll;

pub use text_size::TextLen;
pub use text_size::TextRange;
pub use text_size::TextSize;

pub use crate::api::Language;
pub use crate::api::SyntaxElement;
pub use crate::api::SyntaxElementChildren;
pub use crate::api::SyntaxNode;
pub use crate::api::SyntaxNodeChildren;
pub use crate::api::SyntaxToken;
pub use crate::green::Checkpoint;
pub use crate::green::Children;
pub use crate::green::GreenNode;
pub use crate::green::GreenNodeBuilder;
pub use crate::green::GreenNodeData;
pub use crate::green::GreenToken;
pub use crate::green::GreenTokenData;
pub use crate::green::NodeCache;
pub use crate::green::SyntaxKind;
pub use crate::syntax_text::SyntaxText;
pub use crate::utility_types::Direction;
pub use crate::utility_types::NodeOrToken;
pub use crate::utility_types::TokenAtOffset;
pub use crate::utility_types::WalkEvent;
