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

Remaining work includes live SQL Server comparison, engine-specific depth and
Unicode edge behavior, exact noncharacter-input coercion and syntax diagnostic
parity. Compatibility-level gating for the SQL Server 2022 type constraints is
not implemented. OPENJSON remains unfinished. JSON_VALUE and JSON_QUERY now have a separate
[extraction implementation](json-extraction.md) with its own documented limits.

References:

- [Microsoft ISJSON reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/isjson-transact-sql)
- Copied upstream `packages/engine/src/json.ts` and `json.test.ts` at the revision
  recorded in [reference review](reference-review.md).
