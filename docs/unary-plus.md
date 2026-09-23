# Unary plus

Live SQL Server probes show that unary plus preserves its operand value for
integers, strings, BIT, DATE, DATETIME2, MONEY and DECIMAL, including NULLs.
Column result properties survive unary plus: a non-null derived INT stays fixed
INT with flags 0; nullable input stays IntN with flags 1. Variables and session
counters retain their respective nullability and computed flags.

The SQL AST helper removes unary-plus nodes by moving their operands. The root
translator invokes it before backend-specific lowering, so nonnumeric inputs do
not reach DuckDB's numeric unary operator. Logical result-type inference and
property inference inspect the original operand. No clock, session or database
access enters the helper, and it does not duplicate volatile inputs.

Eight live reference probes are retained in
`artifacts/compatibility/sql-server-unary-plus.json`. Pure tests cover idempotence
and operand preservation. A native test compares values across types, compares
complete result-token streams for nullable and non-null integer columns, and
uses a sequence to verify one evaluation of a volatile input. Client and audit
coverage have been added; full verification of this snapshot remains pending.
Existing literal character-width gaps and unary-minus metadata are separate work.


Current verification: formatting, strict Clippy and all 304 workspace
library tests passed. This includes the native value/token and single-evaluation
tests. The macOS session-counter client suite and Linux result-name verification
are still running against their earlier snapshots; neither validates the new
unary-plus change. Full current client and audit checks remain required.


The current snapshot passed all 344 workspace Rust tests and five focused client
tests. The full client suite and 292-case local audit are running. Typed reference
captures in `sql-server-unary-plus-typed.json` and `unary-plus-typed-after.json`
preserve date objects, including fractional metadata, on both sides of comparison.
These supersede the earlier comparison's JSON date/string representation mismatch.
Five of eight probes now match completely. All eight match rows and execution
completion, with remaining differences confined to unary-minus flags, DECIMAL
wire length, and literal character family/width. Four pre-change probes failed
execution; those failures are preserved in `unary-plus-before.json`.


The 292-case local audit completed. Compared with the preceding Linux naming
snapshot, 290 of 291 existing cases were unchanged. One column in the naming
probe changed from nullable IntN to fixed INT with flags 0, matching the live
reference for unary plus over a non-null column. One unary-plus probe was added.
`unary-plus-audit-diff.json` retains the complete raw comparison; no values or
completion events changed in existing cases. The full macOS client run remains
active.


Full macOS client verification subsequently passed all 359 tests with no failures,
cancellations or skips. Together with formatting, strict Clippy, 344 workspace
Rust tests and the 292-case audit, this closes local verification for the
unary-plus snapshot. Later unary-minus metadata and BIT validation changes are
separate snapshots.
