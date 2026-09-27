# Scalar document roots in JSON extraction

The owner-run SQL Server 2025 fixture
`reference/json-extraction-boundaries.json` records six `$` requests:
`JSON_VALUE` and `JSON_QUERY` each reject the JSON documents `1`, `"text"`
and `null` with error 13609/state 1 and an unexpected character at position
zero. These inputs remain valid for `ISJSON(..., VALUE)`; extraction applies a
different document-root rule.

`msduck_core::json_path::extract` now requires the first non-JSON-whitespace
byte of an extraction document to open an object or array. It parses the path
first, as it did before this change, then checks the root opener. It does not
scan the whole document at that point, because a selected value can return
before an unrelated malformed suffix. The existing `json::root` and ISJSON
rules are unchanged. The core regression reads both retained fixture runs for
the six requests and checks object/array roots and early source-preserving
selection.

This core result is the generic `DOCUMENT` diagnostic. SQL Server's captured
message also names the unexpected character and its offset; exact dynamic text
and wire error ordering still need root-adapter comparison. The capture's
typed descriptors and event trace remain authoritative evidence, not a claim
that this core-only change has achieved full client parity. Wildcard extraction
and other path grammar remain separate gaps.
