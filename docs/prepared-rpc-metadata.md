# Prepared RPC metadata

`sp_prepare` descriptions use the original SQL AST, an explicit catalog snapshot
and parameter declarations. Preparation validates native binding but never steps
the statement. Bound values, returned rows and volatile expression evaluation do
not supply metadata. Effects stay in the root RPC adapter.

The retained fixture `reference/prepared-rpc-metadata.json` contains two matching
fresh pinned SQL Server 2025 captures, with 50 records per run: version/setup and
eight profiles across six preparation variants. Its SHA-256 is
`3018bee5ac4d4bf2019c4238e174d1f83e60842bbcb986682722961be098d2d0`.
Run `node scripts/capture-prepared-rpc-metadata.mjs --check` to validate it.
The generator refuses to replace the immutable fixture.

The profiles cover COUNT/COUNT_BIG, ROW_NUMBER, typed columns/constants, empty
results, INSERT, multiple results and seeded RAND. Each retains preparation,
four executions using NULL/0/9/1, unprepare and before/after state probes.
Omission and explicit option 1 produce metadata; typed RPC options 0, 2 and NULL
produce error 214/state 3, return status 214 and a NULL handle without allocating
one. Two SELECT results produce status 8182 with a valid reusable handle and no
preparation metadata. INSERT preparation emits completion without inserting.
Preparation does not advance the captured RAND stream.

Known single-result declarations produce COLMETADATA, any proven ORDER token,
DONEINPROC, RETURNSTATUS, RETURNVALUE and DONEPROC in captured order. Types,
capacity, scale, precision, flags and collation come from declarations. Unknown
result types or ORDER plans are rejected explicitly. Other non-result/control
flow shapes retain their existing preparation path; their descriptions remain
unproven. This is not complete prepared-statement compatibility.

Run the dedicated tests explicitly:

```
node --test tests/prepared_rpc_metadata_reference.test.mjs tests/prepared_rpc_metadata.test.mjs
```

The public test compares complete preparation responses and execution rows and
descriptors, including NULL and empty results. It retains complete raw responses
in `artifacts/prepared-rpc-metadata/runtime.json`; execution ORDER/completion and
unprepare differences remain separate follow-up work. The version probe also
retains the unsupported SERVERPROPERTY response instead of treating it as a
SQL Server version. The reference captures still require the pinned version.
The package test inventory is separately claimed, so these tests are invoked
explicitly alongside the default clients.

SQL invocation of preparation, wider control flow, session diagnostic-state
effects, typeless NULL error precedence, all unproven ORDER shapes and wider
type coverage need further ground truth and integration. `sp_prepexec` and
`sp_unprepare` keep their existing execution behavior. No changes to engine.rs,
root module registration or catalog acquisition are part of this task.

The `--regressions PATH` capture mode retains a separate preparation-only
plan of 23 profiles across API-default and named option 1. Two fresh matching
48-record runs are retained in the Linux ignored artifact
`artifacts/prepared-rpc-metadata/regression-reference.json` (SHA-256
`5f71ef2b908936b40784b5f2b59c56a8fdf5ffd2d3c7b5daae7d5e71d229563c`). The public scalar regression test embeds the complete
preparation responses for calendar, mixed BIT/integer bitwise, catalog, identity, JSON-presence and bare
NULL projections from that capture. Its smaller plan asserts consecutive handles
starting at 1; every other preparation response field is compared verbatim.
Both plans verify preparation leaves table rows, seeded RAND and transaction
depth unchanged. INSERT OUTPUT INTO retains its captured no-result completion
command without performing either write. Single-table CTE DELETE uses the bounded
root adapter described below and retains the captured no-result command 196.
INSERT OUTPUT uses the existing pure logical projection over target catalog
declarations and retains the INSERT completion command. Other captured regression
shapes remain implementation work, including wider ORDER and temporal
derived declarations. The preparation ORDER fallback preserves existing proven
plans and uses original typed source identities for direct columns, projected
INT-to-variant casts and captured SUM keys. It rejects unresolved/computed keys,
ambiguous aliases and hidden DISTINCT keys. Captured INT-column variant casts
retain preparation fComputed even though execution inference omits it.

DATETIME2 conditionals use explicit contributing declarations: ISNULL retains
the first argument scale, while CASE/COALESCE use the largest known DATETIME2
scale. Any unresolved or mixed-family contributing branch remains a barrier.
The retained ISNULL scale-2/scale-7 response is compared verbatim. For a VALUES
source whose every column has proven DATETIME2 contributing declarations, the
metadata-only clone represents the common scale with a typed NULL declaration.
The original row source and execution AST are unchanged. Unknown/mixed-family
columns prevent this representation; wider set and mixed-column inference remain
unresolved. The complete captured derived VALUES response is also asserted.

A bare projected NULL is declared INT, while NULL function operands retain their
original declaration barriers. VARBINARY(MAX) uses the existing PLP binary codec.


## Prepared single-table CTE DELETE

Companion task #704 adds `src/rpc/prepare_delete.rs` and the immutable
`reference/prepared-cte-delete.json` (SHA-256
`82b301242462bc0ce227173523a96d4b06b15ddf85ec1c8c2fdd9c4ad0120a81`). Two fresh
pinned SQL Server runs agree on all ten records per run: version/setup and plain
or qualified source profiles, through API/named preparation, reuse and rollback.
The capture mode is `--cte-delete PATH`.

The adapter accepts one nonrecursive CTE over a plain single-table wildcard
projection, optional source alias and predicate, with no other query/DELETE
modifiers. It preserves the base relation, alias and predicate once as AST nodes.
A same-named unqualified source remains a self-reference barrier. The existing
native binder validates the equivalent DELETE without stepping it before a
handle is allocated. Original logical SQL supplies preparation framing; the
validated transformed SQL is cached for execution and its actual byte length
counts toward capacity. Wider shapes retain their original binding behavior.
Ordinary batch CTE writes and `sp_prepexec` are not adapted here.

At implementation checkpoint `45d61e5`, all 32 complete prepared DELETE execution
responses match SQL Server verbatim. Parameters NULL, 9, 1 and 0 produce counts
0/0/3/1 for reuse and 0/0/3/4 when each execution is rolled back. All preparation,
unprepare, before/after table rows, descriptors and transaction depths match.
The complete raw runtime comparison is retained in
`artifacts/prepared-rpc-metadata/cte-delete-runtime.json`.

The strict rollback test still fails on ordinary SQL transaction wire framing:
BEGIN/ROLLBACK advertise DONE command 0 instead of the captured 212/210. These
are tracked in owner-approved backlog issue #705; no transaction semantics are
normalized away. Surrounding ordered row snapshots also retain the existing
missing ORDER / ROW instead of NBCROW gaps. The public comparison remains
failing on those exact surrounding responses, so this checkpoint is not merge
ready and does not establish complete CTE or transaction compatibility.

The `--declarations PATH` mode captures preparation-only variant property calls,
conditional/extrema variants, grouped cast ordering and temporal VALUES/conditional
shapes in two fresh pinned containers. It retains complete responses and verifies
that preparation leaves rows, random state and transaction depth unchanged.

The declaration capture at `190911e` retains 20 records/run in
`artifacts/prepared-rpc-metadata/declarations-reference.json` (SHA-256
`8be6f5f5f0026a5b5b013b9bb79d9230485b9b30e3597da68116751479069542`).
SQL_VARIANT_PROPERTY has a fixed SQL_VARIANT return declaration for BaseType,
Precision, dynamically supplied property names and NULL inputs. Preparation
retains nullable/computed flags for every captured property result; the runtime
payload never chooses the compile-time type. Captured DATETIME2 COALESCE retains
fComputed as well. Complete API/named preparation responses are embedded as
additional goldens. Variant conditional and non-window MIN/MAX declarations
also use original explicit casts and resolved catalog fields. Every contributing
conditional operand must prove a supported variant/integral/BIT family; unknown
or other families and COUNT-containing trees remain barriers. Known original
properties survive enrichment, including captured aggregate flags. Grouped cast
ORDER and wider set/window variant shapes remain in the raw capture for further
integration.

Temporal set declarations are retained in `reference/prepared-temporal-sets.json`
(SHA-256 `9b000b8cc9402b1ca396bf8b5cc4c830b15bba6ba562bb71f7d510cee43fd1c1`).
The `--temporal-sets PATH` generator captures 22 complete records per run in two
fresh pinned SQL Server containers. Same-family DATETIME2/DATETIMEOFFSET sets
choose the larger fractional scale; the mixed captured UNION ALL chooses
DATETIMEOFFSET with both operands contributing scale. Shared deterministic set
inference now derives these declarations from explicit types and catalog inputs,
including VALUES columns, without the former root clone that collapsed VALUES
rows. Unknown types, aliases and invalid/missing scales remain unresolved;
temporal arithmetic remains unsupported by the numeric arithmetic rule.

Pure tests check captured declarations, all scale pairs and both operand orders,
including mixed temporal/numeric VALUES columns. Reference integrity tests check
the complete plan, independent agreement, nonexecution and raw completion tokens.
A separate public test compares every preparation field verbatim: SQL Server's
captured INTERSECT/EXCEPT flags are 33 versus UNION's 1, so declaration inference
alone is not evidence of complete wire parity.

The companion `reference/prepared-set-properties.json` retains 104 records/run
from two fresh captures (SHA-256
`fc4d1ee4fe991cca987be3d261f01da58e0c59032a2a35825c28fe2993aa9e50`).
Reproduce with `--set-properties PATH`; this mode adds a separate NOT NULL source
table while leaving the base fixture's setup unchanged. Captured EXCEPT retains
left nullability and provenance; INTERSECT retains left provenance but cannot
emit NULL if either input is declared NOT NULL. UNION combines nullability with
derived provenance. The shared projection rule now distinguishes these operators,
including nested set boundaries. Fixture-backed pure and complete public response
tests cover the captured nullable/nonnullable source, expression and temporal
profiles. Unknown properties remain unknown unless explicit input declarations
prove a nonnullable intersection; row values never establish that proof.

Full verification at a45fee6 exposed a backend regression when temporal AST
results were returned by the numeric `set_type` helper: native numeric set
lowering emitted an unsupported DuckDB DATETIMEOFFSET cast. Temporal merging is
therefore restricted to catalog-based `set_info`; the numeric AST helper keeps
its existing contract. Pure all-scale tests assert that separation, and the
workspace offset-set regression exercises the downstream native consumer.

At ca2beac, exact-head verification passed all1102executed Rust tests (one
pre-existing ignored), formatting, strict workspace Clippy and all-targets build.
Ten of eleven dedicated tests passed: base/scalar/temporal/set-property full
preparation comparisons and reference integrity passed; strict surrounding CTE
transaction completion still failed. All three targeted temporal clients passed.
Default clients passed485/504, failed19, skipped0 in370.9seconds with unchanged
revision, binary and inputs. Raw audit is preserved separately at
`/tmp/msduck-prepared-metadata-audit-ca2beac.json`; it is diagnostic evidence only.

The `--window-declarations PATH` mode captured 22records/run at bc4c07c in two
fresh containers, with matching full captures (SHA-256
`1a0e8940997357e2194097e7ea664b6bbf026376861019f55cdc1fa9bf92a631`).
Complete records are embedded verbatim in the public window preparation test.
Variant COALESCE keys/operands retain fixed INT COUNT and BIGINT COUNT_BIG /
ROW_NUMBER declarations. Typed NULL COUNT is valid; bare NULL retains8117/8180.
The metadata-only clone now uses explicit catalog scopes for each nested query,
so an inner unknown or FLOAT source cannot inherit an outer SQL_VARIANT proof.
Known conditional variant declarations may enrich COUNT partitions/operands;
COUNT-containing trees and bare NULL remain barriers. Captured ROW_NUMBER
conditional-key declarations additionally require valid function/window shape,
proven supported key declarations and a resolved source column in each order key.
The original query and execution operands remain unchanged; no values or volatile
operands are evaluated for these declarations.

The supplemental `--order-declarations` mode captures nine variant ORDER shapes
in two fresh SQL Server containers without executing the prepared statements:
integer casts of declared variant sources, alias/position order, grouped casts,
conditional variant keys, a hidden arithmetic key, DISTINCT and UNION/UNION ALL.
The complete raw artifact is retained separately with SHA256
`09a1e9cd0762f0e64bf6c13011bcdd902b9304d3f113b71065ba7db953def6bf`.
All 18 preparation/unprepare records are embedded verbatim in the dedicated
public regression, including return handles, metadata flags, ORDER bytes,
completion tokens and nonexecution state. These are reference evidence; adding
the regression does not establish that the current server passes it.

The separate `--order-properties` controls retain eight profiles in two fresh
SQL Server runs, including no ORDER, projected variant keys, hidden columns,
hidden arithmetic and projected arithmetic. Full raw SHA256 is
`b19c9afb3f8caf06b62d8f23e0948a2bf1bbe028bae490578531ee14a04522ed`.
All 16 preparation/unprepare responses are embedded verbatim in the public
provenance regression. These controls distinguish a direct INT-to-variant
projection with flags33 from one with a hidden ORDER key and flags1.
Resolved arithmetic identity compares explicit source fields and integer
literals, preserving projected ordinals across qualification and parentheses.

The `--bitwise-declarations` mode captures six BIT-pair, mixed integer-width,
complement, stored/empty and derived-source profiles twice in fresh SQL Server
containers. Full raw SHA256 is
`bce0d8c21056abc81ddde716f932a0e4feb5be42882efd8e8cde1d03aa8c86fa`.
All twelve complete preparation/unprepare responses are embedded verbatim.
BIT/BIT binary operators prepare as BitN, while a BIT/integer pair retains the
integer operand's width. Declarations use explicit parameter and catalog types,
with aliases, approximate types, untyped NULL and unresolved sources remaining
unknown. This preparation evidence does not establish execution wire parity.

The `--ranking-declarations` mode retains six direct, partitioned, empty,
derived, conditional and named-window PERCENT_RANK/CUME_DIST preparations
in two fresh SQL Server runs. Full raw SHA256 is
`28437ac7b677f91f08c5f8556369f4dda0758a2e7b0d9c3e7e82d5177ca30f6b`.
All twelve complete responses are embedded verbatim in the public regression.
The valid window calls have fixed FloatN(8) declarations even for empty input;
source/native validation precedes metadata enrichment. Conditional expression
provenance remains computed, while direct and derived ranking outputs retain
their captured derived flags. No window operand is evaluated for metadata.

The `--grouping-order` mode retains ten integral conditional/grouping profiles
in two fresh SQL Server containers. Full raw SHA256 is
`1ea40d233fa2133498b3f35bc3a1a26e684062db6c46a327824d0e9ccbf38d08`.
All twenty complete preparation/unprepare responses are embedded verbatim,
including parameter-case and qualified CASE identity, COALESCE/ISNULL,
alias/direct-expression/ordinal sorts, ROLLUP, and row-constant controls.
SQL Server retains ORDER for both captured row-constant conditional controls;
no runtime parameter value is used to decide it. The folded CASE profile also
retains its fixed INT descriptor and flags0; GROUPING has fixed TinyInt flags32.
These controls preserve observed descriptor differences rather than assuming
that all integral results use the same wire declaration.

The deterministic `projection::order::expression_identity` helper compares a
bounded scalar AST using explicit field and parameter identity without mutating
its inputs or evaluating expressions. Unknown/ambiguous/alias declarations,
opaque or volatile calls and nested query scopes return unknown. This identity
proof does not establish a result type or authorize optimizer constant folding.
The RPC adapter separately checks supported declarations and native binding.

The `--catalog-declarations` mode retains eight system catalog profiles twice
in fresh SQL Server containers. Full raw SHA256 is
`37fb1991d65b2da420cb8a5246143dca1317d6b02a68afc3b35e5bffec20da60`.
Both eighteen-record runs agree completely. The empty `sys.columns` projection
has 43 fields, while `sys.identity_columns` has 44; their schemas and flags
cannot be treated as a simple concatenation. Direct catalog integer fields
retain fixed wire types and stored projection flags8, and `name` retains sysname user
id256 and flags9. Identity variant fields have flags33 in the unsorted profile,
while a qualified variant projection with a hidden sort has flags9. Captured
TYPE_NAME is NVarChar(256 bytes), flags33; explicit/TRY integer conversions have
nullable integer declarations and flags33. These captures establish reference
requirements, not a current implementation pass. The complete raw artifact is
retained separately; no catalog fixture or descriptor is normalized.

Canonical root catalog snapshots now retain the captured declarations for both
column views. The backend identity view explicitly projects the same 44-column
order instead of concatenating 43 sys.columns fields and four extra fields.
The immutable preparation fixture is `reference/column-catalog-declarations.json`;
all sixteen complete preparation/unprepare responses pass at Rust head
`1b9f87188c1a549317c03200fe0f11aa52b408a2`, alongside nine catalog and 34 RPC
Rust tests, formatting, strict Clippy and the all-target build. These results do
not cover subsequent client expectation changes or establish execution parity.

The separate `--catalog-batches` mode captures three ordinary empty catalog
queries twice in fresh SQL Server containers. Complete raw SHA256 is
`9311f87f5f59c91dd508590807eeaf8b304799a4f2cda3abff1da65f796cc930`.
Both four-record runs agree. The exact eight-scalar query used by the legacy
sys.columns client retains fixed Int, TinyInt, SmallInt and Bit metadata,
without nullable-type length fields. Its dedicated test compares the whole
response, including flags and DONE. Full-view batch responses are retained in
the same diagnostic artifact; their alias metadata is separate execution work,
and no complete batch-view comparison pass is claimed. The default client now
asserts those captured scalar type/length distinctions while keeping its
allocator, DDL and rollback behavior assertions.

JSON declaration reference: two fresh pinned SQL Server 2025 containers independently agreed on all 18 records per run (SHA-256 `ebc113e6ba79a71a0085b3621a7100b827fc421998bb59e9cfa2dca68d73aa81`). Complete records are embedded verbatim in the public regression. JSON_QUERY retains MAX for a MAX input but reports NVARCHAR(4000) for the bounded column control; JSON_VALUE reports NVARCHAR(4000). Both retain explicit input collation. ISNULL removes nullable metadata in the captured non-NULL replacement case. The invalid literal fails during preparation with error 13609; this is evidence, not a reason to evaluate runtime parameters while preparing. The first runtime replay at `2085a97` retains the complete differences in the JSON artifact: JSON_QUERY preparation is unsupported, JSON_VALUE lacks computed provenance and explicit input collation, and the invalid literal currently returns the unsupported-type diagnostic rather than 13609. All sixteen complete responses remain strict regression expectations; no differences are normalized.

At `f2a0f46fb0aee526cc491893ccfdfe7bc7f56943`, formatting, strict workspace/all-target Clippy, all-target build and all nine focused metadata tests pass. Twelve of the sixteen complete JSON preparation/unprepare/state observations match the retained reference, including explicit BIN2 labels/flags, bounded versus MAX declarations, empty input and ISNULL. The two CASE profiles still have unknown declarations, and the two malformed-literal profiles still have the unsupported-type diagnostic. These four raw differences remain strict failures. Independent owner-run review found no confirmed findings; full workspace and existing JSON client verification are running.

JSON compilation error controls: two fresh pinned SQL Server captures independently agree on all 26 records per run (SHA-256 `a2b7366c69f7e7b82911631642f83edaccf45946ac0f6b7e5a2d9ed665cbddcb`). Both empty-input and dead-CASE controls preserve literal compilation errors; malformed path diagnostics precede malformed documents. Literal failures emit ERROR followed by DONEPROC(command 224, status 2), with no return status or handle. The adapter uses the existing deterministic JSON parser only for literal document/path operands after native binding; parameters, casts, columns and volatile operands remain unevaluated. Non-ASCII ANSI literals retain their code-page conversion barrier. Complete expected responses are embedded verbatim; runtime replay of the new implementation is pending.

OUTPUT preparation reference: two fresh pinned SQL Server captures agree on all 16 records per run (SHA-256 `b14687c9f4929571938c89a2e0df5690f9594697b8a07c7198fa7d8ab9cf1a03`), covering joined UPDATE images and CTEs, direct parameter/cast outputs, empty candidates, stored/computed character collation and ordinary UPDATE/DELETE. Preparation retains UPDATE command 197 and DELETE command 196. Joined OUTPUT uses the existing explicit catalog binding/output-field API; ordinary OUTPUT uses the existing deterministic logical projection adapter. Neither path executes DML or changes cached execution SQL. Complete expected responses are retained verbatim; runtime replay is pending.


## Current verified preparation checkpoint

Production revision `4ce3643bd6741b15ebc1cf390e66bd59efc40cfc` passed
formatting, strict workspace/all-target Clippy, all-target build and all 1,116
workspace Rust tests, with one pre-existing ignored test. The 504 default clients
passed 496 and failed eight, with zero skips and unchanged revision, binary and
inputs. The local audit retained all 325 diagnostic cases with zero transport
failures; it did not compare a SQL Server endpoint.

Both strict JSON replays now match all 40 complete preparation responses,
including conditional declarations, input collations, literal errors and
parameter/volatile barriers. Metadata-only character casts can use a proven
built-in numeric operand declaration without evaluating RAND. The core preserves
state14 and bracket position2 for the empty-property path `$.[`; the old default
client assertion still expects a generic state1 message, tracked in #734.

The existing joined OUTPUT client and actual OUTPUT INTO handle/mutation
regression pass. The complete supplemental replay still fails at native UPPER
binding of a private Unicode image carrier, tracked in #735. Its adapter is
reserved by a separate worker. No metadata fallback bypasses that binding error.

The `--source-declarations PATH` capture mode covers mixed variant UNION,
derived DISTINCT, grouped conditional conversion, parameter GENERATE_SERIES and
recursive mixed character anchors. Two fresh pinned SQL Server runs agree on
all 14 records per run (SHA-256 `c0b1ab5828c96eea90e588f02114484ed90ed7304fd9c6a7657fcb13b789e457`).
All twelve complete preparation/unprepare/state controls are retained verbatim
in the strict public regression. The series result uses a non-null fixed INT
descriptor; grouped conversion retains computed flags33; derived hidden variant
keys advertise ORDER ordinal0. These are reference requirements, not a current
implementation pass at the older full-verification revision above.

At `038e57749e2672d692c15cd05a85bb9a13b601c7`, all twelve complete source
preparation/unprepare/state comparisons pass, alongside all six existing variant,
series and recursive-anchor clients. Formatting, strict workspace/all-target
Clippy, the all-target build, four pure ORDER identity tests and twelve root
metadata tests pass. Independent owner-run review found no confirmed findings.
The non-null series declaration carrier is created after generic expression
enrichment; a composed-path regression checks that nullable typed NULL enrichment
cannot replace it. Grouped conditional ordering borrows the original integer
conversion operand through the existing parser marker helper and retains the
resolved-column, built-in variant-family and GROUP BY identity guards. Neither
metadata clone changes execution SQL or evaluates parameter values.

Full workspace/default-client/audit checks of this newer implementation remain
required. The earlier diagnostic gaps tracked in #705, #731, #734 and #735 remain
separate work; the focused pass does not establish complete compatibility.

At `de68fbc1db6119f66a3fbf33ca2e8c0b2b0fffb2`, all 1,119 workspace Rust tests
passed (one pre-existing ignored), as did formatting, strict Clippy and the
all-target build. The broader preparation suite passed 21/23, retaining the
known #705 and #735 failures. Default clients passed 500/504 with zero skips;
APPLY and the obsolete JSON assertion failed, and two servers exited during
startup before SQL execution. Both startup cases passed an exact-revision rerun
with the binary hash unchanged. The full failed run remains recorded. The audit
recorded all 325 cases without transport errors and without a SQL Server
comparison; raw JSON is preserved separately from the progress log.

The `--apply-source-declarations PATH` mode retains six profiles across API
default and option1 preparation, with fourteen records/run in two fresh pinned
SQL Server containers. Both complete runs agree (SHA-256
`09b54e3d3b1c51cce4a9b096deab7e0ed55a07c189c704ded85b4b06739e8027`).
CROSS APPLY retains fixed non-null INT inputs; OUTER APPLY null-extends all
right-side fields. Grouped/chained correlation and both invalid controls retain
complete descriptors, ORDER, errors, return values and completion framing.

At `857a2e9ee1584a7539130dcc9e56d57150723c8a`, all twelve complete APPLY
comparisons pass, along with the existing source, variant-window and literal
JSON error replays. Formatting, strict Clippy, all-target build and all 41 RPC
Rust tests pass; independent owner-run review found no confirmed findings.
Unaliased groups of CROSS JOIN/APPLY are flattened only in a metadata clone.
Native validation and cached execution SQL retain the original join tree.
Unsupported joins and aliased groups remain barriers. Captured failed
preparation emits the primary error, 8180, RETURNSTATUS8180, a NULL handle and
DONEPROC(command224/status2), without allocating a statement. Mapping a native
missing qualifier to4104 requires a unique original column, a later-source
declaration, and no visible left/local declaration; ambiguous or unfamiliar
shapes remain unmapped.

The older default APPLY assertion expects a single error; SQL Server's captured
pair is delivered by tedious as an AggregateError. Its exact two-error assertion
correction and the stale JSON correction await additional owner authorization
over the narrowly claimed default test file. Full verification of the updated
main integration and the latest implementation remains required.


At `395396376cb93b33b0c89e8e88e738797fc6f165`, formatting, strict workspace
Clippy, all-target build and all 1,126 Rust tests passed (one pre-existing
ignored, 152 suites). The dedicated replay completed with 22/24 passing,
retaining only the strict #705 transaction-command and #735 joined OUTPUT
Unicode binding differences. The diagnostic audit recorded 325 cases with zero
transport errors; it does not compare against SQL Server. The raw dedicated log
and actual audit JSON are retained separately. A final default-client run remains
required after authorized corrections to its obsolete assertions.

The `--grouping-errors PATH` mode covers parameter-only, constant-only and
correlated scalar grouping errors, plus a valid local-column/parameter control.
Two fresh pinned SQL Server captures of these profiles and additional missing
column/table probes agree on all fourteen records/run (SHA-256 `e0716ea7e6e69657b13f817d55f8fdae7b2ff827836658d73e4e61b1e4740b8a`).
The eight grouping controls are embedded verbatim in the dedicated replay.
SQL Server emits canonical error164 followed by8180, RETURNSTATUS8180, a NULL
handle and DONEPROC(command224/status2). The valid control allocates handles1/2,
proving that prior failures do not consume handle numbers. The adapter extends
only the already recognized canonical grouping diagnostic for single-query
sp_prepare; unknown errors and other RPC procedures retain their existing path.
All probes preserve table rows, transaction depth and seeded RAND state.
Implementation replay remains pending until the exact new revision is tested.

The additional probes retain unresolved ordinary missing-column and missing-table
preparation differences: DuckDB diagnostic text and incomplete RPC error wrappers
currently differ from SQL Server's207/208 plus8180 responses. They are not
normalized or claimed fixed by the grouping change. Existing default prepared
GROUP BY tests also expect a single error; those assertions require a separately
authorized correction to the captured two-error contract.


At `62521e1a2c8b2f0c8f2246ce7664496010e56a2c`, formatting, strict workspace
Clippy, all-target build and all 1,126 Rust tests passed (one ignored, 152
suites). The dedicated replay passed23/25, including all eight new complete
grouping controls; only #705 and #735 remain failing. The audit recorded325
cases with zero transport errors and no SQL Server comparison. The complete
default inventory finished495/504, zero skipped/todo, not aborted,402857ms;
checkout, test inputs and binary remained unchanged. Eight failures are the
old prepared GROUP BY single-error assertions, and one is the old JSON path
expectation. Both startup-failure cases from the earlier run passed in this full
inventory. Raw client summary, dedicated log and actual audit JSON are retained.

The `--binding-errors PATH` mode replays the entire six-profile binding-error
capture, including the missing-column and missing-table controls. All twelve
complete reference records are embedded verbatim in the dedicated replay.
For simple single-column SELECT from an unquoted ASCII dbo source, the adapter
now requires agreement among the original AST, the native missing-name report
and an explicit read-only catalog snapshot before formatting207/208. A plain
AST template excludes additional clauses, multiple projections, aliases, joins,
CTEs and nested queries that could require different or multiple diagnostics.
A catalog contradicting the missing-name report blocks conversion. Unknown or
unrecognized shapes keep their existing error path; no native report is parsed
as SQL or passed for execution. The wrapper preserves8180, NULL handle,
RETURNSTATUS and DONEPROC without allocating/caching a statement. Root adapters
acquire the snapshot; the diagnostic rule receives it explicitly. Dedicated
replay and workspace verification of this new source-binding change are pending.
