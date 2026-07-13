use std::hash::Hash;
use std::hash::Hasher;

use hashbrown::HashTable;
use hashbrown::hash_table::Entry;
use rustc_hash::FxHasher;

use super::element::GreenElement;
use crate::GreenNode;
use crate::GreenNodeData;
use crate::GreenToken;
use crate::GreenTokenData;
use crate::NodeOrToken;
use crate::SyntaxKind;
use crate::green::GreenElementRef;

/// Interner for GreenTokens and GreenNodes
// XXX: the impl is a bit tricky. As usual when writing interners, we want to
// store all values in one HashSet.
//
// However, hashing trees is fun: hash of the tree is recursively defined. We
// maintain an invariant -- if the tree is interned, then all of its children
// are interned as well.
//
// That means that computing the hash naively is wasteful -- we just *know*
// hashes of children, and we can re-use those.
//
// So here we use `HashTable`, hashbrown's low-level safe API for explicitly
// hashed values, instead of going via a `Hash` impl. Our manual `Hash` and the
// `#[derive(Hash)]` are actually different! At some point we had a fun bug,
// where we accidentally mixed the two hashes, which made the cache much less
// efficient. Storing the values in `HashTable` makes the explicit-hash
// boundary part of the representation rather than a convention on a map key.
#[derive(Default, Debug)]
pub struct NodeCache {
  nodes:  HashTable<GreenNode>,
  tokens: HashTable<GreenToken>,
}

fn token_hash(token: &GreenTokenData) -> u64 {
  let mut hasher = FxHasher::default();
  token.kind().hash(&mut hasher);
  token.text().hash(&mut hasher);
  hasher.finish()
}

fn node_hash(node: &GreenNodeData) -> u64 {
  let mut hasher = FxHasher::default();
  node.kind().hash(&mut hasher);
  for child in node.children() {
    match child {
      NodeOrToken::Node(it) => node_hash(it),
      NodeOrToken::Token(it) => token_hash(it),
    }
    .hash(&mut hasher)
  }
  hasher.finish()
}

fn same_element_identity(left: GreenElementRef<'_>, right: GreenElementRef<'_>) -> bool {
  match (left, right) {
    (NodeOrToken::Node(left), NodeOrToken::Node(right)) => std::ptr::eq(left, right),
    (NodeOrToken::Token(left), NodeOrToken::Token(right)) => std::ptr::eq(left, right),
    (NodeOrToken::Node(_), NodeOrToken::Token(_)) | (NodeOrToken::Token(_), NodeOrToken::Node(_)) => false,
  }
}

impl NodeCache {
  pub(crate) fn node(&mut self, kind: SyntaxKind, children: &mut Vec<(u64, GreenElement)>, first_child: usize) -> (u64, GreenNode) {
    let build_node = move |children: &mut Vec<(u64, GreenElement)>| GreenNode::new(kind, children.drain(first_child..).map(|(_, it)| it));

    let children_ref = &children[first_child..];
    if children_ref.len() > 3 {
      let node = build_node(children);
      return (0, node);
    }

    let hash = {
      let mut hasher = FxHasher::default();
      kind.hash(&mut hasher);
      for &(child_hash, _) in children_ref {
        if child_hash == 0 {
          let node = build_node(children);
          return (0, node);
        }
        child_hash.hash(&mut hasher);
      }
      hasher.finish()
    };

    // Green nodes are fully immutable, so it's ok to deduplicate them.
    // This is the same optimization that Roslyn does
    // https://github.com/KirillOsenkov/Bliki/wiki/Roslyn-Immutable-Trees
    //
    // For example, all `#[inline]` in this file share the same green node!
    // For `libsyntax/parse/parser.rs`, measurements show that deduping saves
    // 17% of the memory for green nodes!
    let entry = self.nodes.entry(
      hash,
      |node| {
        node.kind() == kind && node.children().len() == children_ref.len() && {
          let lhs = node.children();
          let rhs = children_ref.iter().map(|(_, it)| it.as_deref());
          lhs.zip(rhs).all(|(left, right)| same_element_identity(left, right))
        }
      },
      |node| node_hash(node),
    );

    let node = match entry {
      Entry::Occupied(entry) => {
        drop(children.drain(first_child..));
        entry.get().clone()
      }
      Entry::Vacant(entry) => {
        let node = build_node(children);
        entry.insert(node).get().clone()
      }
    };

    (hash, node)
  }

  pub(crate) fn token(&mut self, kind: SyntaxKind, text: &str) -> (u64, GreenToken) {
    let hash = {
      let mut hasher = FxHasher::default();
      kind.hash(&mut hasher);
      text.hash(&mut hasher);
      hasher.finish()
    };

    let entry = self.tokens.entry(
      hash,
      |token| token.kind() == kind && token.text() == text,
      |token| token_hash(token),
    );

    let token = match entry {
      Entry::Occupied(entry) => entry.get().clone(),
      Entry::Vacant(entry) => {
        let token = GreenToken::new(kind, text);
        entry.insert(token).get().clone()
      }
    };

    (hash, token)
  }
}

#[cfg(test)]
mod tests {
  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;

  use super::NodeCache;
  use crate::GreenNodeData;
  use crate::GreenTokenData;
  use crate::SyntaxKind;

  #[test]
  fn token_cache_reuses_only_exact_matches() -> Result<(), TestFailure> {
    let mut cache = NodeCache::default();
    let (first_hash, first) = cache.token(SyntaxKind(1), "same");
    let (matching_hash, matching) = cache.token(SyntaxKind(1), "same");
    let (_, different_kind) = cache.token(SyntaxKind(2), "same");
    let (_, different_text) = cache.token(SyntaxKind(1), "different");

    ensure_eq(&first_hash, &matching_hash, "matching tokens must keep the same explicit hash")?;
    ensure(
      std::ptr::eq::<GreenTokenData>(&*first, &*matching),
      "matching tokens must reuse one allocation",
    )?;
    ensure(
      !std::ptr::eq::<GreenTokenData>(&*first, &*different_kind),
      "different token kinds must not reuse an allocation",
    )?;
    ensure(
      !std::ptr::eq::<GreenTokenData>(&*first, &*different_text),
      "different token text must not reuse an allocation",
    )
  }

  #[test]
  fn small_node_cache_reuses_only_exact_child_identities() -> Result<(), TestFailure> {
    let mut cache = NodeCache::default();
    let (first_token_hash, first_token) = cache.token(SyntaxKind(1), "a");
    let (second_token_hash, second_token) = cache.token(SyntaxKind(1), "b");

    let mut first_children = vec![(first_token_hash, first_token.clone().into())];
    let (first_hash, first) = cache.node(SyntaxKind(3), &mut first_children, 0);
    let mut matching_children = vec![(first_token_hash, first_token.into())];
    let (matching_hash, matching) = cache.node(SyntaxKind(3), &mut matching_children, 0);
    let (_, matching_token) = cache.token(SyntaxKind(1), "a");
    let mut different_kind_children = vec![(first_token_hash, matching_token.into())];
    let (_, different_kind) = cache.node(SyntaxKind(4), &mut different_kind_children, 0);
    let mut different_child_children = vec![(second_token_hash, second_token.into())];
    let (_, different_child) = cache.node(SyntaxKind(3), &mut different_child_children, 0);

    ensure_eq(&first_hash, &matching_hash, "matching nodes must keep the same explicit hash")?;
    ensure(
      std::ptr::eq::<GreenNodeData>(&*first, &*matching),
      "matching small nodes must reuse one allocation",
    )?;
    ensure(
      !std::ptr::eq::<GreenNodeData>(&*first, &*different_kind),
      "different node kinds must not reuse an allocation",
    )?;
    ensure(
      !std::ptr::eq::<GreenNodeData>(&*first, &*different_child),
      "different child identities must not reuse an allocation",
    )
  }

  #[test]
  fn cache_threshold_interns_nodes_with_three_children() -> Result<(), TestFailure> {
    let mut cache = NodeCache::default();
    let mut first_children = Vec::new();
    for text in ["a", "b", "c"] {
      let (hash, token) = cache.token(SyntaxKind(1), text);
      first_children.push((hash, token.into()));
    }
    let (_, first) = cache.node(SyntaxKind(3), &mut first_children, 0);

    let mut matching_children = Vec::new();
    for text in ["a", "b", "c"] {
      let (hash, token) = cache.token(SyntaxKind(1), text);
      matching_children.push((hash, token.into()));
    }
    let (_, matching) = cache.node(SyntaxKind(3), &mut matching_children, 0);

    ensure(
      std::ptr::eq::<GreenNodeData>(&*first, &*matching),
      "nodes at the three-child cache threshold must reuse one allocation",
    )
  }

  #[test]
  fn large_or_uninterned_subtrees_bypass_node_cache() -> Result<(), TestFailure> {
    let mut cache = NodeCache::default();
    let mut large_children = Vec::new();
    for text in ["a", "b", "c", "d"] {
      let (hash, token) = cache.token(SyntaxKind(1), text);
      large_children.push((hash, token.into()));
    }
    let (large_hash, large_node) = cache.node(SyntaxKind(3), &mut large_children, 0);

    let mut matching_large_children = Vec::new();
    for text in ["a", "b", "c", "d"] {
      let (hash, token) = cache.token(SyntaxKind(1), text);
      matching_large_children.push((hash, token.into()));
    }
    let (matching_large_hash, matching_large_node) = cache.node(SyntaxKind(3), &mut matching_large_children, 0);

    let mut first_parent_children = vec![(large_hash, large_node.clone().into())];
    let (first_parent_hash, first_parent) = cache.node(SyntaxKind(4), &mut first_parent_children, 0);
    let mut second_parent_children = vec![(large_hash, large_node.clone().into())];
    let (second_parent_hash, second_parent) = cache.node(SyntaxKind(4), &mut second_parent_children, 0);

    ensure_eq(
      &large_hash,
      &0,
      "nodes with more than three children must use the zero-hash sentinel",
    )?;
    ensure_eq(
      &matching_large_hash,
      &0,
      "matching nodes over the cache threshold must also use the zero-hash sentinel",
    )?;
    ensure(
      !std::ptr::eq::<GreenNodeData>(&*large_node, &*matching_large_node),
      "nodes over the cache threshold must not reuse an allocation",
    )?;
    ensure_eq(&first_parent_hash, &0, "parents containing an uninterned child must use hash zero")?;
    ensure_eq(
      &second_parent_hash,
      &0,
      "every parent containing an uninterned child must use hash zero",
    )?;
    ensure(
      !std::ptr::eq::<GreenNodeData>(&*first_parent, &*second_parent),
      "parents containing an uninterned child must bypass the node cache",
    )
  }
}
