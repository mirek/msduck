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

## Remaining work

Warning 8153 is not emitted yet. Registration and ownership alone do not observe
public aggregate inputs. The deterministic binder still needs a plan that
materializes each operand once and places observation according to captured
aggregate semantics. Root execution must allocate and retain its scope, bind
the ticket, collect its flag after execution, and emit the warning after rows
and before DONE under the correct ANSI_WARNINGS policy.

Keep the distinction between all-NULL and empty groups, and preserve diagnostics
when HAVING removes every row. Do not insert a separate NULL-probing query or
evaluate volatile operands twice. Window frames, correlated execution, errors,
cancellation and DML consumers need exact reference coverage. The reference
contract is in [PR #98](https://github.com/mirek/msduck/pull/98); this integration
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

The binder must still construct the expression as AST, preserve original logical
metadata, bind the ticket independently of parameter values, and verify placement
against the reference matrix. This mechanism alone does not prove window-frame,
optimizer, partial-error or warning-completion behavior.
