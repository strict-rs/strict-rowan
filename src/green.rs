mod builder;
mod element;
mod node;
mod node_cache;
mod token;

pub use self::builder::Checkpoint;
pub use self::builder::GreenNodeBuilder;
use self::element::GreenElement;
pub(crate) use self::element::GreenElementRef;
pub use self::node::Children;
pub(crate) use self::node::GreenChild;
pub use self::node::GreenNode;
pub use self::node::GreenNodeData;
pub use self::node_cache::NodeCache;
pub use self::token::GreenToken;
pub use self::token::GreenTokenData;

/// SyntaxKind is a type tag for each token or node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SyntaxKind(pub u16);

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn assert_send_sync() {
    fn f<T: Send + Sync>() {}
    f::<GreenNode>();
    f::<GreenToken>();
    f::<GreenElement>();
  }

  #[test]
  fn test_size_of() {
    use std::mem::size_of;

    eprintln!("GreenNode          {}", size_of::<GreenNode>());
    eprintln!("GreenToken         {}", size_of::<GreenToken>());
    eprintln!("GreenElement       {}", size_of::<GreenElement>());
  }
}
