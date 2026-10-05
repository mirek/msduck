# REPLICATE implementation work

SQL REPLICATE now executes through deterministic typing/lowering and native
vector adapters. All eight previously blocked RAISERROR boundary-length probes
now match their complete SQL Server captures. Complete REPLICATE and general
SQL Server compatibility remain unfinished.

## Reference behavior

`reference/replicate.json` and `reference/replicate-metadata.json` retain nineteen
live SQL Server probes with values, result descriptors, diagnostics and completion
events. They establish:

- NULL source or count yields NULL; negative INT counts yield NULL, and zero
  yields an empty string. Count conversion truncates fractional numeric inputs
  toward zero; a BIGINT count of 2147483648 raises 8115/state 2 when converted to
  INT. A negative fraction between zero and minus one therefore becomes zero.
- Non-MAX results retain whole copies fitting within 8,000 SQL bytes. For
  example, repeating `abc` 2667 times gives 7,998 VARCHAR bytes; its Unicode
  counterpart repeated 1334 times gives 3,999 UTF-16 units (7,998 bytes).
  The value is not a partial final copy truncated to exactly the byte cap.
- Repeating `🦆x` 1334 times likewise retains 1333 whole copies. Surrogate pairs
  remain intact. This differs from RAISERROR's string precision behavior.
- VARCHAR(MAX)/NVARCHAR(MAX) sources exceed the bounded cap; the probes retain
  8,100 ANSI bytes and 4,100 Unicode units. Empty MAX results keep MAX metadata.
- Fixed CHAR/NCHAR source padding is repeated, but result types are variable
  VARCHAR/NVARCHAR. Binary sources convert to VARCHAR first; the probe containing
  bytes 00/80/41 repeats NUL/euro/A under the reference Windows-1252 collation.
- Positive constant INT counts multiply the declared source capacity, capped at
  the bounded maximum. A VARCHAR(10) variable containing `x`, repeated three
  times, returns `xxx` with capacity 30. Runtime-variable counts use the bounded
  maximum. Foldable integer arithmetic and CAST can produce precise capacities;
  fractional or character count literals in these probes use the maximum.
- Nonpositive constant counts produce a two-byte descriptor capacity: VARCHAR(2)
  or NVARCHAR(1). Result flags are computed and nullable even for non-NULL literals.
- Three arguments produce syntax diagnostic 174, requiring two arguments.

Microsoft's [REPLICATE documentation](https://learn.microsoft.com/en-us/sql/t-sql/functions/replicate-transact-sql)
confirms the bounded byte limit, MAX exception, negative-count NULL behavior and
binary-to-VARCHAR conversion. The live probes supply the whole-copy boundary
rule and descriptor details.

## Deterministic boundary

`msduck_core::replicate::plan` accepts an already-converted string, nullable INT
count, validated source character declaration and an explicit UTF-8 allocation
budget. It borrows the source and checks the complete output length before
allocating or looping over repetitions. Negative/NULL inputs remain distinct
from an empty result. Bounded SQL byte capacity is separate from the adapter's
allocation policy. ANSI text is validated against Windows-1252; Unicode lengths
count UTF-16 units. MAX output must fit the supplied allocation budget.

`result_type` accepts the source declaration and an optional compile-time INT
count. Callers must not substitute a parameter's current runtime value for proof
of a compile-time constant. It preserves MAX and handles fixed-to-variable
families and observed nonpositive-count descriptor widths.

Four core tests cover the captured values/caps/declarations, binary conversion,
Unicode pairs, fixed padding, zero/NULL/negative inputs, INT_MAX bounded counts,
and rejection of excessive MAX allocations before repetition. All 53 core tests, strict core Clippy and formatting pass. These tests do
not establish SQL execution or count-conversion fidelity; the later native-adapter snapshot passed all 377 workspace Rust tests, strict
Clippy and formatting on Linux.

The upstream mssqlite repetition UDF calls JavaScript string repetition without
SQL's bounded cap. Its character inference has several paths; one retains input
width while another multiplies literal counts. Those pieces cannot be copied
unchanged. Relevant upstream files are `packages/engine/src/udf.ts` and
`packages/transpile/src/{functions,character,implicit}.ts` in the inspected
reference checkout.

Remaining work: source type conversion (including numeric/binary inputs), exact
INT conversion diagnostics, constant-folding-aware metadata, selecting native variants from source types,
single evaluation through the SQL lowering path,
SQL signature validation, backend lowering, client tests and full audits. SQL
MAX size limits and other collations also require coverage. The integrated behavior and remaining differences are recorded below.


## Native vector adapter

`src/replicate.rs` registers four internal VARCHAR/INTEGER functions for ANSI and
Unicode sources with bounded or MAX behavior. SQL lowering must choose the
function from the logical source declaration and convert each argument once.
The adapters read only non-NULL vector slots, borrow inline or heap string bytes,
and invoke the same core plan for each row. Input storage remains owned by
DuckDB; output strings are copied through its vector inserter.

Allocation policy limits one cell to 16 MiB of UTF-8 and one invocation's output
to 64 MiB. The chunk budget accommodates ordinary bounded CP1252 results even
when each source character occupies three UTF-8 bytes. Exceeding either budget
raises an explicit error; MAX output is never silently truncated to the budget.
These are implementation resource limits, not SQL Server's MAX size contract.

Two native tests pass. They cover inline/heap strings, 6,000 rows with NULL and
negative counts, whole-copy truncation, Unicode, embedded NUL and CP1252 text,
fixed padding, MAX values beyond 8,000 bytes, and empty input with INT_MAX count.
Independent sequences prove that each native input is evaluated once. An
excessive MAX request fails before allocating its result; a reduced-budget test
exercises cumulative chunk accounting with small allocations and verifies
subsequent query recovery. All 377 workspace Rust tests, strict Clippy and formatting passed on Linux.
The prior local client suite retains its existing binary. SQL lowering and metadata are now connected as described below.


## SQL integration and result declarations

`msduck_sql::replicate` validates arity during batch preflight, chooses a native
variant from explicit source declarations and wraps the result in its logical
VARCHAR/NVARCHAR declaration. Each source and count expression occurs once.
Counts use the existing SQL integer-conversion path; fractional counts truncate
toward zero. Constant integer arithmetic and INT casts use deterministic folding
for result capacity, while variable counts retain maximum bounded capacity.
MAX, fixed padding and source column widths survive this path. Empty string
literals use the reference's minimum source capacity of one.

REPLICATE now triggers catalog snapshot acquisition by the operand binder. This
is required for table columns and SELECT INTO, not only result metadata. A native
integration regression verifies VARCHAR(10) sources producing VARCHAR(30) for
constant count 3 and VARCHAR(8000) for a variable count, plus nested Unicode CTE
results. Source type inference is shared by projection, conditional operands,
DATALENGTH and lowering. Result properties mark REPLICATE nullable/computed.

Two additional BLOB/INTEGER variants perform Windows-1252 binary decoding before
repetition. Their binary input limit is 16 MiB. Numeric source declarations use
live reference capacities: BIT 1, TINYINT 4, SMALLINT 6, INT 12, BIGINT 24,
DECIMAL 41, MONEY/SMALLMONEY 40 and REAL/FLOAT 23. Existing conversion adapters
supply text; the floating-point formatting difference below remains.

`reference/replicate-numeric-inputs.json` adds one numeric conversion matrix.
`reference/replicate-wrapper-metadata.json` adds three probes establishing empty
literal capacities and DATALENGTH's nullable/computed flags even for non-NULL
literals. The shared result-property rule now retains those DATALENGTH flags.

The integrated comparison is retained in
`artifacts/compatibility/replicate-integrated-comparison.json`. It matches 20/23
complete REPLICATE-related captures: 13/14 original probes, 4/5 metadata probes,
0/1 numeric-input matrix and 3/3 wrapper probes. The remaining differences are:

- A count beyond INT range reports 8115 with state 1 instead of reference state 2.
- A companion RIGHT query has correct values but retains incorrect result
  family/width/flags. Its DATALENGTH companions now match.
- REAL/FLOAT source value 1 is formatted as `1.0` rather than `1` by the existing
  cast path; all ten numeric source capacities and the other eight values match.

## REAL/FLOAT implicit source formatting

`scripts/capture-replicate-float.mjs --write-fixture` captured 56 REAL/FLOAT
literal, table-column and typed RPC cases twice in fresh databases on the pinned
SQL Server 2025 image. The complete rows, descriptors, errors and completion
tokens are retained without normalization in `reference/replicate-float.json`.
The implicit conversion used by REPLICATE yields six significant decimal digits.
It chooses fixed notation when the *rounded* decimal exponent is -4 through 5,
otherwise scientific notation with a lowercase `e`, explicit sign and three
exponent digits. For example, 1 becomes `1`, 1.23456789 becomes `1.23457`,
1,000,000 becomes `1e+006` and 0.00001 becomes `1e-005`. The source's physical
precision matters: REAL 0.00009999995 yields `9.99999e-005`, while FLOAT yields
`0.0001`; REAL 1.234565 yields `1.23457` while FLOAT yields `1.23456`. Both
types turn negative zero into `0`; NULL remains NULL. The captured REPLICATE
result descriptor for a constant count of two is VARCHAR(46), independent of
the runtime formatted value.

SQL lowering now passes a typed REAL or FLOAT source directly to a native
adapter instead of first using DuckDB's VARCHAR cast. The adapter converts once
before the existing bounded repetition plan. It retains the same per-cell and
per-chunk allocation limits and rejects non-finite DuckDB values explicitly;
SQL Server has no NaN/Infinity source literals in the retained evidence. This
path also converts a directly cast plain decimal REAL literal from its exact
decimal spelling. DuckDB otherwise double-rounds `CAST(0.00009999995 AS REAL)`
to a different f32 value before formatting; its string-to-REAL cast agrees with
the retained SQL Server result. General REAL arithmetic and non-REPLICATE casts
remain outside this task's scope and may still show source conversion differences.
The change does not alter general CAST/CONVERT float formatting, other source
families, count-conversion diagnostics, RIGHT metadata or SQL MAX size limits.
The earlier 20/23 comparison above describes the pre-change snapshot and is
preserved as historical evidence; fresh reference replay is required to assess
this change's exact public result coverage.

All 82 earlier RAISERROR probes were rerun: 77 now match exactly (up from 69).
The remaining five differ in TRY/CATCH completions, WITH LOG and top-level RETURN
semantics. The ERROR_* metadata matrix still matches 6/7 complete captures.
No raw differences were normalized away.

Three native REPLICATE tests, the pure SQL regression, strict workspace Clippy
and four focused client tests pass. The latter cover typed results, empty/NULL/
negative/fractional counts, whole-copy limits, MAX sizes, binary bytes, volatile
input evaluation, parameter declarations, preflight and the RAISERROR setup.
Local verification of the final SQL snapshot passed all 379 workspace Rust
tests, formatting and strict Clippy. All 307 local audit cases completed. The
raw comparison with the preserved 306-case local baseline has 300 unchanged
captures, six changed captures containing only DATALENGTH flags (1 to 33), one
new REPLICATE probe and no removed cases. It is retained in
`artifacts/compatibility/replicate-sql-audit-diff.json`; its reference fields
denote the previous local output, not SQL Server. Remote verification of that frozen snapshot passed 379 Rust tests, strict
Clippy, formatting and all 372 client tests. All 307 audit captures completed
and match the preserved macOS captures exactly, without normalization. The
comparison is retained in
`artifacts/remote/linux.local/replicate-sql-platform-comparison.json`.

Remaining scope includes the differences above, additional
source expression/type forms, complete signature/name rules, other collations
and SQL MAX sizes beyond current explicit resource limits.
