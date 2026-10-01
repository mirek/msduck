# Application locks

msduck implements SQL Server's application locks: `sp_getapplock`,
`sp_releaseapplock`, `APPLOCK_MODE` and `APPLOCK_TEST` (issue #713). Test
fixtures commonly take a session-owned exclusive lock before setup:

```sql
EXEC sp_getapplock @Resource = N'foo', @LockMode = 'Exclusive',
    @LockOwner = 'Session', @LockTimeout = 0;
```

The code lives in the extension hooks (docs/extension-hooks.md):

| Path | Role |
| --- | --- |
| `crates/msduck-sql/src/dialect/ext/applock.rs` | `EXEC @status = sp_getapplock ...` syntax |
| `src/engine/ext/applock.rs` | Hooks, argument values, `APPLOCK_MODE` and `APPLOCK_TEST` |
| `src/engine/ext/applock/call.rs` | The procedures: binding, validation and completion tokens |
| `src/engine/ext/applock/table.rs` | The process-wide lock table |
| `src/engine/ext/applock/principal.rs` | Database principals |

## Evidence

`scripts/capture-gaps-applock.mjs` records SQL Server's behavior in
`reference/gaps-applock.json`. It uses two fresh containers of the pinned
reference image, and the two runs must agree.

- `--check` validates the retained fixture.
- `--compare PORT` runs the same observations against a local msduck server
  and lists every observation that differs.

The capture holds 131 observations. With this change, 101 of them match
msduck exactly, including the rows, diagnostics (number, state, class,
procedure and line) and every DONE, DONEINPROC and RETURNSTATUS token. The
others are listed under [Remaining differences](#remaining-differences).

Both procedures are T-SQL wrappers around `sys.xp_userlock`. The bodies come
from `OBJECT_DEFINITION(OBJECT_ID('sys.sp_getapplock'))` and from the same
call for `sys.sp_releaseapplock`. msduck follows each body's control flow, so
a call returns the same DONEINPROC tokens as in SQL Server. As a result,
tedious reports a row count of 4 for a successful `sp_getapplock`.

## Procedures

- **Arguments.** Calls take positional or named arguments. Names are
  case-insensitive, and `DEFAULT` selects the parameter's default. Values can
  be literals, variables or scalar expressions.
  - `EXEC @rc = sp_getapplock ...` assigns the return status, converted to
    the variable's type.
  - The procedure name can be qualified (`dbo.`, `sys.`, `master..`). A
    database qualifier runs the call in that database's context, as for any
    system procedure. An unknown database fails with 911.
  - `@Resource` is `nvarchar(255)`, so longer names are truncated. Lock
    modes and owners are matched case-insensitively, ignoring trailing
    spaces.
  - `@LockTimeout` converts strings such as `'0'` and truncates decimals.
    Text that is not a number fails with 8114.
- **Return status.**

  | Status | Meaning |
  | --- | --- |
  | 0 | Granted |
  | 1 | Granted after waiting |
  | -1 | Timed out |
  | -2 | Cancelled |
  | -3 | Deadlock victim |
  | -999 | Validation or other call error |

- **Validation.** The messages and statuses are as in SQL Server:
  - 15625 (`Option '...' not recognized for '@LockMode' parameter.`) and
    15626 (transaction owner outside a transaction) are severity-10
    messages from the procedure. They come with its name and line number,
    and status -999.
  - `xp_userlock` raises severity-16 errors, with status -999 and
    `@@ERROR` 0 after the call:
    - 1224 (NULL resource);
    - 1227 (timeout below -1);
    - 1230 (NULL principal);
    - 1202 (unknown principal);
    - 3918 (releasing a transaction lock outside a transaction);
    - 1223 (lock not held; the message names the principal and the
      truncated resource).

    The order of the checks matches SQL Server: timeout, then resource, then
    principal.
  - Call errors fail the call:
    - 8145 (unknown parameter);
    - 119 (positional argument after a named one);
    - 8144 (too many arguments);
    - 201 (missing `@LockMode`);
    - 137 (undeclared status variable).
- **NOCOUNT.** With NOCOUNT ON, only the failed `xp_userlock` call keeps its
  DONEINPROC, as in SQL Server.
- **Row count and errors.** `@@ROWCOUNT` is 1 after a call, from the body's
  final RETURN.
- **XACT_ABORT.** With XACT_ABORT ON, an `xp_userlock` error becomes the
  call's error, so it dooms an open transaction.

## Locks

- **Identity.** A lock is identified by three parts:
  - the current database;
  - the database principal (`public` by default, case-insensitive);
  - the resource name (exact UTF-16, so case- and space-sensitive).

  The same resource in another database, or under another principal, is a
  different lock.
- **Principals.** The accepted principals are the ones every SQL Server
  database contains:
  - `public`, `dbo`, `guest`, `INFORMATION_SCHEMA` and `sys`;
  - the fixed `db_*` roles.

  msduck has no other users or roles, and every login acts as `dbo`, which
  is a member of all of them.
- **Owners.** A lock is owned by a session's session owner or by its
  transaction owner. Owners are keyed by the session's process-unique token,
  never by its SPID.
  - Each owner keeps a reference count. A lock is freed when every reference
    is released.
  - When an owner requests a different mode, it holds the union of the
    modes, such as `SharedIntentExclusive` or `UpdateIntentExclusive`. That
    mode never weakens before the final release.
  - Locks held by the same session never conflict, whichever owner holds
    them. A request from a session that already holds the resource under
    either owner is a conversion, so it never queues behind other sessions'
    waiters.
- **Compatibility.** Conflicts between sessions follow SQL Server's matrix
  for IS, S, U, IX, SIX, UIX and X. The captured matrix is asserted in Rust
  and tedious tests.
- **Waiting.** A request waits in a FIFO queue. A new request also waits
  behind incompatible queued requests, so it cannot overtake them. A
  conversion (an owner that already holds the lock) waits only for the
  granted group.
- **Timeouts.**
  - `0` fails at once with -1.
  - `N` waits up to N ms.
  - `-1` waits until the lock is granted, the request is a deadlock victim,
    or the request is cancelled (see the limits below).
  - NULL or an omitted timeout uses `@@LOCK_TIMEOUT`. msduck does not
    support `SET LOCK_TIMEOUT`, so that is always -1.
- **Deadlocks.** msduck detects cycles in the wait-for graph of application
  locks at once, while SQL Server's lock monitor takes about a second.
  - The longest-waiting request in the cycle is the victim. It returns -3
    without an error message.
  - Its locks and its transaction are kept, as in SQL Server: "deadlocks
    with application locks don't roll back the transaction".
  - SQL Server picks the victim by cost. In the captures it was sometimes
    the first waiter and sometimes the second, so the fixture records only
    that one victim returns -3 and the other is granted.
- **Release.**
  - Session locks end with `sp_releaseapplock`, a disconnect or
    RESETCONNECTION, through the `session_end` hook.
  - Transaction locks end at the outermost COMMIT or at any ROLLBACK,
    through the `transaction_end` hook. A nested COMMIT keeps them.

## APPLOCK_MODE and APPLOCK_TEST

- **Results.**
  - `APPLOCK_MODE(principal, resource, owner)` returns a nullable
    `nvarchar(32)`: `NoLock` or the held mode, including the union modes.
  - `APPLOCK_TEST(principal, resource, mode, owner)` returns a non-null
    `int`: 1 when the session could take the lock without waiting,
    otherwise 0. Requests queued by other sessions count, as in SQL Server.
- **Owner.** A NULL owner means `Transaction`.
- **Errors.** Errors match SQL Server's numbers and states:
  - 1226 (invalid owner);
  - 1225 state 3 (invalid mode, including union modes), state 2 (NULL mode)
    and state 1 (NULL resource);
  - 1230 (NULL principal);
  - 1202 (unknown principal);
  - 3918 (Transaction owner outside a transaction);
  - 8116 (a literal NULL or a number as an argument);
  - 174 (wrong number of arguments).

## Remaining differences

- **Live cancellation.** The transport reads one request at a time. It does
  not read Attention while a request runs, and `src/server.rs` belongs to
  another task. So a `-1` wait ends only when the lock is granted or the
  request is a deadlock victim.
  - The wait honors the session's request cancellation flag, as used by the
    cancellable read entry point. When it is set, the call returns -2.
  - A client that gives up and disconnects while waiting leaves its request
    queued until it would be granted. Then the session ends and releases the
    lock.
  - `ALTER DATABASE ... WITH ROLLBACK IMMEDIATE` closes a waiting session's
    connection but cannot interrupt the wait. If the session that runs the
    ALTER holds the lock being waited for, the ALTER times out because the
    waiting session does not end.
- **TRY...CATCH.** A severity-16 `xp_userlock` error (for example 1223)
  does not transfer control to CATCH. The exec hook cannot tell whether a
  TRY block is active, so the error follows the uncaught form: -999, and
  execution continues. Under XACT_ABORT ON, the error is the call's error.
  It is reported once, but the batch is not ended, so a later statement
  fails with 3998.
- **Function errors.** Function values are computed when the statement is
  prepared, so runtime errors such as 1226 arrive before the result
  metadata. In SQL Server they arrive after it.
  - The arguments must be constants, variables or expressions over them. A
    column reference fails with an explicit "unsupported" error.
  - A value in a persisted definition (view, default) would be frozen.
  - `sp_prepare` validates a statement with NULL parameter values and no
    transaction. So preparing an `APPLOCK_MODE` or `APPLOCK_TEST` call whose
    resource or principal is a parameter, or whose owner is `Transaction`
    outside a transaction, fails with the runtime error (1225, 1230 or
    3918). SQL Server compiles these. `sp_executesql` is not affected.
- **Compile-time errors.** In SQL Server, 8144, 119, 137 and 8116 are
  compile-time errors, so the batch does not run. msduck raises them when the
  statement runs, after earlier statements.
  - 8145, 201 and 8114 come without the procedure name.
  - A failed call adds RETURNSTATUS 1 before its DONEPROC.
- **Transaction completion tokens.** BEGIN TRAN and COMMIT report command 0
  instead of 212 and 213. This is a general transaction difference, not
  specific to application locks.
- **Not implemented.**
  - `sys.dm_exec_describe_first_result_set` (used by the "function
    declarations" observation).
  - `sys.dm_tran_locks`, which does not list application locks.
  - RPC calls such as tedious `callProcedure('sp_getapplock')`. RPC dispatch
    supports only the `sp_executesql` and `sp_prepare` families; `src/rpc.rs`
    belongs to another task.
- **One lock table per process.** The lock table is process-wide and keyed by
  database name. Separate `Server` instances in one process share it, which
  matters only for embedded use and tests.
