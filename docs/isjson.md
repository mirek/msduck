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

Remaining work includes engine-specific depth and Unicode edge behavior,
complete noncharacter source typing, exact syntax diagnostic parity and the
captured descriptor flags. Compatibility-level gating for the SQL Server 2022 type constraints is
not implemented. OPENJSON remains unfinished. JSON_VALUE and JSON_QUERY now have a separate
[extraction implementation](json-extraction.md) with its own documented limits.

References:

- [Microsoft ISJSON reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/isjson-transact-sql)
- Copied upstream `packages/engine/src/json.ts` and `json.test.ts` at the revision
  recorded in [reference review](reference-review.md).
