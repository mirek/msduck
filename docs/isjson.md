# ISJSON

ISJSON returns INT 1 or 0 for valid/invalid JSON text and propagates SQL NULL.
The default form accepts object and array roots. The optional VALUE constraint
accepts every JSON root; ARRAY and OBJECT select their respective roots; SCALAR
accepts only numbers and strings. Boolean and JSON null literals are VALUE roots
but are not SCALAR roots. Duplicate object keys are accepted.

The scanner adapts lexical rules from upstream
`packages/engine/src/json.ts`: JSON whitespace, quoted keys/strings, escapes,
number grammar, delimiters and full-document consumption. Unlike that recursive
implementation, this validator uses an explicit grammar stack. It does not
convert numbers to machine numeric values, decode strings or allocate a tree.
It rejects comments, trailing commas, control characters in strings, invalid
escapes, leading-zero numbers, NaN/Infinity and trailing documents.

The native function reads bounded string vectors, writes INT results, handles
NULLs and evaluates the input once. AST lowering consumes the type keyword as
syntax and preserves full text length. Integer inference permits use in normal
arithmetic, conditions and CHECK constraints.

Tests reuse upstream's default-root and malformed-document cases, extend them
with all four type constraints, duplicates, large exponents, long Unicode input,
20000-level nesting, prepared calls, keyword/column collisions, empty metadata,
CHECK constraints and 6000-row single-evaluation tests.

The owner-run pinned SQL Server 2025 capture in
`reference/isjson-coercion.json` records 44 default/VALUE requests twice in
fresh databases. It retains rows, result descriptors, errors and DONE tokens.
`scripts/capture-isjson-coercion.mjs` regenerates the capture and compares all
records against the retained fixture. SQL Server accepts character text and
untyped NULL; it rejects INT, BIT, DECIMAL, FLOAT, DATE, DATETIME2,
UNIQUEIDENTIFIER, VARBINARY, XML and TEXT with error 8116/state 1/class 16
before result metadata, including typed numeric and binary NULLs. The message
names the rejected source type. An explicit binary-to-VARCHAR cast is accepted.

The root adapter now binds ISJSON over the source's actual type, so it cannot
silently stringify rejected inputs. Direct XML and TEXT casts are rejected
before DuckDB lowers those types, matching their captured error ordering. The
shared diagnostic adapter maps the exact binder message to 8116 without its
DuckDB wrapper. A standalone client replay in `tests/isjson_coercion.test.mjs`
compares this evidence, preserving known descriptor differences instead of
treating them as a pass. SQL Server's computed INT descriptor has flags 33
where msduck currently emits flags 1; the result-metadata path is reserved by
another claim. Wider XML/TEXT declarations and aliases remain unverified.

`reference/isjson-source-types.json` adds 84 owner-run SQL Server 2025 requests
observed twice in fresh databases. TINYINT, SMALLINT, BIGINT, REAL, MONEY,
SMALLMONEY, TIME, DATETIME, SMALLDATETIME and DATETIMEOFFSET values, typed NULLs
and VALUES source columns all fail with error 8116/state 1/class 16 and the
original source type name before metadata. VARCHAR/NVARCHAR source columns
return 1 or NULL and retain computed INT flags 33. The standalone replay in
`tests/isjson_source_types.test.mjs` retains each raw response and reports the
known flags difference. Native REAL is distinguished from FLOAT, and direct
MONEY/SMALLMONEY/DATETIME/SMALLDATETIME casts preserve their declaration name
at preflight. Those four types' VALUES source columns currently reach ISJSON
after backend lowering has erased their logical type. Their error messages are
therefore recorded as explicit remaining differences, not treated as parity.
For direct casts, SQL Server resolves a missing source column first (207), but
rejects a validly bound cast to these four types with 8116 even if its value
would fail conversion. The adapter keeps the cast operand in the binder path
and rejects it before runtime evaluation. Msduck's shared unresolved-column
diagnostic still reports generic 50000 for these missing-column inputs; the
replay asserts and reports that exact remaining number and message difference.

`reference/isjson-depth.json` retains 110 owner-run SQL Server 2025 ISJSON RPC
requests, repeated in two fresh databases with identical raw rows, descriptors,
diagnostics and completion tokens. Both array and object inputs with a scalar
at nesting level 128 return 1; level 129 raises error 13606/state 1/class 16
after the INT descriptor and before a row. An empty innermost container is
accepted at level 129 but errors at level 130. Invalid syntax encountered before
a valid deep value returns 0, whereas a valid value beyond the limit raises
13606 even when a closing delimiter is missing or trailing text follows. Raw
isolated UTF-16 surrogates, escaped surrogates, a raw supplementary pair, U+2028
and U+FEFF inside a JSON string are accepted. Raw NUL inside a string, U+FEFF
before the document and non-JSON whitespace before the document return 0.

The deterministic core now exposes `json::isjson` and `json::isjson_utf16` with
a distinct depth result, and its fixture-backed test covers every capture. The
older unrestricted `prefix`, `root`, `valid` and `valid_utf16` APIs remain for
other JSON consumers whose depth/error behavior has not been established.
The root ISJSON native adapter now calls the depth-aware functions and emits
the captured runtime error. The diagnostic adapter recognizes only the exact
native error text and restores SQL Server error 13606/state 1/class 16. The
standalone tedious replay compares all 110 raw responses, including the 40
depth errors and their descriptor and completion ordering. Only the already
known INT descriptor flags difference (1 versus SQL Server's 33) remains in
that capture.

Remaining work includes complete noncharacter source typing through
column/alias binding, exact syntax diagnostic parity and the
captured descriptor flags. Compatibility-level gating for the SQL Server 2022 type constraints is
not implemented. OPENJSON remains unfinished. JSON_VALUE and JSON_QUERY now have a separate
[extraction implementation](json-extraction.md) with its own documented limits.

References:

- [Microsoft ISJSON reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/isjson-transact-sql)
- Copied upstream `packages/engine/src/json.ts` and `json.test.ts` at the revision
  recorded in [reference review](reference-review.md).
