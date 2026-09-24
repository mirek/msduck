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

Each statement execution now allocates an owned scope. After logical binding
and backend lowering, a deterministic AST pass wraps recognized unary aggregate
operands with the observation expression below. Root execution binds the ticket
as an additional BLOB parameter. COUNT(*) remains unchanged. Result metadata is
bound before instrumentation, independently of the ticket and runtime values.

Successful statements append warning 8153 after result tokens and before their
completion token when the scope observed NULL and ANSI_WARNINGS is ON. OFF
suppresses this diagnostic; this does not implement its other arithmetic or
truncation semantics. Errors drop the scope but do not yet retain warnings from
partially executed work.

Instrumentation currently visits query statements only. Persisted definitions
must never retain execution tickets. Aggregates inside stored views and DML
consumers require additional integration.

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
or rewrite. Partial errors, stored aggregate views and DML remain incomplete;
full workspace/client/audit verification is recorded separately by revision.
