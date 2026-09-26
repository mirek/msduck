# CONCAT_WS and TRANSLATE reference contract

The retained fixture in reference/concat-ws-translate.json contains 116 SQL
Server programs, each captured in two fresh databases in each of two
independent containers. Both containers used the pinned SQL Server 2025 image
digest 86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a
(ProductVersion 17.0.4065.4, server and database collation
SQL_Latin1_General_CP1_CI_AS). The four raw captures matched exactly. They
retain result rows, TDS column descriptors (type, length, flags, collation),
error number/state/class/line/text, information messages, DONE tokens and RPC
return status.

scripts/capture-concat-ws-translate.mjs regenerates an artifact and compares it
with the retained fixture. `--write-fixture` refuses to run when the fixture
exists, and the fixture is opened exclusively. `--check-fixture` revalidates the
retained file offline: that the four captures are identical and that the key
assertions hold. `--one-database` is a diagnostic mode only.

The programs cover 49 ordinary CONCAT_WS batches, 45 ordinary TRANSLATE
batches, 15 sp_executesql RPC calls, three prepared statements (sp_prepare,
several sp_execute on one handle, sp_unprepare) and setup/version/reuse probes.
All descriptor lengths below are TDS byte lengths as reported by tedious; 65535
denotes MAX.

## CONCAT_WS observed rules

| Input | Captured behavior |
| --- | --- |
| Argument count | 3 to 254 arguments including the separator. `CONCAT_WS()`, `CONCAT_WS(NULL)`, `CONCAT_WS(',','a')`, 255 and 256 arguments all fail with error 189 state 1 class 15, "The concat_ws function requires 3 to 254 arguments.", with no result metadata. 254 arguments succeed. |
| NULL arguments | Skipped without an extra separator (`'a',NULL,'b'` gives `a,b`). Typed NULLs of VARCHAR, NVARCHAR and INT are skipped the same way. |
| All arguments NULL | Returns the empty string, not NULL, with or without a NULL separator. Column rows whose values are all NULL also return the empty string. |
| NULL separator | Literal NULL, typed NULL, NULL column value and NULL RPC/prepared parameter all concatenate without a separator (`ab`). |
| Empty strings | Empty-string arguments are kept and separated (`'a','','b',''` gives `a,,b,`). An empty separator concatenates directly. |
| Spaces | Leading/trailing spaces in values and separators are preserved. CHAR(3) and NCHAR(2) arguments keep their padding (`a  |b `). |
| Nullability | Every CONCAT_WS descriptor has flags 32 (not nullable), including column and parameter inputs. |
| Result family | VARCHAR when all string-typed inputs are VARCHAR/CHAR; NVARCHAR as soon as the separator or any argument is NVARCHAR/NCHAR. Numeric, date/time, uniqueidentifier and binary arguments with VARCHAR separators produce VARCHAR. |
| Bounded width | Width is the sum of argument widths plus one separator width per gap (arguments minus one). Examples: `'-'`,VARCHAR(10),VARCHAR(20) gives VarChar 31; VARCHAR(7) separator with 10/20/30 gives 74; NVARCHAR 10/20 with N'-' gives NVarChar 62 bytes (31 characters). A NULL literal contributes width 0 (`',','a',NULL,'b'` is 4; `NULL,'a','b'` is 2); `''` contributes 1. INT contributes 12 (`CONCAT_WS(0,'a','b')` is 14). |
| Width cap | Bounded widths cap at VARCHAR(8000) / NVARCHAR(4000) (8000 bytes). The value is silently truncated at the cap without an error or message: two VARCHAR(5000) values of 5000 characters give LEN 8000 / DATALENGTH 8000; two NVARCHAR(3000) values give LEN 4000 / DATALENGTH 8000. |
| MAX inputs | A VARCHAR(MAX) or NVARCHAR(MAX) argument, or a VARCHAR(MAX) separator, produces a MAX descriptor (65535). A MAX result holding 10001 bytes was not truncated. |
| Other type widths | Observed totals only (per-type widths were not isolated): five integers of INT/TINYINT/SMALLINT/BIGINT with `','` give VarChar 62; DECIMAL(8,2), FLOAT, REAL, MONEY, BIT with `'|'` give 132; six date/time types give 245; a uniqueidentifier with `'x'` gives 42. |
| Value formatting | DECIMAL keeps scale (`1.50`), FLOAT and REAL `1.5`, MONEY `2.50`, BIT `1`. DATE `2024-01-02`; DATETIME and SMALLDATETIME use the legacy `Jan  2 2024  3:04AM` form; DATETIME2 `2024-01-02 03:04:05.1234567`; TIME(2) `03:04:05.12`; DATETIMEOFFSET(0) `2024-01-02 03:04:05 +01:30`; uniqueidentifier uppercase. The binary 0x4142 is reinterpreted as the characters `AB`, not hex text. |
| Rejected types | SQL_VARIANT and XML arguments fail with error 257 state 3 class 16 ("Implicit conversion from data type ... to varchar is not allowed...") before metadata. An integer separator is accepted and formatted (`a0b`). |
| Collation | An explicit COLLATE on one argument becomes the result collation (Latin1_General_100_BIN2 descriptor, flags 34). Two different explicit collations fail with 468 state 9 ("... in the concat_ws operation."). Two columns with different implicit collations fail with 451 state 1 ("... in concat_ws operator occurring in SELECT statement column 1."). |
| Supplementary characters | Under the default (non-SC) collation, supplementary characters count as two UTF-16 units: `N'😀'` separator with N'a', N'𝄞' gives NVarChar 10 bytes, value `a😀𝄞`, LEN 5. A lone high surrogate (NCHAR(55357)) is preserved unchanged (DATALENGTH 6). N'😀' with a VARCHAR separator yields NVarChar. |
| Predicates | CONCAT_WS in WHERE compares like any string expression (row 1 matched `N'a,u'`). |

RPC and prepared CONCAT_WS derive widths from declared parameter lengths:
NVARCHAR(5) separator with NVARCHAR(10) and NVARCHAR(20) gives NVarChar 70
bytes; the VARCHAR variant gives VarChar 35. A tedious NVarChar parameter
without an explicit length (declared from its one-character value) gave 52
bytes, and NVARCHAR(MAX) (length 5000) gave 65535. VARCHAR(1), NVARCHAR(10),
INT and DATE parameters gave NVarChar 128. Prepared execution reuses the
sp_prepare descriptor (NVarChar 70) for every execution on the handle,
including NULL separators, all-NULL arguments (`''`) and supplementary
characters. RPC completions are DONEINPROC (more=true) then DONEPROC with
return status 0; sp_prepare reports DONEINPROC with row count 0 and the result
descriptor before any execution.

## TRANSLATE observed rules

| Input | Captured behavior |
| --- | --- |
| Argument count | Exactly three. Two or four arguments fail with error 174 state 1 class 15, "The translate function requires 3 argument(s).", before metadata. |
| NULL | Any NULL argument (literal, typed, column value or parameter) returns NULL. |
| Nullability | Every TRANSLATE descriptor has flags 33 (nullable), or 35 with an explicit case-sensitive or binary collation. |
| Result family and width | Bounded inputs always give VARCHAR(8000) or NVARCHAR(4000) (8000 bytes), independent of the declared input width (VARCHAR(10) input gives VarChar 8000). The result is NVARCHAR when any of the three arguments is NVARCHAR (VARCHAR input with N'' characters, NVARCHAR input with VARCHAR characters). A MAX first argument gives MAX (65535); a MAX second argument with a bounded input does not. VARCHAR(MAX) with 9000 characters and NVARCHAR(MAX) with 9000 characters were translated in full (DATALENGTH 9000 and 18000). |
| Mapping | Characters map position by position without chaining (`'abc','ab','bc'` gives `bcc`). With a repeated source character the first mapping wins (`'aab','aa','xy'` gives `xxb`). Empty second and third arguments return the input unchanged; an empty input returns the empty string. |
| Length mismatch | Unequal character counts fail with error 9828 class 16, "The second and third arguments of the TRANSLATE built-in function must contain an equal number of characters." The result descriptor is sent first, no rows follow, and the batch DONE has no row count. State is 1 for VARCHAR evaluations and 3 for NVARCHAR evaluations. Trailing spaces count (`'a '` against `'x'` fails; `'x_'` succeeds). Deletion is not supported: `TRANSLATE(CAST(1.50 AS DECIMAL(8,2)),'.','')` fails with 9828. |
| Row-dependent mismatch | With a column as the second argument, rows 1 to 3 (`axb+c`, NULL, `a-bxc`) were streamed and then 9828 state 3 was raised for the row with an empty separator; the DONE carried no row count. |
| Collation | Matching follows the input collation. The default CI_AS collation matched case-insensitively (`'ABCabc','abc','xyz'` gives `xyzxyz`) but accent-sensitively (`N'eéE',N'e',N'x'` gives `xéx`). Latin1_General_100_BIN2 and Latin1_General_100_CS_AS gave `ABCxyz` and carried that collation on the descriptor. Conflicting explicit collations fail with 468 state 9 ("... in the translate operation."). |
| Padding and conversions | CHAR(5) input keeps padding and the pad spaces translate (`'ab   '` with `'b '`/`'x_'` gives `ax___`). INT input is converted to text (`92329`), DATE input to `2024-01-02` text, and binary 0x4142 to characters (`BB`); all as VarChar 8000. An INT third argument fails with 8116 state 1, "Argument data type int is invalid for argument 3 of translate function." |
| Supplementary characters | Under the default non-SC collation `N'😀'` counts as two characters: mapping it to `N'xy'` gives `axxb`; mapping it to `N'x'` fails with 9828 state 3. Under Latin1_General_100_CI_AS_SC it counts as one: `N'x'` gives `axb`, `N'xy'` fails with 9828 state 3, and replacing `b` with `N'😀'` gives `a😀c`. A lone high surrogate is translated as one unit under the default collation. |
| UTF-8 collation | A VARCHAR value collated Latin1_General_100_CI_AS_SC_UTF8 with NVARCHAR characters gives NVarChar 8000 with that collation (`aéb` to `aeb`). |

RPC and prepared TRANSLATE keep the same family and width rules (NVARCHAR or
VARCHAR 8000 bytes; NVARCHAR(MAX) parameter gives 65535). A runtime length
mismatch under sp_executesql sends the descriptor, error 9828 (state 3 for
NVARCHAR), DONEINPROC without a row count (more=true) and DONEPROC with return
status 9828. A following statement in the same RPC batch still runs and
returns its row, and the final return status is 0. Under sp_execute on a
prepared handle the same mismatch reports return status -6 (state 3 for
NVARCHAR, state 1 for VARCHAR parameters), and later executions on the same
handle succeed. sp_unprepare completes normally afterward.

## Not captured

- Per-type widths of each numeric, date/time and uniqueidentifier argument in
  CONCAT_WS; only the observed totals are retained.
- CONCAT_WS with a MAX separator and bounded arguments beyond 8000 bytes, or
  with NVARCHAR(MAX) values longer than 4000 characters.
- Non-default database collations, SC/UTF-8 collations as the database
  default, and CONCAT_WS with SC collations.
- TRANSLATE and CONCAT_WS inside computed columns, check constraints, views,
  indexes and CASE/UNION type resolution. Descriptors for derived table
  columns and SELECT INTO storage types were also not captured.
- TDS behavior when a client cancels during a row-dependent 9828 stream.
- Behavior on other SQL Server versions or compatibility levels.

## Proposed successors

These are proposals only. This capture task did not change msduck, and nothing
here claims msduck implements these functions.

1. **Deterministic core (`concat-ws-translate-core`).** Scope: new rule modules
   in `msduck-core` (and, if needed, `msduck-sql` expression metadata) with unit
   tests driven by reference/concat-ws-translate.json. Rules: argument-count
   validation (189, 174); CONCAT_WS result family, width summation with the NULL
   literal and `''` widths, the 8000-byte cap, MAX propagation, non-nullable
   flags and silent truncation; TRANSLATE family, fixed 8000-byte or MAX width,
   nullable flags; NULL propagation; position mapping with first-wins
   duplicates and no chaining; collation-aware matching (CI/CS/BIN2, accent
   sensitivity); UTF-16 unit versus SC code-point counting; 9828 state 1/3
   selection; collation conflict errors 468/451; rejected argument types (257,
   8116) and legacy value formatting for non-string arguments. No DuckDB,
   sessions or I/O.
2. **Root integration (`concat-ws-translate-root`).** Scope: root-crate lowering
   of CONCAT_WS and TRANSLATE to the core rules or equivalent DuckDB
   expressions, binding of column and parameter declarations and collations,
   emission of the captured descriptors, and runtime error ordering (descriptor,
   partial rows, 9828, DONE without row count, RPC return status 9828 for
   sp_executesql and -6 for sp_execute, batch continuation). Add a root-side
   comparison test against this fixture for ordinary, sp_executesql and
   prepared paths. This task needs parser, engine and metadata file scope,
   which this capture task did not have.
