# T-SQL cursor reference

`reference/cursor.json` retains 114 ordered SQL Server observations of T-SQL
cursors: decoded rows, full column descriptors, errors and informational
messages (number, state, class, line and procedure name), DONE/DONEINPROC/DONEPROC
tokens, per-request return status, and the browse-mode TABNAME/COLINFO tokens
that accompany cursor FETCH results. `scripts/capture-cursor.mjs` ran against the
pinned SQL Server 2025 image
`sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
Two fresh databases in each of two independent containers produced identical
captures, and the fixture was written only after all four matched (fixture
SHA-256 `8f49e5865abd51b0b83fb70892e1d2dd271063720e159066dee165fce23636a7`).
It contains tedious-decoded values and descriptors, not raw TDS packets.

msduck currently has no cursor statements. `DECLARE CURSOR`, `OPEN`, `FETCH`,
`CLOSE`, `DEALLOCATE`, `WHERE CURRENT OF`, cursor variables, `@@FETCH_STATUS`,
`@@CURSOR_ROWS` and `CURSOR_STATUS` are unimplemented, and the only existing
reference to cursors is the `SET CURSOR_CLOSE_ON_COMMIT OFF` session-option
text. This task changed no parser, engine, catalog, metadata or client-test file.

## Capture method

- The script follows `docs/reference-captures.md` from owner PR #312. Those
  shared helpers are not on `main`, so the script replicates them locally. It
  uses `capturePrepared` for `sp_prepare`/`sp_execute`/`sp_unprepare`, waits for
  the `prepared`/`error` events, clears `request.error` per phase and records only
  errors raised during each execution. When #312 merges, the local copy should
  be replaced by the shared helper.
- Return status: before and after every request (batch, `sp_executesql` RPC,
  procedure RPC, prepared phase), the script clears tedious's carried
  `connection.procReturnStatusValue`. `returnStatus` is recorded only when a
  DONEPROC in that request delivers one; otherwise it stays `null`.
- Every batch and prepared statement gets a unique trailing comment
  `/*<step name>*/`, and the validator rejects repeated text. The two
  `rpc local cursor` executions share their text on purpose, to show that a
  LOCAL cursor does not survive between calls of one statement.
- Constraints are named explicitly. `process.env.TZ = 'UTC'` is set. No step
  reads the clock: `sys.dm_exec_cursors` is projected to `name`, `properties` and
  `is_open` only.
- Rows and messages are bounded per request (1000 rows per set, 200
  messages/done/browse tokens), with overflow counted instead of kept. Captures
  are compared with `assertSameCapture` only. Runs used
  `node --max-old-space-size=2048` under an RSS watchdog that kills above 4 GB;
  peak RSS was about 116 MB.
- Only one server-side `WHILE @@FETCH_STATUS = 0` loop is captured
  (`default loop into variables`, three iterations). Each iteration emits its
  own DONE tokens, and that one batch holds 13, so the count depends on the
  iteration count. Other steps use explicit, bounded `FETCH` sequences.
- Concurrent visibility uses a second connection to the same database. The
  steps alternate strictly: each request completes before the next starts, and
  there is no parallel I/O. The second session runs `SET LOCK_TIMEOUT 5000` so a
  block would be recorded as an error rather than a hang. No step blocked.

### Tooling problems found

- **tedious 20 cannot parse TABNAME (0xA4) or COLINFO (0xA5).** SQL Server sends
  them with the results of every `FETCH` that returns rows to the client, and
  tedious fails the connection with `Unknown type: 164`. The script therefore
  patches `StreamParser.prototype.readToken` locally for these two
  length-prefixed token types only. The patch decodes each token into the
  current request's `browse` list and continues with the next token. The
  decoded order among browse tokens is kept, but their position relative to
  COLMETADATA and ROW is not. Any other client harness (including `npm test`
  suites and the audit runner) needs the same handling before it can consume
  cursor FETCH results from SQL Server or a compatible msduck.
- **A crash left a container behind.** The first diagnostic run died from that
  unhandled parser `error` event before `withReferenceContainer` could clean up.
  The worker removed that one container, which carried its own
  `msduck.owner=<worktree>:<pid>` label, after confirming the PID was gone.
- **Error 1049 reports an unstable line number.** For the one-line batch
  `DECLARE c INSENSITIVE CURSOR LOCAL ...`, SQL Server reported line 17 in one
  container and 18 in another. The value was stable across the databases
  within each container and reproduced in three separate four-database runs.
  Only this field of this step is replaced by `{ "kind": "unstable" }`, and the
  record lists `unstable: ["errors[].lineNumber"]`. Every other step matched
  exactly across 12 captures.
- `sp_prepare` of any multi-statement batch returns status **8182** with no
  metadata. Both cursor batches and the cursor-free control
  `DECLARE @copy INT = @min; SELECT @copy AS copy` do this, so it is not
  cursor-specific. A single `SELECT` (HASHBYTES fixture) returns 0 with
  metadata.

## Observed rules

### Session globals and database defaults

- A fresh session reports `@@CURSOR_ROWS = 0` and `@@FETCH_STATUS = 0`.
  `@@FETCH_STATUS` is per connection and reflects the last FETCH on any cursor.
  It survives CLOSE and DEALLOCATE: it was still 0 after deallocating a cursor
  whose last fetch succeeded.
- A fresh database has `is_local_cursor_default = 0` and
  `is_cursor_close_on_commit_on = 0`, so a cursor declared without a scope keyword
  is GLOBAL.

### Declared and effective model

`sys.dm_exec_cursors(@@SPID).properties` and `@@CURSOR_ROWS` immediately after
OPEN (synchronous population; the default cursor threshold was not changed):

| Declaration (LOCAL) | Effective properties | `@@CURSOR_ROWS` |
|---|---|---|
| none | `TSQL \| Dynamic \| Optimistic` | -1 |
| `STATIC` | `TSQL \| Snapshot \| Read Only` | 5 |
| `KEYSET` | `TSQL \| Keyset \| Optimistic` | 5 |
| `DYNAMIC` | `TSQL \| Dynamic \| Optimistic` | -1 |
| `FAST_FORWARD` | `TSQL \| Fast_Forward \| Read Only` | -1 |
| `SCROLL` | `TSQL \| Keyset \| Optimistic` | 5 |
| `SCROLL ... FOR UPDATE OF qty` | `TSQL \| Keyset \| Optimistic` | 5 |
| `SCROLL SCROLL_LOCKS` | `TSQL \| Keyset \| Scroll Locks` | 5 |
| `KEYSET TYPE_WARNING` over a heap (no unique index) | `TSQL \| Snapshot \| Read Only` + info 16956 | 2 |
| `DYNAMIC TYPE_WARNING ... ORDER BY` non-indexed column | `TSQL \| Keyset \| Optimistic` + info 16956 | 5 |
| `STATIC` over no rows | `TSQL \| Snapshot \| Read Only` | 0 |
| `DYNAMIC` over no rows | `TSQL \| Dynamic \| Optimistic` | -1 |

The properties suffix is `Local (0)` or `Global (0)`. The ISO form
`INSENSITIVE SCROLL CURSOR ... FOR READ ONLY` is a global Snapshot/Read Only
cursor. Info 16956 (state 1, class 0), "The created cursor is not of the
requested type.", appears only with `TYPE_WARNING`, once per converted cursor.
After OPEN, `CURSOR_STATUS` is 1, except 0 for an empty STATIC cursor. An empty
DYNAMIC cursor still reports 1.

Option conflicts fail at compile time with error 1048 (class 15, line 1):
`FAST_FORWARD SCROLL`, `FORWARD_ONLY SCROLL`, `STATIC ... FOR UPDATE` and
`READ_ONLY ... FOR UPDATE`. Mixing ISO and T-SQL syntax
(`INSENSITIVE CURSOR LOCAL`) is error 1049 (class 15; line unstable, see above).

### CURSOR_STATUS and lifecycle errors

- `CURSOR_STATUS('global'|'local', name)`: -1 when declared but not open or when
  closed, 1 when open (0 when open and empty for a STATIC cursor), and -3 when the
  name does not exist in that scope. A global cursor reports -3 for 'local'.
- `CURSOR_STATUS('variable', '@v')`: -2 when unassigned, after DEALLOCATE of the
  variable, or when the variable was not an OUTPUT target; -1 when assigned but
  not open or when closed; 1 when open.
- After CLOSE, `@@CURSOR_ROWS` is 0. After a reopen of a dynamic cursor it is -1
  again, and the reopened cursor restarts at the first row.
- Errors (all class 16):

  | Case | Error |
  |---|---|
  | FETCH on a closed cursor | 16917, state 2 |
  | CLOSE on a closed cursor | 16917, state 1 |
  | OPEN on an open cursor | 16905, state 1 (the cursor keeps its position) |
  | FETCH or OPEN of an unknown or deallocated name | 16916 (line 0) |
  | DEALLOCATE of an unknown name | 16916 (line 1) |
  | Declaring an existing global name | 16915 |
  | OPEN of an unassigned cursor variable | 16950, state 2 (line 0) |

  None of these errors aborts the batch: the statements that follow run. The
  duplicate-declaration batch keeps the first declaration. The failing FETCH sets
  `@@FETCH_STATUS` to -1.

### FETCH

- FETCH without INTO returns one result set per FETCH. The row carries an extra
  trailing `ROWSTAT` INT column (COLMETADATA flags 0). The visible columns keep
  their table flags (for example 8 = updatable).
- A FETCH past either end returns the same COLMETADATA with zero rows and sets
  `@@FETCH_STATUS = -1`.
- Every FETCH result set is accompanied by browse tokens (97 FETCH row sets, 189
  browse tokens in one capture). A table source sends
  TABNAME `[["dbo","<table>"]]` plus COLINFO, with one entry per column
  including ROWSTAT. COLINFO status 0x08 marks the key column for dynamic and
  keyset cursors, and is 0 for STATIC and FAST_FORWARD. ROWSTAT is table 0,
  status 0x14 (hidden | expression). A constant `SELECT 1` source sends only
  COLINFO, with status 0x04 for the expression column.
- Scroll navigation over 5 rows (`SCROLL STATIC`): the sequence
  `LAST PRIOR FIRST PRIOR NEXT ABSOLUTE 3 ABSOLUTE -2 RELATIVE -1 RELATIVE 0 RELATIVE 10 PRIOR ABSOLUTE 0 NEXT ABSOLUTE 99 ABSOLUTE -99 NEXT`
  returns rows `5 4 1 - 1 3 4 3 3 - 5 - 1 - - 1` with statuses
  `0 0 0 -1 0 0 0 0 0 -1 0 -1 0 -1 -1 0`. PRIOR from before the first row stays
  before it, and NEXT then returns row 1. PRIOR after running past the end
  returns the last row. `ABSOLUTE 0` and out-of-range positions return no row.
- `FETCH ... INTO` accepts variables for ABSOLUTE/RELATIVE (INT and SMALLINT
  observed). A failed FETCH INTO leaves the target variables unchanged.
- Fetch-type restrictions (class 16, statement-level; later statements run):
  - PRIOR on the default forward-only dynamic cursor: 16911, "fetch: The fetch
    type prior cannot be used with forward only cursors."
  - LAST on FAST_FORWARD, and ABSOLUTE on `FORWARD_ONLY STATIC`: 16911, with the
    lowercase type name.
  - ABSOLUTE on `SCROLL DYNAMIC`: 16925, "The fetch type Absolute cannot be used
    with dynamic cursors." RELATIVE and LAST work on that cursor.
- INTO with the wrong number of variables: 16924, with status -1 and variables
  unchanged. The cursor stays open.
- INTO a variable whose type cannot hold the value (VARCHAR `'a'` into INT):
  conversion error 245, which aborts the batch.
- A three-iteration `WHILE @@FETCH_STATUS = 0` loop leaves `@@FETCH_STATUS = -1`
  and the INTO variables holding the last row fetched.

### Positioned UPDATE and DELETE (`WHERE CURRENT OF`)

- Before the first FETCH: 16931, "There are no rows in the current fetch
  buffer." This is followed by info 3621, "The statement has been terminated."
- Updating a column outside the `FOR UPDATE OF` list: 16932 + 3621. Targeting a
  table the cursor does not include: 16933 + 3621.
- A STATIC, FAST_FORWARD or `KEYSET READ_ONLY` cursor: 16929, "The cursor is
  READ ONLY." + 3621. These errors are statement-level.
- A successful positioned UPDATE or DELETE sets `@@ROWCOUNT = 1`.
  `FETCH RELATIVE 0` on a KEYSET cursor refetches the updated values. After a
  positioned DELETE, the refetch returns a placeholder row `[0, '          ', 0]`
  (zero, or the column blank-padded to its declared width) with `ROWSTAT = 2`
  and `@@FETCH_STATUS = -2`. FETCH PRIOR back onto the deleted key returns the
  same placeholder.
- A positioned UPDATE of a deleted current row: 16947, "No rows were updated or
  deleted." + 3621.
- `WHERE CURRENT OF GLOBAL name`, a DYNAMIC cursor, and a cursor variable
  (`WHERE CURRENT OF @cv`) all perform positioned modifications.

### Cursor variables

`SET @cv = CURSOR <options> FOR ...` allocates a cursor. `SET @alias = c_named`
makes the variable an alias: fetching from either advances the same cursor.
Deallocating the alias leaves the named cursor usable, and the variable reads
-2. A procedure's `CURSOR VARYING OUTPUT` parameter returns an open cursor
(`CURSOR_STATUS` 1, `@@CURSOR_ROWS` 2 for the STATIC cursor it opened). Passing
the variable without `OUTPUT` succeeds but leaves the caller's variable at -2.

### Scope across batches, procedures and dynamic SQL

- A GLOBAL cursor persists across batches. A LOCAL cursor is deallocated at the
  end of its batch (-3 in the next batch; FETCH raises 16916).
- A LOCAL and a GLOBAL cursor may share a name. An unqualified name resolves to
  the LOCAL one, and `GLOBAL name` selects the global one.
- `ALTER DATABASE CURRENT SET CURSOR_DEFAULT LOCAL` takes effect for a
  declaration later in the same batch.
- A LOCAL cursor declared in a procedure is gone after the procedure returns.
  For the procedure RPC, `returnStatus` 7 arrives with its DONEPROC. A GLOBAL
  cursor opened in a procedure remains open for the caller, both after a batch
  `EXEC` and after a procedure RPC (status 3 in both cases). Re-invoking the
  procedure while that global cursor is still open was not captured.
- A procedure cannot see its caller's LOCAL cursor: 16916 with procName
  `dbo.cur_probe_proc`, line 0, and return status -6. It can see and advance its
  caller's GLOBAL cursor.
- `EXEC sp_executesql`: a LOCAL cursor declared inside is gone afterwards, and a
  GLOBAL one stays open for the caller. The dynamic batch cannot see the outer
  batch's LOCAL cursor: 16916, and `sp_executesql`'s return status is 16916.
- An EXEC inside a language batch produces a DONEPROC in that same request, so
  its status (3, or 0 for `sp_executesql`) is attributed to that batch. A later
  batch without EXEC reports `null`.
- Parameterized `sp_executesql` and prepared executions of a LOCAL cursor can
  repeat freely: each execution has its own scope, and `@@CURSOR_ROWS` follows
  each parameter value (2, then 4).
- A prepared GLOBAL cursor: the first execution creates it. Later executions
  raise 16915 (already exists) and 16905 (already open), return status -6, and
  still run the remaining statements against the existing cursor. The
  `@@CURSOR_ROWS` values stay at 2, and the FETCH continues from row 5 and then
  returns an empty set. `sp_unprepare` succeeds with status 0.

### Transactions

With `CURSOR_CLOSE_ON_COMMIT` OFF, STATIC and KEYSET cursors opened before
`BEGIN TRANSACTION` stay open (status 1) and keep fetching after both COMMIT and
ROLLBACK.

### Visibility of another session's committed changes

Starting rows `(10,a) (20,b) (30,c) (40,d)`. The primary session opens a global
cursor `ORDER BY id` and fetches row 10. The second session then commits
`UPDATE val='B' WHERE id=20`, `DELETE id=30`, `UPDATE id=45 WHERE id=40` and
`INSERT (35,'new'),(5,'before')`. The primary session then fetches NEXT five
times:

| Cursor | Rows after 10 | Statuses | `@@CURSOR_ROWS` |
|---|---|---|---|
| STATIC | 20 b, 30 c, 40 d | 0 0 0 -1 -1 | 4 |
| KEYSET | 20 **B**, placeholder, placeholder | 0 -2 -2 -1 -1 | 4 |
| DYNAMIC | 20 B, 35 new, 45 d | 0 0 0 -1 -1 | -1 |
| FAST_FORWARD | 20 B, 35 new, 45 d | 0 0 0 -1 -1 | -1 |
| FORWARD_ONLY (dynamic) | 20 B, 35 new, 45 d | 0 0 0 -1 -1 | -1 |

A KEYSET cursor sees non-key updates. It reports deleted rows and key-updated
rows (the old key 40) as -2 placeholders with `ROWSTAT` 2. It does not see
inserts or the new key 45. The dynamic models see all committed changes after
the current position, and none before it (row 5 is never returned).

## Gaps (not captured)

- API server cursors (`sp_cursoropen`, `sp_cursorfetch`, `sp_cursoroption`,
  `sp_cursor`, `sp_cursorclose`, `sp_cursorprepexec`) and their RPC result
  shapes. Info 16954 and the other API-only messages.
- Wire position of TABNAME/COLINFO relative to COLMETADATA/ROW, raw bytes, and
  the COLMETADATA flags for ROWSTAT in other TDS versions.
- Asynchronous keyset/static population (`sp_configure 'cursor threshold'`), and
  `@@CURSOR_ROWS` negative partial counts.
- `SCROLL_LOCKS` and `OPTIMISTIC` conflict behavior: 16934 optimistic
  concurrency failures, lock holding and blocking between sessions, and
  isolation levels other than READ COMMITTED. Also `READ_COMMITTED_SNAPSHOT`
  and SNAPSHOT isolation visibility.
- The session's own non-positioned changes seen through each cursor type.
  Inserts visible to KEYSET after reopen. Visibility for rows before the current
  position with SCROLL DYNAMIC (FETCH PRIOR/FIRST).
- `CURSOR_CLOSE_ON_COMMIT ON`, `SET CURSOR_CLOSE_ON_COMMIT`, rollback inside
  triggers and `XACT_ABORT` interaction. Cursors over temp tables, table
  variables, views, joins, `DISTINCT`, `TOP`, aggregates and `UNION` (implicit
  conversions to STATIC). `FOR UPDATE` over non-updatable queries.
- `sp_describe_cursor*`, `sp_cursor_list`, and the other
  `sys.dm_exec_cursors` columns, which include clock-derived values.
- Nested procedure depth greater than 1, triggers declaring cursors, and cursor
  variables assigned to other variables or passed through several procedure
  levels. `DEALLOCATE` of an alias while the cursor is open in a procedure.
- Conversion rules for every INTO target type. FETCH INTO a table-valued or
  NULL-typed variable. Cursor names longer than 128 characters and delimited
  cursor names.
- Loop DONE-token counts for more than three iterations; only one bounded loop
  is retained.

## Proposed successors

Each successor should cite this fixture and replay its steps with an
independent client that tolerates TABNAME/COLINFO.

1. **Deterministic core (`msduck-core`, ~1 task).** A pure cursor model with
   explicit inputs:
   - declared options to effective model, from the table above. Conversion needs
     an explicit "has unique index / order-by matches index" input from the
     caller.
   - the 1048/1049 option-conflict checks.
   - the fetch-type restriction matrix (16911 with lowercase type name, 16925).
   - `@@FETCH_STATUS`, `@@CURSOR_ROWS` and `CURSOR_STATUS` transitions for
     declare/open/fetch/close/deallocate.
   - scroll position arithmetic, including the before-first and after-last
     states.
   - the lifecycle error table.
   All of this is testable against the fixture without DuckDB.
2. **TDS tokens (`msduck-tds`).** Encoders for TABNAME and COLINFO, and a
   FETCH result-set builder that appends the `ROWSTAT` column and placeholder
   rows for -2. Bounded decoders for the test harness.
3. **Syntax (`msduck-sql`).** Parse DECLARE CURSOR (ISO and T-SQL forms),
   OPEN/FETCH/CLOSE/DEALLOCATE with GLOBAL qualifiers and variables, `SET @v =
   CURSOR`, `CURSOR VARYING OUTPUT` parameters and `WHERE CURRENT OF`. Resolve
   names against an explicit scope snapshot.
4. **Root session registry and materialization (`msduck`).**
   - per-connection global and per-frame local cursor tables, and
     cursor-variable handles. Frames for batches, procedures, `sp_executesql`
     and prepared executions, with local deallocation at frame exit.
   - STATIC as a DuckDB snapshot table. KEYSET as a key table plus per-FETCH
     refetch that yields -2 placeholders. DYNAMIC and FAST_FORWARD as
     key-positioned re-queries.
   - positioned UPDATE/DELETE with the 169xx diagnostics.
   - `CURSOR_CLOSE_ON_COMMIT` handling, and second-session visibility tests
     against this fixture.
5. **Client harness.** Add TABNAME/COLINFO tolerance to the shared tedious
   harness (a `scripts/lib` change, which needs its own task) so audit and
   client tests can consume cursor output.
