//! Immutable red-cursor payloads and iterative parent-chain destruction.

use triomphe::Arc;

use crate::GreenNode;
use crate::GreenToken;
use crate::TextSize;

/// Immutable payload for a red syntax node.
pub(crate) struct SyntaxNodeData {
  /// Strong parent-node link; roots have no parent.
  pub(crate) parent: Option<Arc<SyntaxNodeData>>,
  /// Immutable green node represented by this cursor.
  pub(crate) green:  GreenNode,
  /// Element index in the parent, or zero for a root.
  pub(crate) index:  usize,
  /// Checked absolute UTF-8 byte offset.
  pub(crate) offset: TextSize,
}

/// Immutable payload for a red syntax token.
pub(crate) struct SyntaxTokenData {
  /// Strong parent-node link.
  pub(crate) parent: Option<Arc<SyntaxNodeData>>,
  /// Immutable green token represented by this cursor.
  pub(crate) green:  GreenToken,
  /// Token index in its parent.
  pub(crate) index:  usize,
  /// Checked absolute UTF-8 byte offset.
  pub(crate) offset: TextSize,
}

/// Iteratively release a uniquely owned parent chain.
fn release_parent_chain(mut parent: Option<Arc<SyntaxNodeData>>) {
  let mut retained_green_nodes = Vec::new();
  while let Some(parent_arc) = parent {
    match Arc::try_unwrap(parent_arc) {
      Ok(mut parent_payload) => {
        retained_green_nodes.push(parent_payload.green.clone());
        parent = parent_payload.parent.take();
      }
      Err(shared_parent) => {
        drop(shared_parent);
        parent = None;
      }
    }
  }

  while let Some(green_node) = retained_green_nodes.pop() {
    drop(green_node);
  }
}

impl Drop for SyntaxNodeData {
  fn drop(&mut self) {
    release_parent_chain(self.parent.take());
  }
}

impl Drop for SyntaxTokenData {
  fn drop(&mut self) {
    release_parent_chain(self.parent.take());
  }
}
