# Percentile ordering declaration policy

`crates/msduck-sql/src/percentile_order_type.rs` stages the deterministic
source-type rules captured in `reference/percentile-order-type.json` and
documented in `docs/percentile-order-type.md`. It consumes an explicit logical
source type and optional caller-resolved collation label; it never reads catalog,
session, transport, environment or parameter values and never evaluates an operand.

For eligible numeric CONT sources the result is FLOAT(53). DISC retains the
source declaration, including character width, decimal precision/scale and
temporal scale; character results retain the supplied collation label. Every
known successful percentile expression is logically nullable, independently of
row values or emptiness. Missing source declarations return unknown. Uncaptured
TEXT, NTEXT and IMAGE eligibility returns an explicit unsupported result.

Ineligible CONT sources return the captured 402/state 1/severity 16 identity
and source-specific message, including VARCHAR(MAX) and NVARCHAR(MAX). DISC XML
returns the captured 305/state 1/severity 16 sort rejection. This declaration
policy does not fabricate prepared 8180 sequencing or wire tokens: preparation
and execution adapters must emit the captured phases independently.

The path-imported integration test checks all 192 applicable request declarations
and their preparation/execution descriptors or binding errors in each of the two
retained runs, including empty/all-NULL input and rebound parameters. It compares
logical types and captured metadata, not SQL storage width with Tedious's
DecimalN length field. Character collation resolution remains caller-owned;
this module preserves labels and does not infer weights or validate names.
Client DECIMAL/temporal/variant decoding limits remain described in the reference
note and do not become claims of exact arithmetic or raw wire equality.

This module is deliberately not registered or wired into runtime paths because
those files are reserved by other workers. Export and integration must bind
ordering types using explicit catalog snapshots, preserve unknown barriers,
apply the declaration result without evaluating volatile operands again and
emit binding diagnostics even for empty input. Successful prepared descriptors
must precede value bindings and stay stable across NULL/non-NULL executions.
The existing complete replay remains the baseline for runtime descriptor/error
and token differences. Staging this policy does not change server behavior or
complete percentile compatibility.
