# JSON extraction array ranges

`reference/json-advanced-path.json` records 142 `JSON_VALUE` and `JSON_QUERY`
requests against the pinned SQL Server 2025 image in each of two fresh
databases. An independent recapture matched the retained rows, typed column
descriptors, diagnostics and ordered events. Each record includes its source
document and path as data, so the core test replays both runs without
interpreting SQL text.

For the captured `$[first to last]` paths, both endpoints are inclusive. A
singleton selects one array element. Multiple selected values return SQL NULL
in lax mode; strict mode reports 13608/state 1 for `JSON_VALUE` and
13624/state 3 for `JSON_QUERY`. Zero selected values return SQL NULL in lax
mode or 13608/state 1 in strict mode. A selected JSON null returns SQL NULL,
including in strict mode and when another branch selects a value. A singleton
with the wrong result kind reports 13623/state 2 for `JSON_VALUE` or
13624/state 2 for `JSON_QUERY` in strict mode. A strict range extending past
the array end reports 13659/state 1 when a non-NULL value was selected.

Singleton ranges can finish before an unrelated malformed suffix. Wider or
missing ranges validate the whole document and report 13609 for malformed
JSON. Reversed ranges report 13660/state 1. SQL Server 2025 also reports
13660/state 2 for `[last]` and `[0 to last]`, and 13660/state 5 for comma
lists such as `[0,1]`. These errors take precedence over document validation
in the captured cases.

The deterministic core now evaluates ranges with source-preserving slices,
UTF-16 path and value handling, and an explicit bounded array traversal.
The fixture test matches the result or diagnostic identity of all 284
observations through both the UTF-8 and UTF-16 entry points. The ordinary
public path parser and `JSON_PATH_EXISTS` still reject ranges.

Remaining differences are recorded rather than hidden:

- SQL Server's 13659 text includes the selected index, source UTF-16
  position and requested end bound; the core's static error contract retains
  the number and state but cannot yet format those dynamic fields.
- Malformed path errors retain a generic 13607/state 1 rather than SQL
  Server's character, offset and state. Malformed document errors retain
  generic 13609 text rather than its character and position.
- This core replay does not establish root adapter, wire descriptor or event
  ordering parity. The raw reference fixture preserves that evidence for
  integration work.

The captured subset does not imply support for the full advanced JSON path
grammar.
