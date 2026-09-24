# DML OUTPUT investigation

The server now executes OUTPUT projections using INSERT's inserted row, UPDATE's
inserted row and DELETE's deleted row through native RETURNING. It preserves
logical metadata independently of the backend, including empty results, and
preparation binds without executing writes. OUTPUT INTO now routes these native
images through typed bindings and the existing INSERT conversion path. UPDATE
old/new/source images use staged capture, including joins, key changes, parameters
and CTE input. Joined DELETE, writable derived targets and generated-column images
remain unfinished.
The investigation entries below retain their individual snapshot boundaries;
the final joined-execution section describes current dispatch.
Preflight rejects the reference-proven invalid pseudo-table, unqualified-column,
wildcard and subquery cases before batch execution.

`reference/output-dml.json` records 16 SQL Server 2025 programs, each with an
explicit setup and readback of both the changed table and the OUTPUT INTO sink.
It covers INSERT stars and empty results, UPDATE old/new images and changed
keys, DELETE images, joined source columns, OUTPUT INTO, constraint failures,
invalid pseudo-table references, subqueries, NOCOUNT and raw Unicode units.
The baseline comparison is `artifacts/compatibility/output-dml-before.json`.

Important observed requirements:

- An UPDATE can return both the old and new key when the key changes. Joined
  UPDATE/DELETE can also expose columns from the joined source.
- OUTPUT INTO writes participate in the same statement outcome as the DML.
  In the captured NOT NULL failure, error 515 is caught, the inserted target row
  is absent and the sink is empty.
- A failing UPDATE returned one OUTPUT row before error 2627 was caught, while
  readback showed the original table unchanged. Result delivery and statement
  rollback therefore cannot be modeled as a single success-only result buffer.
- INSERT with deleted.id and DELETE with inserted.id fail with 4104 before
  mutation. An OUTPUT subquery fails with 10705.
- Empty OUTPUT still describes its result. Bounded NVARCHAR output preserves
  raw UTF16 units and the column's logical declaration.

The upstream mssqlite implementation uses SQLite RETURNING for simple images
and a temporary old-row snapshot joined by rowid for UPDATE deleted/inserted
pairs. See `packages/engine/src/output.ts` and the `updateWithOutput`
path in `packages/engine/src/execute.ts` at the recorded upstream revision.
That strategy cannot be copied unchanged into this backend.

A probe of the bundled DuckDB through its Rust connection API is retained in
`artifacts/compatibility/output-backend-probe.log`. INSERT, UPDATE and DELETE
RETURNING produced rows. UPDATE RETURNING old.n,new.n failed binding. Rowids
changed from 0/1 to 4/5 across the tested updates, including a primary-key change;
a post-update rowid join would not recover those original images. This requires
explicit old/new row correspondence independent of mutable backend rowids.

Implementation needs to preserve single evaluation of predicates, assignments
and OUTPUT expressions, typed empty-result metadata, statement atomicity,
transaction state, constraint diagnostics and correct completion command/count
fields. A deterministic output plan can validate qualifiers, expand declared
columns and describe expressions from explicit catalog snapshots; the root
adapter must acquire images, execute writes and route results or sink writes.
A simple OUTPUT-to-RETURNING rewrite is insufficient for the full feature.

The Microsoft OUTPUT documentation also describes returning rows from failed
statements and restrictions on pseudo-tables and output targets:
[OUTPUT clause reference](https://learn.microsoft.com/en-us/sql/t-sql/queries/output-clause-transact-sql?view=sql-server-ver17)


Parser/preflight verification: all 93 SQL-crate tests and strict SQL-crate Clippy
passed in the isolated development checkout. The Linux build passed, and the
new client regression verifies exact diagnostics and that preceding writes do
not run. IF/WHILE, TRY/CATCH and NOCOUNT client regressions passed alongside it.
The updated 16-case comparison is retained in
`artifacts/compatibility/output-dml-after.json`; full execution gaps remain in
that capture rather than being excluded. Full workspace/client/audit checks of
this parser/preflight snapshot are pending.

## Native image-pairing experiments

The bundled DuckDB rejects source aliases in both UPDATE FROM RETURNING and
MERGE RETURNING. It also rejects old/new pseudo-table aliases and subqueries in
RETURNING. A target-only MERGE RETURNING succeeds, but does not expose the old
source image. The program and complete observations are saved as
`artifacts/compatibility/output-pairing-probe.rs` and `.log`.
The [DuckDB MERGE documentation](https://duckdb.org/docs/current/sql/statements/merge_into)
describes affected-row RETURNING; source-image availability here was tested
against the actual bundled backend rather than inferred from that syntax.

A second native prototype materializes old values and converted new assignment
values together before writing. UPDATE joins that materialization to the target
using the original rowid during the write, rather than joining old rowids to
post-update rowids. OUTPUT can then use the explicitly paired images. The
program and observations are saved as
`artifacts/compatibility/output-images-probe.rs` and `.log`.

The prototype verifies:

- Both primary keys can change while each old/new image pair remains intact.
- A sequence-backed assignment is evaluated exactly once per affected row.
- A duplicate-key write failure restores the original target and empty sink.
- An OUTPUT sink NOT NULL failure also rolls back the target write and sink.

This is a native experiment, not an implemented server OUTPUT adapter. Its
transaction encloses the complete operation; statement undo inside an already
open SQL transaction remains unresolved. Joined sources must choose one row
per target before evaluating assignments, and output binding must preserve
logical types, source columns, defaults, computed values and single evaluation.
The SQL Server partial-row-before-error observation remains an explicit gap.
These requirements still apply when implementing the deterministic image plan
and the root execution adapter.


## Native single-image execution

`msduck_sql::output::NativePlan` retains a logical image projection and operation
while lowering available images to RETURNING. Catalog acquisition and expression
binding stay in the root; binding the logical projection before native lowering
preserves declared Unicode length semantics in OUTPUT expressions. Protocol
command codes also stay root-side. Autocommit OUTPUT execution uses an internal
transaction so execution/encoding failures can roll back the write.

The focused Linux client tests passed for the native reference cases, including
prepared INSERT with an emoji and LEN=2, and preparation with zero writes.
The 16-case OUTPUT matrix now has seven complete matches including readback;
`reference/output-native-expressions.json` and `reference/output-unqualified.json`
add eight more matching cases. Their full raw comparisons are retained in the
corresponding `*-native.json` artifacts. The permanent client test additionally
checks decoded completion command fields on successful programs. Formatting, strict workspace Clippy and all 403 Rust tests passed on Linux.
All 382 client tests also passed with no failures, cancellations or skips.
The audit also completed all 316 captures. The raw comparison against the
preceding 315-case snapshot is retained in
`artifacts/compatibility/output-native-audit-diff.json`.

Remaining execution gaps include old/new and joined-source images,
partial row delivery on errors, and storing isolated UTF16 surrogate units in a
declared NVARCHAR column. Those cases remain in the full comparison. Unsupported
image/sink paths are rejected explicitly before writing instead of silently
returning a different image.

## OUTPUT INTO destination evidence

`reference/output-sinks.json` records twelve additional live SQL Server programs
with target and sink readback, and decoded completion tokens. The generator and
raw log are retained in `artifacts/compatibility/output-sinks-reference.mjs` and
`.log`. The initial native-image server matched none of these complete programs;
`artifacts/compatibility/output-sinks-before.json` preserves all differences.

The reference establishes that explicit sink column order controls assignment,
omitted columns receive their defaults, and an empty source inserts nothing.
Successful sink writes emit no OUTPUT result set, and the following @@ROWCOUNT
reports the original INSERT, UPDATE or DELETE count. Missing sink columns yield
207, a missing sink yields 208, and too many output expressions for an explicit
column list yield 121 (severity 15). These binding failures leave the target and
sink unchanged. A caught NOT NULL sink failure also leaves both unchanged.

An enabled CHECK constraint makes the sink invalid even when the proposed value
satisfies it: SQL Server reports 333 naming the constraint before the target
write. A UNIQUE constraint is allowed. Therefore ordinary INSERT validation is
necessary but insufficient for OUTPUT destinations. Foreign keys, triggers,
views, identity columns and failures inside an explicit transaction still need
dedicated destination evidence.

The execution adapter should bind the sink before acquiring or mutating target
rows, retain logical source declarations beside captured values, and reuse the
existing INSERT storage conversion and default handling. Rebinding Arrow values
using only physical types would lose distinctions such as MONEY/DECIMAL and
VARCHAR/NVARCHAR, especially for NULLs. Wire descriptors are not the compiler's
type contract. The root can drain native RETURNING into owned typed values,
release backend borrows, and route those values into a bound sink write within
the same statement transaction. The implementation below follows this design;
explicit-transaction statement undo and partial result delivery on failure
remain unresolved.

The deterministic native plan now carries an optional `Sink` with its qualified
table name and optional ordered column list. It decodes the parser's function
representation without evaluating it, preserves identifier quoting, and rejects
expression-valued destination columns before changing the AST. Omitted lists
remain distinct from explicit lists for later default/identity binding. The
root still rejected sink execution in that plan-only snapshot. Unit coverage
includes all three DML operations and a
CTE-wrapped INSERT.

The sink-plan snapshot passed formatting, all 404 workspace Rust tests, strict
workspace Clippy and both focused OUTPUT client tests locally. Its full local
client run finished with 381 passes and one timeout cancellation in the BIT
aggregate test; the audit did not start. The isolated test passed against the
same binary. This does not turn the original full run into a pass. The full
Linux client run above covers the preceding native-image snapshot.


The preceding parser/preflight snapshot completed its full Linux verification:
402 Rust tests, 381 client tests and 315 audit captures, with formatting and
strict Clippy passing. That run does not cover the later native execution path.


## Typed sink execution

The root now acquires the destination object, ordered writable columns and
constraints before executing the target write. Logical source declarations come
from `TypeMetadata::logical_type`, independently of NULL values and Arrow's
physical types. Unknown declarations remain an explicit binding failure. A
bound INSERT is prepared without execution, and destination defaults and storage
conversions use the ordinary INSERT path.

Native RETURNING values are drained into owned values with a configured 64 MiB
materialization limit. After releasing the native statement, the adapter inserts
these rows into the destination. Autocommit wraps the target and all sink writes
in one internal transaction. Sink writes emit no result sets and preserve the
original DML count and command. A caught sink failure emits the reference's
counted-zero DML completion before CATCH entry; target-query failures do not
expose RETURNING metadata for an INTO statement.

The focused Linux tests verify destination binding, defaults, empty results,
prepared binding without writes, Unicode, currency, exact TIME/DATETIME2,
binary and fixed character values. A two-row sink failure rolls back both source
rows and the first sink row. CHECK constraints and foreign-key participation
are rejected before mutation; UNIQUE destinations work. Constraint acquisition
uses the backend's [constraint metadata](https://duckdb.org/docs/current/sql/meta/duckdb_table_functions#duckdb_constraints).
Exact diagnostics for all forbidden destination kinds remain incomplete.

Two shared gaps exposed by the sink tests were corrected: numeric CASE/COALESCE
projection types now combine known branch declarations, and implicit currency
to character storage uses style-zero formatting before storage-width validation.
`reference/output-sink-currency.json` records live MONEY and SMALLMONEY cases;
both complete programs now match, including sink readback. The general sink
matrix now matches ten of twelve complete programs. The two remaining cases
fail earlier in ALTER ADD CHECK/UNIQUE support. Full mismatch records remain in
`artifacts/compatibility/output-sinks-execution.json`.

The new prepared sink test verifies stored DATETIME2 fractions through YEAR and
DATEPART. CAST of a stored DATETIME2 to VARCHAR still exposes a separate existing
backend-struct formatting gap. The original native RETURNING path cannot bind parameters directly in OUTPUT;
the materialized projection path below now handles these values. Other remaining scope includes old/new and joined images,
statement undo inside explicit transactions, partial row delivery on failure,
full destination diagnostics and identity/session-counter behavior. The current
adapter rebinds each sink row; it does not yet batch large materializations.

All three focused OUTPUT client tests passed on Linux. Full verification of the
final sink execution snapshot has passed formatting, strict Clippy and all 406
workspace Rust tests. All 383 client tests passed without failures,
cancellations or skips. All 317 audit captures completed; all 316 preceding
captures were unchanged, with one new OUTPUT INTO probe. Raw comparison is
retained in `artifacts/compatibility/output-sink-audit-diff.json`.

## Parameterized projection materialization investigation

`reference/output-parameters.json` records eight SQL Server programs with local
variables inside OUTPUT expressions: Unicode, typed NULL/empty results,
arithmetic, quoted text, binary, decimal, INTO routing and variable reassignment.
Each preserves target/sink readback and completion tokens. This remains an
baseline execution gap: none of the eight programs matched the native-only
server. Those raw differences remain in
`artifacts/compatibility/output-parameters-before.json`.

The native parameter probe is retained in
`artifacts/compatibility/output-variables-probe.rs` and `.log`. Direct RETURNING
parameters fail binding. Bound SET VARIABLE values can be read from RETURNING,
but RESET VARIABLE fails in an aborted transaction, rollback leaves the value
present, and an already-prepared statement retains the value seen at binding.
That approach is not used by the server.

The alternative native prototype in `output-arrow-probe.rs` captures RETURNING
record batches, appends them to a private image relation, and evaluates a
separate parameterized SELECT. It preserves NULLs, raw surrogate bytes,
DATETIME2 ticks, nanosecond TIME, decimal values, Boolean and UUID types. A
sequence in the projection is evaluated once per row. On sink failure, rollback
restores target and sink and removes the private relation; the success path
drops the private relation before commit. Programs and before/after logs reside
in `artifacts/compatibility/`.

The prototype exposed a vendored Arrow conversion bug: nanosecond time was
mapped and cast to microsecond TIME. That mapping and conversion are corrected,
and `appender-arrow` is enabled. The permanent `tests/arrow_images.rs` regression
passed for 6,001 rows, including native chunk boundaries, nested values and
extension metadata. This supplies a tested native transfer mechanism; replacing
the server's OUTPUT expression evaluation still requires retaining logical
declarations, result properties, empty-result metadata and binding validation
across the private relation. No parameter values are interpolated into SQL.

The Arrow transfer snapshot passed formatting, all 407 workspace Rust tests and
strict workspace Clippy locally. Full client/audit checks are running separately
from the preceding frozen Linux sink-execution verification.

The deterministic native plan now detects local parameters specifically in the
OUTPUT projection. `MaterializedPlan` separates an empty image-description query
from the original projection over an explicit adapter-supplied relation name,
and switches the DML's RETURNING list to its row image. DML source parameters and
CTEs stay in the write statement; the image and output queries do not rerun
those CTEs. The original logical projection remains available for metadata.
Tests cover INSERT, UPDATE, DELETE and CTE-wrapped INTO, including cases where
parameters appear only in DML input and do not require output materialization.
All 97 SQL-crate tests and strict SQL-crate Clippy passed. The plan-only snapshot
then passed full Linux verification: 408 Rust tests, 383 client tests and 317
audit captures. This precedes the runtime integration below.


## Parameterized OUTPUT execution

The root now selects materialization when an OUTPUT projection contains local
parameters. It creates a transaction-owned relation in `temp.main`, captures the
native row images through Arrow, and evaluates the bound projection over those
images. Parameters remain bound values. Logical metadata comes from the original
projection, so physical image storage does not determine result widths, labels
or provenance. Empty results retain their descriptors. Both returned rows and
OUTPUT INTO use the same projection path.

Preparation validates a typed-NULL version of the original RETURNING expression
without executing DML. Internal statements stay as ASTs through validation;
rendering and reparsing them had incorrectly rejected native UPDATE/DELETE
RETURNING syntax. This validation also retains native aggregate/window rejection
before a write. The image relation is removed after successful evaluation; on a
native failure, the enclosing rollback removes its transactional creation.
Statement undo inside an already-open SQL transaction remains unfinished.

All eight complete SQL Server programs now match, including metadata, completion
tokens and target/sink readback. The comparison is
`artifacts/compatibility/output-parameters-materialized.json`. Four focused OUTPUT
client tests passed on Linux. Additional prepared tests passed for reassigned
values, quoted text, a surrogate pair, an isolated high surrogate, INTO, and a
CTE-driven INSERT. The root regression checks preparation with zero writes,
zero-row metadata, native aggregate rejection, target/sink failures and private
relation cleanup. Shared result inference now uses parameter declarations when
recovering binary widths, correcting VARBINARY(4) metadata in the reference case.

The final runtime snapshot passed formatting, strict workspace Clippy and all
409 Rust tests on Linux. Full client/audit verification is running. The separate
local Arrow client run passed 383 tests; its later audit built the initial
runtime adapter and captured 317 cases, all unchanged from the preceding Linux
planner snapshot. That mixed-stage evidence is kept separate in
`artifacts/compatibility/output-arrow-local-audit-diff.json` and does not replace
the current frozen verification. Old/new image pairing, joined-source images,
full failure-stream fidelity and explicit-transaction statement undo remain open.


## Parameterized expression failure evidence

`reference/output-expression-errors.json` captures eight complete SQL Server
programs: division by a zero INT parameter and conversion of a VARCHAR parameter
to INT, each with nonempty/empty INSERT input and returned/INTO output. Captures
include raw DONE tokens, result descriptors, caught errors and ordered target
and sink readback. The reference generator is preserved as
`artifacts/compatibility/output-expression-errors-reference.mjs`.

The current materialized runtime matches three of eight programs completely
(`artifacts/compatibility/output-expression-errors-before.json`). All eight
preserve target and sink contents. SQL Server suppresses both expression errors
for empty input. Nonempty input raises 8134 or 245, with XACT_STATE() zero after
autocommit rollback. Returned output exposes its empty result descriptor before
CATCH; INTO does not expose an output descriptor. Both emit a zero-row INSERT
completion (command 195) before entering CATCH.

The remaining differences identify three implementation issues:

- Returned-output failures currently emit SELECT command 193 instead of INSERT
  command 195, although descriptors, caught errors and readback match.
- INTO conversion failures omit the zero-row DML completion before CATCH.
- INTO division cannot bind a logical source type for `10/@p`, so it raises
  50000 before execution even for empty input. This is missing type inference,
  not premature evaluation of division. The existing member-expression fallback
  can infer literal arithmetic but ordinary projection inference is incomplete.

Diagnostic probes without TRY/CATCH confirm the last error's exact cause in
`artifacts/compatibility/output-expression-errors-uncaught-local.json`; those
uncaught programs have no SQL Server comparison and are labeled accordingly.
These comparisons used the frozen Linux runtime binary while its verification
continued; they did not resynchronize or rebuild the remote workspace.


The next implementation adds numeric binary-expression inference to ordinary
projection binding using explicit parameter declarations and the existing
arithmetic rules. Unknown operands remain unknown. Failed result prefixes now
carry their original command, and materialized INTO expression failures carry
the same DML failure context as destination insertion failures. The eight
reference programs are permanent client regressions; focused and full checks
are in progress for this snapshot.

The fixed snapshot matches all eight programs exactly, including raw DONE tokens
(`artifacts/compatibility/output-expression-errors-fixed.json`). Formatting, all
410 workspace Rust tests and strict workspace Clippy passed locally. Full
client/audit checks are running separately from the preceding frozen Linux run.
All five focused OUTPUT client tests also passed, including the new failure
reference matrix and existing prepared, sink and row-image regressions.

The preceding frozen Linux materialized-runtime verification has now completed:
409 Rust tests, 384 client tests, formatting and strict Clippy passed, and all
318 audit cases were captured. The 317 prior audit captures are unchanged, with
one added parameterized OUTPUT probe. The raw snapshot comparison is retained
in `artifacts/compatibility/output-materialized-runtime-audit-diff.json`. This
run predates the expression-failure fixes and does not validate those changes.


## UPDATE and DELETE expression failures

`reference/output-update-delete-errors.json` adds sixteen SQL Server programs
covering UPDATE/DELETE, parameterized and row-dependent division, empty/nonempty
inputs, and returned/INTO output. The preceding INSERT failure fix matches
fourteen completely. Native (nonmaterialized) INTO expression failures omitted
the zero-row command 197/196 completion before CATCH; all target/sink readbacks
already matched. Baseline evidence is retained in
`artifacts/compatibility/output-update-delete-errors-before.json`.

Native INTO execution now carries the same DML failure context as materialized
projection execution. A permanent client test compares all sixteen programs,
including raw DONE tokens. The Linux build completed; focused verification is
running. The local full client run still uses the preceding INSERT-failure
binary; its later audit build may include this update and must be recorded as
separate stages rather than a frozen verification of one snapshot.

All six focused OUTPUT client tests passed on Linux. All sixteen UPDATE/DELETE
programs now match exactly, including raw DONE tokens, metadata and readback;
comparison evidence is in
`artifacts/compatibility/output-update-delete-errors-fixed.json`. A full frozen
Linux verification is being started for this final failure-context snapshot.


## Deterministic paired UPDATE acquisition plan

`msduck-sql::output_update::plan` now builds a capture query and a write AST from
an explicit stored-column snapshot, private relation name and alias, and an
UPDATE whose assignments have already been bound and converted for storage.
Capture retains every old column alongside its new value and original physical
row identity. The write reads only captured assignment values and joins the
original identity during mutation. It does not rerun the predicate or volatile
assignments and never joins old identities to post-update row identities.

This follows the useful snapshot idea in mssqlite's `updateWithOutput`, but
replaces its post-update rowid join, which the DuckDB experiments disproved.
The pure plan leaves its input AST unchanged. It rejects duplicate/unknown
assignment columns, physical-rowid shadowing and unresolved joined or limited
row selection. Joined-source selection, generated columns and DEFAULT expansion
must be resolved before extending or invoking this primitive.

The SQL-crate tests pass, and `tests/output_images.rs` executes the plan against
DuckDB: key changes preserve old/new pairs, simultaneous assignments read old
values, a sequence-backed assignment runs once per row, and a duplicate-key
failure rolls back both writes and the transaction-owned image table. This is
plan/native-adapter evidence, not server OUTPUT support. Logical output binding,
projection rewriting, image ownership and protocol integration remain necessary
before UPDATE OUTPUT deleted/inserted can use it. Partial rows on failure and
statement undo inside explicit transactions remain unresolved.

Formatting and SQL-crate Clippy passed. Workspace Rust tests and workspace
Clippy are running for the plan snapshot. The ongoing Linux verification covers
the preceding failure-context snapshot and does not contain this new plan.

The acquisition-plan snapshot completed all 413 workspace Rust tests and strict
workspace Clippy. Projection binding added afterward is verified separately.


The paired-image plan now also rewrites bound OUTPUT projections onto private
old/new slots. Qualified stars expand in target declaration order; original
column labels and explicit aliases survive rewriting, including quoted names.
Parameters remain AST expressions for the execution adapter to bind. Invalid
image qualifiers and missing image columns retain typed 4104/207 errors.
A separate logical projection describes both images against the original target
for binding and metadata; its empty predicate prevents accidental acquisition.
Private materialization columns must never substitute for those declarations.

The native regression now executes the generated output projection rather than
a hand-written SELECT over private slots. SQL-crate tests, that native test,
formatting and strict workspace Clippy pass. Full workspace tests are running
for this projection snapshot. Server integration still requires choosing the
paired plan before native OUTPUT lowering, preserving logical fields, building
capture only after assignment conversion, binding projection parameters, and
owning capture/write/projection cleanup in the statement transaction. Joined
sources, generated values and failure-row streaming remain part of the broader
OUTPUT implementation scope.


## Paired UPDATE runtime integration

The server now selects paired acquisition for unjoined UPDATE OUTPUT that reads
`deleted`. Preparation binds a target-only version of the projection against
same-typed target declarations, retaining aggregate/window rejection without
executing the write. Runtime acquires stored columns from the native table
catalog, rejects generated columns, and creates the deterministic plan after
assignment conversions. A bound, empty capture query describes the temporary
relation before output projection preparation. The real capture runs once;
Arrow moves the paired values into the image relation, the write consumes the
captured assignments, and the projection reads paired slots with bound values.
The existing result/sink path retains original logical fields and UPDATE counts.
Cleanup belongs to the enclosing autocommit statement transaction.

`reference/output-paired-images.json` contains eight new SQL Server programs.
All eight match exactly, including raw completion tokens and readback: key
changes, simultaneous assignments, DEFAULT, local parameters, typed empty
output, INTO, a CTE input and quoted image wildcards. Evidence is preserved in
`artifacts/compatibility/output-paired-images-paired.json`. The original OUTPUT
matrix gains matching old/new UPDATE cases 2, 3 and 8. Its canonical results and
readback now match 12/16; including raw tokens, 9/16 match. Three previously
known compile-diagnostic command differences explain that distinction. Joined
UPDATE/DELETE source images, the partial row before a failed UPDATE and isolated
surrogate storage remain the other original matrix gaps; no differences were
normalized away in `artifacts/compatibility/output-dml-paired.json`.

The root regression checks zero writes during preparation, key changes, INTO
and private-image cleanup. The permanent client test covers the eight programs
and repeated prepared executions, including an empty result. Linux build and
local focused root tests passed. Full workspace and focused client verification
are running for this integrated snapshot.

The earlier frozen failure-context snapshot completed 410 Rust tests, 386 client
tests and 319 audit captures. The separate local run completed 385 clients and
319 audit captures across changing snapshots. Its raw audit comparison matches
318 Linux cases; `derived table apply` returns its two unordered rows in the
opposite order. This remains recorded in
`artifacts/compatibility/output-errors-local-audit-diff.json` and is not treated
as exact equality.

The integrated snapshot passed 415 workspace Rust tests, formatting and strict
workspace Clippy locally. All seven focused OUTPUT client tests passed on Linux,
including repeated prepared paired-image executions. Added rollback assertions
for duplicate-key and sink NOT NULL failures are being verified separately;
the final frozen Linux verification is running with those assertions included.
The two final focused root tests also passed, including both new rollback
assertions. Frozen Linux Rust verification passed all 415 tests; full clients
and the compatibility audit remain running.


## Joined-source acquisition evidence

`reference/output-joined-images.json` records ten live SQL Server programs:
unique and duplicate UPDATE matches, sink routing and an arity error, key-changing
parameterized updates, LEFT JOIN null extension, a CTE source, empty input, and
unique/duplicate joined DELETE matches. All ten currently differ from the server
(`artifacts/compatibility/output-joined-images-before.json`); joined-source OUTPUT
is still unsupported. The generator is retained in
`artifacts/compatibility/output-joined-images-reference.mjs`.

Both duplicate-match reference programs emitted one row for the affected target
and preserved the selected source in output. The captured conflicting choice
was extra=7 rather than 9. That particular choice is an observation, not a
contract: Microsoft's [UPDATE documentation](https://learn.microsoft.com/en-us/sql/t-sql/queries/update-transact-sql?view=sql-server-ver17)
states that conflicting FROM matches leave the chosen update value undefined.
Raw reference values remain intact; future comparisons must not normalize away
such differences or mistake one observed choice for a guaranteed selection rule.

The native regression in `tests/output_join_images.rs` proves a two-stage
acquisition mechanism. First, materialize one joined input per original target
identity, retaining target/source structs. Then evaluate converted assignments
over those selected inputs and materialize paired images. The test deliberately
orders candidate choices to make its instrumentation reproducible; production
SQL need not promise that ordering. It verifies two sequence evaluations for
two targets despite four joined inputs, no evaluation of an error expression on
discarded matches, key-change pairing, source-value retention after source-table
mutation, and rollback of both writes and private relations.

The native regression passes. This is not joined OUTPUT implementation. The
next deterministic plan must preserve the original join tree (including outer
join null extension), resolve the target relation before acquisition, exclude
NULL target identities introduced by outer joins, bind source column references
to explicit captured fields, and select candidates before evaluating assignment
expressions. UPDATE then uses paired values; DELETE can use the selected old
images directly. Logical result declarations must remain separate from private
struct storage, and target/source alias or column-name collisions must be bound
explicitly rather than relying on DuckDB's whole-row alias shorthand used by
this controlled experiment.

The joined-acquisition test snapshot passed all 416 workspace Rust tests,
formatting and strict workspace Clippy locally. This adds native test coverage
without changing server execution; the preceding paired-runtime frozen Linux
client/audit verification continues separately.


## Deterministic joined candidate plan

`msduck-sql::output_join::plan` now constructs the first acquisition stage from
an explicit WITH clause, unchanged FROM join tree, predicate, qualified target
identity and qualified source-column references. Its input cannot contain
assignment expressions in captured slots. It keeps one input per target through
ROW_NUMBER partitioning, without promising a source ordering, and filters NULL
target identities before selection. This preserves unmatched real targets in
outer joins while excluding unmatched source-only rows. Column slots and their
case-insensitive reference lookup are explicit; whole-row alias shorthand is
not used. Input CTEs, parameters, quoted names and outer-join structure survive
unchanged.

Pure tests verify those boundaries and reject unqualified/duplicate captured
references. A native regression executes the generated query for a FULL JOIN
with duplicate matches, an unmatched target and an unmatched source. It confirms
two selected targets, the NULL source for the unmatched target, exclusion of the
source-only row, exactly two volatile assignment evaluations, paired key-changing
writes and rollback. Both native joined-acquisition tests pass. Full workspace
Rust and Clippy verification are running for this plan snapshot.

This is still an acquisition primitive rather than joined OUTPUT support. The
adapter must resolve target/source scopes into the explicit column snapshot,
materialize the candidate stage before constructing assignment images, bind
parameters separately for each stage, rewrite bound source references using the
slot map, and retain logical OUTPUT metadata and destination preflight. It must
not reuse a parameter vector from the original UPDATE when the candidate query
contains only the predicate's parameter subset. Existing frozen paired-runtime
verification continues independently and excludes this new primitive.

The joined candidate-plan snapshot completed all 419 workspace Rust tests,
formatting and strict workspace Clippy locally. Joined OUTPUT execution remains
pending adapter integration.


## Joined UPDATE target resolution

Eight additional SQL Server programs in `reference/output-join-targets.json`
cover target aliases, base names with aliased FROM references, schema-qualified
names, ambiguous self joins, the unique unaliased self-join target, a writable
derived target, a missing target and an independent target absent from FROM.
All eight currently differ from the server; baseline evidence is preserved in
`artifacts/compatibility/output-join-targets-before.json` and the reference
generator in `artifacts/compatibility/output-join-targets-reference.mjs`.

`msduck-sql::output_target::resolve` now resolves these relation choices over an
explicit name-to-object-ID snapshot. It gives visible aliases priority, uses
object identities to recognize differently qualified references, and selects
the sole unaliased reference when the object appears multiple times. Ambiguous
objects retain reference error 8154, and missing targets retain 208. Independent
targets are prepended once. Existing inner/outer/nested join trees and predicates
are not flattened or moved. Derived target aliases remain derived relations;
resolving their writable base and propagating a hidden row identity is still a
separate requirement, not evidence of derived-target execution support.

The pure resolver tests pass. The native FULL JOIN regression now uses this
resolver before candidate planning: UPDATE names the base table while FROM gives
it an alias. It still selects two real targets, excludes the unmatched source,
retains the unmatched target's NULL source and evaluates assignments twice.
Full workspace verification is running for this snapshot.

Integration must also replace the checked-clone canonicalization in
`msduck-sql::batch::parse` and DML scope setup in `aggregate_columns`, both of
which currently reject outer joins through the old flattening path. Resolver
tests intentionally use the raw server dialect AST to exercise the new boundary;
production batch validation remains intact until joined execution can consume
that boundary correctly. Joined OUTPUT is not yet enabled by this change.

The frozen paired-UPDATE runtime verification completed: 415 Rust tests, 387
client tests, formatting and strict Clippy passed, with 320 audit captures.
All 319 previous Linux captures are unchanged; one paired-image probe was added.
The raw comparison is `artifacts/compatibility/output-paired-runtime-audit-diff.json`.
That snapshot predates joined candidate/target primitives.

The target-resolution snapshot passed all 421 workspace Rust tests, formatting
and strict workspace Clippy locally.


## Rebinding joined assignment inputs

`output_join::Plan::rebind` now returns a scalar AST whose canonical qualified
source references address the selected candidate's private slots. It leaves
local/global parameters available for per-stage binding and preserves date-part
keywords. Missing columns and qualifiers retain typed diagnostics. The input AST
is unchanged even when a later reference fails. This primitive expects prior
column resolution; it does not guess which source an unqualified name denotes.

Subqueries require separate lexical scope binding and are explicitly rejected
by this rebinder. An inner alias that happens to match a captured outer alias
must not be rewritten by a flat visitor. This restriction belongs to the current
primitive; full joined-assignment support still requires scope-aware subquery
handling rather than treating that restriction as completed compatibility.

Pure tests cover parameter preservation, keyword handling, quoted identifiers,
unknown names and failure without input mutation. The native FULL JOIN regression
now obtains its assignment expressions from the original UPDATE and runs the
rebinder before evaluating them over selected candidates. Both native joined
acquisition tests pass, including key changes, NULL source retention, volatile
single evaluation and rollback. Full workspace Rust and Clippy verification is
running. Server joined OUTPUT integration and the combined old/new/source output
binding remain unfinished.

All 422 workspace Rust tests passed. Strict workspace Clippy and the focused
rebinding regression passed after an equivalent boolean simplification requested
by Clippy. Joined server execution remains pending integration.

## Catalog binding for joined row capture

The root `output_join::bind` API now acquires object identities and logical
catalog declarations, resolves the UPDATE target, and constructs the candidate
capture plan without executing user expressions or creating image tables.
Parameter declarations flow into CTE and derived-source metadata independently
of their current values. Captured fields follow source and declaration order.
Canonical qualifiers preserve AST identifier boundaries, including quoted aliases
and column names containing dots.

The binding retains two distinct metadata views: target declarations for old/new
images and source fields with outer-join null extension. It checks those target
declarations against physical storage before planning row identity capture.
Derived and CTE targets still need writable-projection resolution; a CTE that
shadows a real table is explicitly rejected rather than bound to that table.
Generated targets, user columns shadowing rowid, unresolved sources, and ordered
or limited updates also require further planning support.

Native tests check preparation without writes or temporary image tables,
non-evaluation of a volatile CTE expression, retained NVARCHAR parameter width,
quoted identifier boundaries, and metadata surviving source-table removal.
Executing the generated FULL JOIN capture produces one candidate per real target,
retains an unmatched target with NULL source values, and discards unmatched source
rows. This API remains separate from server dispatch: joined assignment execution,
combined old/new/source OUTPUT binding, and writable derived targets are still
unfinished. The upstream mssqlite UPDATE-with-deleted implementation rejects FROM,
so it does not supply the missing joined execution path.

## Paired images from joined candidates

`output_update::from_candidates` now builds the second acquisition stage and
the write from a materialized candidate relation. `output_join::Binding::images`
connects catalog-bound target declarations to this deterministic planner.
Assignments must already have canonical column references and storage conversions.
The paired capture reads old values and assignment inputs from candidate slots,
retains unchanged columns, and carries all selected source slots into the image.
The write consumes new values and uses original row identity only to locate the
target during that write. It does not re-run the original join or predicate.

Native coverage now executes generated plans for both stages and the write:
FULL JOIN null extension, duplicate source matches, volatile assignment evaluation
once per target, key changes, retained source values after source-table mutation,
old/new wildcard projection, and transaction rollback. A catalog-backed test
drives the same path from the original UPDATE and real declarations. Pure tests
check retained parameters, unchanged-column images, no repeated selection,
and rejection of missing or unbound input columns without mutating the input AST.

Server dispatch still uses the earlier unjoined path. Combined old/new/source
OUTPUT projection, general scoped assignment binding (including subqueries),
and staged parameter binding remain prerequisites for completing joined execution.

The paired-candidate snapshot passed all 427 workspace Rust tests, strict
workspace Clippy and formatting. An initial local run exhausted disk space during
debug stripping and left three truncated test binaries; those binaries and stale
Rust caches were removed before the successful full retry. DuckDB native caches
were retained. Evidence is recorded in
`artifacts/compatibility/output-joined-pairs-verification.json`.

## Combined old/new/source projection

Joined paired plans now retain a canonical source-column map for OUTPUT.
Projection supports old/new image references alongside captured source columns,
mixed scalar expressions, qualified source wildcards, aliases and parameters.
Source stars retain declaration order and quoted identifier boundaries. The
target's own FROM qualifier is excluded: its images must use inserted/deleted.
Those image names take precedence over similarly named source aliases. Missing
columns, qualifiers and wildcard prefixes return typed 207/4104/107 errors, and projection leaves caller
ASTs unchanged on failure. Subqueries require separate validation and are rejected
at this projection boundary rather than rewritten across scopes.

`Binding::output_fields` infers logical result fields from its owned catalog
snapshot and OUTPUT scope. Target images retain original declarations; other
source fields retain join nullability. This inference needs no live tables and
does not derive metadata from private image slots. Tests check NVARCHAR parameter
width after source tables are dropped, image/source nullability, labels, and a
native mixed OUTPUT projection after the underlying source values have changed.

These APIs complete physical projection for already-bound scalar expressions.
General scoped assignment binding, staged parameter translation, preparation,
error/completion handling and server dispatch still need to consume the joined
pipeline before it constitutes a server feature.

Seven SQL Server 2025 reference programs are captured in
`reference/output-joined-projection.json`, including metadata, completion tokens
and ordered readback. They confirm image-name precedence over source aliases,
source wildcard ordering, quoted names containing dots, and rejection of target
alias references in OUTPUT. The reference distinguished invalid wildcard
prefixes (107, severity 15) from unknown column qualifiers (4104), correcting the
planner's initial wildcard diagnostic. The syntax follows the
[Microsoft OUTPUT specification](https://learn.microsoft.com/en-us/sql/t-sql/queries/output-clause-transact-sql?view=sql-server-ver17).

The preceding catalog-binding Linux snapshot completed formatting, strict Clippy,
425 Rust tests, 387 client tests and 320 audit captures. All 320 raw audit cases
match the preceding paired-runtime baseline unchanged. This verification predates
the paired-candidate and combined-projection changes; their current frozen run
is recorded separately in `artifacts/compatibility/output-joined-projection-verification.json`.

## Scoped joined assignments

The deterministic `output_bind` pass resolves assignment references against
explicit catalog and lexical scopes. Captured UPDATE inputs become private
candidate slots; inner query columns retain their own bindings. It handles
unqualified and qualified references, nearest-scope shadowing, correlated scalar
queries, derived queries, APPLY, set branches, CTE declaration boundaries,
ORDER BY output names and JOIN ON visibility. Qualifier components remain AST
identifiers, so a quoted alias containing a dot is distinct from a schema path.
Rewritten SELECT columns keep their original labels for enclosing derived queries.

`Binding::images` now uses this pass, and catalog acquisition includes tables
mentioned only in assignment subqueries. Unknown columns, ambiguous names and
missing qualifiers produce typed 207/209/4104 errors without changing caller ASTs.
Unresolved source shapes, query pipes and a collision between a private image
alias and a subquery source remain explicit planning errors. Storage conversion
and backend lowering remain separate responsibilities; this is not server dispatch.

Seven SQL Server scope programs are preserved in
`reference/output-assignment-scopes.json`. Native replay checks all seven programs'
row values, ordered target readback and binding-error attributes. It covers inner
aliases shadowing target aliases, unqualified correlation, derived projection
labels, ambiguous inner columns, and ON references preceding a later alias with
the same name. The first program's OUTPUT rows occur in reverse order in native
replay; both raw sequences are retained in
`artifacts/compatibility/output-assignment-scopes-native.json`. This is native
planner evidence, not an exact TDS or server compatibility comparison.

All 431 workspace Rust tests passed. A new seven-program native replay test and
strict Clippy/format checks passed afterward. The earlier combined-projection
Linux snapshot completed 428 Rust tests, 387 client tests and 320 audit captures.
The latest scoped-assignment snapshot has a separate frozen Linux verification
record in `artifacts/compatibility/output-assignment-scopes-verification.json`.


## Session binding of joined stages

The root session adapter binds candidate capture, assignment images, write and
OUTPUT independently, with an owned parameter vector per stage. Preparation
composes translated ASTs only for DuckDB binding, offsets their placeholder
indexes, and never executes that composed query. Execution must preserve the
materialization boundaries. Original CTE declarations remain available to RHS
subqueries. Changed parameter declarations require a fresh catalog binding.

The generated native QUALIFY predicate is kept outside T-SQL ranking validation;
user expressions still receive normal validation. Adding ORDER BY to its
ROW_NUMBER caused a reproducible DuckDB internal vector-type error in the native
scope replay on Linux, so the candidate plan retains its original unordered
partition. Regression coverage checks that user ROW_NUMBER without ORDER BY is
still rejected.

The adapter test covers independently bound parameters, Unicode values, typed
NULLs, rebinding, prepare-only behavior, volatile evaluation once, and rollback.
This API accepts already validated and typed plans. Normal joined OUTPUT server
dispatch, assignment storage conversions and completion/error integration remain
unfinished. Verification is recorded in
`artifacts/compatibility/output-stages-verification.json`.


## Typed joined assignment preparation

`Session::prepare_joined_output` now acquires the logical binding, expands
compound assignments, binds operands using explicit private-relation declarations,
and applies storage conversions before the inserted image is materialized.
Defaults expand for native-converted types as well as emulated types. Native
casts finish the image conversion for REAL and DECIMAL, so OUTPUT observes the
same rounded values that the target stores. Money-to-character conversion uses
the logical RHS type. Candidate, image and output parameters remain independent.

The pure operand binder accepts explicit relation declarations keyed by identifier
components. CTEs retain precedence, and quoted dots remain distinct from multipart
names. Catalog acquisition and session values remain in the root adapter.

Six SQL Server programs are preserved in `reference/output-typed-stages.json`.
Native tests check the successful conversion program's values, logical character
width, key changes, DEFAULT preparation, overflow before write, cleanup and rollback.
The three aggregate/window compile errors are checked against the reference's
number, state, severity and message; client coverage verifies that preflight
prevents earlier writes in the batch. Raw reference captures retain overflow and
string truncation diagnostics, metadata and completion events. These captures do
not establish full wire equivalence for the new joined path.

Normal joined OUTPUT dispatch and its sink/result/error integration remain
unfinished. The typed preparation API is ready for that integration; it does not
execute a statement or create an image. See
`artifacts/compatibility/output-typed-stages-verification.json` for verification.


## Joined UPDATE execution

Normal SQL batches and prepared requests now dispatch joined UPDATE OUTPUT to
three materialization stages: capture one candidate/source row per physical target,
capture converted old/new/source values, then write using the original identity.
OUTPUT evaluates over those captured images. Source aliases and qualified stars,
outer joins, CTEs, scalar assignment subqueries, key changes, typed parameters and
OUTPUT INTO use this path. The unordered candidate partition does not promise
which source wins when a join supplies conflicting values for one target.

Preparation binds all stages and the optional sink without creating images or
executing expressions. Root-side images enforce the existing 64 MiB limits and
are cleaned up on success or failure. Autocommit uses the existing statement
transaction wrapper. A session test covers FULL JOIN null extension, duplicate
sources, volatile assignment evaluation once, explicit rollback, and atomic
failure on target constraints, assignment overflow and sink constraints.

Result encoding and sink insertion are shared with the existing execution path.
Logical declarations preserve source/image metadata even for empty results and
NULL bindings. Joined-plan compilation failures carry an explicit compilation
context, preventing same-level TRY/CATCH handling and retaining the SQL-batch
compilation completion command. Uncaught described DML failures now emit the
statement-terminated information token and original DML completion command,
while allowing later batch statements. Integer assignment conversion overflow
retains the reference state; caught DML errors reset the affected-row count.

Client checks compare selected zero/one-row reference programs exactly, including
metadata, readback and raw completion tokens, and exercise prepared rebinding with
Unicode and NULLs. The broader 38-program replay preserves every raw difference,
including unordered multirow output. The current replay has 31/38 canonical
matches and 28/38 matches including raw completion tokens and readback; these are
observations of this capture, not an ordering guarantee. Ten additional SQL Server runtime-error
programs are in `reference/output-joined-runtime-errors.json`; the first eight
match complete runtime streams, including TRY/CATCH and NOCOUNT. Float and text
conversion error details remain different in the last two captures.

Remaining scope includes joined DELETE, writable derived/CTE/view targets,
generated columns, pruning unused computed source inputs, partial result delivery
on failure, complete constraint/truncation diagnostics, syntax-only compilation completion
commands, and statement undo inside explicit transactions. The current raw replay and verification are recorded in
`artifacts/compatibility/output-joined-dispatch.json` and
`artifacts/compatibility/output-joined-dispatch-verification.json`.


A supplementary raw UTF-16 write check exposed an existing shared-storage gap:
NVARCHAR/NCHAR columns declared through CREATE TABLE use DuckDB VARCHAR, while
raw surrogate units use the explicit Unicode struct carrier. Writing that carrier
currently stringifies it and can raise a misleading truncation error. Four SQL
Server write programs are preserved in `reference/unicode-character-storage.json`,
with native observations in
`artifacts/compatibility/unicode-character-storage-local.json`. The audit now
retains this failing storage probe. A raw surrogate OUTPUT parameter itself
round-trips through joined prepared execution; persisting those units in declared
character columns remains required work and is not covered by the passing joined
execution tests.
