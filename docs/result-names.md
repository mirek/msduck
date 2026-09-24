# Result column names

Logical projection fields now retain SQL result labels before backend lowering.
The root adapter uses these names when the logical and physical result widths
agree. Unknown projections retain the backend fallback instead of guessing
column positions. This reuses the existing field contract and catalog snapshot;
it adds no database or session dependency to the SQL crate.

Unnamed expressions, scalar subqueries and variables have empty labels. Explicit
aliases retain their spelling and duplicates. Column references, parentheses and
unary plus retain the written column name. Stars expand in source order, and set
operations take names from their left member. CTE name validation shares the
same syntax rule, including unary plus.

Nine live reference probes are retained in
`artifacts/compatibility/sql-server-result-names.json`, paired with
`result-names-after.json`. All names match, and seven captures match completely.
Two captures preserve other metadata differences: a literal NVARCHAR width and
unary-plus INT nullability. The Linux focused test run passed all four selected
client tests, covering names, counters, DECLARE and temporal CHOOSE. Pure SQL
tests also pass, including immutable ASTs, star ordering and set-member labels.
Full Linux formatting, Clippy, workspace, client and 291-case audit verification
is running. This does not establish full SQL Server compatibility.


Full Linux verification completed with formatting, strict Clippy, 342 Rust tests,
358 client/harness tests and 291 audit cases. Against the preceding 290-case
macOS capture, 284 cases were unchanged; six changed only column labels (31
labels total). Thirty changed labels match the retained full reference corpus.
The remaining label belongs to the newer DECLARE probe, whose focused live
comparison is exact after this fix. Reports are retained in
`artifacts/remote/linux.local/result-names-audit-diff.json` and
`result-names-reference-review.json`. No row or completion-event differences
occurred in this cross-platform comparison. Later unary operator changes have
separate verification runs.
