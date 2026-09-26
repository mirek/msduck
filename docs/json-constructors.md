# JSON_OBJECT, JSON_ARRAY and JSON_MODIFY reference contract

`reference/json-constructors.json` retains 247 SQL Server programs per run
(2 setup statements, 226 ordinary batches and 19 RPC `sp_executesql` calls).
The generator ran every program in two fresh databases in each of two
independent containers, all using the pinned SQL Server 2025 image digest
`86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
All four raw captures matched. Each record keeps the query, any RPC parameter
declarations and values, rows, TDS descriptors (type, length, flags,
collation), error number/state/class/line/text, information messages and DONE
tokens. Run `node scripts/capture-json-constructors.mjs OUTPUT` to recapture
and compare with the retained fixture; `--write-fixture` refuses to overwrite
an existing fixture, and `--one-database` is a faster diagnostic probe that
cannot write it.

Values below are quoted exactly as the fixture holds them. Where this page
summarizes, the fixture record named in the table is authoritative.

## Result type and descriptors

| Form | Captured behavior |
| --- | --- |
| `JSON_OBJECT`, `JSON_ARRAY`, `JSON_MODIFY` over character input | TDS `NVarChar` with length 65535 (MAX), flags 33, database default collation (`SQL_Latin1_General_CP1_CI_AS`, sort ID 52). `sp_describe_first_result_set` reports `nvarchar(max)`, `max_length` -1, nullable, for every captured shape including `JSON_OBJECT()`, `JSON_ARRAY()`, a `VARCHAR` value and a `VARCHAR(20)` or `NVARCHAR(MAX)` `JSON_MODIFY` input. `SELECT ... INTO` creates three nullable `nvarchar` columns with `max_length` -1. |
| `JSON_MODIFY` over `VARCHAR` input | Still `NVarChar(MAX)`; a non-ASCII `NVARCHAR` value is kept (`{"a":"xé"}`), and output may exceed the input's declared width (`VARCHAR(10)` input grew to 58 characters). |
| `RETURNING JSON`, or a `JSON`-typed value/input | With Tedious (which does not negotiate the native JSON type), the column arrives as TDS `VarChar` length 65535 with a UTF-8 collation (codepage `utf-8`, sort ID 0, version 2). `sp_describe_first_result_set` reports `varchar(max)` with `Latin1_General_100_BIN2_UTF8`. `JSON_OBJECT('j':CAST(N'{"x":1}' AS JSON))`, `JSON_ARRAY(CAST(N'[1]' AS JSON))` and `JSON_MODIFY(CAST(... AS JSON),...)` take this type without `RETURNING JSON`; the JSON value is embedded unescaped. |
| Width | Results are MAX regardless of inputs: two `NVARCHAR(4000)` values produced a 16030-byte object; a 10000-character value produced 10008 characters (20016 bytes). No truncation or overflow error was observed. |

## JSON_OBJECT

| Input | Captured behavior |
| --- | --- |
| `JSON_OBJECT()` | `{}` |
| Pairs | `key:value` only; `JSON_OBJECT('a',1)` is error 102 state 10. Output has no whitespace, keys in argument order: `{"a":1,"b":"x"}`. |
| NULL value | Default and explicit `NULL ON NULL` emit `"a":null`. `ABSENT ON NULL` omits the pair; all-absent yields `{}`. A NULL `JSON_QUERY` result follows the same rule. Specifying both clauses is error 102 state 20. |
| NULL key | Literal, typed NULL, column or RPC parameter NULL key: error 13638 state 1 class 16 `User error : Name parameter value in 'json_object' cannot be null`, raised after the column descriptor. A table scan emitted the preceding non-null row before the error, then a DONE without a row count. |
| Key typing | Keys are converted to text: `1` → `"1"`, `1.5` → `"1.5"`, `DATE` → `"2024-01-02"`, binary `0x41` → base64 `"QQ=="`, `CHAR(3)` keeps padding `"k  "`, empty key `""` allowed. Keys are escaped like values. |
| Duplicate keys | Kept as written: `{"a":1,"a":2}`; with `ABSENT ON NULL`, `('a':NULL,'a':2)` yields `{"a":2}`. |
| Nested / JSON inputs | `JSON_OBJECT`, `JSON_ARRAY`, `JSON_QUERY` and `JSON_MODIFY` results embed as JSON (`{"q":{"x":1}}`). A plain string with JSON text is escaped as a string (`{"q":"{\"x\":1}"}`); `JSON_VALUE` returns a string (`{"q":"1"}`). `JSON_QUERY(N'not json')` fails with 13609 state 1 before any descriptor. |
| Syntax | Missing value or trailing comma: error 102 state 1. |

## JSON_ARRAY

| Input | Captured behavior |
| --- | --- |
| `JSON_ARRAY()` | `[]` |
| NULL element | Default and explicit `ABSENT ON NULL` drop it (`[1,2]`, all-null `[]`); `NULL ON NULL` emits `null` (`[1,null,2]`). Note the default differs from `JSON_OBJECT`. |
| Nesting | `[[1,2],{"a":1},[]]`; `JSON_QUERY` embeds; a JSON-looking string is escaped (`["[1,2]"]`). |
| Trailing comma | Error 102 state 1. |

## Scalar families (same in JSON_OBJECT values and JSON_ARRAY elements)

| Family | Captured text |
| --- | --- |
| `TINYINT`/`SMALLINT`/`INT`/`BIGINT` | Bare decimal integer: `255`, `-32768`, `-2147483648`, `9223372036854775807` (no precision loss). |
| `BIT` | `true` / `false` |
| `DECIMAL`/`NUMERIC` | Bare number keeping declared scale: `-12.340`, `1.50`, `0.00001`, `12345678901234567890`. |
| `MONEY`/`SMALLMONEY` | Bare number with four decimals: `12.3456`, `-1.5000`. |
| `FLOAT` | Bare 16-significant-digit scientific: `1.000000000000000e-001`, `1.000000000000000e+300`, `3.000000000000000e+000`, `-0.000000000000000e+000`. |
| `REAL` | `1.0000000e-001` |
| `DATE`/`TIME`/`DATETIME`/`SMALLDATETIME`/`DATETIME2`/`DATETIMEOFFSET` | Quoted ISO 8601 with `T` separator and declared fractional scale: `"2024-01-02"`, `"03:04:05.1234567"`, `"2024-01-02T03:04:05.123"`, `"2024-01-02T03:04:00"`, `"2024-01-02T03:04:05"` (scale 0), `"2024-01-02T03:04:05.1+05:30"`. |
| `UNIQUEIDENTIFIER` | Quoted upper-case `"6F9619FF-8B86-D011-B42D-00C04FC964FF"`. |
| `CHAR`/`NCHAR` | Quoted with trailing padding kept: `"ab   "`. `VARCHAR`, `NVARCHAR` and MAX forms are quoted unchanged. |
| `BINARY`/`VARBINARY`/MAX/`ROWVERSION` | Quoted base64: `"AQI="`, `"QUJD"`, `"AAAAAAAAB9E="`. |
| `XML` | Quoted string with `/` escaped: `"<a>1<\/a>"`. |
| `SQL_VARIANT` holding `INT` | Bare `1`. |
| `HIERARCHYID`, `GEOMETRY` (CLR) | Error 13666 after the descriptor: state 2 `json_object and json_objectagg does not support CLR type as parameters`; state 3 `json_array does not support CLR type as parameters`. |

String escaping (keys, values and `JSON_MODIFY` values alike): `"` → `\"`,
`\` → `\\`, `/` → `\/`, TAB/LF/CR/FF/BS → `\t` `\n` `\r` `\f` `\b`, other
U+0001–U+001F → `\u0001`…`\u001f` (lower-case hex). U+007F, `é`, U+2028 and a
supplementary character (surrogate pair) were emitted unescaped. A `VARCHAR`
value with `CHAR(233)` produced `é`.

## JSON_MODIFY

| Input | Captured behavior |
| --- | --- |
| Replace | `$.a` with `2` → `{"a":2}`; a string value is quoted; `strict` on an existing key behaves the same. Original whitespace is preserved around untouched and replaced tokens (`{ "a" : 2 ,  "b" : [ 1 ] }`). |
| Insert | Default and explicit `lax` add a missing key at the end (`{"a":1,"b":2}`). `strict` on a missing key: error 13608 state 2 `Property cannot be found on the specified JSON path.` |
| NULL value | `lax`: deletes the key (`{"b":2}`); deleting a missing key returns input unchanged. `strict`: sets `null` (`{"a":null}`); missing key is 13608. Array element `$.arr[1]` with NULL sets `null` (`[1,null,3]`); `append` with NULL appends `null`. |
| Append | `append $.arr` appends (`[1,2,3]`); missing array under lax (also spelled `append lax`) creates `"arr":[3]`; `append strict` missing: 13608; appending to a non-array under lax returns input unchanged, under strict: 13621 state 1 `Array cannot be found in the specified JSON path.` |
| Array index | `$.arr[1]` replaces; out-of-range lax returns unchanged; strict: 13608. `$[0]` on a root array works (`["x",2]`). |
| Nested path | Existing nested key updates. Missing parent or path through a scalar: lax returns unchanged, strict 13608. |
| Quoted keys | `$."a b"` matches; `$."a\"b"` inserts a key containing a quote, emitted as `{"a\"b":2}`. |
| Duplicate keys | Replace updates only the first occurrence (`{"a":3,"a":2}`); lax NULL removes only the first (`{"a":2}`). |
| JSON values | `JSON_QUERY`, `JSON_OBJECT`, `JSON_ARRAY` results embed as JSON; plain JSON-looking text is escaped as a string. |
| Value typing | `INT`, `BIGINT`, `BIT` (`true`), `DECIMAL` (`-12.340`), `FLOAT` (`1.000000000000000e-001`) are accepted. `MONEY`, `DATE`, `DATETIME2`, `UNIQUEIDENTIFIER`, `VARBINARY` and `XML` fail with 8116 state 1 `Argument data type <type> is invalid for argument 3 of json_modify function.` before any descriptor — unlike the constructors, which accept them. |
| NULL input | Typed or untyped NULL expression returns a NULL row. |
| NULL path | Literal `NULL`: 8116 state 1 (argument 2, lower-case `json_modify`) at compile time. Typed NULL or RPC NULL path: 8116 state 8, upper-case `JSON_MODIFY`, after the descriptor. |
| Invalid JSON | 13609 state 7 after the descriptor, e.g. `Unexpected character 'n' is found at position 0.`; empty string and truncated input report character `'.'` (positions 0 and 6); scalar `1` is rejected. |
| Invalid path | 13607 (`JSON path is not properly formatted.`) with state 22 for missing `$` or unknown mode (`loose`), 14 for empty path or trailing dot, 21 for non-numeric or negative index; `$` alone: 13619 state 1 `Unsupported JSON path found in argument 2 of JSON_MODIFY.`; `$.*`: 13660 state 4 `JsonModify not yet supported for advanced JSON array accessors.` |
| Argument types/arity | Integer input or integer path: 8116 state 1 (arguments 1, 2). Two arguments: 174 state 1 class 15 `The json_modify function requires 3 argument(s).` Variable and concatenated-expression paths are accepted. |
| Table use | Per-row application and `UPDATE ... SET doc=JSON_MODIFY(doc,'append $.list',id)` are retained, including a NULL-document row. |

## Diagnostics and completions

Compile-time failures (syntax 102, arity 174, argument-type 8116 state 1,
constant `JSON_QUERY` 13609 state 1) produce no column descriptor and a DONE
without a row count. Runtime failures (13607, 13608, 13609 state 7, 13619,
13621, 13638, 13660, 13666, 8116 state 8) arrive after the result descriptor,
with zero or more preceding rows, followed by a DONE without a row count.
Successful batches end with DONE carrying the row count. RPC calls end with
`doneInProc` (row count, more) and `doneProc`; failing RPC calls carry only
`doneProc`. Bound values follow the same value rules: `NVARCHAR` string `a"b`
→ `"a\"b"`, `DECIMAL(10,3)` 1.5 → `1.500`, `BIGINT` 9007199254740993 exact,
`FLOAT` 0.5 → `5.000000000000000e-001`, `DATETIME2(3)` → `"2024-01-02T03:04:05.123"`,
and a replayed `JSON_MODIFY` call gave the same result.

## Not captured

- Native JSON type on the wire: Tedious receives `RETURNING JSON` and
  `JSON`-typed results as `varchar(max)` UTF-8; a client negotiating the JSON
  feature may see a different descriptor.
- `JSON_OBJECTAGG`, `JSON_ARRAYAGG`, `JSON_CONTAINS`, `JSON` type methods
  (`.modify`), `FOR JSON` interplay, and compatibility-level differences.
- Collation variants of the result beyond the database default, and very large
  (over 2 GB) documents.
- Non-default `SET` options (for example `ANSI_WARNINGS OFF`) and `TRY_`-style
  error suppression; error-line numbers are retained but not analyzed.
- Ordering stability of keys produced by `JSON_MODIFY` on documents with
  unusual whitespace beyond the single captured case.

## Proposed successors

msduck does not implement these functions; this task only retains evidence.

1. **Deterministic core** (`json-constructors-core-v1`, scope `msduck-core`
   JSON value module and its tests): scalar-family-to-JSON text formatting
   (integers, `BIT`, decimal/money scale, 16/7-digit scientific float/real,
   ISO date/time forms, base64 binary, padded character types), string
   escaping, `NULL ON NULL`/`ABSENT ON NULL` with the differing defaults,
   duplicate-key preservation, JSON-vs-string embedding by source kind, the
   `JSON_MODIFY` path grammar (lax/strict/append, quoted keys, indexes) with
   the captured error numbers and states, lax/strict insert/delete/append
   semantics, whitespace-preserving replacement, and the `nvarchar(max)` /
   JSON-type result declaration rules. Tests replay this fixture's values
   without DuckDB.
2. **SQL layer and root integration** (`json-constructors-root-v1`, scope
   `msduck-sql` parsing/binding of `key:value`, `NULL|ABSENT ON NULL` and
   `RETURNING JSON`, argument-type checks at compile time; root adapter
   lowering and client tests): evaluate operands once per row, route
   compile-time errors before descriptors and runtime errors after them,
   emit the captured `NVarChar(MAX)` descriptors, collations and DONE/RPC
   completion tokens, and replay this fixture through Tedious before
   claiming compatibility. DuckDB's `json_object`/`json_array` spelling is
   not evidence that these SQL Server rules hold.

## Implementation: deterministic core rules

`crates/msduck-sql/src/json_constructor.rs` (task `json-constructors-core-v1`)
implements the rules above over already-evaluated operands. It depends only on
`std` and `msduck_core` (`json_escape`, `json`, `json_path`, `for_json::base64`,
`money`, `value::Decimal`, `datetime2`, `datetimeoffset`), has no DuckDB, I/O
or clock access, and is not registered in `lib.rs` yet: its integration test
includes it with `#[path]`. Parsing, binding and root wiring remain the
`json-constructors-root-v1` successor.

| API | Behavior |
| --- | --- |
| `object(pairs, clause)`, `array(elements, clause)` | Render `Scalar` operands with the scalar-family table, escape keys and strings with `json_escape`, embed `Scalar::Json` sources unescaped, keep duplicate keys and argument order, and apply `NULL ON NULL`/`ABSENT ON NULL`. A NULL key is runtime 13638 state 1; a CLR operand is runtime 13666 state 2 (object) or 3 (array). |
| `null_clause(constructor, written)` | Defaults (`NULL ON NULL` for objects, `ABSENT ON NULL` for arrays) and the compile-time 102 state 20 for both object clauses. |
| `constructor_result(returning_json, argument_types)` | `ResultType::Json` for `RETURNING JSON` or a `JSON`-typed argument, otherwise `NVarCharMax`; `system_type_name()` and `fixed_collation()` give the described type and the UTF-8 collation. |
| `modify_signature(types)` | Compile-time arity 174 and argument-type 8116 state 1 checks for the captured types, and the result type (`Json` for JSON input). |
| `modify(input, path, value)` | NULL input returns NULL; NULL path is runtime 8116 state 8. The path grammar (`append`, `lax`, `strict`, `$`, `.key`, `."quoted"`, `[n]`) reports 13607 states 14/21/22 with positions, 13619 for `$` and 13660 for `.*`. Documents must have an object or array root; failures are 13609 state 7 at the root token or at the end of truncated input. Edits splice the original text, so untouched whitespace is kept: replace the first matching key, lax NULL deletes the first match, strict NULL writes `null`, lax insert appends a member, `append` extends or (lax) creates an array, and missing targets are 13608 state 2 (strict) or unchanged (lax); appending to a non-array under strict is 13621 state 1. |

`Error::Compile` errors precede the result descriptor and `Error::Runtime`
errors follow it, matching the capture's timing. `crates/msduck-sql/tests/json_constructors.rs`
replays all four retained runs: per run, 229 cases compare exact text, NULLs,
row counts, descriptor type/length/collation family and error
number/state/class/message; 11 cases (DATALENGTH/LEN, ISJSON,
`sp_describe_first_result_set`, `SELECT ... INTO` and the post-UPDATE SELECT)
are checked with case-specific assertions; 7 cases are not applicable (two
setup statements, four parser syntax errors, and `JSON_QUERY`'s own 13609
state 1).

These return `Error::Unsupported` instead of guessing: object keys other than
integers, decimals, `DATE`, binary and character types; `SQL_VARIANT` holding
anything but `INT`; non-ASCII `CHAR` padding; invalid `Scalar::Json` text;
repeated or array-conflicting ON NULL clauses; JSON_MODIFY types, arities, path
spellings (upper-case or misplaced modes, other key characters, `[*]`, leading
zeros, indexes beyond `INT`) and error positions after a mode or in non-ASCII
text; document errors away from the root token or the end of input, or after
leading whitespace; a document that is invalid together with an invalid path
(precedence was not captured); key steps on arrays and index steps on objects;
strict NULL array elements and lax NULL out-of-range elements; and `append`
through a scalar or missing parent, or creating an array with NULL. Text is
handled as Rust strings, so isolated UTF-16 surrogates are not represented;
whitespace around deleted or appended members follows a simple splice rule
beyond the single captured whitespace case.
