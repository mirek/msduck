# Deterministic CONCAT_WS and TRANSLATE work

The new `crates/msduck-sql/src/concat_ws.rs` module plans character declarations
from explicit argument declarations and catalog properties, then evaluates
already SQL-converted UTF-16 text. It does not acquire catalogs, execute source
expressions or consult a session/backend. Planning takes no parameter values.
The module remains unregistered under this task's three-file scope; its
integration test includes it by path. GREATEST/LEAST has since merged, so a
separately claimed export task can register the module.

The authority is the complete, independently reproduced SQL Server capture in
[concat-ws-translate.md](concat-ws-translate.md), merged through PR #807. The
blocked original reference task 253 and unclaimed core task 290 remain preserved;
this worker owns the fresh successor 812 and its independent receipt.

Current behavior includes CONCAT_WS arity 189, literal NULL width 0, empty literal
width 1 supplied by the binder, separator width per argument gap, character
family/MAX promotion, an INT conversion width 12, bounded width caps, NULL
skipping and empty-result behavior. TRANSLATE preserves first-mapping precedence
without chaining, counts UTF-16 units or supplementary characters from explicit
catalog properties, propagates NULL, distinguishes error 9828 states 1/3 and keeps
MAX only when its first argument is MAX. Compile diagnostics retain number,
state, severity and text; root adapters still own source locations, wire order
and statement/transaction effects.

Collation labels combine through existing core rules. Unknown, absent or duplicate
catalog entries are barriers. An implicit CONCAT_WS collation conflict also needs
an explicit SELECT projection position to render diagnostic 451; absent statement
context returns `UnknownDiagnosticContext`, rather than inventing column 1.
The position is nonzero and passed through `plan_with_context`. Comparison weights are an explicit matcher input
and may return unknown. The tests provide only comparisons established for the
captured cases; they do not claim to implement all linguistic weights. Numeric,
legacy temporal and GUID formatting remains in separate caller conversion code;
uncaptured per-type formatting widths remain unknown. Unsupported legacy types
and ANSI UTF-8 evaluation do not acquire invented behavior. Character payloads
must respect their declared widths and fixed-width padding. Core CP1252 encoding
validates ANSI payloads; Unicode payloads retain isolated surrogate units.

Eighteen private Rust tests pass using cached, compiler-compatible Linux
dependencies and isolated temporary binaries, with strict Clippy and formatting.
They compare 21 ordinary character cases across all four captures (84 comparisons),
26 declaration/collation cases (104 comparisons), 10 supplementary/UTF-8/mismatch
cases (40 comparisons), 14 character RPC requests (56 comparisons), four column
batches (16 comparisons), four supplementary/explicit integer-text cases (16
comparisons), prepared CONCAT_WS rebindings and 32 prepared TRANSLATE executions.
They also check the 254-argument boundary, predicate inputs, isolated surrogate
fidelity, bounded truncation and MAX behavior, and 13 complete compile diagnostics
across all four captures. Column evaluation retains the three successful rows
before the fourth row's runtime error. Subsequent statement execution and wire
completion ordering remain shell obligations, not claims about this core.

The test reader converts only the four raw `"\ud83d-a"` strings into exact UTF-16
unit carriers because serde_json cannot represent an isolated surrogate in a Rust
String. The fixture remains unchanged; the tests compare original units instead
of replacements. MAX DATALENGTH retains the captured BIGINT string representation.
Unknown catalog entries, duplicate names, unavailable comparison weights, missing
conversion contracts and invalid payloads remain explicit barriers.

Independent review found missing width/encoding validation for converted numeric
payloads. The regression failed on the original checkpoint and passes after every
payload receives those checks. A second review found an implicit-collation error
assumed SELECT column 1 without context; a regression rejects a mutation restoring
that default. Positions 2/17 are rendering tests, not new SQL Server captures.

Noncharacter source formatting is not implemented here. The fixture's aggregate
CONCAT_WS integer-family, decimal/float/money/bit, temporal, GUID and binary cases,
mixed temporal RPC, and TRANSLATE decimal/date/binary source conversions require
separate conversion adapters and independently established declaration widths.
Tests supply already-converted INT text only where its width 12 is established.
This does not certify those other formatters. Server setup, connection reuse,
request execution, comparison/predicate lowering and completion tokens require
root integration. ANSI UTF-8 result evaluation remains explicitly unsupported. A noncharacter
TRANSLATE first operand with an explicit MAX conversion width also returns
`UnknownConversion` until its result shape is established; it must not advertise
8000 and silently truncate. The new VARBINARY(MAX) regression fails on checkpoint
2adc076 and passes with that barrier. Reference task #814 is capturing this
missing rule alongside individual source-format declarations.

Full final-head checks, CI and review must pass before merge. Registration and
runtime binding/execution need separate claimed successors. This deterministic
module does not establish server compatibility.

The owner-pinned mirek/mssqlite dispatcher at
`7f71f2081602f8e3051998f5c11f058e65fe24ec` maps CONCAT_WS to SQLite and TRANSLATE
to a custom backend function. Its AST/backend separation is useful context;
those mappings are not SQL Server ground truth and are not copied as semantics.
