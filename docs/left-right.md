# LEFT and RIGHT compatibility work

The initial server delegated LEFT and RIGHT to DuckDB. SQL integration now uses
the deterministic core and native adapters described below. Eleven live SQL Server
probes in `reference/left-right.json` establish that this is insufficient for
SQL Server values, conversions, diagnostics and result declarations. The file
records the pinned SQL Server image and version alongside complete captures.

`artifacts/compatibility/left-right-baseline-comparison.json` preserves the local
comparison. None of the eleven complete captures matches. Ordinary string
values match in the first five probes, but result declarations differ. Other
probes expose missing numeric/binary source conversion, fractional count
conversion, invalid negative length diagnostics and Unicode slicing differences.

## Observed reference rules

- Results use variable character families even for CHAR/NCHAR sources. Fixed
  source padding participates in slicing.
- A positive constant INT length narrows bounded result capacity to the smaller
  of the source declaration and requested length. Variable lengths preserve
  source capacity. MAX sources retain MAX declarations.
- Zero produces an empty value with minimum result capacity one. NULL counts
  produce NULL while preserving the source capacity in the captured example.
  All captured result columns have nullable/computed flags 33.
- Numeric and binary sources undergo character conversion. The binary probe
  decodes bytes 00/80 as NUL/euro under the reference CP1252 collation.
- Fractional numeric counts truncate toward zero; text counts convert to INT.
  Their result capacities remain the source capacity in the captured examples.
- Constant negative lengths raise 536/state 6 without preceding result metadata,
  naming the left or right function. DuckDB currently returns shortened strings.
- Count 2147483648 raises 8115/state 2 after result metadata in the reference.
- Under the captured non-SC collation, LEFT/RIGHT count UTF-16 code units.
  LEFT(N'🦆xy',1) returns the high surrogate; RIGHT(N'x🦆',1) returns the low
  surrogate. The current server returns the entire emoji in both cases.

## Implementation direction

Typing and constant-count inference belong in the deterministic SQL crate;
character slicing belongs in the deterministic value core. Conversion, native
vector access and evaluation belong in root adapters. Arguments must be evaluated
once. The Unicode result representation must retain isolated UTF-16 surrogates
through native execution and wire output; a Rust UTF-8 string alone cannot
represent the observed result. Fixing only metadata would leave incorrect SQL
semantics in place. Broader collation support and expression-dependent error
phases require additional reference evidence.

## Deterministic core

`msduck_core::left_right` now exposes borrowed ANSI-byte and UTF-16-unit slicing
for explicitly converted, non-NULL arguments. It allocates no output, caps the
requested count to source length before indexing, retains fixed padding, and
preserves isolated surrogates for subsequent operations. Negative counts carry
typed 536/state 6 diagnostics naming the function. NULL evaluation precedence
and compilation versus execution timing remain adapter responsibilities.

Result declarations convert fixed families to variable families, narrow bounded
capacity for proven nonnegative constant INT counts, and retain source width for
unknown counts and MAX for MAX inputs. Four tests cover the reference values,
composed surrogate slicing, extreme counts, errors and declarations. All 57 core
tests pass. Full workspace verification also passed: 383 Rust tests, strict
Clippy across all targets and formatting. This module is not yet connected to
SQL execution.

The upstream checkout's `packages/engine/src/udf.ts` uses JavaScript UTF-16 slicing
and returns UTF-16LE bytes for isolated-surrogate results. That representation
strategy is relevant, but its untyped byte fallback cannot be copied directly
into DuckDB's typed vectors. Its negative-length errors also use state 1 and
uppercase function names, unlike these live captures (state 6, lowercase).
The Rust adapters need a logical Unicode carrier distinct from SQL binary data,
with explicit conversions for nested expressions, persistence and TDS encoding.

## UTF-16 wire values

`msduck_tds::unicode_value` accepts raw UTF-16 units for bounded NVARCHAR/NCHAR
and the server's NVARCHAR(MAX) descriptor. It retains isolated surrogates,
checks declared widths before appending any bytes, distinguishes NULL from empty
values, and writes MAX values with PLP framing. NCHAR requires already padded
input. The root's existing Unicode text and NULL output now uses this shared
encoder, converting valid Rust strings into UTF-16 units at the adapter boundary.

Three independent byte-vector tests cover bounded and MAX values, isolated
surrogates, fixed padding, NULL/empty framing, and rejection without partial
output. The 19-test TDS suite and strict TDS Clippy pass. The wire snapshot also passed
386 workspace Rust tests, strict workspace Clippy and formatting. Local client
and audit verification is still running. Native expression
results, persistence and parameters still need a Unicode representation that
can carry isolated surrogates; this wire path alone does not complete LEFT/RIGHT.


## Native Unicode carrier

The root adapter `unicode_carrier` introduces a DuckDB STRUCT containing a named
`__msduck_utf16le` BLOB field. SQL binary values remain a separate physical type.
Internal functions pack valid UTF-8 input and apply the deterministic non-SC
LEFT/RIGHT rules to this carrier. They preserve parent NULLs, reject non-NULL
carriers with missing or odd-length payloads, and retain isolated surrogate units.
Input/output cells have an explicit 16 MiB budget and output chunks 64 MiB;
these limits are implementation policy, not SQL MAX compatibility claims.

Native tests exercise 6,000 materialized rows, nested slicing, direct TDS byte
encoding, malformed carriers, connection recovery, disk reopen and single
volatile-input evaluation. The carrier functions are registered internally;
SQL LEFT/RIGHT lowering, logical declaration binding, general Unicode casts,
parameter transport, comparison/collation rules and automatic result encoding
still need integration. Existing SQL calls continue using the old path.

The local Unicode wire verification was launched before this carrier change.
Its client phase uses the earlier built binary; its following audit rebuilds
current sources. Treat those phases as different source snapshots. A separate
remote Rust run verifies the native carrier source snapshot.


The three native carrier tests pass, including disk reopen and 6,000 single
volatile evaluations. Strict workspace Clippy and formatting also pass. The final carrier snapshot passed all 389 workspace Rust tests, strict Clippy
and formatting on Linux.

## Carrier result integration

Arrow result inspection now recognizes the exact one-field Unicode struct and
retains its named byte payload in an owned backend value. Row encoding passes
those units to the shared UTF-16 encoder. Unknown struct shapes remain unsupported;
malformed Unicode payloads fail before appending their value bytes. Bounded
Unicode declarations can override the physical carrier's default MAX descriptor.
Prepared error metadata recognizes the carrier as well, so an unavailable
UTF-8 representation does not prevent describing the result.

A session-level test verifies the actual ROW bytes for an isolated high surrogate
and metadata for an empty result. The tedious regression covers high/low
surrogates, NULL, empty values, a nested CTE result and connection reuse through
the internal carrier functions. Four native tests and strict workspace Clippy
pass. The frozen remote snapshot passed all 390 Rust tests, strict Clippy and
formatting. Its focused tedious test also passed with no failures or skips.
This adds result transport; general SQL LEFT/RIGHT calls and parameter conversion
still require integration. FOR JSON and other consumers of owned values also
need explicit Unicode-carrier support.

Full remote client and audit verification is now running against the carrier
result snapshot. Do not treat the focused client test as full compatibility proof.


## SQL integration and reference comparison

LEFT/RIGHT now trigger catalog acquisition and lower through shared source type
inference, SQL INT count conversion and family-specific native adapters. ANSI
and binary sources produce VARCHAR; Unicode sources produce the typed carrier.
A constant result-width argument preserves the Unicode declaration after AST
lowering without evaluating a parameter's current value as a constant. The
packer accepts existing carriers for nested expressions. Result properties
retain nullable/computed flags. Typed negative-length errors survive the native
string boundary with number 536, state 6 and the original function name.

`reference/left-right-casts.json` adds four live probes. Unicode casts preserve
individual code units and NCHAR pads with spaces; ANSI casts replace each
surrogate unit with `?`, including two replacements for a surrogate pair under
the captured non-SC collation. Deterministic `CharacterType::cast_utf16` implements
these rules. Carrier-aware casts preserve values through direct nested slicing
and explicit NVARCHAR/NCHAR/VARCHAR/CHAR conversions. Additional CP1252 best-fit
mappings remain unsupported.

`artifacts/compatibility/left-right-integrated-comparison.json` matches 12 of 15
complete reference captures: 8/11 original probes and 4/4 cast probes. All
captured values and successful result declarations match. The remaining three
captures differ only in extra metadata before the two constant negative-length
errors and state 1 rather than 2 for INT overflow. No differences were normalized.

The two focused tedious tests pass, including exact result descriptors, raw
surrogates, NULL/empty values, argument conversions, nested calls, catalog-bound
columns, SELECT INTO and direct carrier casts. Pure SQL/core tests, four native
carrier tests and strict workspace Clippy also pass. All 392 workspace Rust tests, formatting and strict Clippy passed. The
308-case local audit is running, with full client verification queued afterward.
The focused tests used a temporary example server binary to preserve the binary
used by the earlier client suite; its example source was removed after comparison.

The earlier wire-only client suite passed 372 tests. Its following audit rebuild
picked up an in-progress source edit and failed compilation, so it produced no
new audit evidence. The current full chain replaces that failed audit attempt.
A separate remote full verification still concerns the preceding carrier-result
snapshot, before SQL LEFT/RIGHT lowering.

All earlier RAISERROR/REPLICATE captures were rerun and retained in
`artifacts/compatibility/left-right-regression-comparison.json`. The REPLICATE
metadata matrix now matches 5/5, improving the REPLICATE total to 21/23. Remaining
RAISERROR and ERROR_* differences are unchanged.

Remaining work includes expression-dependent error phases, overflow state,
complete function syntax, other collations, carrier-aware general string
operations/comparisons/JSON, parameter assignment and further casts over
materialized or derived carrier columns. The direct cast tests do not establish
that every downstream consumer handles this representation correctly.


## LEN and DATALENGTH consume Unicode carriers

Known Unicode inputs now pass through the common packer to carrier-aware length
functions. LEN counts UTF-16 units after excluding trailing U+0020 spaces only;
DATALENGTH counts every stored byte. Parent NULLs remain NULL and malformed
carrier payloads fail explicitly. SQL lowering retains INT results for bounded
inputs and BIGINT results for MAX inputs. The LEN result-property rule now
retains nullable/computed flags even for non-NULL literals, as verified live.

Shared character-family/width merging also feeds conditional operand inference.
CASE, COALESCE and the existing first-argument selection rules can retain known
character declarations when binding these consumers. The native regression uses
a declared SQL table seeded with 6,000 rows, checks NULLs and lengths after
SELECT INTO, and verifies single evaluation with a sequence. A raw DuckDB range
source has incomplete logical catalog inference; the test seeds the declared
SQL table through range rather than treating range as a SQL Server row source.

`reference/unicode-lengths.json` retains ten live captures covering raw surrogate
halves, pairs, fixed padding, NULL/empty values, MAX widths, whitespace, CTEs,
SELECT INTO, literal metadata and CASE. All ten complete captures match in
`artifacts/compatibility/unicode-length-comparison.json`, without normalization.
Five native carrier tests, the core length test, SQL declaration tests and strict
workspace Clippy pass. All three focused tedious tests pass, covering Unicode lengths alongside
the earlier carrier and LEFT/RIGHT regressions. Full remote verification is now
running against a frozen snapshot containing the length changes and new audit
probe (309 total cases).

The preceding carrier-result snapshot completed remote verification: 390 Rust
tests, 373 client tests and all 307 audit captures. Its captures match the
preserved local 307-case baseline exactly; evidence is retained in
`artifacts/remote/linux.local/unicode-result-platform-comparison.json`.
The subsequent local LEFT/RIGHT audit completed 308 cases: 306 old captures
unchanged, one unordered APPLY row reversal and one new probe. Raw differences
are retained in `artifacts/compatibility/left-right-audit-diff.json`. That chain's
client phase rebuilt later sources; final length validation uses the separate
frozen remote snapshot.


## Unicode variable and RPC bindings

Core scalar values now distinguish raw UTF-16 text from SQL binary bytes.
Well-formed UTF-16 normalizes losslessly to ordinary text; isolated surrogates
retain their original units. The backend adapter transports these units as
bound BLOB bytes, and the SQL adapter explicitly reconstructs the named Unicode
carrier instead of attempting to bind DuckDB structs or casting them to display
text. Result widths still come from logical parameter declarations.

DECLARE, SET and SELECT assignment can retain raw units from the evaluated
carrier. Direct variable-to-variable character casts use the existing
carrier-aware conversions. Incoming NVARCHAR RPC values accept isolated
surrogates, reject odd byte lengths and retain existing width/PLP bounds. SQL
statement text and parameter declaration strings still require valid Rust text;
this change concerns data values, not the SQL parser's source representation.
SQL binary parameters retain their original type and bytes.

Eight live variable cases and three RPC cases are retained in
`reference/unicode-bindings.json` and `reference/unicode-rpc-bindings.json`.
All eleven complete captures match in
`artifacts/compatibility/unicode-bindings-comparison.json`. Cases cover fixed
padding, MAX, NULL reassignment, nested slicing, ANSI conversion and declaration
widths. Byte-vector tests cover isolated units, PLP split across byte boundaries,
truncated packets and malformed odd-length data. An adapter test distinguishes
raw Unicode bindings from untagged binary values and rejects malformed carriers.

The focused native Unicode tests and all deterministic crate tests pass.
The client regression additionally checks unchanged binary identity and
connection reuse. Full workspace verification for this binding snapshot remains
pending while the preceding local and remote suites finish; those live binaries
and the remote source snapshot are being preserved. Broader carrier consumers,
output parameters and casts from materialized carrier columns remain unfinished.


Binding verification completed 170 deterministic-crate tests and all 199 root
library tests, plus strict workspace Clippy and formatting. The dedicated
variable/RPC tedious test passes; the preceding three carrier/LEFT-RIGHT/length
client regressions also pass. Earlier RAISERROR, ERROR_* and REPLICATE captures
were rerun in `artifacts/compatibility/unicode-bindings-regression-comparison.json`.
Full workspace/client/audit verification of the binding snapshot remains pending.


The Unicode-length snapshot completed frozen Linux verification: 394 Rust tests,
375 client tests and all 309 audit captures. Against the preceding 308-case local
LEFT/RIGHT audit, 305 captures are unchanged; two change only LEN flags from 1
to 33, one reverses two unordered APPLY rows, and one new probe is added. Raw
differences are retained in
`artifacts/remote/linux.local/unicode-length-audit-diff.json`.
The earlier local client run had 373 passes and one temporal test cancellation
at its 30-second timeout; that exact test passed an isolated rerun. This is not
recorded as a clean full local pass. A new frozen remote run now includes both
the Unicode-binding and PRINT fixes; see docs/print.md for PRINT reference scope.
