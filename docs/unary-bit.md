# BIT unary-minus validation

Ten live captures in `artifacts/compatibility/sql-server-unary-bit.json` establish
error 8117, state 1, severity 16 for BIT negation. They cover CAST, CONVERT,
variables, derived columns, nested unary plus, COALESCE and ISNULL. Statically
known invalid expressions reject the batch before preceding writes, including
unselected IF branches and same-level TRY blocks.

A deterministic unary-operator check now consumes parameter declarations and an
optional column-type resolver. Pure batch preflight applies it to casts and
variables without evaluating initializers. The existing lexical operand binder
applies it to source columns, preserving scope resolution. Its exact diagnostic
is recognized at the root string boundary, and source-binding error 8117 bypasses
same-level CATCH.

Pure tests preserve input ASTs and check typed diagnostics for unexecuted paths.
The native test compares the complete error/DONE bytes, verifies zero inserted
rows, checks derived-column binding and confirms nonexecuting prepared validation.
Formatting, pure SQL tests, the focused native test and strict Clippy passed.
Client and audit probes are added; their execution and full verification are
pending while earlier snapshots run.

Batch-wide catalog binding remains incomplete: a source-column-only error is
recognized when its statement is bound, rather than by the current pure
batch preflight. This change does not establish pre-execution validation of all
catalog-dependent expressions or fix other unary-operator diagnostics.


All 307 workspace library tests pass for the BIT validation source. The saved
pre-change capture `unary-bit-before.json` confirms that a preceding INSERT used
to remain applied when BIT negation returned backend error 50000; its subsequent
COUNT is 1, whereas the live reference is 0. Current full Rust and wire checks
are running after completion of the preceding executable's client suite.


Current focused reference verification is complete: all ten captures in
`unary-bit-after.json` match SQL Server exactly, including the subsequent zero-row
write guard. All 347 workspace Rust tests and seven selected client tests passed;
formatting and strict Clippy passed. The full macOS client suite and 295-case
audit are running for this snapshot. The broader unary-minus comparison now
matches six of eight captures; DECIMAL wire length and minimum-INT overflow
reporting remain separate gaps.


The 295-case macOS audit completed. All 293 preceding captures match the Linux
unary-minus snapshot exactly; the two new BIT probes retain diagnostics and the
write-guard count. `unary-bit-audit-diff.json` preserves this comparison. The full
macOS client suite is still running for this snapshot.
