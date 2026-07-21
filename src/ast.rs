//! Typed AST wrappers and source-location pointers.

use std::fmt;
use std::hash::Hash;
use std::hash::Hasher;
use std::marker::PhantomData;

use thiserror::Error;

use crate::GreenNode;
use crate::Language;
use crate::NodeOrToken;
use crate::SyntaxKind;
use crate::SyntaxNode;
use crate::SyntaxNodeChildren;
use crate::TextRange;

/// Failure to resolve a source pointer against a purported tree root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ResolveError {
  /// The supplied cursor has a parent and therefore is not a tree root.
  #[error("pointer resolution requires a root node")]
  RootHasParent,
}

/// Failure in a typed AST operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AstError {
  /// An AST implementation rejected the syntax kind produced by cloning its own syntax node.
  #[error("AST cast rejected cloned syntax kind {kind:?}")]
  CloneCastRejected {
    /// Raw syntax kind rejected by the faulty cast implementation.
    kind: SyntaxKind,
  },
}

/// Typed AST wrapper contract over a language-specific syntax node.
pub trait AstNode {
  /// Language used by this AST node.
  type Language: Language;

  /// Test whether this AST wrapper accepts a typed syntax kind.
  fn can_cast(kind: <Self::Language as Language>::Kind) -> bool
  where
    Self: Sized;

  /// Convert a syntax node into this AST wrapper when its kind is accepted.
  fn cast(node: SyntaxNode<Self::Language>) -> Option<Self>
  where
    Self: Sized;

  /// Borrow this wrapper's underlying syntax node.
  fn syntax(&self) -> &SyntaxNode<Self::Language>;

  /// Clone this AST subtree into an independent root cursor.
  ///
  /// # Errors
  ///
  /// Returns [`AstError::CloneCastRejected`] when `Self::cast` rejects the kind produced from its
  /// own source syntax.
  fn clone_subtree(&self) -> Result<Self, AstError>
  where
    Self: Sized,
  {
    let syntax = self.syntax().clone_subtree();
    let kind = <Self::Language as Language>::kind_to_raw(syntax.kind());
    Self::cast(syntax).ok_or(AstError::CloneCastRejected {
      kind,
    })
  }
}

/// Stable source-kind and source-range pointer to a syntax node.
#[derive(Clone)]
pub struct SyntaxNodePtr<L: Language> {
  /// Typed syntax kind at pointer creation.
  kind:   L::Kind,
  /// Absolute source range at pointer creation.
  range:  TextRange,
  /// Structurally comparable source subtree used to reject non-equivalent trees.
  source: GreenNode,
}

impl<L: Language> SyntaxNodePtr<L> {
  /// Create a pointer from an immutable syntax node.
  pub fn new(node: &SyntaxNode<L>) -> Self {
    Self {
      kind:   node.kind(),
      range:  node.text_range(),
      source: node.green().clone(),
    }
  }

  /// Resolve this pointer against an equivalent tree root.
  ///
  /// A valid but non-equivalent tree, wrong kind, or absent range returns `Ok(None)`.
  ///
  /// # Errors
  ///
  /// Returns [`ResolveError::RootHasParent`] only when `root` is not a root cursor.
  pub fn resolve(&self, root: &SyntaxNode<L>) -> Result<Option<SyntaxNode<L>>, ResolveError> {
    if root.parent().is_some() {
      return Err(ResolveError::RootHasParent);
    }
    if !root.text_range().contains_range(self.range) {
      return Ok(None);
    }
    let mut current = root.clone();
    loop {
      if current.text_range() == self.range && current.kind() == self.kind {
        return Ok(self.source.structurally_eq(current.green()).then_some(current));
      }
      current = match current.child_or_token_at_range(self.range) {
        Ok(Some(NodeOrToken::Node(child))) => child,
        Ok(Some(NodeOrToken::Token(_))) | Ok(None) | Err(_) => return Ok(None),
      };
    }
  }

  /// Cast this pointer to a typed AST pointer when the target wrapper accepts its kind.
  pub fn cast<NodeType: AstNode<Language = L>>(self) -> Option<AstPtr<NodeType>> {
    NodeType::can_cast(self.kind).then_some(AstPtr {
      raw: self
    })
  }

  /// Return the typed syntax kind stored by this pointer.
  pub fn kind(&self) -> L::Kind {
    self.kind
  }

  /// Return the absolute source range stored by this pointer.
  pub fn text_range(&self) -> TextRange {
    self.range
  }
}

impl<L: Language> fmt::Debug for SyntaxNodePtr<L> {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter
      .debug_struct("SyntaxNodePtr")
      .field("kind", &self.kind)
      .field("range", &self.range)
      .finish()
  }
}

impl<L: Language> PartialEq for SyntaxNodePtr<L> {
  fn eq(&self, other: &Self) -> bool {
    self.kind == other.kind && self.range == other.range && self.source.structurally_eq(&other.source)
  }
}

impl<L: Language> Eq for SyntaxNodePtr<L> {}

impl<L: Language> Hash for SyntaxNodePtr<L> {
  fn hash<HasherType: Hasher>(&self, state: &mut HasherType) {
    self.kind.hash(state);
    self.range.hash(state);
    self.source.hash(state);
  }
}

/// Typed AST pointer retaining its wrapper type.
pub struct AstPtr<NodeType: AstNode> {
  /// Underlying syntax-node pointer.
  raw: SyntaxNodePtr<NodeType::Language>,
}

impl<NodeType: AstNode> AstPtr<NodeType> {
  /// Create a typed pointer from an AST node.
  pub fn new(node: &NodeType) -> Self {
    Self {
      raw: SyntaxNodePtr::new(node.syntax()),
    }
  }

  /// Resolve this pointer against an equivalent tree root.
  ///
  /// # Errors
  ///
  /// Returns [`ResolveError::RootHasParent`] only when `root` is not a root cursor.
  pub fn resolve(&self, root: &SyntaxNode<NodeType::Language>) -> Result<Option<NodeType>, ResolveError> {
    self.raw.resolve(root).map(|node| node.and_then(NodeType::cast))
  }

  /// Return the underlying syntax-node pointer.
  pub fn syntax_node_ptr(&self) -> SyntaxNodePtr<NodeType::Language> {
    self.raw.clone()
  }

  /// Cast this pointer to another AST wrapper accepting the same stored kind.
  pub fn cast<Other: AstNode<Language = NodeType::Language>>(self) -> Option<AstPtr<Other>> {
    Other::can_cast(self.raw.kind).then_some(AstPtr {
      raw: self.raw
    })
  }
}

impl<NodeType: AstNode> fmt::Debug for AstPtr<NodeType> {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.debug_struct("AstPtr").field("raw", &self.raw).finish()
  }
}

impl<NodeType: AstNode> Clone for AstPtr<NodeType> {
  fn clone(&self) -> Self {
    Self {
      raw: self.raw.clone()
    }
  }
}

impl<NodeType: AstNode> PartialEq for AstPtr<NodeType> {
  fn eq(&self, other: &Self) -> bool {
    self.raw == other.raw
  }
}

impl<NodeType: AstNode> Eq for AstPtr<NodeType> {}

impl<NodeType: AstNode> Hash for AstPtr<NodeType> {
  fn hash<HasherType: Hasher>(&self, state: &mut HasherType) {
    self.raw.hash(state);
  }
}

impl<NodeType: AstNode> From<AstPtr<NodeType>> for SyntaxNodePtr<NodeType::Language> {
  fn from(pointer: AstPtr<NodeType>) -> Self {
    pointer.raw
  }
}

/// Iterator over direct children accepted by one AST wrapper type.
#[derive(Debug, Clone)]
pub struct AstChildren<NodeType: AstNode> {
  /// Underlying typed syntax-node iterator.
  inner:     SyntaxNodeChildren<NodeType::Language>,
  /// AST wrapper marker.
  node_type: PhantomData<NodeType>,
}

impl<NodeType: AstNode> AstChildren<NodeType> {
  /// Create a typed AST child iterator.
  fn new(parent: &SyntaxNode<NodeType::Language>) -> Self {
    Self {
      inner:     parent.children(),
      node_type: PhantomData,
    }
  }
}

impl<NodeType: AstNode> Iterator for AstChildren<NodeType> {
  type Item = NodeType;

  fn next(&mut self) -> Option<Self::Item> {
    self.inner.find_map(NodeType::cast)
  }
}

/// Common typed AST child and token lookup helpers.
pub mod support {
  use super::AstChildren;
  use super::AstNode;
  use crate::Language;
  use crate::SyntaxNode;
  use crate::SyntaxToken;

  /// Return the first direct child accepted by `NodeType`.
  pub fn child<NodeType: AstNode>(parent: &SyntaxNode<NodeType::Language>) -> Option<NodeType> {
    parent.children().find_map(NodeType::cast)
  }

  /// Iterate over direct children accepted by `NodeType`.
  pub fn children<NodeType: AstNode>(parent: &SyntaxNode<NodeType::Language>) -> AstChildren<NodeType> {
    AstChildren::new(parent)
  }

  /// Return the first direct token with `kind`.
  pub fn token<LanguageType: Language>(parent: &SyntaxNode<LanguageType>, kind: LanguageType::Kind) -> Option<SyntaxToken<LanguageType>> {
    parent
      .children_with_tokens()
      .filter_map(|element| element.into_token())
      .find(|token| token.kind() == kind)
  }
}

#[cfg(test)]
mod tests {
  use std::collections::HashSet;

  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;
  use strict_test_support::ensure_ok;
  use strict_test_support::ensure_some;

  use super::AstError;
  use super::AstNode;
  use super::AstPtr;
  use super::ResolveError;
  use super::SyntaxNodePtr;
  use super::support;
  use crate::GreenElement;
  use crate::GreenNode;
  use crate::GreenToken;
  use crate::SyntaxKind;
  use crate::SyntaxNode;
  use crate::test_support::TestLanguage;
  use crate::test_support::ensure_same_hash;
  use crate::test_support::typed_token_root;

  /// Consistent typed wrapper accepting one const-selected syntax kind.
  #[derive(Clone, PartialEq, Eq)]
  struct TestNode<const KIND: u16>(SyntaxNode<TestLanguage>);

  impl<const KIND: u16> AstNode for TestNode<KIND> {
    type Language = TestLanguage;

    fn can_cast(kind: SyntaxKind) -> bool {
      kind == SyntaxKind(KIND)
    }

    fn cast(node: SyntaxNode<Self::Language>) -> Option<Self> {
      Self::can_cast(node.kind()).then_some(Self(node))
    }

    fn syntax(&self) -> &SyntaxNode<Self::Language> {
      &self.0
    }
  }

  /// Typed wrapper accepting kind one.
  type GoodNode = TestNode<1>;

  /// Wrapper accepting a different kind for pointer-cast rejection coverage.
  type OtherNode = TestNode<9>;

  /// Deliberately inconsistent wrapper used to validate typed clone failure.
  struct FaultyNode(SyntaxNode<TestLanguage>);

  impl AstNode for FaultyNode {
    type Language = TestLanguage;

    fn can_cast(kind: SyntaxKind) -> bool {
      kind == SyntaxKind(1)
    }

    fn cast(_node: SyntaxNode<Self::Language>) -> Option<Self> {
      None
    }

    fn syntax(&self) -> &SyntaxNode<Self::Language> {
      &self.0
    }
  }

  /// Build a typed root with one node child containing one token.
  fn tree(child_kind: u16, text: &str) -> Result<SyntaxNode<TestLanguage>, TestFailure> {
    let token = ensure_ok(GreenToken::new(SyntaxKind(2), text), "the AST token fixture must allocate")?;
    let child = ensure_ok(
      GreenNode::new(SyntaxKind(child_kind), [GreenElement::from(token)]),
      "the AST child fixture must allocate",
    )?;
    let root = ensure_ok(
      GreenNode::new(SyntaxKind(0), [GreenElement::from(child)]),
      "the AST root fixture must allocate",
    )?;
    Ok(SyntaxNode::new_root(root))
  }

  #[test]
  fn syntax_pointer_resolves_only_roots_with_equivalent_subtrees() -> Result<(), TestFailure> {
    let root = tree(1, "same")?;
    let child = ensure_some(root.first_child(), "the source child must exist")?;
    let pointer = SyntaxNodePtr::new(&child);
    let resolved = ensure_some(
      ensure_ok(pointer.resolve(&root), "the pointer must accept the correct root")?,
      "the source pointer must resolve",
    )?;
    ensure_eq(&resolved, &child, "resolution must return the original location")?;

    let equivalent = tree(1, "same")?;
    ensure(
      ensure_ok(pointer.resolve(&equivalent), "an equivalent root must be valid")?.is_some(),
      "a separately allocated equivalent tree must resolve",
    )?;
    let non_equivalent = tree(1, "else")?;
    ensure(
      ensure_ok(pointer.resolve(&non_equivalent), "a non-equivalent root remains a valid argument")?.is_none(),
      "same-kind and same-range but different content must not resolve",
    )?;
    let wrong_kind = tree(3, "same")?;
    ensure(
      ensure_ok(pointer.resolve(&wrong_kind), "a wrong-kind tree remains a valid argument")?.is_none(),
      "a wrong-kind location must not resolve",
    )?;
    let absent_range = tree(1, "")?;
    ensure(
      ensure_ok(pointer.resolve(&absent_range), "a shorter tree remains a valid argument")?.is_none(),
      "an absent source range must not resolve",
    )?;
    ensure(
      pointer.resolve(&child) == Err(ResolveError::RootHasParent),
      "a non-root argument must be rejected distinctly",
    )
  }

  #[test]
  fn ast_pointer_and_clone_preserve_typed_success_and_failure() -> Result<(), TestFailure> {
    let root = tree(1, "value")?;
    let child = ensure_some(root.first_child(), "the typed child must exist")?;
    let good = ensure_some(GoodNode::cast(child.clone()), "the good wrapper must accept kind one")?;
    let pointer = AstPtr::new(&good);
    let resolved = ensure_some(
      ensure_ok(pointer.resolve(&root), "the typed pointer must accept the root")?,
      "the typed pointer must resolve",
    )?;
    ensure_eq(resolved.syntax(), good.syntax(), "typed resolution must retain the syntax location")?;
    let cloned = ensure_ok(good.clone_subtree(), "a consistent AST wrapper must clone")?;
    ensure(cloned.syntax().parent().is_none(), "an AST clone must be an independent root")?;
    ensure_eq(
      &cloned.syntax().to_string(),
      &"value".to_owned(),
      "an AST clone must preserve subtree text",
    )?;

    let faulty = FaultyNode(child);
    ensure(
      faulty.clone_subtree().map(|_| ())
        == Err(AstError::CloneCastRejected {
          kind: SyntaxKind(1)
        }),
      "a faulty cast implementation must return AstError instead of panicking",
    )?;
    let raw_pointer = pointer.syntax_node_ptr();
    ensure(
      raw_pointer.kind() == SyntaxKind(1),
      "typed pointers must expose their raw pointer kind",
    )?;
    ensure(
      raw_pointer.clone().cast::<GoodNode>().is_some(),
      "pointer casts must accept a compatible wrapper",
    )?;
    ensure(
      raw_pointer.cast::<FaultyNode>().is_some(),
      "pointer casting is governed by can_cast, independently of a faulty cast body",
    )
  }

  #[test]
  fn ast_support_helpers_filter_nodes_and_tokens_by_contract() -> Result<(), TestFailure> {
    let root = tree(1, "value")?;
    ensure(
      support::child::<GoodNode>(&root).is_some(),
      "the child helper must return the first accepted node",
    )?;
    ensure_eq(
      &support::children::<GoodNode>(&root).count(),
      &1,
      "the children helper must iterate accepted direct nodes",
    )?;
    let child = ensure_some(root.first_child(), "the token parent must exist")?;
    ensure(
      support::token(&child, SyntaxKind(2)).is_some(),
      "the token helper must find a matching direct token",
    )?;
    ensure(
      support::token(&child, SyntaxKind(9)).is_none(),
      "the token helper must return None for an absent kind",
    )
  }

  #[test]
  fn pointer_value_semantics_preserve_kind_range_structure_and_wrapper_type() -> Result<(), TestFailure> {
    let root = tree(1, "value")?;
    let child = ensure_some(root.first_child(), "the pointer source child must exist")?;
    let pointer = SyntaxNodePtr::new(&child);
    ensure(pointer.kind() == SyntaxKind(1), "the syntax pointer must retain its typed kind")?;
    ensure(
      pointer.text_range() == child.text_range(),
      "the syntax pointer must retain its exact source range",
    )?;
    ensure_eq(
      &format!("{pointer:?}"),
      &"SyntaxNodePtr { kind: SyntaxKind(1), range: 0..5 }".to_owned(),
      "syntax-pointer diagnostics must expose kind and range without exposing storage",
    )?;
    let pointer_clone = pointer.clone();
    ensure(
      pointer == pointer_clone,
      "cloning a syntax pointer must preserve complete value identity",
    )?;
    ensure_same_hash(
      &pointer,
      &pointer_clone,
      std::hash::Hash::hash,
      "equal syntax pointers must produce equal structural hashes",
    )?;
    ensure(
      pointer.clone().cast::<OtherNode>().is_none(),
      "a syntax pointer must reject an AST wrapper that does not accept its stored kind",
    )?;

    let good = ensure_some(GoodNode::cast(child), "the pointer source must cast to GoodNode")?;
    let typed = AstPtr::new(&good);
    let typed_clone = typed.clone();
    ensure(typed == typed_clone, "cloning an AST pointer must preserve wrapper-typed identity")?;
    ensure(
      format!("{typed:?}").contains("SyntaxNodePtr"),
      "AST-pointer diagnostics must expose the underlying syntax pointer",
    )?;
    let mut pointers = HashSet::new();
    ensure(pointers.insert(typed.clone()), "the first AST pointer must enter a hash set")?;
    ensure(
      !pointers.insert(typed_clone),
      "an equal cloned AST pointer must not create a second hash-set entry",
    )?;
    ensure(
      typed.clone().cast::<OtherNode>().is_none(),
      "an AST pointer must reject an incompatible wrapper cast",
    )?;
    let raw_from_typed: SyntaxNodePtr<TestLanguage> = typed.into();
    ensure(
      raw_from_typed == pointer,
      "converting an AST pointer back to a syntax pointer must preserve its complete value",
    )?;

    let alternate_root = typed_token_root(SyntaxKind(0), SyntaxKind(2), "value")?;
    ensure(
      ensure_ok(pointer.resolve(&alternate_root), "the alternate root must be a valid root")?.is_none(),
      "resolution must reject a token where the pointer requires a structurally equivalent node",
    )
  }

  #[test]
  fn pointer_equality_distinguishes_kind_range_and_structural_content() -> Result<(), TestFailure> {
    let source_root = tree(1, "value")?;
    let source = ensure_some(source_root.first_child(), "the source pointer node must exist")?;
    let source_pointer = SyntaxNodePtr::new(&source);

    let different_kind_root = tree(9, "value")?;
    let different_kind = ensure_some(different_kind_root.first_child(), "the alternate-kind node must exist")?;
    let different_kind_pointer = SyntaxNodePtr::new(&different_kind);
    let other = ensure_some(
      OtherNode::cast(different_kind),
      "the alternate-kind AST wrapper must accept kind nine",
    )?;

    let different_range_root = tree(1, "longer")?;
    let different_range = ensure_some(different_range_root.first_child(), "the alternate-range node must exist")?;
    let different_range_pointer = SyntaxNodePtr::new(&different_range);

    let different_content_root = tree(1, "other")?;
    let different_content = ensure_some(different_content_root.first_child(), "the alternate-content node must exist")?;
    let different_content_pointer = SyntaxNodePtr::new(&different_content);

    let nested = ensure_ok(
      GreenNode::new(SyntaxKind(1), [GreenElement::from(ensure_ok(
        GreenToken::new(SyntaxKind(2), "value"),
        "the nested pointer token must allocate",
      )?)]),
      "the nested pointer node must allocate",
    )?;
    let nested_root = ensure_ok(
      GreenNode::new(SyntaxKind(0), [
        GreenElement::from(ensure_ok(
          GreenToken::new(SyntaxKind(2), "prefix"),
          "the nested pointer prefix must allocate",
        )?),
        GreenElement::from(nested),
      ]),
      "the nested pointer root must allocate",
    )?;
    let nested_root = SyntaxNode::<TestLanguage>::new_root(nested_root);
    let nested_source = ensure_some(nested_root.last_child(), "the nested pointer node must exist after the prefix")?;
    let nested_pointer = SyntaxNodePtr::new(&nested_source);
    let nested_resolution = ensure_some(
      ensure_ok(
        nested_pointer.resolve(&nested_root),
        "a pointer below a wider ancestor range must resolve",
      )?,
      "the nested pointer must descend through its ancestor",
    )?;

    ensure(
      (
        source_pointer == different_kind_pointer,
        source_pointer == different_range_pointer,
        source_pointer == different_content_pointer,
        other.syntax().kind(),
        nested_resolution.to_string(),
      ) == (false, false, false, SyntaxKind(9), "value".to_owned()),
      "syntax-pointer identity must independently include kind, range, structure, and typed wrapper access",
    )
  }
}
