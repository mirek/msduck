# SELECT TOP PERCENT and WITH TIES

`SELECT TOP (n) PERCENT` and `SELECT TOP (n) [PERCENT] WITH TIES` now execute.
This replaces the "unsupported TOP PERCENT/WITH TIES" rejection that
[TOP in set operations](top-set-operations.md) and the
[reference notes](select-top-percent-reference.md) describe. Plain `TOP (n)`
still lowers to the existing validated `LIMIT`.

## Lowering

`msduck_sql::top::ranked` rewrites the query AST before backend translation and
has no database, clock or session inputs. It keeps the SELECT and adds a
`QUALIFY` filter, which DuckDB applies after `GROUP BY`, `HAVING` and window
evaluation:

- Without ties, the filter is `row_number() OVER (ORDER BY keys) <= limit`.
  With ties it uses `rank()`. A row's rank does not exceed the count exactly
  when it comes before the last counted row or ties with it on every ORDER BY key.
- For PERCENT, the limit is `ceil(p * count(*) OVER () / 100)`, where `count(*)`
  counts the rows TOP sees. This is Microsoft's documented rounding up, and it
  matches every captured boundary: 14.285% of 7 rows is 1, 14.286% is 2.
- The percentage is converted to `DOUBLE` and checked in an uncorrelated scalar
  subquery. The TOP expression is therefore evaluated once per statement. NULL
  raises 1014 and values outside 0–100 raise 1031 at run time, for example with
  a variable or RPC parameter.
- A WITH TIES count without PERCENT passes through the existing native count
  validator, also inside a scalar subquery.
- ORDER BY keys are copied into the window. As SQL Server resolves them, a
  select-list alias or ordinal refers to the projected expression, and any other
  key refers to the source columns. The query's own ORDER BY is left as written.
  Without an ORDER BY, the window orders by a constant `NULL`, so TOP can still
  return any rows.
- `SELECT DISTINCT` must remove duplicates before TOP counts rows. The DISTINCT
  select becomes a derived table named `__msduck_top_distinct`. The filter and
  final ORDER BY then refer to that table's output column names, and any
  `SELECT INTO` target moves to the outer select.

## Diagnostics

Constant arguments are checked before execution. These errors have state 1 and
class 15, as captured:

| Condition | Error |
| --- | --- |
| WITH TIES without ORDER BY | 1062 |
| PERCENT constant below 0 or above 100 | 1031 |
| NULL percentage | 1014 |
| Negative WITH TIES count | 127 |
| NULL WITH TIES count | 1060 |
| DISTINCT ORDER BY key not in the select list | 145 |

The runtime 1014 message used by plain TOP and FETCH now also has class 15.
Before, it was class 16 with DuckDB's `Invalid Input Error:` prefix.

## Verification

`crates/msduck-sql/tests/select_top_percent.rs` replays every compile-time
diagnostic in both retained runs of `reference/select-top-percent.json`. It also
checks the lowered SQL for single evaluation, alias and ordinal resolution,
DISTINCT wrapping and unchanged plain TOP.

The tedious test "SELECT TOP PERCENT and WITH TIES replay the retained SQL Server
capture" runs all 50 captured cases. The direct batches use the same `capture()`
helper as the reference script. The two RPC executions use a FLOAT parameter.
The four prepared executions reuse one handle with 25, NULL, 50 and 25. The test
compares the complete canonical result with both retained runs: rows, column
descriptors, errors, info messages, DONE tokens, return status and row counts.
It sorts rows only within equal-score groups for tie cases, as the capture
script does. Each run must match on 48 cases. The two retained differences are
listed exactly in the test:

- `server identity`: msduck reports its own version and collation.
- `text percent`: SQL Server sends the column metadata and then error 8114
  (state 5, class 16, "Error converting data type varchar to float."). msduck
  reports DuckDB's conversion error before sending any metadata.

The prepared NULL percentage matches SQL Server: it returns error 1014 after
the two-column metadata token, and reusing the handle then succeeds.

## Remaining gaps

- Converting character percentages to float (the `text percent` difference above).
- WITH TIES counts that are fractional or of another type use backend integer
  coercion. SQL Server's 1060 applies to MERGE fractional counts; SELECT was not
  captured. Runtime negative and NULL WITH TIES counts raise 1014, which was not
  captured either.
- Plain TOP still raises 1014 for negative and NULL constants. SQL Server's
  WITH TIES captures show 127 and 1060 for those constants, but plain TOP was
  not captured, so its behavior is unchanged.
- DISTINCT with TOP PERCENT/WITH TIES is explicitly unsupported when an ORDER BY
  key names an unnamed expression, when output names repeat, or when the select
  list has a wildcard. Non-DISTINCT ordinals that point at a wildcard are also
  rejected explicitly.
- Percentages are computed in double precision. Every captured boundary is
  matched, but SQL Server's internal arithmetic at other extreme precisions has
  not been compared.
- A volatile ORDER BY key such as `NEWID()` is evaluated once for the TOP window
  and again for the final sort. The selected rows are still a valid TOP result,
  but their output order is independent of the ranking.
- DML `TOP` (INSERT/UPDATE/DELETE) and collation-sensitive tie comparison of
  character keys are not covered.
