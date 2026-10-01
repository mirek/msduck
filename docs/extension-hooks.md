# Extension hooks

Some SQL Server features are large but mostly self-contained. Application
locks, backup and restore, stored procedures, functions, triggers, MERGE,
temporary tables and similar features each live in a feature module. The
engine calls those modules through fixed hooks, so features can be developed
in parallel without editing `src/engine.rs` or
`crates/msduck-sql/src/dialect.rs`.

Every hook defaults to "decline", so a stub module leaves existing behavior
unchanged.

## Layout

| Path | Owns |
| --- | --- |
| `crates/msduck-sql/src/dialect/ext.rs` | Parse dispatch, the custom-statement carrier and `leading_words` |
| `crates/msduck-sql/src/dialect/ext/<feature>.rs` | The feature's syntax: `parse` and `owns` |
| `src/engine/ext.rs` | The `Feature` trait, runtime dispatch, per-session `State` and `reenter` |
| `src/engine/ext/<feature>.rs` | The feature's runtime: a `Hooks` value implementing `Feature`, and its `State` |
| `src/engine/ext/modules.rs` | Stored definitions of procedures, functions and triggers |
| `tests/compat/<feature>.test.mjs` | tedious coverage; CI runs every file in `tests/compat/` |

The features are:

- `applock`, `backup`, `bulk`, `catalog`, `computed`, `constraints`;
- `conversion`, `functions`, `identifiers`, `json_string`, `keys`, `merge`;
- `outer_dml`, `procedures`, `rowversion_identity`, `temp_tables`,
  `transactions`, `triggers`.

Dispatch follows the order of the `FEATURES` lists in both `ext.rs` files.

## Syntax hooks (`msduck-sql`)

- **`parse(parser)`** runs first in `ServerDialect::parse_statement`, before
  any built-in statement parsing. A feature claims the next statement by
  returning `Some`. Otherwise it returns `None` without consuming tokens; use
  `parser.try_parse` or peek ahead before committing.
- **`owns(statement)`** marks a statement the feature validates itself.
  - The batch parser skips the UPDATE/DELETE target-shape canonicalization
    checks for a top-level owned statement.
  - Preflight still checks variable references. It skips the DDL shape checks
    (protected names, TRUNCATE, schema and view placement, ALTER TABLE forms)
    for owned statements at any depth.
  - For top-level owned statements only, preflight also skips the separate
    validators: window placement, grouping, predicates, OUTPUT, aggregates in
    UPDATE, session functions, REPLICATE, LEFT/RIGHT, unary operators and
    RAISERROR.
  - Those validators still see an owned native statement nested inside IF,
    WHILE, BEGIN...END or TRY. A feature whose statements those validators
    would reject, and which can appear nested, should parse them into a
    carrier.
- **`carrier(kind, payload, args)`** wraps a statement that has no sqlparser
  equivalent, such as BACKUP or WAITFOR.
  - `kind` names the feature's statement.
  - `payload` is an opaque string that the feature encodes, for example as
    JSON.
  - `args` holds expressions that must stay visible to variable checks and
    binding.

  `custom(statement)` decodes a carrier. Carriers are always owned.
- **`leading_words(sql, n)`** returns a batch's first unquoted words in upper
  case, skipping comments. Batch hooks use it to recognize statements before
  parsing.

Preflight treats `@name` in an `EXEC proc @name = value` argument as the
procedure's parameter name, not as a reference to a caller variable. Values
are still checked, so `EXEC p @a = @undeclared` still fails with "Must
declare the scalar variable".

## Runtime hooks (`Feature`)

| Hook | Runs | Use |
| --- | --- | --- |
| `batch_begin`, `batch_end` | Around every SQL batch, RPC batch and nested body run through the engine's batch loop, including batches a `batch` hook claims. Every feature sees both; ends run in reverse feature order | Batch and module scope, such as table variables and temporary tables dropped when a procedure or RPC request ends. Batches nest strictly, so the calls pair up like brackets |
| `batch` | At the start of every SQL batch and RPC batch, before parsing | Statements that must be alone in their batch and keep their source text, such as CREATE PROCEDURE, FUNCTION or TRIGGER. Return the whole token stream, including the final DONE |
| `exec` | For `EXEC` leaf statements, after preflight, in place of the built-in "unsupported" path | System and user procedures. The engine appends `Exec.tokens`, then RETURNSTATUS and DONEPROC, as it does for `sp_set_session_context`. An error becomes the call's error, with status 1 |
| `statement` | At the start of `Session::execute` for every other leaf statement, before database qualification and catalog bookkeeping | Custom carriers, or intercepting DML and DDL. `Ok(None)` continues with the possibly modified statement |
| `rewrite_statement`, `rewrite_expr` | After session functions are lowered, for statements and for scalar evaluations (SET, DECLARE, IF and WHILE conditions, RETURN) | Session-aware lowering, such as `SCOPE_IDENTITY()` or names of temporary objects |
| `lower_expr` | At the end of the translator's post-visit of each expression | Pure lowering to native functions |
| `isolation` | When a SQL or transaction-manager request selects an isolation level | Accept or reject levels. `None` defers to the built-in rule, which accepts read committed and snapshot |
| `transaction_end` | After the outermost COMMIT or any ROLLBACK | Release transaction-owned resources |
| `save_transaction`, `rollback_to` | On a transaction-manager savepoint request, or a ROLLBACK naming something other than the outermost transaction | Savepoints. Return ENVCHANGE tokens; the transaction stays open |
| `register` | Once per DuckDB instance, after the built-in scalar functions | Native scalar and table functions |
| `bootstrap_database` | For every database at startup, creation and attach, after the built-in catalogs | Idempotent catalog tables and views |
| `session_start`, `session_end` | When a session is created; when it is dropped, including RESETCONNECTION and disconnect | Session-owned resources |

When a hook fails after producing output (for example, a procedure whose
first statement returned rows), return `Err(ext::Partial { tokens, error
}.into())`. The engine writes the tokens and then handles the error like any
other statement error: TRY/CATCH, `@@ERROR`, XACT_ABORT and doomed
transactions. A successful `Exec` with a non-zero `status` is not an error.
Its DONEPROC carries no error flag.

Rewrites can see the same tree more than once, for example a SET statement
and then its value expression, so they must be idempotent.

Feature modules are children of `engine`, so they may use `Session`'s private
fields and methods. Typical examples are `db`, `transactions`,
`batch_response_inner` (to run procedure bodies) and `execute`.

To run a statement that a feature has intercepted through the ordinary path,
wrap the call in `ext::reenter(session, "<feature>", |session| ...)`. This
suspends that feature's own `batch`, `exec` and `statement` hooks for the
call. The other features still see the statement, so for example triggers and
foreign-key actions compose.

Per-session state lives in the feature's `State` type, reachable as
`session.ext.<feature>`. State shared across sessions, such as lock tables,
belongs in the feature module, keyed by database and by `session.ext.token`.

The token is unique for the life of the process. Do not key on the SPID:
RESETCONNECTION creates the replacement session (running its
`session_start`) under a temporary SPID and in `master`, then swaps SPIDs and
enters the login database, and only then drops the old session (running its
`session_end`). `session_end` also runs when a `session_start` failed, so it
must tolerate missing state.

## Module store

`modules` keeps procedures (`P`), scalar functions (`FN`), inline functions
(`IF`), multi-statement table-valued functions (`TF`) and triggers (`TR`) in
`main.__msduck_modules`, one table per database. Each row stores:

- the original definition text;
- the schema;
- the parent table, for triggers;
- a disabled flag;
- feature-owned JSON properties.

Object ids come from the same sequence as tables and views. The module
appends its rows to `sys.objects` by renaming the built-in view to
`sys.__msduck_core_objects` at bootstrap. As a result, `OBJECT_ID`,
`OBJECT_NAME`, `sys.all_objects` and the existence checks that later
`CREATE` statements use all see modules.

`CREATE TABLE` and `CREATE VIEW` also fail with 2714 when a module of that
name exists, because a duplicate name would make the `OBJECT_ID` name map
ambiguous.

The API is `create` (which fails with 2714 for a duplicate name), `alter`,
`rename`, `set_disabled`, `remove`, `find`, `by_id` and `list`.

## Verification

- `tests/extension_hooks.rs` restarts a file-backed server with a user
  database and checks that catalogs and session lifecycle are unchanged.
- `tests/compat/extension_hooks.test.mjs` checks named EXEC arguments through
  tedious.
- Unit tests cover the carrier, `leading_words` and the module store.
