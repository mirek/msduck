# Aggregate diagnostic integration

The database owner in `Server::open` creates and registers one explicit
`statement_diagnostics::Registry`. `server::Connection` pairs a native DuckDB
connection with a clone of that registry. Its `try_clone` retains both parts;
its shared-reference dereference keeps native database adapter calls available.

`Session::new` consumes this wrapper and retains the registry independently of
its native connection field. Server login and TLS paths pass the wrapper through
to the same constructor. No native function reads a mutable current-session
slot. An authentication failure drops the unopened connection normally without
allocating a statement diagnostic context.

`Session::diagnostic_scope` opens an explicit execution context. The caller must
keep its guard alive until query execution and diagnostic collection finish.
Different statements use different scopes, and scope drop unregisters the
ticket. Connection clones share registration, while independent server database
owners have independent registries. A ticket from one database cannot resolve
in another database's callback.

This changes the Rust construction API: `Session::new` and `serve_connection`
accept `server::Connection`, obtained from `Server::connection`, rather than an
unaccompanied `duckdb::Connection`. The wrapper prevents losing execution
services during ordinary connection cloning. `Session::db` remains the native
DuckDB connection for existing adapter code.

## Execution integration under verification

Each statement execution with ANSI_WARNINGS ON allocates an owned scope. After logical binding
and backend lowering, a deterministic AST pass wraps recognized unary aggregate
operands with the observation expression below. Root execution binds the ticket
as an additional BLOB parameter. COUNT(*) remains unchanged. Result metadata is
bound before instrumentation, independently of the ticket and runtime values.

Successful statements append warning 8153 after result tokens and before their
completion token when the scope observed NULL and ANSI_WARNINGS is ON. OFF
suppresses this diagnostic; this does not implement its other arithmetic or
truncation semantics. Errors drop the scope but do not yet retain warnings from
partially executed work.

With ANSI_WARNINGS OFF, execution receives no diagnostic scope, binds no ticket,
and leaves the native aggregate plan uninstrumented, including windows and
scalar assignments. Switching ON takes effect for the next statement. This
avoids diagnostic allocation and frame materialization when warning 8153 is
disabled; it does not change the remaining ANSI arithmetic policy gaps. The
ON path still needs a scalable alternative to materializing window frames.

Instrumentation visits query statements and ordinary INSERT/UPDATE/DELETE
execution. SET and DECLARE scalar subqueries receive the owning statement's
scope explicitly, including checked scalar plans. Persisted definitions must
never retain execution tickets. Aggregates inside stored views and specialized
DML execution paths still require additional integration.

Keep the distinction between all-NULL and empty groups, and preserve diagnostics
when HAVING removes every row. Do not insert a separate NULL-probing query or
evaluate volatile operands twice. Window frames, correlated execution, errors,
cancellation and DML consumers need exact reference coverage. The reference
contract is in [the captured reference](aggregate-warnings.md); this integration
must retain its raw diagnostics and ordering rather than ignore them for a pass.

## Single-evaluation operand mechanism

A native regression proves this backend expression evaluates its operand once:

```sql
list_extract(list_transform([operand], diagnostic_value ->
  CASE WHEN __msduck_observe_null(ticket, diagnostic_value IS NULL)
       THEN NULL ELSE diagnostic_value END), 1)
```

The operand occurs outside the lambda and is materialized as one list element;
the lambda's two references read that element. A 6000-row volatile sequence
probe confirms one evaluation per input row. Native type/value checks retain
integer, DECIMAL(38,10), VARCHAR, binary, TIME_NS and Unicode STRUCT payloads,
including an unpaired surrogate and a typed NULL carrier. This avoids requiring
a second source query or global materialization merely to inspect NULLness.

The AST pass parses only a fixed backend template and substitutes caller-owned
operand and ticket nodes without revisiting inserted expressions. Pure tests
check single occurrence, identifier preservation, DISTINCT, windows and COUNT(*).
The full 126-case client replay compares values, descriptors, diagnostics,
completion state and event order, retaining raw differences under
`artifacts/compatibility/aggregate-warnings-ON.json` and `-OFF.json`.
All 126 captured programs now match exactly, including event order and the
TRY/CATCH completion reset of @@ROWCOUNT. The prepared-execution regression
passes with repeated nullable/nonnullable inputs and setting changes. All seven
character-extrema tests also pass, including the five exact BIN2 reference
comparisons that previously retained 20 missing warning messages. Pure AST
tests and strict workspace/all-target Clippy pass.

These checks cover the captured window and optimizer shapes, not every frame
or rewrite. Partial errors, stored aggregate views and specialized DML remain incomplete;
full workspace/client/audit verification is recorded separately by revision.

At `e10a8cd`, all 634 workspace Rust tests pass. The 325-case local diagnostic
audit changes only by adding 25 warning-8153 messages compared with `5e18a0e`;
that audit is not a SQL Server equivalence test.

The additional 50 boundary programs in [PR #104](https://github.com/mirek/msduck/pull/104)
exposed concrete defects at `e10a8cd`: the pre-window operand observer emitted
three false warnings for NULLs consumed by no frame, and diagnostics were missing
from stored views, ordinary DML, SET/DECLARE subqueries and partial failures.
That replay matched 25/50 programs, with two setup failures and 129 differences
across the remaining 23 programs. It also retains independent descriptor and
error-detail gaps. The original 126-case pass must not be generalized to these
boundaries; this integration remains draft.

## Window frame correction under verification

The initial window correction collects the operand with LIST over the original
partition, order and frame. A singleton lambda binds that resulting frame once,
compares its length with its non-NULL count, and observes NULL elimination only
for values in that frame. It applies the original aggregate to the frame with
`list_aggregate`; COUNT restores zero for an empty frame. The original operand
appears once, outside the lambda. Ordinary grouped aggregates retain their
existing operand observer.

The generated expression passes native tests for unused NULLs, entirely empty
frames, consumed NULLs, empty COUNT, integer/decimal/binary/temporal/UTF16 types,
and exactly 6000 calls from a 6000-row volatile operand. Three pure AST tests
also pass. All 11 focused client tests pass: the original 126 reference programs,
all 10 window-boundary programs, repeated prepared execution and the seven
character-extrema tests. Strict workspace/all-target Clippy and the all-target
build pass. Full Rust/client regression runs and the complete boundary replay
are recorded separately by revision.

This implementation materializes frame values and reaggregates them. Wide
overlapping frames can therefore require substantially more time and memory
than the original native aggregate. It is a correctness correction under
verification, not a completed performance design; bounded-memory staging or
native aggregate observation remains necessary before treating it as ready for
large workloads. The other boundary defects above remain open.

## Bounded COUNT window execution under verification

COUNT windows now use `__msduck_count_frame`, a native aggregate returning
`STRUCT(value BIGINT, eliminated BOOLEAN)`. Its state contains a count, NULL
presence and an overflow flag, independent of the frame width. The ANY input
adapter reads validity only; it never interprets the operand payload. Special
NULL handling retains the distinction between an empty frame and an all-NULL
frame. State combination carries both count and NULL presence.

The deterministic rewrite preserves the original operand, partition, ordering
and frame on this aggregate. A singleton lambda binds its result once, passes
the returned NULL flag to the statement observer, and extracts the count.
COALESCE preserves zero for an empty frame. Observation happens on the returned
frame result, keeping intermediate segment-tree state construction free of
diagnostic effects. COUNT(*) remains untouched. Other window aggregates retain
the LIST implementation and its unresolved wide-frame cost.

Four pure AST tests and eight native diagnostic tests pass on Linux, as does
strict workspace/all-target Clippy. The native tests retain unused/consumed
NULL frames, all-NULL typed operands, 6000 volatile evaluations, and exact
counts across 100000 expanding frames. A generated-expression test counts
6000 non-NULL sequence values and checks both prefix counts and sequence usage;
it makes no assumption about which sequence value is assigned to each row.
All twelve focused client tests, all 640 workspace Rust tests and all 404
standard client tests pass on Linux for the runtime committed at `094a9a7`.
The client run had zero failures or cancellations and took 1072761ms. The
325-case diagnostic capture remains under verification; passing client tests
does not establish equivalence for the unresolved boundary programs below.

## DML and assignment integration under verification

The native session regression passes for ordinary INSERT, UPDATE, DELETE,
SET and DECLARE, with ANSI_WARNINGS ON/OFF and an UPDATE with no target rows.
Assignment evaluators share the caller-owned statement scope instead of creating
a separate warning channel. Preparation does not receive an execution scope.
All twenty exact DML/assignment reference programs pass against the rebuilt
server. The complete focused suite passes all twelve tests, retaining the
original 126 programs, ten window programs, prepared execution and character
extrema. The complete boundary replay now matches 35/50 programs: two setup
failures and 55 differences across thirteen executions remain, with no new or
worsened case compared with `43d9c9d`. This does not yet resolve stored-view
expansion, joined OUTPUT paths or partial-error warning ordering.

The required full CI job runs the focused diagnostic and character-extrema
suites explicitly after `npm test`, with one test file at a time. This checks
the 126 initial reference programs, ten window programs, twenty DML/assignment
programs and prepared-execution isolation, retaining a separate CI log. These
files also remain directly runnable. The complete fifty-program boundary replay
still retains the unresolved failures described above; it is not a passing CI
gate. `npm test` alone does not include the focused suites.

Run the complete boundary comparison with the existing server build:

```sh
MSDUCK_AGGREGATE_BOUNDARY_AUDIT=1 node --test tests/aggregate_diagnostics.test.mjs
```

This command currently fails on the known gaps. It runs all fifty programs,
retains setup errors as well as execution differences, and writes the complete
observations to `artifacts/compatibility/aggregate-all-boundaries.json` before
asserting exact equality. No expected failure is converted into a match. The
full comparison is explicitly skipped in ordinary CI; the implemented subsets
remain mandatory. This replaces the workstation-specific temporary replay.
