# Unary-minus result properties

Live SQL Server probes distinguish negative numeric literals from runtime
negation. Numeric literals retain non-null computed metadata (flags 32), including
parenthesized literals. Negated columns, variables, counters and numeric casts
are nullable computed expressions (flags 33), even when the source is non-null.
This rule is now explicit in deterministic result-property inference.

Eight typed live captures are retained in
`artifacts/compatibility/sql-server-unary-minus.json`, with before/after results
in `unary-minus-before.json` and `unary-minus-after.json`. Five captures match
completely. Remaining differences are DECIMAL wire length, BIT operand rejection
(number 50000 instead of 8117), and minimum-INT overflow diagnostics and metadata
emission. These differences are preserved; the property change does not fix
operator validation or execution-time error reporting.

Formatting, pure SQL tests and strict Clippy passed. Both unary-plus and
unary-minus focused Linux client tests passed. Full Linux verification, including
the 293-case audit, is running for this snapshot. The concurrent macOS run covers
the preceding unary-plus snapshot and cannot validate the unary-minus change.


The later BIT validation change now makes six of the eight captures match
completely. `unary-minus-after.json` was refreshed from that macOS executable.
The remaining captures differ in DECIMAL wire length and minimum-INT overflow
metadata/error reporting; BIT rejection now has exact reference diagnostics.


Full Linux verification completed for the unary-minus metadata snapshot with
formatting, strict Clippy, 345 Rust tests, 360 client tests and 293 audit cases.
Compared with the preceding 292-case capture, 290 cases were unchanged; two
changed only computed flags, and one new probe was added. Raw differences remain
in `unary-minus-audit-diff.json`. This snapshot precedes BIT rejection and decimal
wire changes.
