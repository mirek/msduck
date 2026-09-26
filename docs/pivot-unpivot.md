# PIVOT and UNPIVOT reference contract

The retained fixture in reference/pivot-unpivot.json contains 174 SQL Server
records per capture: 6 setup batches, 156 single-batch programs, 7
`sp_executesql` RPC calls, 4 prepared sequences (10 `sp_execute` calls in
total, plus 1 execution skipped after a failed `sp_prepare`), and a final
session-reusability probe. Each record was captured in two fresh databases in
each of two independent containers. Both containers used the pinned SQL Server
2025 image digest
86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a. All four raw
captures matched exactly. The fixture keeps result rows, TDS column
descriptors, error and information messages (number, state, class, line and
text), DONE/DONEINPROC/DONEPROC tokens with row counts, and return statuses.

scripts/capture-pivot-unpivot.mjs regenerates an artifact (default
`artifacts/pivot-unpivot-reference-v1/capture.json`) and compares it with the
retained fixture through the bounded `assertSameCapture` helper.
Before any container starts, the script rejects an output path that resolves
to the retained fixture. Resolution uses realpath, so it catches symlinked
directories and a file that does not exist yet. `--write-fixture` refuses to
run if the fixture exists; it checks existence only. `--one-database` is a diagnostic mode
that never writes or compares the fixture.

## Capture conventions

The script follows docs/reference-captures.md from PR #312
(`origin/work/prepared-capture-helper-v1`). Because those helpers are not on
main, the script contains its own copies of the batch/RPC request helper,
`capturePrepared` (from 8f2ad7b) and `refuseFixtureOutput` (from 12013d4). It does not change `scripts/lib`.

- The return status that tedious carries on the connection is cleared before
  each request and each prepared step. A status is recorded only when it
  arrives during that request or step.
- `sp_prepare` completes through the `prepared` or `error` event. Each
  execution records only the server errors raised during that execution.
  tedious' sticky `request.error` is cleared before each phase. When
  `sp_prepare` raises an error or returns no positive handle, the executions
  are recorded as `skipped: 'prepare failed'` and no `sp_unprepare` is sent.
- Rows (2000 per result set) and messages/DONE tokens (200 per request) are
  bounded. None of the retained records reached a bound.
- `process.env.TZ = 'UTC'`. There are no clock reads and no server `WHILE`
  loops. All constraints are named (`pk_pv_sales`, `pk_pv_region`,
  `pk_pv_wide`, `df_pv_wide_nn5`).
- Each RPC and prepared statement has unique text (a trailing
  `/*rpc ...*/` or `/*prepared ...*/` comment). No two parameterized cases
  share text.
- Every program that returns more than one row has an ORDER BY that makes the
  order total. Describe queries use
  `sys.dm_exec_describe_first_result_set(...) ORDER BY column_ordinal`, not
  the order of rows returned by the procedure.
- Runs used `node --max-old-space-size=2048` under an RSS watchdog that kills
  above 4 GB. Peak RSS was about 116 MB.

## Source data

`dbo.pv_sales(id, region VARCHAR(10), yr INT, qtr CHAR(2), amount INT,
price DECIMAL(9,2), note NVARCHAR(20), big BIGINT, ratio FLOAT, flag BIT,
day DATE)` has nine rows. The data includes a NULL `qtr` (id 6), a NULL
`region` (id 7), a lowercase `'q1'` (id 8) and NULL `amount`s (ids 3 and 9).
`dbo.pv_wide` holds UNPIVOT inputs of mixed types, a NOT NULL column, a
case-sensitive collation column and the special names `[a b]` and `[x]]y]`.
`dbo.pv_region` supplies join rows. The server collation is
SQL_Latin1_General_CP1_CI_AS.

## PIVOT rules observed

### Aggregates

| Form | Captured behavior |
| --- | --- |
| SUM, AVG, MIN, MAX over INT | INT results. A pivot cell with no matching rows, or only NULL values, is NULL. AVG uses integer division (east Q1 = 14 from 10, 20, 13). |
| COUNT / COUNT_BIG | A cell with no matching rows is 0, not NULL. This includes a missing pivot value and a NULL-only group. The TDS descriptor is still nullable IntN (4 or 8 bytes), and `is_nullable` is 1 in the describe output and in SELECT INTO. |
| STDEV, VAR / STDEVP, VARP | FLOAT. With one row, STDEV/VAR give NULL and STDEVP/VARP give 0. |
| SUM(DECIMAL(9,2)) / AVG(DECIMAL(9,2)) | DECIMAL(38,2) / DECIMAL(38,6), following ordinary aggregate typing. |
| SUM(BIGINT), AVG(FLOAT), MAX(NVARCHAR(20)), MIN(DATE) | BIGINT, FLOAT, NVARCHAR(20) (40-byte descriptor, server collation) and DATE. |
| APPROX_COUNT_DISTINCT | Accepted. Gives BIGINT, with 0 for empty cells, and raises information message 8153. |
| CHECKSUM_AGG | Error 406 (class 16): not invariant to NULLs. |
| MAX(bit), SUM(bit) | Error 8117, the ordinary aggregate operand error. |
| COUNT(*) | Syntax error 102 near `*`. |
| COUNT(DISTINCT ...) | Syntax error 156 near DISTINCT. |
| Aggregate over an expression (`SUM(amount+1)`) | Syntax error 102 near `+`. The argument must be a column. |
| STRING_AGG(note, ',') | Syntax error 102 near `,`. Only a single argument is accepted. |
| Two aggregates | Syntax error 102. |
| ABS(...), GROUPING(...) | Error 195: not a recognized aggregate function. |

SUM, AVG, MIN, MAX, COUNT and the statistical aggregates skip NULL
`amount` values inside a PIVOT without raising information message 8153.
Message 8153 does appear when an outer aggregate or window aggregate is
applied to the pivoted output ('pivot group by output', 'pivot window over
output'). With `SET ANSI_WARNINGS OFF` the rows are the same.

### Grouping, output shape and scoping

- Implicit grouping uses every source column except the pivot column and
  the aggregated column. `SELECT *` over the base table therefore produces
  one row per `id`.
- `SELECT *` column order is the grouping columns in source order, then the
  pivot columns in IN-list order (for example `yr,region,Q2,Q1`).
- Grouping columns keep their source type and nullability, for example
  `id` INT NOT NULL (flags 0). Every pivot column is nullable.
- A NULL grouping value forms its own group (the `region` NULL row).
- With no grouping columns, a non-empty input gives exactly one row. An
  empty input gives **no** row, even with COUNT. This differs from a scalar
  aggregate. An empty grouped input also gives zero rows. Both return the
  full descriptor and `DONE` with row count 0.
- Rows whose pivot value is NULL or not in the IN list contribute to no
  pivot column, but they still create their group (id 6 and id 9 rows give
  COUNT 0).
- The pivot column and the value column are not visible after PIVOT (207).
  The source alias is not visible either (4104). Columns are qualified by the
  PIVOT alias. Omitting the alias is syntax error 156 at the next keyword.
- Using the value column as the pivot column (`SUM(amount) FOR amount IN
  ([10],[20])`) is accepted. The remaining columns become the grouping.
- An unknown pivot or value column gives 207.

### Pivot values (IN list)

- IN list entries must be identifiers: bracketed, double-quoted (with
  QUOTED_IDENTIFIER ON) or regular. String literals, numeric literals and
  variables are syntax errors (102), including a bound RPC parameter.
  Under `SET QUOTED_IDENTIFIER OFF` (inside `EXEC(...)`), `"Q1"` is a string
  and gives 102.
- The identifier text becomes the output column name exactly as written
  after unescaping: `[a]]b]` gives `a]b`, `[Q1 ]` gives `Q1 `, and `[02023]`
  gives `02023`. The name is also the value matched after conversion to the
  pivot column's type:
  - Character matching uses the pivot column's collation and trailing-space
    rules. `[q1]` matches `'Q1'` and `'q1'` under CI. `[Q1 ]` matches `'Q1'`.
    `[Q1x]` never matches CHAR(2) and gives no error.
  - `[NULL]` is the string `'NULL'`. NULL pivot values never match.
  - Numeric, date and bit keys are converted: `[02023]` matches 2023,
    `[20240201]` matches DATE 2024-02-01, and `[0]`/`[1]` match BIT.
    `[1.50]` and `[1.5]` both match 1.50. Distinct names with equal
    converted values are both accepted and receive the same aggregate.
  - A value that cannot be converted (`[abc]` or `[2023.0]` for INT, or
    `[2024-13-01]` for DATE) gives two errors: 8114 (converting nvarchar)
    and then 473 ("The incorrect value ... is supplied in the PIVOT
    operator"). No result descriptor is returned.
- Duplicate output names give 8156 ("specified multiple times for 'p'").
  Names are compared with the database collation and trailing-space
  rules, so `[Q1],[q1]` and `[Q1],[Q1 ]` are duplicates. This holds even
  when the pivot column has a case-sensitive collation.
- A name equal to the pivot or value column gives 265. A name equal to a
  grouping column gives 265 followed by 8156. Duplicate names in the source
  derived table give 8156 for the derived alias and then for the PIVOT
  alias.
- `[]` gives 1038 (state 4, class 15). A 128-character name is accepted.
  A 129-character name gives 103.

### Composition

PIVOT works on a derived table, inside and over a CTE, in a view (with
nullable view columns), in scalar and EXISTS subqueries, in CROSS APPLY
correlated to an outer row, as a join input (inner and left), in
INSERT ... SELECT, in UPDATE ... FROM, in SELECT INTO, with a table hint on
its base-table source, with TOP, and with WHERE/ORDER BY/window functions on
pivoted columns. `FROM a JOIN b ON ... PIVOT (...)` pivots the joined row
set, and the columns of both sides become grouping columns. PIVOT can be
chained: PIVOT then UNPIVOT, UNPIVOT then PIVOT, or a second PIVOT over
PIVOT output, which groups by the earlier pivot columns. PIVOT is allowed in
a recursive CTE anchor. In the recursive member it gives 4190. Dynamic
PIVOT through `STRING_AGG(QUOTENAME(...)) WITHIN GROUP (ORDER BY ...)` and
`sp_executesql` returns the expected row set and a DONEPROC with status 0.

## UNPIVOT rules observed

- Rows whose unpivoted value is NULL are dropped. A source row with every
  listed column NULL disappears. A `CROSS APPLY (VALUES ...)` contrast keeps
  them.
- The name column is NVARCHAR(128): an NVarChar descriptor of 256 bytes
  with the server collation, nullable. SELECT INTO creates a nullable
  `nvarchar(128)` column. Its values are the source column names as stored
  in the catalog, not as written in the IN list: `[Q1]` produces `q1`.
  Escaped names come out unescaped (`a b`, `x]y`).
- The value column has the common source type and is always nullable,
  even when a listed column is NOT NULL (`q1,nn5` gives nullable INT).
- `SELECT *` order is the remaining source columns in source order, then
  the **value** column, then the **name** column.
- All listed columns must have exactly the same type, length, precision,
  scale and collation. INT/SMALLINT, VARCHAR(5)/VARCHAR(10),
  VARCHAR/NVARCHAR, VARCHAR/CHAR, DECIMAL(5,2)/DECIMAL(6,2) and
  VARCHAR(5) with differing collations each give 8167. In every captured
  two-column case the error names the second listed column. CASTs in a derived table resolve
  the conflict, and VARCHAR(MAX) is accepted (65535 TDS length).
- A column listed twice gives 277. A name or value column that clashes with
  an existing column gives 265 then 8156 (for alias `u`). An unknown listed
  column gives 207. Listed source columns are not visible after UNPIVOT
  (207). String literals in the list give 102.
- An empty source returns the descriptor and zero rows. UNPIVOT works in
  CTEs, over joined derived tables, directly after a JOIN, and as a join
  input.

## Parameterized and prepared use

- `sp_executesql` parameters can appear in the PIVOT/UNPIVOT source. The
  pivoted type follows the parameter-dependent expression type:
  `amount*@m` with `@m DECIMAL(5,2)` gives DECIMAL(38,2), and
  `@tag+note` with NVARCHAR(10) gives NVARCHAR(30) (60 bytes). A NULL
  filter parameter returns the descriptor and zero rows. A parameter in the
  IN list gives syntax error 102.
- A failing `sp_executesql` call ends with DONEPROC and a return status
  equal to the error number: 8134 for divide by zero after the descriptor,
  and 102 for the IN-list syntax error.
- Prepared PIVOT and UNPIVOT statements return their descriptor from
  `sp_prepare` (DONEINPROC 0, status 0). Each `sp_execute` returns the
  descriptor and rows for its values. In 'prepared pivot divide', execution 2
  (`@d = 0`) sends the descriptor, then error 8134, then return status -6.
  Execution 3 succeeds with status 0 and no carried error.
- 'prepared pivot invalid' (duplicate IN names) fails in `sp_prepare` with
  8156 then 8180 and return status 8180, and no result descriptor. Following
  the PR #312 skip rule, its execution is recorded as skipped and no
  `sp_unprepare` is sent. An earlier branch-only capture sent them anyway and
  recorded client-caused 8009 and 8179 errors. That capture is not retained.

Completion tokens: successful SELECTs end with DONE and the row count. A
compile-time error ends with DONE and no row count. SELECT INTO, INSERT and
UPDATE batches add a DONE with the affected-row count for each statement.
EXEC and RPC paths use DONEINPROC/DONEPROC as recorded. Describe results
come from the DMF, so they have ordinary DONE row counts.

## Gaps not captured

- DONE status bits (error, count-valid, attention) are not retained. Only
  row counts, the `more` flag and the token kind are.
- Server collations other than SQL_Latin1_General_CP1_CI_AS, database
  compatibility levels other than the image default, and PIVOT over binary, XML, sql_variant,
  CLR or MAX-typed pivot keys.
- UNPIVOT over NCHAR/NVARCHAR MAX, sql_variant, XML or computed columns;
  UNPIVOT name-column behavior with case-sensitive database collations.
- PIVOT/UNPIVOT inside MERGE sources, OUTPUT INTO, triggers, indexed or
  schema-bound views, and table-valued functions.
- Error precedence between PIVOT errors and other errors in one statement,
  apart from the recorded 265/8156, 8114/473 and 8156/8180 pairs.
- Row order without ORDER BY. The fixture deliberately does not record it.
- Execution plans, statistics and PIVOT rewrites to GROUP BY/CASE.
- Whether ANSI_WARNINGS changes arithmetic overflow inside PIVOT
  aggregates. Only the NULL-elimination message was checked.
- What SQL Server does with `sp_execute`/`sp_unprepare` after a failed
  `sp_prepare`. Those calls are skipped.

## Proposed successors

This task does not claim or add any msduck implementation. The existing
recursive-CTE validator already reports 4190 for PIVOT in a recursive
member (see ROADMAP.md). The fixture confirms that number.

1. **Deterministic core** (`msduck-sql`/`msduck-core`, no DuckDB): a
   PIVOT/UNPIVOT binder over an explicit source column snapshot that:
   - derives the grouping columns and the `SELECT *` order, with UNPIVOT
     emitting the value column before the name column;
   - validates aggregate forms (406, 195, the syntax cases, 8117);
   - converts IN identifiers to the pivot column type (8114+473) and
     detects name duplicates with the database collation and trailing-space
     rules (8156), name clashes (265, 265+8156) and name limits (1038, 103);
   - checks exact UNPIVOT type identity (8167), duplicates (277) and hidden
     columns (207);
   - computes result metadata: aggregate typing, nullable pivot columns,
     COUNT cells that are 0 but described as nullable, and a nullable
     NVARCHAR(128) name column.
   Pure tests can use the fixture's descriptors and error sequences.
2. **Root adapter** (`msduck`): lower validated PIVOT to grouped conditional
   aggregation. Keep NULL/unmatched keys out of every cell but keep their
   groups. Return no row for an empty ungrouped input. Do not emit 8153 for
   the pivot's own NULL skips. Lower UNPIVOT so it drops NULL values and
   uses catalog names for the name column. Emit the captured descriptors,
   DONE/DONEPROC tokens and return statuses on the batch, `sp_executesql`
   and prepared paths, including -6 for a failed `sp_execute` and the
   error-number status for `sp_executesql`. Compare these through a
   client test that replays the fixture.

Both successors need their own owner-published tasks. Parser, engine,
catalog, metadata and client-test files were outside this task's scope.
