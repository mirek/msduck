# Scalar DECLARE completion behavior

Live SQL Server 2025 captures distinguish initialized and uninitialized scalar
declarations. A statement containing at least one initializer sets @@ROWCOUNT to
1 and emits one counted completion, regardless of the number of declarations.
An explicit NULL initializer and a scalar subquery returning no rows still count
as initialization. NOCOUNT hides the completion count without changing
@@ROWCOUNT.

A statement without initializers preserves the previous @@ROWCOUNT and emits no
intermediate completion. A batch containing only uninitialized declarations still
ends with a count-free DONE; a trailing declaration lets the preceding completion
terminate the batch.

The engine now distinguishes these cases after successful execution. Errors still
follow the existing error path, and initializer evaluation retains the variable's
logical declaration. This is a scalar declaration change; table variables and
cursor declarations are not implemented by this rule.

Evidence is retained in `artifacts/compatibility/sql-server-declare-count.json`
and `sql-server-declare-count-extra.json`. The second capture contains fourteen
queries including standalone and trailing declarations. The upstream TypeScript
project's scalar declaration handler was inspected; it returns no completion
item, so it does not supply the needed SQL Server distinction directly.

All fourteen live-reference pairs now match result values and completion events.
Two standalone declaration pairs match completely. Other pairs retain unrelated
metadata differences: nullable @@ROWCOUNT descriptors and an unnamed UNION
expression label. Raw differences remain in `declare-count-after.json`.

Formatting, strict Clippy and all 341 workspace Rust tests passed for the DECLARE
change. Five focused client tests pass, including raw completion sequences,
total affected rows, preserved @@ROWCOUNT, NULL/subquery initialization, multiple
declarations, NOCOUNT and the subsequent temporal CHOOSE correction. The updated
289-case audit is running; full client verification for this snapshot remains
pending. Full SQL Server compatibility remains open.


The 289-case DECLARE audit completed. Of 288 previous cases, 279 were unchanged;
seven changed declaration/completion counts, one corrected temporal CHOOSE flags,
and one unordered APPLY query reversed two rows. Raw changes are retained in
`declare-count-audit-diff.json`. Where prior live reference captures exist, the
changed declaration counts and temporal flags now match. The nested FOR JSON
total count remains different (4 locally versus 6 on the reference).
`declare-count-reference-review.json` preserves positional comparisons, including
the unordered row differences; no sorting or normalization was applied.


After adding explicit session-counter declarations, twelve of the fourteen
DECLARE pairs match the reference completely. The other two differ only in the
unnamed UNION expression label (`1` locally versus an empty name). The refreshed
`declare-count-after.json` retains these raw differences. @@ROWCOUNT wire metadata
now matches the reference, including non-null fixed INT encoding.


The session-counter snapshot containing this DECLARE change completed all 357
macOS client/harness tests with no failures or skips and all 290 audit cases.
Its formatting, strict Clippy and 341 Rust tests had already passed. Later result
label and unary-plus work is verified separately.


After logical result labels and unary-plus work, all fourteen DECLARE captures
match the live reference exactly, including names, metadata, rows and completion
events. `declare-count-after.json` was refreshed against the current executable.
This is focused reference evidence; the current snapshot's full client/audit
verification remains in progress.
