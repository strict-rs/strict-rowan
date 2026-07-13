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

/// Let's start with defining all kinds of tokens and
/// composite nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum SyntaxKind {
  LeftParen,
  RightParen,
  Word,
  Whitespace,
  Error,
  List,
  Atom,
  Root,
}

/// Some boilerplate is needed, as rowan settled on using its own
/// `struct SyntaxKind(u16)` internally, instead of accepting the
/// user's `enum SyntaxKind` as a type parameter.
///
/// First, to easily pass the enum variants into rowan via `.into()`:
impl From<SyntaxKind> for rowan::SyntaxKind {
  fn from(kind: SyntaxKind) -> Self {
    Self(match kind {
      SyntaxKind::LeftParen => 0,
      SyntaxKind::RightParen => 1,
      SyntaxKind::Word => 2,
      SyntaxKind::Whitespace => 3,
      SyntaxKind::Error => 4,
      SyntaxKind::List => 5,
      SyntaxKind::Atom => 6,
      SyntaxKind::Root => 7,
    })
  }
}

/// Second, implementing the `Language` trait teaches rowan to convert between
/// these two SyntaxKind types, allowing for a nicer SyntaxNode API where
/// "kinds" are values from our `enum SyntaxKind`, instead of plain u16 values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Lang {}

impl rowan::Language for Lang {
  type Kind = SyntaxKind;

  fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
    match raw.0 {
      0 => SyntaxKind::LeftParen,
      1 => SyntaxKind::RightParen,
      2 => SyntaxKind::Word,
      3 => SyntaxKind::Whitespace,
      4 => SyntaxKind::Error,
      5 => SyntaxKind::List,
      6 => SyntaxKind::Atom,
      7 => SyntaxKind::Root,
      8.. => SyntaxKind::Error,
    }
  }

  fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
    kind.into()
  }
}

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
/// Note that `parse` does not return a `Result`:
/// by design, syntax tree can be built even for
/// completely invalid source code.
fn parse(text: &str) -> Parse {
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
    fn parse(mut self) -> Parse {
      self.builder.start_node(SyntaxKind::Root.into());
      loop {
        match self.sexp() {
          SexpRes::Eof => break,
          SexpRes::RParen => {
            self.builder.start_node(SyntaxKind::Error.into());
            self.errors.push(ParseError::UnmatchedRightParen);
            self.bump();
            self.builder.finish_node();
          }
          SexpRes::Ok => {}
        }
      }
      self.skip_ws();
      self.builder.finish_node();

      Parse {
        green_node: self.builder.finish(),
        errors:     self.errors,
      }
    }

    fn list(&mut self) {
      self.builder.start_node(SyntaxKind::List.into());
      self.bump();
      loop {
        match self.sexp() {
          SexpRes::Eof => {
            self.errors.push(ParseError::MissingRightParen);
            break;
          }
          SexpRes::RParen => {
            self.bump();
            break;
          }
          SexpRes::Ok => {}
        }
      }
      self.builder.finish_node();
    }

    fn sexp(&mut self) -> SexpRes {
      self.skip_ws();
      let kind = match self.current() {
        None => return SexpRes::Eof,
        Some(SyntaxKind::RightParen) => return SexpRes::RParen,
        Some(kind) => kind,
      };
      match kind {
        SyntaxKind::LeftParen => self.list(),
        SyntaxKind::Word => {
          self.builder.start_node(SyntaxKind::Atom.into());
          self.bump();
          self.builder.finish_node();
        }
        SyntaxKind::Error => self.bump(),
        SyntaxKind::Whitespace => self.skip_ws(),
        SyntaxKind::RightParen => return SexpRes::RParen,
        SyntaxKind::List | SyntaxKind::Atom | SyntaxKind::Root => {
          self.builder.start_node(SyntaxKind::Error.into());
          self.bump();
          self.builder.finish_node();
        }
      }
      SexpRes::Ok
    }

    /// Advance one token, adding it to the current branch of the tree builder.
    fn bump(&mut self) {
      if let Some((kind, text)) = self.tokens.pop() {
        self.builder.token(kind.into(), text.as_str());
      }
    }

    /// Peek at the first unprocessed token
    fn current(&self) -> Option<SyntaxKind> {
      self.tokens.last().map(|(kind, _)| *kind)
    }

    fn skip_ws(&mut self) {
      while self.current() == Some(SyntaxKind::Whitespace) {
        self.bump()
      }
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
      Op::Add => arg1 + arg2,
      Op::Sub => arg1 - arg2,
      Op::Mul => arg1 * arg2,
      Op::Div if arg2 == 0 => return None,
      Op::Div => arg1 / arg2,
    };
    Some(result)
  }
}

impl Parse {
  fn root(&self) -> Option<Root> {
    Root::cast(self.syntax())
  }
}

fn evaluate(text: &str) -> Vec<Option<i64>> {
  let parsed = parse(text);
  match parsed.root() {
    Some(root) => root.sexps().map(|sexp| sexp.eval()).collect(),
    None => Vec::new(),
  }
}

const EXAMPLE_SEXPS: &str = "
92
(+ 62 30)
(/ 92 0)
nan
(+ (* 15 2) 62)
";

fn main() -> io::Result<()> {
  let results = evaluate(EXAMPLE_SEXPS);
  let stderr = io::stderr();
  let mut output = stderr.lock();
  writeln!(output, "{results:?}")
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
    .map(|token| (token.len, kind(token.kind)))
    .scan(0usize, |start_offset, (len, kind)| {
      let token_text = text[*start_offset..*start_offset + len].to_owned();
      *start_offset += len;
      Some((kind, token_text))
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use rowan::Language;
  use rowan::NodeOrToken;
  use strict_test_support::TestFailure;
  use strict_test_support::ensure;
  use strict_test_support::ensure_eq;
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
    let parsed = parse(text);
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
    let parsed = parse(") (");
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
    let results = evaluate(super::EXAMPLE_SEXPS);
    ensure(
      results == [Some(92), Some(92), None, None, Some(92)],
      "evaluation must distinguish arithmetic values from division-by-zero and non-number failures",
    )
  }

  #[test]
  fn language_maps_unknown_raw_kinds_to_error() -> Result<(), TestFailure> {
    ensure(
      Lang::kind_from_raw(rowan::SyntaxKind(u16::MAX)) == SyntaxKind::Error,
      "unknown raw syntax kinds must recover as Error",
    )?;
    for kind in [
      SyntaxKind::LeftParen,
      SyntaxKind::RightParen,
      SyntaxKind::Word,
      SyntaxKind::Whitespace,
      SyntaxKind::Error,
      SyntaxKind::List,
      SyntaxKind::Atom,
      SyntaxKind::Root,
    ] {
      ensure(
        Lang::kind_from_raw(Lang::kind_to_raw(kind)) == kind,
        "every declared S-expression syntax kind must round-trip",
      )?;
    }
    Ok(())
  }
}
