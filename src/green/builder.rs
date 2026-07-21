//! Protocol-checked construction of green trees.

use std::fmt;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;

use thiserror::Error;

use crate::NodeOrToken;
use crate::green::GreenError;
use crate::green::GreenNode;
use crate::green::SyntaxKind;
use crate::green::node_cache::CachedElement;
use crate::green::node_cache::NodeCache;

/// Pointer-identity token used to keep checkpoints scoped to one builder or frame.
#[derive(Clone)]
struct Identity(Arc<()>);

impl Identity {
  /// Allocate a new unique identity token.
  fn new() -> Self {
    Self(Arc::new(()))
  }
}

impl PartialEq for Identity {
  fn eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.0, &other.0)
  }
}

impl Eq for Identity {}

impl Hash for Identity {
  fn hash<HasherType: Hasher>(&self, state: &mut HasherType) {
    std::ptr::hash(Arc::as_ptr(&self.0), state);
  }
}

impl fmt::Debug for Identity {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.debug_tuple("Identity").finish()
  }
}

/// A reusable location within one open builder frame.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Checkpoint {
  /// Builder that created this checkpoint.
  builder:     Identity,
  /// Open frame that owned the child position.
  frame:       Identity,
  /// Child position within the frame.
  child_index: usize,
}

/// A green builder protocol or allocation failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuildError {
  /// Green construction failed.
  #[error(transparent)]
  Green(#[from] GreenError),
  /// No syntax node is currently open.
  #[error("no syntax node is open")]
  NoOpenNode,
  /// A checkpoint belongs to another builder.
  #[error("checkpoint belongs to another builder")]
  ForeignCheckpoint,
  /// A checkpoint's owning frame is no longer current.
  #[error("checkpoint frame is stale")]
  StaleCheckpoint,
  /// A checkpoint points beyond its current frame.
  #[error("checkpoint index {index} is out of bounds for {child_count} children")]
  CheckpointOutOfBounds {
    /// Rejected child index.
    index:       usize,
    /// Current number of frame children.
    child_count: usize,
  },
  /// `finish` was called while syntax nodes remained open.
  #[error("builder has {count} unclosed nodes")]
  UnclosedNodes {
    /// Number of unclosed syntax nodes.
    count: usize,
  },
  /// `finish` found zero or multiple root elements.
  #[error("builder produced {count} root elements instead of one")]
  InvalidRootArity {
    /// Number of completed root elements.
    count: usize,
  },
  /// The sole root element was a token rather than a node.
  #[error("builder root must be a node")]
  RootMustBeNode,
}

/// One open node or the builder's permanent root collection frame.
#[derive(Debug)]
struct Frame {
  /// Stable frame identity used by checkpoints.
  identity: Identity,
  /// Node kind.
  kind:     SyntaxKind,
  /// Completed children in source order.
  children: Vec<CachedElement>,
}

/// Permanent collection for completed root elements.
#[derive(Debug)]
struct RootFrame {
  /// Stable identity used by checkpoints captured outside an open node.
  identity: Identity,
  /// Completed root elements in source order.
  children: Vec<CachedElement>,
}

/// An owned or exclusively borrowed node cache.
enum BuilderCache<'cache> {
  /// Cache owned by this builder.
  Owned(NodeCache),
  /// Cache borrowed exclusively from the caller.
  Borrowed(&'cache mut NodeCache),
}

impl fmt::Debug for BuilderCache<'_> {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Owned(_) => formatter.write_str("Owned(NodeCache)"),
      Self::Borrowed(_) => formatter.write_str("Borrowed(NodeCache)"),
    }
  }
}

impl BuilderCache<'_> {
  /// Borrow the selected cache mutably.
  fn get_mut(&mut self) -> &mut NodeCache {
    match self {
      Self::Owned(cache) => cache,
      Self::Borrowed(cache) => cache,
    }
  }
}

/// A protocol-checked builder for one immutable green tree.
#[derive(Debug)]
pub struct GreenNodeBuilder<'cache> {
  /// Unique builder identity used to reject foreign checkpoints.
  identity: Identity,
  /// Permanent collection for completed root elements.
  root:     RootFrame,
  /// Open syntax-node frames.
  frames:   Vec<Frame>,
  /// Cache selected by the constructor.
  cache:    BuilderCache<'cache>,
}

/// Create a builder around its selected cache ownership mode.
fn builder_with_cache(cache: BuilderCache<'_>) -> GreenNodeBuilder<'_> {
  GreenNodeBuilder {
    identity: Identity::new(),
    root: RootFrame {
      identity: Identity::new(),
      children: Vec::new(),
    },
    frames: Vec::new(),
    cache,
  }
}

impl GreenNodeBuilder<'_> {
  /// Create a builder with a private node cache.
  pub fn new() -> GreenNodeBuilder<'static> {
    builder_with_cache(BuilderCache::Owned(NodeCache::default()))
  }

  /// Create a builder that reuses an exclusively borrowed node cache.
  pub fn with_cache(cache: &mut NodeCache) -> GreenNodeBuilder<'_> {
    builder_with_cache(BuilderCache::Borrowed(cache))
  }

  /// Append a token to the current frame.
  ///
  /// # Errors
  ///
  /// Returns [`BuildError::Green`] when token construction fails. The builder is unchanged on
  /// failure.
  pub fn token(&mut self, kind: SyntaxKind, text: &str) -> Result<(), BuildError> {
    let token = self.cache.get_mut().token(kind, text)?;
    self.current_children_mut().push(token);
    Ok(())
  }

  /// Start a new node and make it the current frame.
  pub fn start_node(&mut self, kind: SyntaxKind) {
    self.frames.push(Frame {
      identity: Identity::new(),
      kind,
      children: Vec::new(),
    });
  }

  /// Finish the current node and append it to its parent frame.
  ///
  /// # Errors
  ///
  /// Returns [`BuildError::NoOpenNode`] at the permanent root frame, or [`BuildError::Green`] when
  /// node construction fails. The builder is unchanged on failure.
  pub fn finish_node(&mut self) -> Result<(), BuildError> {
    let (frame, parent_frames) = self.frames.split_last_mut().ok_or(BuildError::NoOpenNode)?;
    let node = self.cache.get_mut().node(frame.kind, &frame.children)?;
    let parent_count = parent_frames.len();
    if let Some(parent) = parent_frames.last_mut() {
      parent.children.push(node);
    } else {
      self.root.children.push(node);
    }
    self.frames.truncate(parent_count);
    Ok(())
  }

  /// Capture the current child position for a later `start_node_at` call.
  pub fn checkpoint(&self) -> Checkpoint {
    let (frame_identity, children) = self.current_identity_and_children();
    Checkpoint {
      builder:     self.identity.clone(),
      frame:       frame_identity.clone(),
      child_index: children.len(),
    }
  }

  /// Start a node whose initial children begin at a checkpoint in the current frame.
  ///
  /// # Errors
  ///
  /// Returns [`BuildError::ForeignCheckpoint`], [`BuildError::StaleCheckpoint`], or
  /// [`BuildError::CheckpointOutOfBounds`] in that validation order. The builder is unchanged on
  /// failure.
  pub fn start_node_at(&mut self, checkpoint: &Checkpoint, kind: SyntaxKind) -> Result<(), BuildError> {
    if checkpoint.builder != self.identity {
      return Err(BuildError::ForeignCheckpoint);
    }
    let (current_identity, current_children) = self.current_identity_and_children();
    if checkpoint.frame != *current_identity {
      return Err(BuildError::StaleCheckpoint);
    }
    let child_count = current_children.len();
    if checkpoint.child_index > child_count {
      return Err(BuildError::CheckpointOutOfBounds {
        index: checkpoint.child_index,
        child_count,
      });
    }
    let moved_children = current_children
      .get(checkpoint.child_index..)
      .ok_or(BuildError::CheckpointOutOfBounds {
        index: checkpoint.child_index,
        child_count,
      })?
      .to_vec();
    self.current_children_mut().truncate(checkpoint.child_index);
    self.frames.push(Frame {
      identity: Identity::new(),
      kind,
      children: moved_children,
    });
    Ok(())
  }

  /// Finish this builder and return its sole root node.
  ///
  /// # Errors
  ///
  /// Returns [`BuildError::UnclosedNodes`] before root validation, then
  /// [`BuildError::InvalidRootArity`], then [`BuildError::RootMustBeNode`].
  pub fn finish(self) -> Result<GreenNode, BuildError> {
    let open_count = self.frames.len();
    if open_count != 0 {
      return Err(BuildError::UnclosedNodes {
        count: open_count
      });
    }
    let root_count = self.root.children.len();
    if root_count != 1 {
      return Err(BuildError::InvalidRootArity {
        count: root_count
      });
    }
    let mut roots = self.root.children.into_iter();
    match roots.next().map(|cached| cached.green) {
      Some(NodeOrToken::Node(node)) => Ok(node),
      Some(NodeOrToken::Token(_)) => Err(BuildError::RootMustBeNode),
      None => Err(BuildError::InvalidRootArity {
        count: 0
      }),
    }
  }

  /// Borrow the current frame identity and its completed children.
  fn current_identity_and_children(&self) -> (&Identity, &[CachedElement]) {
    match self.frames.last() {
      Some(frame) => (&frame.identity, &frame.children),
      None => (&self.root.identity, &self.root.children),
    }
  }

  /// Borrow the current frame's completed children mutably.
  fn current_children_mut(&mut self) -> &mut Vec<CachedElement> {
    match self.frames.last_mut() {
      Some(frame) => &mut frame.children,
      None => &mut self.root.children,
    }
  }
}

impl Default for GreenNodeBuilder<'static> {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use std::collections::HashSet;

  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;
  use strict_test_support::ensure_ok;

  use super::BuildError;
  use super::GreenNodeBuilder;
  use crate::NodeCache;
  use crate::SyntaxKind;

  /// Append one token through the public builder protocol.
  fn append(builder: &mut GreenNodeBuilder<'_>, text: &str) -> Result<(), TestFailure> {
    ensure_ok(builder.token(SyntaxKind(1), text), "the builder token must succeed")
  }

  /// Build the same one-token root against an optionally shared cache.
  fn build_with_cache(cache: &mut NodeCache) -> Result<crate::GreenNode, TestFailure> {
    let mut builder = GreenNodeBuilder::with_cache(cache);
    builder.start_node(SyntaxKind(0));
    append(&mut builder, "value")?;
    ensure_ok(builder.finish_node(), "the cached root frame must finish")?;
    ensure_ok(builder.finish(), "the cached root must complete")
  }

  #[test]
  fn builder_constructs_balanced_trees_and_reuses_checkpoints() -> Result<(), TestFailure> {
    let mut builder = GreenNodeBuilder::new();
    builder.start_node(SyntaxKind(0));
    let checkpoint = builder.checkpoint();
    append(&mut builder, "a")?;
    builder.start_node(SyntaxKind(2));
    append(&mut builder, "b")?;
    ensure_ok(builder.finish_node(), "the balanced nested node must finish")?;
    ensure_ok(
      builder.start_node_at(&checkpoint, SyntaxKind(3)),
      "a checkpoint must remain valid after balanced nested work returns to its frame",
    )?;
    ensure_ok(builder.finish_node(), "the first checkpoint wrapper must finish")?;
    ensure_ok(
      builder.start_node_at(&checkpoint, SyntaxKind(4)),
      "the same checkpoint must be reusable in its still-open owning frame",
    )?;
    ensure_ok(builder.finish_node(), "the second checkpoint wrapper must finish")?;
    ensure_ok(builder.finish_node(), "the root frame must finish")?;
    let root = ensure_ok(builder.finish(), "the balanced builder must produce one root")?;
    ensure_eq(
      &root.to_string(),
      &"ab".to_owned(),
      "checkpoint wrapping must preserve lossless text",
    )?;

    let mut checkpoints = HashSet::new();
    ensure(checkpoints.insert(checkpoint.clone()), "a checkpoint must be hashable")?;
    ensure(!checkpoints.insert(checkpoint), "cloned checkpoints must compare equal")
  }

  #[test]
  fn borrowed_cache_reuses_complete_green_allocations() -> Result<(), TestFailure> {
    let mut cache = NodeCache::default();
    let first = build_with_cache(&mut cache)?;
    let second = build_with_cache(&mut cache)?;
    ensure(
      first.ptr_eq(&second),
      "builders borrowing the same cache must reuse an equal small root allocation",
    )
  }

  #[test]
  fn builder_debug_distinguishes_owned_and_borrowed_cache_state() -> Result<(), TestFailure> {
    let owned = GreenNodeBuilder::default();
    let owned_debug = format!("{owned:?}");
    ensure(
      owned_debug.contains("cache: Owned(NodeCache)"),
      "default construction must expose its private owned-cache mode in diagnostics",
    )?;
    ensure(
      format!("{:?}", owned.checkpoint()).contains("child_index: 0"),
      "checkpoint diagnostics must expose the stable child position without exposing identities",
    )?;

    let mut cache = NodeCache::default();
    let borrowed = GreenNodeBuilder::with_cache(&mut cache);
    ensure(
      format!("{borrowed:?}").contains("cache: Borrowed(NodeCache)"),
      "borrowed construction must expose its distinct cache ownership mode in diagnostics",
    )
  }

  #[test]
  fn checkpoint_validation_is_ordered_and_rejected_calls_preserve_state() -> Result<(), TestFailure> {
    let foreign = GreenNodeBuilder::new().checkpoint();
    let mut builder = GreenNodeBuilder::new();
    builder.start_node(SyntaxKind(0));
    ensure(
      builder.start_node_at(&foreign, SyntaxKind(9)) == Err(BuildError::ForeignCheckpoint),
      "builder ownership must be validated before frame state",
    )?;

    let early = builder.checkpoint();
    append(&mut builder, "a")?;
    append(&mut builder, "b")?;
    append(&mut builder, "c")?;
    let late = builder.checkpoint();
    builder.start_node(SyntaxKind(2));
    ensure(
      builder.start_node_at(&early, SyntaxKind(9)) == Err(BuildError::StaleCheckpoint),
      "a checkpoint must be stale while a different frame is current",
    )?;
    ensure_ok(builder.finish_node(), "the unchanged nested frame must still finish")?;
    ensure_ok(
      builder.start_node_at(&early, SyntaxKind(3)),
      "the checkpoint must revive after balanced work returns to its frame",
    )?;
    ensure_ok(builder.finish_node(), "the shrinking wrapper must finish")?;
    ensure(
      builder.start_node_at(&late, SyntaxKind(9))
        == Err(BuildError::CheckpointOutOfBounds {
          index:       3,
          child_count: 1,
        }),
      "a same-frame checkpoint beyond the current child count must be rejected",
    )?;
    ensure_ok(builder.finish_node(), "a rejected checkpoint must leave the root finishable")?;
    let root = ensure_ok(builder.finish(), "the builder must remain usable after every rejection")?;
    ensure_eq(&root.to_string(), &"abc".to_owned(), "rejected calls must not alter completed text")?;
    ensure(
      GreenNodeBuilder::new().start_node_at(&late, SyntaxKind(9)) == Err(BuildError::ForeignCheckpoint),
      "a closed-frame checkpoint remains foreign to another builder",
    )
  }

  #[test]
  fn finish_reports_protocol_failures_in_locked_precedence() -> Result<(), TestFailure> {
    let mut no_open = GreenNodeBuilder::new();
    ensure(
      no_open.finish_node() == Err(BuildError::NoOpenNode),
      "finishing the permanent root frame must be rejected",
    )?;
    no_open.start_node(SyntaxKind(0));
    ensure_ok(no_open.finish_node(), "the builder must remain usable after NoOpenNode")?;
    let _root = ensure_ok(no_open.finish(), "the recovered builder must finish")?;

    let mut unclosed = GreenNodeBuilder::new();
    unclosed.start_node(SyntaxKind(0));
    ensure(
      unclosed.finish()
        == Err(BuildError::UnclosedNodes {
          count: 1
        }),
      "unclosed frames must be reported before root arity",
    )?;
    ensure(
      GreenNodeBuilder::new().finish()
        == Err(BuildError::InvalidRootArity {
          count: 0
        }),
      "an empty completed builder must report zero root elements",
    )?;

    let mut multiple = GreenNodeBuilder::new();
    for kind in [SyntaxKind(1), SyntaxKind(2)] {
      multiple.start_node(kind);
      ensure_ok(multiple.finish_node(), "each independent root node must finish")?;
    }
    ensure(
      multiple.finish()
        == Err(BuildError::InvalidRootArity {
          count: 2
        }),
      "multiple completed roots must report their exact arity",
    )?;

    let mut token_root = GreenNodeBuilder::new();
    append(&mut token_root, "token")?;
    ensure(
      token_root.finish() == Err(BuildError::RootMustBeNode),
      "root kind validation must follow successful arity validation",
    )
  }
}
