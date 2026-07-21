//! Iterative child and preorder traversal for immutable red cursors.

use thiserror::Error;

use crate::NodeOrToken;
use crate::WalkEvent;
use crate::cursor::SyntaxElement;
use crate::cursor::SyntaxNode;
use crate::green::SyntaxKind;

/// Misuse of preorder subtree skipping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TraversalError {
  /// The most recently yielded event did not enter a node subtree.
  #[error("no entered node subtree is available to skip")]
  NoNodeSubtree,
  /// A skip was already requested for the current entered node.
  #[error("the current node subtree is already marked for skipping")]
  SkipAlreadyRequested,
}

/// Iterator over direct child nodes.
#[derive(Clone, Debug)]
pub struct SyntaxNodeChildren {
  /// Shared direct-child iteration state.
  state: DirectChildrenState<SyntaxNode>,
}

impl SyntaxNodeChildren {
  /// Create a direct child-node iterator.
  pub(crate) fn new(parent: SyntaxNode) -> Self {
    Self {
      state: DirectChildrenState::new(parent),
    }
  }

  /// Keep only child nodes whose raw kind satisfies `matcher`.
  pub fn by_kind<Matcher>(self, matcher: Matcher) -> SyntaxNodeChildrenByKind<Matcher>
  where
    Matcher: Fn(SyntaxKind) -> bool,
  {
    SyntaxNodeChildrenByKind {
      inner: self,
      matcher,
    }
  }
}

impl Iterator for SyntaxNodeChildren {
  type Item = SyntaxNode;

  fn next(&mut self) -> Option<Self::Item> {
    self.state.advance(SyntaxNode::first_child, SyntaxNode::into_next_sibling)
  }
}

/// Kind-filtered direct child-node iterator.
#[derive(Clone, Debug)]
pub struct SyntaxNodeChildrenByKind<Matcher>
where
  Matcher: Fn(SyntaxKind) -> bool,
{
  /// Unfiltered child iterator.
  inner:   SyntaxNodeChildren,
  /// Raw kind predicate.
  matcher: Matcher,
}

impl<Matcher> Iterator for SyntaxNodeChildrenByKind<Matcher>
where
  Matcher: Fn(SyntaxKind) -> bool,
{
  type Item = SyntaxNode;

  fn next(&mut self) -> Option<Self::Item> {
    self.inner.find(|node| (self.matcher)(node.kind()))
  }
}

/// Iterator over direct child nodes and tokens.
#[derive(Clone, Debug)]
pub struct SyntaxElementChildren {
  /// Shared direct-child iteration state.
  state: DirectChildrenState<SyntaxElement>,
}

impl SyntaxElementChildren {
  /// Create a direct element iterator.
  pub(crate) fn new(parent: SyntaxNode) -> Self {
    Self {
      state: DirectChildrenState::new(parent),
    }
  }

  /// Keep only elements whose raw kind satisfies `matcher`.
  pub fn by_kind<Matcher>(self, matcher: Matcher) -> SyntaxElementChildrenByKind<Matcher>
  where
    Matcher: Fn(SyntaxKind) -> bool,
  {
    SyntaxElementChildrenByKind {
      inner: self,
      matcher,
    }
  }
}

impl Iterator for SyntaxElementChildren {
  type Item = SyntaxElement;

  fn next(&mut self) -> Option<Self::Item> {
    self
      .state
      .advance(SyntaxNode::first_child_or_token, SyntaxElement::into_next_sibling_or_token)
  }
}

/// Shared state machine for direct-child iterators.
#[derive(Clone, Debug)]
struct DirectChildrenState<Element> {
  /// Parent whose children are traversed.
  parent:      SyntaxNode,
  /// Last yielded element retained for allocation-reusing advancement.
  current:     Option<Element>,
  /// Whether the first child has already been selected.
  initialized: bool,
}

impl<Element: Clone> DirectChildrenState<Element> {
  /// Create an uninitialized direct-child state machine.
  fn new(parent: SyntaxNode) -> Self {
    Self {
      parent,
      current: None,
      initialized: false,
    }
  }

  /// Select the first child or advance the uniquely reusable current child.
  fn advance(
    &mut self,
    first: impl FnOnce(&SyntaxNode) -> Option<Element>,
    next: impl FnOnce(Element) -> Option<Element>,
  ) -> Option<Element> {
    self.current = if self.initialized {
      self.current.take().and_then(next)
    } else {
      self.initialized = true;
      first(&self.parent)
    };
    self.current.clone()
  }
}

/// Kind-filtered direct element iterator.
#[derive(Clone, Debug)]
pub struct SyntaxElementChildrenByKind<Matcher>
where
  Matcher: Fn(SyntaxKind) -> bool,
{
  /// Unfiltered element iterator.
  inner:   SyntaxElementChildren,
  /// Raw kind predicate.
  matcher: Matcher,
}

impl<Matcher> Iterator for SyntaxElementChildrenByKind<Matcher>
where
  Matcher: Fn(SyntaxKind) -> bool,
{
  type Item = SyntaxElement;

  fn next(&mut self) -> Option<Self::Item> {
    self.inner.find(|element| (self.matcher)(element.kind()))
  }
}

/// Skip eligibility after the most recent preorder event.
#[derive(Debug, Clone)]
enum NodeSkipState {
  /// No node entry is available.
  Unavailable,
  /// A node was entered and can be skipped.
  Eligible(SyntaxNode),
  /// A skip has been requested for the entered node.
  Requested(SyntaxNode),
}

impl NodeSkipState {
  /// Request skipping while preserving rejected state transitions.
  fn request(&mut self) -> Result<(), TraversalError> {
    match self {
      Self::Eligible(node) => {
        *self = Self::Requested(node.clone());
        Ok(())
      }
      Self::Requested(_) => Err(TraversalError::SkipAlreadyRequested),
      Self::Unavailable => Err(TraversalError::NoNodeSubtree),
    }
  }
}

/// Preorder traversal over nodes only.
#[derive(Debug, Clone)]
pub struct Preorder {
  /// Root at which traversal stops.
  start:      SyntaxNode,
  /// Next event under normal traversal.
  next_event: Option<WalkEvent<SyntaxNode>>,
  /// Current skip eligibility.
  skip_state: NodeSkipState,
}

impl Preorder {
  /// Create a node-only preorder traversal.
  pub(crate) fn new(start: SyntaxNode) -> Self {
    Self {
      next_event: Some(WalkEvent::Enter(start.clone())),
      start,
      skip_state: NodeSkipState::Unavailable,
    }
  }

  /// Compute the event following `current` without recursion.
  fn following_event(&self, current: &WalkEvent<SyntaxNode>) -> Option<WalkEvent<SyntaxNode>> {
    match current {
      WalkEvent::Enter(node) => node
        .first_child()
        .map_or_else(|| Some(WalkEvent::Leave(node.clone())), |child| Some(WalkEvent::Enter(child))),
      WalkEvent::Leave(node) if node == &self.start => None,
      WalkEvent::Leave(node) => node
        .next_sibling()
        .map_or_else(|| node.parent().map(WalkEvent::Leave), |sibling| Some(WalkEvent::Enter(sibling))),
    }
  }
}

impl Iterator for Preorder {
  type Item = WalkEvent<SyntaxNode>;

  fn next(&mut self) -> Option<Self::Item> {
    if let NodeSkipState::Requested(node) = &self.skip_state {
      self.next_event = Some(WalkEvent::Leave(node.clone()));
    }
    let current = self.next_event.take()?;
    self.next_event = self.following_event(&current);
    self.skip_state = match &current {
      WalkEvent::Enter(node) => NodeSkipState::Eligible(node.clone()),
      WalkEvent::Leave(_) => NodeSkipState::Unavailable,
    };
    Some(current)
  }
}

/// Preorder traversal over nodes and tokens.
#[derive(Debug, Clone)]
pub struct PreorderWithTokens {
  /// Root element at which traversal stops.
  start:      SyntaxElement,
  /// Next event under normal traversal.
  next_event: Option<WalkEvent<SyntaxElement>>,
  /// Current node-only skip eligibility.
  skip_state: NodeSkipState,
}

/// Implement the shared public skip transition for each preorder representation.
macro_rules! implement_skip_subtree {
  ($preorder:ident) => {
    impl $preorder {
      /// Skip descendants of the node most recently entered.
      ///
      /// # Errors
      ///
      /// Returns [`TraversalError::NoNodeSubtree`] outside a node-entry window and
      /// [`TraversalError::SkipAlreadyRequested`] for a duplicate request. Rejected calls do not
      /// change traversal state.
      pub fn skip_subtree(&mut self) -> Result<(), TraversalError> {
        self.skip_state.request()
      }
    }
  };
}

implement_skip_subtree!(Preorder);
implement_skip_subtree!(PreorderWithTokens);

impl PreorderWithTokens {
  /// Create a preorder traversal that includes tokens.
  pub(crate) fn new(start: SyntaxNode) -> Self {
    let start = SyntaxElement::from(start);
    Self {
      next_event: Some(WalkEvent::Enter(start.clone())),
      start,
      skip_state: NodeSkipState::Unavailable,
    }
  }

  /// Compute the event following `current` without recursion.
  fn following_event(&self, current: &WalkEvent<SyntaxElement>) -> Option<WalkEvent<SyntaxElement>> {
    match current {
      WalkEvent::Enter(NodeOrToken::Node(node)) => node.first_child_or_token().map_or_else(
        || Some(WalkEvent::Leave(SyntaxElement::from(node.clone()))),
        |child| Some(WalkEvent::Enter(child)),
      ),
      WalkEvent::Enter(NodeOrToken::Token(token)) => Some(WalkEvent::Leave(SyntaxElement::from(token.clone()))),
      WalkEvent::Leave(element) if element == &self.start => None,
      WalkEvent::Leave(element) => element.next_sibling_or_token().map_or_else(
        || element.parent().map(|parent| WalkEvent::Leave(SyntaxElement::from(parent))),
        |sibling| Some(WalkEvent::Enter(sibling)),
      ),
    }
  }
}

impl Iterator for PreorderWithTokens {
  type Item = WalkEvent<SyntaxElement>;

  fn next(&mut self) -> Option<Self::Item> {
    if let NodeSkipState::Requested(node) = &self.skip_state {
      self.next_event = Some(WalkEvent::Leave(SyntaxElement::from(node.clone())));
    }
    let current = self.next_event.take()?;
    self.next_event = self.following_event(&current);
    self.skip_state = match &current {
      WalkEvent::Enter(NodeOrToken::Node(node)) => NodeSkipState::Eligible(node.clone()),
      WalkEvent::Enter(NodeOrToken::Token(_)) | WalkEvent::Leave(_) => NodeSkipState::Unavailable,
    };
    Some(current)
  }
}

#[cfg(test)]
mod tests {
  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;
  use strict_test_support::ensure_ok;
  use strict_test_support::ensure_some;

  use super::TraversalError;
  use crate::GreenElement;
  use crate::GreenNode;
  use crate::GreenToken;
  use crate::NodeOrToken;
  use crate::SyntaxKind;
  use crate::WalkEvent;
  use crate::cursor::SyntaxNode;
  use crate::test_support::event_trace;

  /// Build a node from a fallible green fixture.
  fn node(kind: u16, children: impl IntoIterator<Item = GreenElement>) -> Result<GreenNode, TestFailure> {
    ensure_ok(
      GreenNode::new(SyntaxKind(kind), children),
      "the traversal node fixture must allocate",
    )
  }

  /// Build a token from a fallible green fixture.
  fn token(text: &str) -> Result<GreenToken, TestFailure> {
    ensure_ok(GreenToken::new(SyntaxKind(1), text), "the traversal token fixture must allocate")
  }

  /// Build a root with two child nodes and one direct token.
  fn traversal_tree() -> Result<SyntaxNode, TestFailure> {
    let first = node(2, [GreenElement::from(token("a")?)])?;
    let second = node(3, std::iter::empty())?;
    Ok(SyntaxNode::new_root(node(0, [
      GreenElement::from(first),
      GreenElement::from(token("b")?),
      GreenElement::from(second),
    ])?))
  }

  #[test]
  fn node_preorder_skip_state_is_explicit_and_transactional() -> Result<(), TestFailure> {
    let root = traversal_tree()?;
    let mut preorder = root.preorder();
    ensure(
      preorder.skip_subtree() == Err(TraversalError::NoNodeSubtree),
      "skipping before the first event must be rejected",
    )?;
    let root_enter = ensure_some(preorder.next(), "the root enter event must exist")?;
    ensure(matches!(root_enter, WalkEvent::Enter(_)), "the first event must enter the root")?;
    let first_enter = ensure_some(preorder.next(), "the first child enter event must exist")?;
    ensure(
      matches!(first_enter, WalkEvent::Enter(ref node) if node.kind() == SyntaxKind(2)),
      "the traversal must enter the first child",
    )?;
    ensure_ok(preorder.skip_subtree(), "skipping immediately after a node enter must succeed")?;
    ensure(
      preorder.skip_subtree() == Err(TraversalError::SkipAlreadyRequested),
      "a duplicate skip request must be rejected without advancing",
    )?;
    let first_leave = ensure_some(preorder.next(), "the skipped child leave event must remain")?;
    ensure(
      matches!(first_leave, WalkEvent::Leave(ref node) if node.kind() == SyntaxKind(2)),
      "skipping must omit descendants but preserve the matching leave",
    )?;
    let second_enter = ensure_some(preorder.next(), "the second child enter event must follow")?;
    ensure(
      matches!(second_enter, WalkEvent::Enter(ref node) if node.kind() == SyntaxKind(3)),
      "node-only preorder must skip the intervening token and enter the next node",
    )?;
    ensure(
      preorder.skip_subtree() == Ok(()),
      "a new node-entry window must accept a fresh skip",
    )?;
    let _second_leave = ensure_some(preorder.next(), "the second child leave must exist")?;
    let _root_leave = ensure_some(preorder.next(), "the root leave must exist")?;
    ensure(preorder.next().is_none(), "the traversal must exhaust after the root leave")?;
    ensure(
      preorder.skip_subtree() == Err(TraversalError::NoNodeSubtree),
      "skipping after exhaustion must be rejected",
    )
  }

  #[test]
  fn token_entries_never_open_a_skip_window() -> Result<(), TestFailure> {
    let root = traversal_tree()?;
    let mut preorder = root.preorder_with_tokens();
    let _root_enter = ensure_some(preorder.next(), "the root enter must exist")?;
    let _first_node_enter = ensure_some(preorder.next(), "the first child node enter must exist")?;
    let token_enter = ensure_some(preorder.next(), "the first token enter must exist")?;
    ensure(
      matches!(token_enter, WalkEvent::Enter(NodeOrToken::Token(ref token)) if token.text() == "a"),
      "preorder-with-tokens must emit the token entry",
    )?;
    ensure(
      preorder.skip_subtree() == Err(TraversalError::NoNodeSubtree),
      "entering a token must not expose a subtree skip",
    )?;
    let token_leave = ensure_some(preorder.next(), "the token leave must remain after rejection")?;
    ensure(
      matches!(token_leave, WalkEvent::Leave(NodeOrToken::Token(ref token)) if token.text() == "a"),
      "a rejected skip must leave the next event unchanged",
    )
  }

  #[test]
  fn child_filters_and_complete_element_preorder_preserve_live_iterator_state() -> Result<(), TestFailure> {
    let root = traversal_tree()?;
    ensure(
      root.children().by_kind(|kind| kind == SyntaxKind(2)).count() == 1,
      "node filtering must retain the one matching direct child",
    )?;
    ensure(
      root.children().by_kind(|kind| kind == SyntaxKind(9)).next().is_none(),
      "node filtering must exhaust cleanly when no kind matches",
    )?;
    let mut progressed_nodes = root.children();
    let first = ensure_some(progressed_nodes.next(), "the first child node must exist")?;
    ensure(
      first.kind() == SyntaxKind(2),
      "node iteration must begin with the first source node",
    )?;
    let second = ensure_some(
      progressed_nodes.by_kind(|kind| kind == SyntaxKind(3)).next(),
      "filtering a progressed iterator must continue from its live position",
    )?;
    ensure(second.kind() == SyntaxKind(3), "progressed filtering must select the later node")?;

    ensure(
      root.children_with_tokens().by_kind(|kind| kind == SyntaxKind(1)).count() == 1,
      "element filtering must retain the direct token and exclude nested tokens",
    )?;
    ensure(
      root
        .children_with_tokens()
        .by_kind(|kind| kind == SyntaxKind(9))
        .next()
        .is_none(),
      "element filtering must exhaust cleanly on a matcher miss",
    )?;
    let mut progressed_elements = root.children_with_tokens();
    let _first_element = ensure_some(progressed_elements.next(), "the first direct element must exist")?;
    let token = ensure_some(
      progressed_elements
        .by_kind(|kind| kind == SyntaxKind(1))
        .next()
        .and_then(NodeOrToken::into_token),
      "filtering a progressed element iterator must find the direct token",
    )?;
    ensure_eq(&token.text(), &"b", "the progressed element filter must preserve token text")?;

    let events = event_trace(root.preorder_with_tokens(), |element| element.kind());
    ensure(
      events
        == vec![
          (true, SyntaxKind(0)),
          (true, SyntaxKind(2)),
          (true, SyntaxKind(1)),
          (false, SyntaxKind(1)),
          (false, SyntaxKind(2)),
          (true, SyntaxKind(1)),
          (false, SyntaxKind(1)),
          (true, SyntaxKind(3)),
          (false, SyntaxKind(3)),
          (false, SyntaxKind(0)),
        ],
      "complete element preorder must emit balanced node and token events in source order",
    )?;

    let mut skipped = root.preorder_with_tokens();
    let _root_enter = ensure_some(skipped.next(), "the root enter event must exist")?;
    ensure_ok(
      skipped.skip_subtree(),
      "element preorder must accept a skip immediately after entering a node",
    )?;
    ensure(
      skipped.skip_subtree() == Err(TraversalError::SkipAlreadyRequested),
      "element preorder must reject a duplicate skip without advancing",
    )?;
    let leave = ensure_some(skipped.next(), "skipping the root must retain its leave event")?;
    ensure(
      matches!(leave, WalkEvent::Leave(NodeOrToken::Node(ref node)) if node.kind() == SyntaxKind(0)),
      "skipping the root must omit all descendants and emit the matching leave",
    )?;
    ensure(
      skipped.skip_subtree() == Err(TraversalError::NoNodeSubtree),
      "the leave event must close the subtree-skip window",
    )?;
    ensure(skipped.next().is_none(), "a root skip must exhaust traversal after the root leave")
  }
}
