use std::fmt;

use serde::ser::Serialize;
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use serde::ser::Serializer;

use crate::NodeOrToken;
use crate::api::Language;
use crate::api::SyntaxNode;
use crate::api::SyntaxToken;

struct SerDisplay<T>(T);
impl<T: fmt::Display> Serialize for SerDisplay<T> {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    serializer.collect_str(&self.0)
  }
}

struct DisplayDebug<T>(T);
impl<T: fmt::Debug> fmt::Display for DisplayDebug<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Debug::fmt(&self.0, f)
  }
}

/// Serialize the common syntax-element envelope and one variant-specific payload.
fn serialize_element<SerializerType, Kind, Payload>(
  serializer: SerializerType,
  kind: Kind,
  text_range: crate::TextRange,
  payload_name: &'static str,
  payload: &Payload,
) -> Result<SerializerType::Ok, SerializerType::Error>
where
  SerializerType: Serializer,
  Kind: fmt::Debug,
  Payload: Serialize,
{
  let mut state = serializer.serialize_map(Some(3))?;
  state.serialize_entry("kind", &SerDisplay(DisplayDebug(kind)))?;
  state.serialize_entry("text_range", &text_range)?;
  state.serialize_entry(payload_name, payload)?;
  state.end()
}

struct Children<T>(T);

/// Return the serializable child sequence for a node.
fn node_payload<L: Language>(node: &SyntaxNode<L>) -> Children<&SyntaxNode<L>> {
  Children(node)
}

/// Return the serializable exact text for a token.
fn token_payload<L: Language>(token: &SyntaxToken<L>) -> &str {
  token.text()
}

/// Implement the common nested node/token wire envelope.
macro_rules! implement_syntax_serialize {
  ($type:ident, $payload_name:literal, $payload:path) => {
    impl<L: Language> Serialize for $type<L> {
      fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
      where
        S: Serializer,
      {
        let payload = $payload(self);
        serialize_element(serializer, self.kind(), self.text_range(), $payload_name, &payload)
      }
    }
  };
}

implement_syntax_serialize!(SyntaxNode, "children", node_payload);
implement_syntax_serialize!(SyntaxToken, "text", token_payload);

impl<L: Language> Serialize for Children<&'_ SyntaxNode<L>> {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut state = serializer.serialize_seq(None)?;
    self.0.children_with_tokens().try_for_each(|element| match element {
      NodeOrToken::Node(it) => state.serialize_element(&it),
      NodeOrToken::Token(it) => state.serialize_element(&it),
    })?;
    state.end()
  }
}
