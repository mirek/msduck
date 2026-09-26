# CHARINDEX and PATINDEX reference contract

The retained fixture in reference/charindex-patindex.json contains 211 SQL
Server programs: 4 setup statements, 179 ordinary batches, 23 sp_executesql
RPC calls, 4 sp_prepare/sp_execute/sp_unprepare sequences (19 executions in
total) and a final connection reuse check. Each program was captured in two
fresh databases in each of two independent containers. Both containers used
the pinned SQL Server 2025 image digest
86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a
(ProductMajorVersion 17, server and database collation
SQL_Latin1_General_CP1_CI_AS). The four raw captures matched exactly. They
retain result rows, TDS column descriptors, error number/state/class/text,
information messages and DONE tokens. scripts/capture-charindex-patindex.mjs
regenerates an artifact, checks it against the retained fixture, and refuses
to overwrite an existing fixture.

Values are one-based positions. BIGINT results appear in the fixture as the
decimal strings tedious returns for BIGINT. The rules below are exactly what
the fixture shows. They are not claims about untested inputs. msduck does
not use them at runtime yet; the deterministic core rules are described in
the Implementation section at the end.

## Result declarations

| Input | Captured descriptor |
| --- | --- |
| Non-MAX search expression (CHAR, VARCHAR, NVARCHAR, TEXT, NTEXT, BINARY, or converted INT/DECIMAL/DATETIME/UNIQUEIDENTIFIER) | IntN with length 4 (INT) |
| VARCHAR(MAX), NVARCHAR(MAX) or VARBINARY(MAX) search expression | IntN with length 8 (BIGINT), including NULL MAX input |
| MAX find expression (CHARINDEX) or MAX pattern (PATINDEX) with a non-MAX search expression | INT. Only the searched expression selects BIGINT. |
| BIGINT start with non-MAX search | INT |
| Parameters (RPC and prepared) | Same family rule. An NVARCHAR(MAX) or VARCHAR(MAX) parameter gives BIGINT. |

Every captured result column has flags 33 and no collation, including
expressions over NOT NULL columns. sys.dm_exec_describe_first_result_set
reports int/bigint with is_nullable 1 for the same forms. The sp_prepare
response carries the same descriptor and a DONEINPROC with count 0.

## CHARINDEX values

| Input | Captured behavior |
| --- | --- |
| NULL find, search or start (typed or untyped) | NULL. CHARINDEX(NULL,NULL) and CHARINDEX(NULL,NULL,NULL) also return NULL with an INT descriptor. |
| Empty find | 0, including with a start and with an empty search. Empty search with non-empty find is also 0. |
| Start 0 or negative (INT or BIGINT) | Searches from position 1. |
| Start beyond the length, including 2147483647 | 0 |
| DECIMAL start | Truncated: 2.1, 2.5 and 2.9 all behave as 2. 3000000000.0 gives error 8115. |
| BIGINT start, non-MAX search | Values within INT work. 3000000000 and -3000000000 (CAST, literal, RPC or prepared) give error 8115 state 2 after the INT column descriptor, with no row and a DONE without a count. |
| BIGINT start, MAX search | 3000000000 gives 0 and -3000000000 searches from position 1 (no overflow). |
| SMALLINT and TINYINT start | Accepted. |
| FLOAT, MONEY, BIT, DATE or VARCHAR (including '2') start | Error 8116 state 1 before any metadata: "Argument data type ... is invalid for argument 3 of charindex function." |
| Trailing spaces | Significant in the find expression: CHARINDEX('a ','a') is 0 for VARCHAR and NVARCHAR. Spaces in the search expression are found: CHAR(10) padding matches ' ' at position 3. |
| Mixed VARCHAR/NVARCHAR arguments | Accepted. |
| MAX search beyond 8000 bytes | Positions past 8000 (9001, 8999) and NVARCHAR past 4000 characters (5001) are returned. |
| INT/DECIMAL/DATETIME/UNIQUEIDENTIFIER search | Converted to text: '3' in 12345 is 3, '.' in 12.50 is 3, '2024' in DATETIME '2024-01-02' is 8, '-' in a GUID is 9. |
| INT find | Error 8116 for argument 1. RPC with an NVARCHAR find and INT search returned 3. |
| BINARY/VARBINARY | Byte search is accepted: 0x62 in 0x616263 is 2. VARBINARY(MAX) gives BIGINT. |
| TEXT/NTEXT search | Accepted, INT result. |
| XML search | Error 257 state 3 (implicit conversion from xml to varchar). |
| Two or four arguments | Error 189 class 15: "The charindex function requires 2 to 3 arguments." |

## PATINDEX values

| Input | Captured behavior |
| --- | --- |
| No wildcard | The whole search must match: PATINDEX('abc','abc') is 1 and PATINDEX('b','abc') is 0. 'a%' is 1, '%c' is 3, '%b' on 'abc' is 0. |
| '%' pattern | 1, including on an empty search. |
| Empty pattern | 0 on non-empty search, 1 on empty search. A pattern with characters on an empty search is 0. |
| NULL pattern | NULL. |
| Untyped NULL search | Error 8116: "Argument data type NULL is invalid for argument 2 of patindex function." A typed NULL search returns NULL. |
| _ and multiple % | '%b_d%' is 2 and '%b%d%' is 2 on 'abcd'. |
| Classes | [c-e], [xyz], [^a], [^a-c] and [0-9] match as ranges/sets. [%], [_], [[] and [ ] match literally. An unbracketed ] is literal (3). [a^], [-a] and [a-] match ^ or - literally. |
| Unmatched forms | '[]]', '[]', reversed range '[z-a]' and an unclosed '[a' return 0, without error. |
| Escapes | PATINDEX has no ESCAPE clause (syntax error 156 class 15). Backslash is not an escape. |
| Trailing spaces | VARCHAR search trailing spaces are ignored: '%b' on 'ab  ' is 2, and on CHAR(10) 'ab' is 2. NVARCHAR search trailing spaces are not ignored: N'%b' on N'ab  ' is 0 (batch, RPC and prepared). Pattern trailing spaces are significant: 'ab ' on 'ab' is 0, while '%b ' on 'ab ' is 2. |
| INT pattern | Accepted: PATINDEX(3,'3') is 1. |
| INT, VARBINARY or XML search | Error 8116 for argument 2. |
| TEXT/NTEXT search | Accepted, INT result. |
| Three arguments or one argument | Error 174 class 15: "The patindex function requires 2 argument(s)." |
| MAX search beyond 8000 bytes | 9001 as BIGINT. |

## Collations

Both functions use the collation of the arguments. The captured default is
case-insensitive and accent-sensitive.

| Case | CHARINDEX | PATINDEX |
| --- | --- | --- |
| 'B' in 'abc', default | 2 | 2 |
| Latin1_General_CS_AS on either argument | 0 | 0 |
| e in café, default / Latin1_General_CI_AI | 0 / 4 | 0 / 4 |
| Latin1_General_BIN2 | 'B' in 'abcB' is 4; e in cafée is 5 | [A-C] on 'xbB' is 3; [a-f] on xé is 0 |
| Range [A-C] on 'xb', CS_AS | n/a | 2. The CS range still includes lowercase b. |
| Range [a-c] on 'xB', SQL_Latin1_General_CP1_CS_AS | n/a | 2 |
| Range [a-f] on xé, default | n/a | 2 |
| 'ss' in straße (NVARCHAR), default / BIN2 | 5 / 0 | |
| 'ab' in a + soft hyphen + b, default / BIN2 | 0 / 0 | |
| Explicit conflicting collations | Error 468 state 9: "Cannot resolve the collation conflict ... in the charindex operation." | Same, "in the patindex operation." |
| UTF-8 VARCHAR (Latin1_General_100_CI_AS_SC_UTF8), 'b' in éb | 2 (character position, not byte) | |

## Supplementary characters

U+1F600 was built from NCHAR(0xD83D)+NCHAR(0xDE00), or passed as a JavaScript
string for RPC and prepared parameters.

| Case | Default collation | Latin1_General_100_CI_AS_SC | Latin1_General_BIN2 |
| --- | --- | --- | --- |
| CHARINDEX(N'b', emoji+N'b') | 3 (UTF-16 code units; also RPC) | 2 (code points) | |
| CHARINDEX(emoji, N'a'+emoji) | 0 | 2 | |
| CHARINDEX(NCHAR(0xD83D), N'a'+emoji) | 0 | 0 | |
| CHARINDEX(NCHAR(0xDE00), N'a'+emoji) | | | 3 |
| CHARINDEX(N'b', emoji+N'b', 2) | | 2 | |
| PATINDEX(N'%b%', emoji+N'b') | 3 | 2 | |
| PATINDEX(N'_b', emoji+N'b') | 0 (also RPC and prepared) | 1 | |
| PATINDEX(N'__b', emoji+N'b') | 0 | 0 | |
| PATINDEX(N'%'+emoji+N'%', N'a'+emoji) | 1 | | |

The fixture does not explain the default-collation results where the pair is
not found by CHARINDEX but PATINDEX('%'+emoji+'%') returns 1, or where
'__b' does not match two code units. An implementation must copy these
results, not infer them from UTF-16 or code point counting.

## Protocol shape

Successful batches end with DONE count 1. RPC and prepared executions end
with DONEINPROC count 1 followed by DONEPROC. Type errors (8116, 257, 468,
189, 174, 156) occur before any column metadata. Overflow error 8115 occurs
after the column descriptor and before a row. After it, a batch has a DONE
without a count and RPC/prepared calls have a DONEINPROC without a count and
a DONEPROC. A prepared handle survives an 8115 execution: the following execution
returns its own 8115, and sp_unprepare succeeds. Column-sourced queries over rows with
NULL, empty and nonpositive-start values match the scalar rules. The session
remains reusable (@@TRANCOUNT 0) after all programs.

## Not captured

- Other server or database default collations. Only the collations named
  above were used: no _KS/_WS/_VSS variants, non-Latin collations, or
  UTF-8 NVARCHAR interaction beyond one case.
- PATINDEX or CHARINDEX find/pattern expressions longer than 8000 bytes,
  MAX find values past 8000 bytes, and patterns built from columns with
  differing collations.
- SET ANSI_PADDING, ANSI_NULLS or other session options. Every program ran
  with tedious defaults.
- Collation precedence between parameters and columns, beyond one COLLATE
  on a parameter (CS_AS, returning 0).
- Computed columns, constraints, indexes, WHERE sargability and plan
  behavior. Only one WHERE filter per function was captured.
- sp_describe_first_result_set and sp_describe_undeclared_parameters for
  parameterized forms. Only the DMV over literal/column forms was captured.
- Attention/cancel during long MAX searches, and performance.

## Proposed successors

1. **charindex-patindex-core-v1** (deterministic core). Scope: a new
   msduck-core module and its unit tests. It should hold:
   - Result family selection: INT, or BIGINT when the searched argument is
     a MAX type.
   - Argument type admissibility and the error numbers for each position
     (8116, 257, 189, 174).
   - Start normalization: nonpositive to 1, DECIMAL truncation, and INT
     overflow 8115 for non-MAX searches only.
   - Empty/NULL results.
   - The trailing-space asymmetry between VARCHAR and NVARCHAR PATINDEX
     searches.
   - A PATINDEX pattern matcher covering the captured bracket and
     unmatched-bracket rules.

   Comparisons should go through an explicit collation input: BIN2 code
   unit comparison, and SC versus non-SC counting. Collation-sensitive
   equality for case/accent, expansions (ß=ss) and ignorables may start as
   explicit unsupported results for collations whose weights are not
   modelled. Unit vectors can come from this fixture.
2. **charindex-patindex-sql-v1** (msduck-sql binding). Scope: expression
   metadata and projection inference for both functions over explicit
   catalog snapshots. It should cover:
   - The INT/BIGINT descriptor from the bound search type.
   - The arity and type errors before execution.
   - Collation conflict error 468 from the bound collations.

   It should reject the ESCAPE form at parse time.
3. **charindex-patindex-root-v1** (root integration). Scope: backend
   lowering in the root crate, and root-side wire tests against this
   fixture for batch, sp_executesql and sp_prepare/sp_execute. It should:
   - Call the core evaluator once per row, passing collations and evaluated
     values, instead of DuckDB instr/strpos/LIKE, whose semantics differ.
   - Emit the captured descriptor (flags 33).
   - Keep the overflow error after metadata with the captured DONE shapes.

   Shared parser, engine, catalog, metadata and client-test files were
   outside this reference task's scope and were not changed.

## Implementation (charindex-patindex-core-v1)

`crates/msduck-core/src/charindex.rs` holds the deterministic rules. It uses
only std and is not yet registered in `lib.rs`. Its integration test
`crates/msduck-core/tests/charindex_patindex.rs` includes the file with
`#[path]`. Registration, SQL binding and root wiring are separate successors.

Evaluation has two stages:

- `resolve_charindex` and `resolve_patindex` take the bound `ArgType` of each
  argument. They return either a signature, or a `Rejection` raised before
  any metadata. The rejection is error 189 or 174 for arity, error 8116 for
  the argument position, or error 257 for an XML search with a VARCHAR find.
  Arity is checked first. If several arguments are invalid, the result is
  unsupported, because the fixture only rejects one argument at a time.
  `result_type()` is BIGINT only for a VARCHAR(MAX), NVARCHAR(MAX) or
  VARBINARY(MAX) searched expression.
- `Charindex::evaluate` and `Patindex::evaluate` evaluate one row. They take
  NULL-able UTF-16 or byte operands and one resolved `Collation`, and return
  `Evaluation::Value`, `Evaluation::Error` (8115 after the descriptor) or
  `Evaluation::Unsupported`.

The caller does the following:

- Resolves collation precedence and conflicts (error 468).
- Rejects the ESCAPE syntax (error 156).
- Decodes non-Unicode text from its code page.
- Converts INT, DECIMAL, DATETIME and UNIQUEIDENTIFIER search values, and an
  INT pattern, to their VARCHAR text.

A start value is `Start::Integer` or `Start::Decimal` (unscaled value and
scale):

- For a non-MAX search, the start is converted to INT, truncating a DECIMAL,
  and out-of-range values give 8115.
- For a MAX search, an integer start stays BIGINT.
- A nonpositive start searches from position 1.
- A NULL operand gives NULL.
- An empty find gives 0.

The core also reports these uncaptured orders as unsupported:

- An overflowing start together with a NULL or empty operand.
- A DECIMAL start with a MAX search.

PATINDEX follows these rules:

- It parses `%`, `_`, literals and bracket classes.
- A closing bracket always ends a class, so `[]` never matches.
- `^` negates a class only in first position.
- A leading or trailing `-` in a class is literal.
- An unclosed `[` never matches.
- The result is the first position where the pattern, less its leading `%`,
  matches the rest of the search. A pattern made only of `%` returns 1.
- A VARCHAR or CHAR search with a VARCHAR or INT pattern may end its match
  before trailing spaces.
- An NVARCHAR search with an NVARCHAR pattern keeps trailing spaces
  significant.
- A search with trailing spaces under any other type combination is
  unsupported.

`Collation::from_name` recognizes these names, ignoring ASCII case like the
other msduck-core collation identity comparisons, and returns `None` for any
other. The fixture spells every collation in canonical case.

- `SQL_Latin1_General_CP1_{CI|CS}_{AS|AI}`
- `Latin1_General[_100]_{CI|CS}_{AS|AI}`
- `Latin1_General_100_{CI|CS}_{AS|AI}_SC[_UTF8]`
- `Latin1_General[_100]_BIN2`

Rejected names include kana-sensitive, width-sensitive and
variation-selector-sensitive collations, `_BIN`, and other languages.

Comparison and counting work as follows:

- BIN2 compares UTF-16 code units. Range order is modelled for Unicode
  operands without surrogates and for ASCII non-Unicode operands.
- `_SC` counts positions and `_` in code points. Other collations count
  UTF-16 units.
- Linguistic collations model three groups of characters:
  - Printable ASCII.
  - Latin-1 letters that are an ASCII base letter with one diacritic.
  - U+1F600, the one captured supplementary character, under `_SC` only.
- Within those groups, letters compare by base letter, accent and case, as
  the collation's CI/CS and AI/AS flags say. Other distinct characters are
  unequal.
- Range order is modelled only for digit and letter primary differences.

These cases return unsupported:

- Any operand character outside the modelled groups, for example ß
  expansion, soft hyphen, full-width forms, controls and unpaired
  surrogates under `_SC`.
- Every surrogate under a non-`_SC` linguistic collation, which covers the
  unexplained default-collation results.
- Case or accent ties inside a range.
- Symbol range order.
- Uncaptured argument types.
- Mixed binary and character operands.
- The class forms `[^]`, an unclosed `[^`, a dash after a range, and a dash
  range endpoint.

The test replays all four retained runs. In each run it covers the
following:

- 197 exact comparisons of batch, sp_executesql and prepared-execution
  results. Each compares the descriptor length (INT or BIGINT), the value,
  or the error number, state, class and message.
- 13 captured results that must return unsupported:
  - Eight batch surrogate cases: three CHARINDEX and four PATINDEX cases
    under the default collation, and the lone high surrogate under `_SC`.
  - The ß expansion and soft hyphen.
  - The surrogate cases over sp_executesql and sp_prepare.
- 7 column-sourced, WHERE and describe-first-result-set checks.

It skips 9 programs: setup, environment, reuse, the two 468 collation
conflicts and the ESCAPE syntax error. They belong to the SQL binding
successor. The test also checks that every captured case name is mapped.
