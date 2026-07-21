//! Explicit-hash interning for small immutable green subtrees.

use std::hash::Hash;
use std::hash::Hasher;

use hashbrown::HashTable;
use hashbrown::hash_table::Entry;
use rustc_hash::FxHasher;

use crate::GreenNode;
use crate::GreenToken;
use crate::SyntaxKind;
use crate::green::GreenElement;
use crate::green::GreenError;

/// A green element paired with its explicit cacheability state.
#[derive(Debug, Clone)]
pub(super) struct CachedElement {
  /// `Some` stores the reusable subtree hash; `None` marks an intentionally uncached subtree.
  pub(super) hash:  Option<u64>,
  /// Immutable green element.
  pub(super) green: GreenElement,
}

/// A cached green node and the hash used by `HashTable`.
#[derive(Debug)]
struct CachedNode {
  /// Precomputed explicit subtree hash.
  hash:  u64,
  /// Interned green node.
  green: GreenNode,
}

/// A cached green token and the hash used by `HashTable`.
#[derive(Debug)]
struct CachedToken {
  /// Precomputed token hash.
  hash:  u64,
  /// Interned green token.
  green: GreenToken,
}

/// An interner for green tokens and nodes with at most three cacheable children.
#[derive(Debug, Default)]
pub struct NodeCache {
  /// Interned small nodes.
  nodes:  HashTable<CachedNode>,
  /// Interned tokens.
  tokens: HashTable<CachedToken>,
}

/// Hash a token kind and exact UTF-8 text.
fn token_hash(kind: SyntaxKind, text: &str) -> u64 {
  let mut hasher = FxHasher::default();
  kind.hash(&mut hasher);
  text.hash(&mut hasher);
  hasher.finish()
}

/// Fold a node kind and child cache states into one explicit hash.
///
/// `Some(0)` remains cacheable; only `None` propagates uncacheability.
pub(super) fn fold_node_hash(kind: SyntaxKind, child_hashes: impl IntoIterator<Item = Option<u64>>) -> Option<u64> {
  let mut hasher = FxHasher::default();
  kind.hash(&mut hasher);
  for child_hash in child_hashes {
    child_hash?.hash(&mut hasher);
  }
  Some(hasher.finish())
}

/// Compare a cached node boundary using child allocation identities.
fn node_matches(cached: &CachedNode, kind: SyntaxKind, children: &[CachedElement]) -> bool {
  cached.green.kind() == kind
    && cached.green.children().len() == children.len()
    && cached
      .green
      .children()
      .zip(children)
      .all(|(left, right)| left.to_owned().ptr_eq(&right.green))
}

/// Resolve an interner entry without duplicating occupied/vacant ownership mechanics.
fn resolve_entry<Cached, Green>(
  entry: Entry<'_, Cached>,
  create: impl FnOnce() -> Result<Cached, GreenError>,
  select: impl Fn(&Cached) -> &Green,
) -> Result<Green, GreenError>
where
  Green: Clone,
{
  match entry {
    Entry::Occupied(cached) => Ok(select(cached.get()).clone()),
    Entry::Vacant(vacant) => Ok(select(vacant.insert(create()?).get()).clone()),
  }
}

/// Pair one successfully built green handle with a reusable explicit hash.
fn cacheable_element(hash: u64, green: impl Into<GreenElement>) -> CachedElement {
  CachedElement {
    hash:  Some(hash),
    green: green.into(),
  }
}

/// Pair one deliberately uncached green handle with explicit uncacheability.
fn uncacheable_element(green: impl Into<GreenElement>) -> CachedElement {
  CachedElement {
    hash:  None,
    green: green.into(),
  }
}

impl NodeCache {
  /// Intern a token and return its explicit cache state.
  pub(super) fn token(&mut self, kind: SyntaxKind, text: &str) -> Result<CachedElement, GreenError> {
    let hash = token_hash(kind, text);
    let entry = self.tokens.entry(
      hash,
      |cached| cached.green.kind() == kind && cached.green.text() == text,
      |cached| cached.hash,
    );
    let green = resolve_entry(
      entry,
      || {
        GreenToken::new(kind, text).map(|green| CachedToken {
          hash,
          green,
        })
      },
      |cached| &cached.green,
    )?;
    Ok(cacheable_element(hash, green))
  }

  /// Intern a small cacheable node or build an uncached node when policy requires it.
  pub(super) fn node(&mut self, kind: SyntaxKind, children: &[CachedElement]) -> Result<CachedElement, GreenError> {
    let green_children = || children.iter().map(|child| child.green.clone());
    if children.len() > 3 {
      return GreenNode::new(kind, green_children()).map(uncacheable_element);
    }

    let hash = match fold_node_hash(kind, children.iter().map(|child| child.hash)) {
      Some(hash) => hash,
      None => {
        return GreenNode::new(kind, green_children()).map(uncacheable_element);
      }
    };

    let entry = self
      .nodes
      .entry(hash, |cached| node_matches(cached, kind, children), |cached| cached.hash);
    let green = resolve_entry(
      entry,
      || {
        GreenNode::new(kind, green_children()).map(|green| CachedNode {
          hash,
          green,
        })
      },
      |cached| &cached.green,
    )?;
    Ok(cacheable_element(hash, green))
  }
}

#[cfg(test)]
mod tests {
  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_ok;
  use strict_test_support::ensure_some;

  use super::CachedElement;
  use super::CachedNode;
  use super::NodeCache;
  use super::fold_node_hash;
  use super::node_matches;
  use crate::GreenNode;
  use crate::GreenToken;
  use crate::SyntaxKind;

  /// Intern one token through the production cache path.
  fn cached_token(cache: &mut NodeCache, kind: u16, text: &str) -> Result<CachedElement, TestFailure> {
    ensure_ok(cache.token(SyntaxKind(kind), text), "the cached token must allocate")
  }

  /// Extract a cached element's node handle.
  fn cached_node(element: CachedElement) -> Result<GreenNode, TestFailure> {
    ensure_some(element.green.into_node(), "the cache result must be a node")
  }

  /// Extract a cached element's token handle.
  fn cached_token_handle(element: CachedElement) -> Result<GreenToken, TestFailure> {
    ensure_some(element.green.into_token(), "the cache result must be a token")
  }

  #[test]
  fn token_cache_reuses_only_exact_kind_and_text() -> Result<(), TestFailure> {
    let mut cache = NodeCache::default();
    let first = cached_token(&mut cache, 1, "same")?;
    let matching = cached_token(&mut cache, 1, "same")?;
    let different_kind = cached_token(&mut cache, 2, "same")?;
    let different_text = cached_token(&mut cache, 1, "different")?;

    ensure(first.hash == matching.hash, "matching tokens must retain the same explicit hash")?;
    let first = cached_token_handle(first)?;
    ensure(
      first.ptr_eq(&cached_token_handle(matching)?),
      "matching tokens must reuse one allocation",
    )?;
    ensure(
      !first.ptr_eq(&cached_token_handle(different_kind)?),
      "a different token kind must not reuse the allocation",
    )?;
    ensure(
      !first.ptr_eq(&cached_token_handle(different_text)?),
      "different token text must not reuse the allocation",
    )
  }

  /// Build and then repeat one node shape with a shared cache.
  fn repeated_node(cache: &mut NodeCache, kind: u16, children: &[CachedElement]) -> Result<(CachedElement, CachedElement), TestFailure> {
    let first = ensure_ok(cache.node(SyntaxKind(kind), children), "the first cached node must build")?;
    let second = ensure_ok(cache.node(SyntaxKind(kind), children), "the repeated cached node must build")?;
    Ok((first, second))
  }

  #[test]
  fn cache_interns_zero_through_three_children_and_propagates_uncacheability() -> Result<(), TestFailure> {
    let mut cache = NodeCache::default();
    let a = cached_token(&mut cache, 1, "a")?;
    let b = cached_token(&mut cache, 1, "b")?;
    let c = cached_token(&mut cache, 1, "c")?;
    let d = cached_token(&mut cache, 1, "d")?;

    for children in [Vec::new(), vec![a.clone()], vec![a.clone(), b.clone()], vec![
      a.clone(),
      b.clone(),
      c.clone(),
    ]] {
      let (first, second) = repeated_node(&mut cache, 3, &children)?;
      ensure(first.hash.is_some(), "a node at the cache threshold must be cacheable")?;
      ensure(
        cached_node(first)?.ptr_eq(&cached_node(second)?),
        "equal nodes with at most three cacheable children must intern",
      )?;
    }

    let four = vec![a.clone(), b, c, d];
    let (large_first, large_second) = repeated_node(&mut cache, 4, &four)?;
    ensure(large_first.hash.is_none(), "a four-child node must be deliberately uncacheable")?;
    let large_first_node = cached_node(large_first.clone())?;
    ensure(
      !large_first_node.ptr_eq(&cached_node(large_second)?),
      "matching four-child nodes must bypass interning",
    )?;

    let uncacheable_child = CachedElement {
      hash:  None,
      green: large_first_node.into(),
    };
    let (parent_first, parent_second) = repeated_node(&mut cache, 5, &[uncacheable_child])?;
    ensure(parent_first.hash.is_none(), "an uncacheable child must make its parent uncacheable")?;
    ensure(
      !cached_node(parent_first)?.ptr_eq(&cached_node(parent_second)?),
      "parents of uncacheable children must bypass interning",
    )
  }

  #[test]
  fn explicit_hash_state_distinguishes_zero_from_uncacheable() -> Result<(), TestFailure> {
    ensure(
      fold_node_hash(SyntaxKind(7), [Some(0)]).is_some(),
      "a legitimate zero child hash must remain cacheable",
    )?;
    ensure(
      fold_node_hash(SyntaxKind(7), [None]).is_none(),
      "only the explicit None state may propagate uncacheability",
    )
  }

  #[test]
  fn node_boundary_matching_requires_kind_arity_and_child_identity() -> Result<(), TestFailure> {
    let mut cache = NodeCache::default();
    let child = cached_token(&mut cache, 1, "child")?;
    let cached_green = cached_node(ensure_ok(
      cache.node(SyntaxKind(2), std::slice::from_ref(&child)),
      "the boundary node must allocate",
    )?)?;
    let cached = CachedNode {
      hash:  0,
      green: cached_green,
    };
    ensure(
      node_matches(&cached, SyntaxKind(2), std::slice::from_ref(&child)),
      "matching kind, arity, and interned child identity must match the node boundary",
    )?;
    ensure(
      !node_matches(&cached, SyntaxKind(3), std::slice::from_ref(&child)),
      "a different node kind must reject the cache candidate",
    )?;
    ensure(
      !node_matches(&cached, SyntaxKind(2), &[]),
      "a different child count must reject the cache candidate",
    )?;

    let separate_token = ensure_ok(
      GreenToken::new(SyntaxKind(1), "child"),
      "the structurally equal separate token must allocate",
    )?;
    let separate_child = CachedElement {
      hash:  child.hash,
      green: separate_token.into(),
    };
    ensure(
      !node_matches(&cached, SyntaxKind(2), &[separate_child]),
      "structural token equality without interned child identity must not match a cached parent",
    )
  }

  #[test]
  fn table_growth_retains_cached_allocations() -> Result<(), TestFailure> {
    let mut cache = NodeCache::default();
    let original = cached_token_handle(cached_token(&mut cache, 1, "stable")?)?;
    for index in 0..2_048 {
      let text = format!("token-{index}");
      let _entry = cached_token(&mut cache, 1, &text)?;
    }
    let repeated = cached_token_handle(cached_token(&mut cache, 1, "stable")?)?;
    ensure(
      original.ptr_eq(&repeated),
      "constant-time stored hashes must preserve entries across table growth",
    )
  }
}
