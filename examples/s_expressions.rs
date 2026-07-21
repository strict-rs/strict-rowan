//! In this tutorial, we will write parser
//! and evaluator of arithmetic S-expressions,
//! which look like this:
//! ```
//! (+ (* 15 2) 62)
//! ```
//!
//! It's suggested to read the conceptual overview of the design
//! alongside this tutorial:
//! https://rust-analyzer.github.io/book/contributing/syntax.html

use std::io;
use std::io::Write;

use rowan::BuildError;
use thiserror::Error;

#[derive(Debug, Error)]
enum ExampleError {
  #[error(transparent)]
  Build(#[from] BuildError),
  #[error(transparent)]
  Io(#[from] io::Error),
}

#[path = "support/language.rs"]
mod language_support;

language_support::define_language!(Lang, SyntaxKind, Error, [
  LeftParen, RightParen, Word, Whitespace, Error, List, Atom, Root,
]);

/// GreenNode is an immutable tree, which is cheap to change,
/// but doesn't contain offsets and parent pointers.
use rowan::GreenNode;
/// You can construct GreenNodes by hand, but a builder
/// is helpful for top-down parsers: it maintains a stack
/// of currently in-progress nodes
use rowan::GreenNodeBuilder;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParseError {
  UnmatchedRightParen,
  MissingRightParen,
}

/// The parse results are stored as a "green tree".
/// We'll discuss working with the results later
struct Parse {
  green_node: GreenNode,
  errors:     Vec<ParseError>,
}

/// Now, let's write a parser.
/// Syntax diagnostics remain successful parse outcomes, while builder protocol and allocation
/// failures propagate separately.
fn parse(text: &str) -> Result<Parse, BuildError> {
  struct Parser {
    /// input tokens, including whitespace,
    /// in *reverse* order.
    tokens:  Vec<(SyntaxKind, String)>,
    /// the in-progress tree.
    builder: GreenNodeBuilder<'static>,
    /// the list of syntax errors we've accumulated
    /// so far.
    errors:  Vec<ParseError>,
  }

  /// The outcome of parsing a single S-expression
  enum SexpRes {
    /// An S-expression (i.e. an atom, or a list) was successfully parsed
    Ok,
    /// Nothing was parsed, as no significant tokens remained
    Eof,
    /// An unexpected ')' was found
    RParen,
  }

  impl Parser {
    fn parse(mut self) -> Result<Parse, BuildError> {
      self.builder.start_node(SyntaxKind::Root.into());
      while self.parse_root_item()? {}
      self.skip_ws()?;
      self.builder.finish_node()?;

      Ok(Parse {
        green_node: self.builder.finish()?,
        errors:     self.errors,
      })
    }

    /// Parse or recover one root-level expression and report whether parsing should continue.
    fn parse_root_item(&mut self) -> Result<bool, BuildError> {
      match self.sexp()? {
        SexpRes::Eof => Ok(false),
        SexpRes::RParen => {
          self.builder.start_node(SyntaxKind::Error.into());
          self.errors.push(ParseError::UnmatchedRightParen);
          self.bump()?;
          self.builder.finish_node()?;
          Ok(true)
        }
        SexpRes::Ok => Ok(true),
      }
    }

    fn list(&mut self) -> Result<(), BuildError> {
      self.builder.start_node(SyntaxKind::List.into());
      self.bump()?;
      while self.parse_list_item()? {}
      self.builder.finish_node()
    }

    /// Parse one list item and report whether the current list remains open.
    fn parse_list_item(&mut self) -> Result<bool, BuildError> {
      match self.sexp()? {
        SexpRes::Eof => {
          self.errors.push(ParseError::MissingRightParen);
          Ok(false)
        }
        SexpRes::RParen => {
          self.bump()?;
          Ok(false)
        }
        SexpRes::Ok => Ok(true),
      }
    }

    fn sexp(&mut self) -> Result<SexpRes, BuildError> {
      self.skip_ws()?;
      let kind = match self.current() {
        None => return Ok(SexpRes::Eof),
        Some(SyntaxKind::RightParen) => return Ok(SexpRes::RParen),
        Some(kind) => kind,
      };
      match kind {
        SyntaxKind::LeftParen => self.list()?,
        SyntaxKind::Word => {
          self.builder.start_node(SyntaxKind::Atom.into());
          self.bump()?;
          self.builder.finish_node()?;
        }
        SyntaxKind::Error => self.bump()?,
        SyntaxKind::Whitespace => self.skip_ws()?,
        SyntaxKind::RightParen => return Ok(SexpRes::RParen),
        SyntaxKind::List | SyntaxKind::Atom | SyntaxKind::Root => {
          self.builder.start_node(SyntaxKind::Error.into());
          self.bump()?;
          self.builder.finish_node()?;
        }
      }
      Ok(SexpRes::Ok)
    }

    /// Advance one token, adding it to the current branch of the tree builder.
    fn bump(&mut self) -> Result<(), BuildError> {
      if let Some((kind, text)) = self.tokens.pop() {
        self.builder.token(kind.into(), text.as_str())?;
      }
      Ok(())
    }

    /// Peek at the first unprocessed token
    fn current(&self) -> Option<SyntaxKind> {
      self.tokens.last().map(|(kind, _)| *kind)
    }

    fn skip_ws(&mut self) -> Result<(), BuildError> {
      while self.current() == Some(SyntaxKind::Whitespace) {
        self.bump()?;
      }
      Ok(())
    }
  }

  let mut tokens = lex(text);
  tokens.reverse();
  Parser {
    tokens,
    builder: GreenNodeBuilder::new(),
    errors: Vec::new(),
  }
  .parse()
}

/// To work with the parse results we need a view into the
/// green tree - the Syntax tree.
/// It is also immutable, like a GreenNode,
/// but it contains parent pointers, offsets, and
/// has identity semantics.
type SyntaxNode = rowan::SyntaxNode<Lang>;
type SyntaxToken = rowan::SyntaxToken<Lang>;
type SyntaxElement = rowan::SyntaxElement<Lang>;

impl Parse {
  fn syntax(&self) -> SyntaxNode {
    SyntaxNode::new_root(self.green_node.clone())
  }

  fn errors(&self) -> &[ParseError] {
    &self.errors
  }
}

impl std::fmt::Debug for Parse {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter
      .debug_struct("Parse")
      .field("green_node", &self.green_node)
      .field("errors", &self.errors())
      .finish()
  }
}

/// So far, we've been working with a homogeneous untyped tree.
/// It's nice to provide generic tree operations, like traversals,
/// but it's a bad fit for semantic analysis.
/// This crate itself does not provide AST facilities directly,
/// but it is possible to layer AST on top of `SyntaxNode` API.
/// Let's write a function to evaluate S-expression.
///
/// For that, let's define AST nodes.
/// It'll be quite a bunch of repetitive code, so we'll use a macro.
///
/// For a real language, you'd want to generate an AST. I find a
/// combination of `serde`, `ron` and `tera` crates invaluable for that!
macro_rules! ast_node {
  ($ast:ident, $kind:ident) => {
    #[derive(PartialEq, Eq, Hash)]
    #[repr(transparent)]
    struct $ast(SyntaxNode);

    impl $ast {
      fn cast(node: SyntaxNode) -> Option<Self> {
        if node.kind() == SyntaxKind::$kind {
          Some(Self(node))
        } else {
          None
        }
      }
    }
  };
}

ast_node!(Root, Root);
ast_node!(Atom, Atom);
ast_node!(List, List);

enum Sexp {
  Atom(Atom),
  List(List),
}

impl Sexp {
  fn cast(node: SyntaxNode) -> Option<Self> {
    if let Some(atom) = Atom::cast(node.clone()) {
      Some(Self::Atom(atom))
    } else {
      List::cast(node).map(Self::List)
    }
  }

  fn eval(&self) -> Option<i64> {
    match self {
      Self::Atom(atom) => atom.eval(),
      Self::List(list) => list.eval(),
    }
  }
}

impl Root {
  fn sexps(&self) -> impl Iterator<Item = Sexp> + '_ {
    self
      .0
      .children_with_tokens()
      .filter_map(SyntaxElement::into_node)
      .filter_map(Sexp::cast)
  }
}

enum Op {
  Add,
  Sub,
  Div,
  Mul,
}

impl Atom {
  fn token(&self) -> Option<SyntaxToken> {
    self.0.first_token()
  }

  fn eval(&self) -> Option<i64> {
    self.token()?.text().parse().ok()
  }

  fn as_op(&self) -> Option<Op> {
    let token = self.token()?;
    let op = match token.text() {
      "+" => Op::Add,
      "-" => Op::Sub,
      "*" => Op::Mul,
      "/" => Op::Div,
      _ => return None,
    };
    Some(op)
  }
}

impl List {
  fn sexps(&self) -> impl Iterator<Item = Sexp> + '_ {
    self
      .0
      .children_with_tokens()
      .filter_map(SyntaxElement::into_node)
      .filter_map(Sexp::cast)
  }

  fn eval(&self) -> Option<i64> {
    let mut sexps = self.sexps();
    let op = match sexps.next()? {
      Sexp::Atom(atom) => atom.as_op()?,
      Sexp::List(_) => return None,
    };
    let arg1 = sexps.next()?.eval()?;
    let arg2 = sexps.next()?.eval()?;
    let result = match op {
      Op::Add => arg1.checked_add(arg2)?,
      Op::Sub => arg1.checked_sub(arg2)?,
      Op::Mul => arg1.checked_mul(arg2)?,
      Op::Div => arg1.checked_div(arg2)?,
    };
    Some(result)
  }
}

impl Parse {
  fn root(&self) -> Option<Root> {
    Root::cast(self.syntax())
  }
}

fn evaluate(text: &str) -> Result<Vec<Option<i64>>, BuildError> {
  let parsed = parse(text)?;
  Ok(match parsed.root() {
    Some(root) => root.sexps().map(|sexp| sexp.eval()).collect(),
    None => Vec::new(),
  })
}

const EXAMPLE_SEXPS: &str = "
92
(+ 62 30)
(/ 92 0)
nan
(+ (* 15 2) 62)
";

fn main() -> Result<(), ExampleError> {
  let results = evaluate(EXAMPLE_SEXPS)?;
  let stderr = io::stderr();
  let mut output = stderr.lock();
  writeln!(output, "{results:?}")?;
  Ok(())
}

/// Split the input string into a flat list of tokens
/// (such as LeftParen, Word, and Whitespace)
fn lex(text: &str) -> Vec<(SyntaxKind, String)> {
  fn tok(kind: SyntaxKind) -> m_lexer::TokenKind {
    m_lexer::TokenKind(rowan::SyntaxKind::from(kind).0)
  }

  fn kind(token: m_lexer::TokenKind) -> SyntaxKind {
    match token.0 {
      0 => SyntaxKind::LeftParen,
      1 => SyntaxKind::RightParen,
      2 => SyntaxKind::Word,
      3 => SyntaxKind::Whitespace,
      4 => SyntaxKind::Error,
      5.. => SyntaxKind::Error,
    }
  }

  let lexer = m_lexer::LexerBuilder::new()
    .error_token(tok(SyntaxKind::Error))
    .tokens(&[
      (tok(SyntaxKind::LeftParen), r"\("),
      (tok(SyntaxKind::RightParen), r"\)"),
      (tok(SyntaxKind::Word), r"[^\s()]+"),
      (tok(SyntaxKind::Whitespace), r"\s+"),
    ])
    .build();

  lexer
    .tokenize(text)
    .into_iter()
    .scan(text, |remaining, token| {
      let token_text = remaining.get(..token.len)?.to_owned();
      *remaining = remaining.get(token.len..)?;
      Some((kind(token.kind), token_text))
    })
    .collect()
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
  use super::ParseError;
  use super::SyntaxElement;
  use super::SyntaxKind;
  use super::evaluate;
  use super::parse;

  #[test]
  fn parser_builds_lossless_nested_tree_without_errors() -> Result<(), TestFailure> {
    let text = "(+ (* 15 2) 62)";
    let parsed = ensure_ok(parse(text), "balanced input must build")?;
    ensure(parsed.errors().is_empty(), "balanced input must not produce parse errors")?;
    let root = parsed.syntax();
    ensure(root.kind() == SyntaxKind::Root, "the parsed syntax must have a Root node")?;
    ensure_eq(&root.to_string(), &text.to_owned(), "the parsed tree must remain lossless")?;
    ensure_eq(
      &format!("{:?}", root),
      &"Root@0..15".to_owned(),
      "the root range must span the input",
    )?;
    ensure_eq(&root.children().count(), &1, "the root must contain one top-level list")?;

    let list = ensure_some(root.children().next(), "the root must contain its list")?;
    ensure(list.kind() == SyntaxKind::List, "the top-level child must be a List")?;
    let children = list
      .children_with_tokens()
      .map(|child| {
        let child: SyntaxElement = child;
        format!("{:?}@{:?}", child.kind(), child.text_range())
      })
      .collect::<Vec<_>>()
      .join("|");
    let expected = concat!(
      "LeftParen@0..1|", "Atom@1..2|", "Whitespace@2..3|", "List@3..11|", "Whitespace@11..12|", "Atom@12..14|", "RightParen@14..15",
    );
    ensure_eq(
      &children,
      &expected.to_owned(),
      "the list must preserve token kinds, node kinds, ranges, whitespace, and source order",
    )
  }

  #[test]
  fn parser_recovers_from_unbalanced_parentheses() -> Result<(), TestFailure> {
    let parsed = ensure_ok(parse(") ("), "unbalanced input must still build")?;
    ensure(
      parsed.errors() == [ParseError::UnmatchedRightParen, ParseError::MissingRightParen],
      "unbalanced parentheses must produce ordered typed errors",
    )?;
    let root = parsed.syntax();
    ensure_eq(&root.to_string(), &") (".to_owned(), "recovery must preserve every source byte")?;

    let mut recovered_nodes = root.children();
    let unmatched = ensure_some(recovered_nodes.next(), "recovery must retain the unmatched right parenthesis")?;
    ensure(
      unmatched.kind() == SyntaxKind::Error,
      "the unmatched right parenthesis must be under Error",
    )?;
    let unmatched_token = ensure_some(
      unmatched.children_with_tokens().next().and_then(NodeOrToken::into_token),
      "the Error node must retain the right-parenthesis token",
    )?;
    ensure(
      unmatched_token.kind() == SyntaxKind::RightParen,
      "the unmatched token must preserve RightParen kind",
    )?;

    let unterminated = ensure_some(recovered_nodes.next(), "recovery must retain the unterminated list")?;
    ensure(
      unterminated.kind() == SyntaxKind::List,
      "the unterminated left parenthesis must be under List",
    )?;
    let unterminated_token = ensure_some(
      unterminated.children_with_tokens().next().and_then(NodeOrToken::into_token),
      "the List node must retain the left-parenthesis token",
    )?;
    ensure(
      unterminated_token.kind() == SyntaxKind::LeftParen,
      "the unterminated token must preserve LeftParen kind",
    )
  }

  #[test]
  fn evaluator_distinguishes_values_and_invalid_expressions() -> Result<(), TestFailure> {
    let results = ensure_ok(evaluate(super::EXAMPLE_SEXPS), "example expressions must build")?;
    ensure(
      results == [Some(92), Some(92), None, None, Some(92)],
      "evaluation must distinguish arithmetic values from division-by-zero and non-number failures",
    )?;
    let invalid = ensure_ok(
      evaluate("(+ 9223372036854775807 1)\n(% 1 2)\n(/ -9223372036854775808 -1)"),
      "overflow and invalid-operator syntax must still build",
    )?;
    ensure(
      invalid == [None, None, None],
      "checked addition, invalid operators, and checked division overflow must all return None",
    )
  }

  #[test]
  fn language_maps_unknown_raw_kinds_to_error() -> Result<(), TestFailure> {
    super::language_support::verify_language::<Lang>()
  }
}
