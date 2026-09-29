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
observations through both the UTF-8 and UTF-16 entry points, including the
complete dynamic 13659 message and captured 13607 malformed-path and 13609
malformed-document diagnostics. Structured core errors carry range bounds or
the unexpected character and UTF-16 position to the root adapter. The TDS integration test matches
the captured 13659, 13607 and 13609 errors' numbers, states, classes and messages,
then checks a successful range result and its column type on the same connection. The
ordinary public path parser and `JSON_PATH_EXISTS` still reject ranges.

Remaining differences are recorded rather than hidden:

- The captured malformed-document and malformed-path forms match, but this does
  not establish complete 13609 coverage for other JSON grammar or complete
  13607 coverage for other path grammar. The document cursor is derived only
  after the extraction parser rejects the input; early selected values retain
  the observed behavior even when a later document suffix is malformed.
- The 13659 wire test covers the captured error fields and one successful
  result descriptor. It does not establish full adapter, descriptor or event
  ordering parity for every advanced path. The raw reference fixture
  preserves that evidence for further integration work.

The captured subset does not imply support for the full advanced JSON path
grammar.
