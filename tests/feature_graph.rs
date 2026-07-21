//! Dev-target feature-graph contracts.

use std::io::Cursor;

use strict_test_support::TestFailure;
use strict_test_support::ensure;
use strict_test_support::ensure_ok;

/// Prove that dev targets activate `serde_json`'s reader-backed `std` surface.
#[test]
fn serde_json_std_supports_reader_backed_dev_tests() -> Result<(), TestFailure> {
  let input = Cursor::new(br#"{"rowan":"strict"}"#);
  let parsed = ensure_ok(
    serde_json::from_reader::<_, serde_json::Value>(input),
    "the dev dependency must expose serde_json's std reader API",
  )?;
  ensure(
    parsed.get("rowan").and_then(serde_json::Value::as_str) == Some("strict"),
    "reader-backed JSON parsing must preserve the dev-target payload",
  )?;

  let malformed = Cursor::new(br#"{"rowan":}"#);
  ensure(
    serde_json::from_reader::<_, serde_json::Value>(malformed).is_err(),
    "reader-backed JSON parsing must reject malformed dev-target input",
  )
}
