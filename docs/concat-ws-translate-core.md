# Deterministic CONCAT_WS and TRANSLATE work

The new `crates/msduck-sql/src/concat_ws.rs` module plans character declarations
from explicit argument declarations and catalog properties, then evaluates
already SQL-converted UTF-16 text. It does not acquire catalogs, execute source
expressions or consult a session/backend. Planning takes no parameter values.
The module remains unregistered under this task's three-file scope; its
integration test includes it by path. GREATEST/LEAST has since merged, so a
separately claimed export task can register the module.

The authority is the complete, independently reproduced SQL Server capture in
[concat-ws-translate.md](concat-ws-translate.md), merged through PR807. The
blocked original reference task253 and unclaimed core task290 remain preserved;
this worker owns the fresh successor812 and its independent receipt.

Current behavior includes CONCAT_WS arity189, literal NULL width0, empty literal
width1 supplied by the binder, separator width per argument gap, character
family/MAX promotion, an INT conversion width12, bounded width caps, NULL
skipping and empty-result behavior. TRANSLATE preserves first-mapping precedence
without chaining, counts UTF-16 units or supplementary characters from explicit
catalog properties, propagates NULL, distinguishes error9828 states1/3 and keeps
MAX only when its first argument is MAX. Compile diagnostics retain number,
state, severity and text; root adapters still own source locations, wire order
and statement/transaction effects.

Collation labels combine through existing core rules. Unknown, absent or duplicate
catalog entries are barriers. An implicit CONCAT_WS collation conflict also needs
an explicit SELECT projection position to render diagnostic451; absent statement
context returns `UnknownDiagnosticContext`, rather than inventing column1.
The position is nonzero and passed through `plan_with_context`. Comparison weights are an explicit matcher input
and may return unknown. The tests provide only comparisons established for the
captured cases; they do not claim to implement all linguistic weights. Numeric,
legacy temporal and GUID formatting remains in separate caller conversion code;
uncaptured per-type formatting widths remain unknown. Unsupported legacy types
and ANSI UTF-8 evaluation do not acquire invented behavior. Character payloads
must respect their declared widths and fixed-width padding. Core CP1252 encoding
validates ANSI payloads; Unicode payloads retain isolated surrogate units.

Twelve private Rust tests currently pass using cached, compiler-compatible Linux
dependencies and isolated temporary binaries. They cover21 ordinary character
cases across all four captures (84 comparisons), another26 declaration and
collation cases (104 comparisons), another10 supplementary/UTF-8/mismatch cases
(40 comparisons), prepared CONCAT_WS rebindings and32 prepared
TRANSLATE executions, the254-argument CONCAT_WS boundary, isolated surrogate
fidelity, captured bounded truncation/MAX behavior and ten compile diagnostic
cases in every capture. The new declaration cases retain typed NULL widths,
fixed-width padding, mixed families, MAX-first versus MAX-mapping distinctions,
and raw collation descriptors. Prepared TRANSLATE checks preserve descriptors
through NULL bindings, exact9828 errors and subsequent successful reuse. The test reader converts only the
four raw `"\ud83d-a"` strings into an exact UTF-16 unit carrier because serde_json
cannot represent an isolated surrogate in a Rust String. It retains the original
fixture unchanged and checks those exact units rather than replacing them.
MAX DATALENGTH expectations retain the captured BIGINT string representation.
Independent review found that converted numeric payloads could escape width and
ANSI encoding validation; the new regression fails on the original checkpoint
and passes after every supplied payload receives those checks. A second review
found the implicit-collation diagnostic assumed SELECT column1 without context;
the regression rejects a mutation restoring that default. Context positions2/17
are rendering tests, not additional SQL Server reference captures.

This is unfinished work. Remaining applicable fixture cases, byte-domain edge
checks, independent review, full exact-head gates and CI must be completed before
merge. Registration and runtime binding/execution need separate successors;
this module does not establish server compatibility.

The owner-pinned mirek/mssqlite dispatcher at
`7f71f2081602f8e3051998f5c11f058e65fe24ec` maps CONCAT_WS to SQLite and TRANSLATE
to a custom backend function. Its AST/backend separation is useful context;
those mappings are not SQL Server ground truth and are not copied as semantics.
