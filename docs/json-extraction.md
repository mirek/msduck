# JSON_VALUE and JSON_QUERY

JSON_VALUE returns scalar text with NVARCHAR(4000) metadata. Strings are decoded;
numbers retain their original spelling and booleans return their literal text.
JSON null returns SQL NULL. JSON_QUERY returns the original object or array
fragment with NVARCHAR(MAX) metadata, retaining internal whitespace and duplicate
keys. Its omitted path defaults to `$`.

Paths support optional lax/strict modes, case-sensitive property names,
double-quoted/escaped keys and zero-based array indexes. A duplicate property
selects its first occurrence. Missing paths or wrong selected types return NULL
in lax mode and report 13608, 13623 or 13624 in strict mode. Invalid document and
path syntax report 13609 and 13607. JSON_VALUE values exceeding 4000 UTF-16 units
return NULL in lax mode and report 13625 in strict mode. NULL input or path
propagates NULL.

The implementation adapts upstream `json.ts` path selection and scalar/fragment
rules in `msduck-core::json_path`, sharing the core's iterative ISJSON scanner.
SQL syntax lowering, DuckDB vectors and diagnostic wrapping remain in root
adapters. OPENJSON imports the same core path helpers. Selection borrows source slices;
numbers and container fragments are never parsed through a floating-point value
or serialized again. `serde_json` decodes selected string values and quoted keys.
Selection validates preceding members/elements and the selected value, but can
return a match before an unrelated malformed suffix. Missing paths and root
extraction validate the entire document. Native extraction diagnostics are mapped once for both wire errors and
TRY/CATCH. Wrong-kind strict errors 13623/13624 retain state 2; the other current
extraction diagnostics retain state 1. DuckDB's scalar-error prefix is removed
from these known messages. Bare rethrow preserves that identity, and explicit
THROW retains its supplied number/state even if its text matches a JSON error.
The native function evaluates document and path once per row. Metadata inference
retains JSON_VALUE's width through views and empty results.

Tests adapt upstream lexical-number, decoded-string, first-duplicate, fragment,
strict-path and UTF-16 width examples. Additional tests cover vector NULLs,
6000-row single evaluation, prepared paths, stored inputs, arithmetic conversion
and empty/view metadata. Additional cases cover malformed text before/after
matches, nested early descent, missing-path validation, prepared error recovery
and 6000-row extraction from invalid documents with valid early matches.

Remaining work:

- Live SQL Server comparison, detailed syntax-error positions and remaining error precedence.
- Exact validation order for wrong-kind matches, truncated ancestors and root scalars.
- Full path grammar, wildcard/range extensions and JSON_VALUE RETURNING.
- Source-collation propagation and noncharacter input coercion.
- Isolated surrogate code units in decoded strings/keys (explicitly unsupported).
- Complete OPENJSON coercion and path support (see [OPENJSON notes](openjson.md)),
  JSON_MODIFY, native JSON storage and FOR JSON integration.

References:

- [Microsoft JSON_VALUE reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/json-value-transact-sql)
- [Microsoft JSON_QUERY reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/json-query-transact-sql)
- [Microsoft JSON path expressions](https://learn.microsoft.com/en-us/sql/relational-databases/json/json-path-expressions-sql-server)
- Copied upstream `packages/engine/src/json.ts` and `json.test.ts` at the revision
  recorded in [reference review](reference-review.md).
