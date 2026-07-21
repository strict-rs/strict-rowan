//! Example that takes the input
//! 1 + 2 * 3 + 4
//! and builds the tree
//! - Marker(Root)
//!   - Marker(Operation)
//!     - Marker(Operation)
//!       - "1" Token(Number)
//!       - "+" Token(Add)
//!       - Marker(Operation)
//!         - "2" Token(Number)
//!         - "*" Token(Mul)
//!         - "3" Token(Number)
//!     - "+" Token(Add)
//!     - "4" Token(Number)

use std::io::Write;
use std::iter::Peekable;

use rowan::BuildError;
use rowan::GreenNodeBuilder;
use rowan::NodeOrToken;
use thiserror::Error;

#[derive(Debug, Error)]
enum ExampleError {
  #[error(transparent)]
  Build(#[from] BuildError),
  #[error(transparent)]
  Io(#[from] std::io::Error),
  #[error("syntax-tree indentation overflow")]
  IndentationOverflow,
}

#[path = "support/language.rs"]
mod language_support;

language_support::define_language!(Lang, SyntaxKind, Error, [
  Whitespace, Add, Sub, Mul, Div, Number, Error, Operation, Root,
]);

type SyntaxNode = rowan::SyntaxNode<Lang>;
type SyntaxElement = rowan::SyntaxElement<Lang>;

struct Parser<I: Iterator<Item = (SyntaxKind, String)>> {
  builder: GreenNodeBuilder<'static>,
  iter:    Peekable<I>,
}

impl<I: Iterator<Item = (SyntaxKind, String)>> Parser<I> {
  fn peek(&mut self) -> Result<Option<SyntaxKind>, BuildError> {
    while self.iter.peek().is_some_and(|(kind, _)| *kind == SyntaxKind::Whitespace) {
      self.bump()?;
    }
    Ok(self.iter.peek().map(|(kind, _)| *kind))
  }

  fn bump(&mut self) -> Result<(), BuildError> {
    if let Some((token, string)) = self.iter.next() {
      self.builder.token(token.into(), string.as_str())?;
    }
    Ok(())
  }

  fn parse_val(&mut self) -> Result<(), BuildError> {
    match self.peek()? {
      Some(SyntaxKind::Number) => self.bump()?,
      Some(_) => {
        self.builder.start_node(SyntaxKind::Error.into());
        self.bump()?;
        self.builder.finish_node()?;
      }
      None => {}
    }
    Ok(())
  }

  fn handle_operation(&mut self, tokens: &[SyntaxKind], next: fn(&mut Self) -> Result<(), BuildError>) -> Result<(), BuildError> {
    let checkpoint = self.builder.checkpoint();
    next(self)?;
    while self.peek()?.is_some_and(|kind| tokens.contains(&kind)) {
      self.builder.start_node_at(&checkpoint, SyntaxKind::Operation.into())?;
      self.bump()?;
      next(self)?;
      self.builder.finish_node()?;
    }
    Ok(())
  }

  fn parse_mul(&mut self) -> Result<(), BuildError> {
    self.handle_operation(&[SyntaxKind::Mul, SyntaxKind::Div], Self::parse_val)
  }

  fn parse_add(&mut self) -> Result<(), BuildError> {
    self.handle_operation(&[SyntaxKind::Add, SyntaxKind::Sub], Self::parse_mul)
  }

  fn parse(mut self) -> Result<SyntaxNode, BuildError> {
    self.builder.start_node(SyntaxKind::Root.into());
    self.parse_add()?;
    self.builder.finish_node()?;

    Ok(SyntaxNode::new_root(self.builder.finish()?))
  }
}

fn parse_tokens(tokens: impl IntoIterator<Item = (SyntaxKind, String)>) -> Result<SyntaxNode, BuildError> {
  Parser {
    builder: GreenNodeBuilder::new(),
    iter:    tokens.into_iter().peekable(),
  }
  .parse()
}

fn documented_tokens() -> Vec<(SyntaxKind, String)> {
  [
    (SyntaxKind::Number, "1"),
    (SyntaxKind::Whitespace, " "),
    (SyntaxKind::Add, "+"),
    (SyntaxKind::Whitespace, " "),
    (SyntaxKind::Number, "2"),
    (SyntaxKind::Whitespace, " "),
    (SyntaxKind::Mul, "*"),
    (SyntaxKind::Whitespace, " "),
    (SyntaxKind::Number, "3"),
    (SyntaxKind::Whitespace, " "),
    (SyntaxKind::Add, "+"),
    (SyntaxKind::Whitespace, " "),
    (SyntaxKind::Number, "4"),
  ]
  .into_iter()
  .map(|(kind, text)| (kind, text.to_owned()))
  .collect()
}

fn write_tree(output: &mut impl Write, element: SyntaxElement) -> Result<(), ExampleError> {
  let mut stack = vec![(0_usize, element)];
  while let Some((indent, current)) = stack.pop() {
    let kind = current.kind();
    write!(output, "{:indent$}", "", indent = indent)?;
    match current {
      NodeOrToken::Node(node) => {
        writeln!(output, "- {kind:?}")?;
        let child_indent = indent.checked_add(2).ok_or(ExampleError::IndentationOverflow)?;
        let children = node.children_with_tokens().collect::<Vec<_>>();
        for child in children.into_iter().rev() {
          stack.push((child_indent, child));
        }
      }
      NodeOrToken::Token(token) => writeln!(output, "- {:?} {kind:?}", token.text())?,
    }
  }
  Ok(())
}

fn main() -> Result<(), ExampleError> {
  let ast = parse_tokens(documented_tokens())?;
  let stdout = std::io::stdout();
  let mut output = stdout.lock();
  write_tree(&mut output, ast.into())
}

#[cfg(test)]
mod tests {
  use rowan::NodeOrToken;
  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;
  use strict_test_support::ensure_ok;
  use strict_test_support::ensure_some;

  use super::Lang;
  use super::SyntaxKind;
  use super::documented_tokens;
  use super::parse_tokens;
  use super::write_tree;

  #[test]
  fn parser_preserves_precedence_lossless_text_and_rendered_tree() -> Result<(), TestFailure> {
    let root = ensure_ok(parse_tokens(documented_tokens()), "the documented tokens must build")?;
    ensure_eq(
      &root.to_string(),
      &"1 + 2 * 3 + 4".to_owned(),
      "the parsed tree must retain every source token",
    )?;

    let mut rendered = Vec::new();
    ensure_ok(write_tree(&mut rendered, root.clone().into()), "the syntax tree must render")?;
    let rendered = ensure_ok(String::from_utf8(rendered), "the rendered syntax tree must be UTF-8")?;
    let expected = concat!(
      "- Root\n", "  - Operation\n", "    - Operation\n", "      - \"1\" Number\n", "      - \" \" Whitespace\n", "      - \"+\" Add\n",
      "      - Operation\n", "        - \" \" Whitespace\n", "        - \"2\" Number\n", "        - \" \" Whitespace\n",
      "        - \"*\" Mul\n", "        - \" \" Whitespace\n", "        - \"3\" Number\n", "      - \" \" Whitespace\n",
      "    - \"+\" Add\n", "    - \" \" Whitespace\n", "    - \"4\" Number\n",
    );
    ensure_eq(
      &rendered,
      &expected.to_owned(),
      "the documented syntax tree rendering must remain exact",
    )?;

    let final_addition = ensure_some(root.children().next(), "the root must contain the final addition")?;
    ensure(
      final_addition.kind() == SyntaxKind::Operation,
      "the final addition must be the root operation",
    )?;
    let left_addition = ensure_some(
      final_addition.children().next(),
      "the final addition must contain its left-hand addition",
    )?;
    ensure(
      left_addition.kind() == SyntaxKind::Operation,
      "the left-hand addition must remain nested under the final addition",
    )?;
    let multiplication = ensure_some(
      left_addition.children().next(),
      "the left-hand addition must contain its multiplication operand",
    )?;
    ensure(
      multiplication.kind() == SyntaxKind::Operation,
      "multiplication must remain nested inside the left-hand addition",
    )
  }

  #[test]
  fn parser_wraps_unexpected_values_without_losing_text() -> Result<(), TestFailure> {
    let root = ensure_ok(parse_tokens([(SyntaxKind::Add, "+".to_owned())]), "recovery input must build")?;
    ensure_eq(&root.to_string(), &"+".to_owned(), "an unexpected value token must remain lossless")?;
    let error = ensure_some(root.children().next(), "an unexpected value must produce an error node")?;
    ensure(error.kind() == SyntaxKind::Error, "the recovery node must have Error kind")?;
    let token = ensure_some(
      error.children_with_tokens().next().and_then(NodeOrToken::into_token),
      "the error node must retain the unexpected token",
    )?;
    ensure(token.kind() == SyntaxKind::Add, "the retained token must preserve its Add kind")?;
    ensure_eq(
      &token.text().to_owned(),
      &"+".to_owned(),
      "the retained token text must remain exact",
    )?;

    let empty = ensure_ok(parse_tokens(std::iter::empty()), "empty input must build")?;
    ensure(empty.kind() == SyntaxKind::Root, "empty input must still produce a Root node")?;
    ensure_eq(&empty.to_string(), &String::new(), "empty input must have empty text")?;
    ensure_eq(
      &empty.children_with_tokens().count(),
      &0,
      "empty input must not synthesize an error child",
    )
  }

  #[test]
  fn language_maps_unknown_raw_kinds_to_error() -> Result<(), TestFailure> {
    super::language_support::verify_language::<Lang>()
  }
}
