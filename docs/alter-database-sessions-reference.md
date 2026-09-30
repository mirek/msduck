# ALTER DATABASE options and session termination reference

`scripts/capture-alter-database-sessions.mjs` records SQL Server 2025
(17.0.4065.4, the pinned reference image) behavior for issue #680:

```sql
ALTER DATABASE [x] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
SELECT program_name FROM sys.dm_exec_sessions WHERE session_id = @@SPID;
ALTER DATABASE [probe_db] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;
DROP DATABASE [probe_db];
```

`reference/alter-database-sessions.json` retains 92 observations from two fresh
containers, which were identical. Each observation keeps descriptors, rows,
ERROR and INFO tokens and the raw DONE bodies (status, CurCmd, row count). The
script connects with tedious using the application name `msduck-capture` and
the workstation ID `msduck-host`.

```sh
node scripts/capture-alter-database-sessions.mjs            # fresh capture, compared with the fixture
node scripts/capture-alter-database-sessions.mjs --check    # validate the retained fixture offline
```

## Observations

### Completion and option state

- A successful ALTER DATABASE ends with `DONE` CurCmd 215 (status 0, or 1 with
  more results). This holds for `READ_COMMITTED_SNAPSHOT ON|OFF`, `SINGLE_USER`,
  `RESTRICTED_USER` and `MULTI_USER`, with no termination clause, `WITH NO_WAIT`,
  `WITH ROLLBACK IMMEDIATE` and `WITH ROLLBACK AFTER 1 SECONDS`. Repeating the
  current value also succeeds. One `SET` accepts several options
  (`SET SINGLE_USER, READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE`).
- `sys.databases` reports `user_access` 0/1/2 as `MULTI_USER`, `SINGLE_USER` and
  `RESTRICTED_USER`, and `is_read_committed_snapshot_on` as a nullable `bit`
  (declaration column 20; `user_access` is 8, `user_access_desc` 9 with
  collation `Latin1_General_CI_AS_KS_WS`, `snapshot_isolation_state` 18).
- `ALTER DATABASE CURRENT` alters the session's database. The session may set
  its own database to `SINGLE_USER` and keeps using it.

### Refusals

| Case | Tokens | DONE |
|---|---|---|
| Unknown database | 5011 state 5 class 14, then 5069 state 1 class 16 | status 2, CurCmd 215 |
| `master`, `READ_COMMITTED_SNAPSHOT` | 5058 state 2 class 16 | status 2, CurCmd 253 |
| `master`, `SINGLE_USER` or `MULTI_USER` | 5058 state 5 class 16 | status 2, CurCmd 253 |
| `CURRENT` while in `master` | 12104 state 2 class 16 | status 2, CurCmd 253 |
| Inside `BEGIN TRANSACTION` | 226 state 6 class 16; the transaction stays open (`@@TRANCOUNT` 1, `XACT_STATE()` 1) | status 2, CurCmd 215 |
| `WITH NO_WAIT` while other sessions use the database | 5070 state 2 class 16, then 5069 | status 2, CurCmd 215 |

### ROLLBACK IMMEDIATE

- With other sessions in the database, `WITH ROLLBACK IMMEDIATE` sends INFO
  5060 state 2 ("... Estimated rollback completion: 0%.") and then 5060 state 1
  ("... 100%.") before `DONE` CurCmd 215. The other sessions' connections are
  closed (tedious reports `ESOCKET` "socket hang up"). An open transaction in a
  terminated session is rolled back. With no other session, no 5060 is sent.
- A request running in a terminated session (`WAITFOR DELAY`) gets ERROR 596
  state 1 class 21 ("Cannot continue the execution because the session is in
  the kill state.") and `DONE` status 0x102, CurCmd 253, before its connection
  closes.
- The 5060 pair also appeared, with no other user session in the database, in
  two places: `ALTER DATABASE CURRENT SET SINGLE_USER WITH ROLLBACK IMMEDIATE`
  right after that session enabled `READ_COMMITTED_SNAPSHOT`, and the second
  ALTER in the owner's batch, after the first one had already terminated the
  only user session. Both followed enabling `READ_COMMITTED_SNAPSHOT`. The
  capture does not identify the terminated work; a server background task in
  the database is the likely explanation, not a verified one.

### SINGLE_USER admission

- The session that sets `SINGLE_USER` holds the database even when its own
  current database is `master`. `sys.dm_tran_locks` shows it holding a shared
  `DATABASE` lock, which survives `USE [probe_db]; USE [master]`.
- While one session holds a single-user database:
  - another `USE` fails with ERROR 924 state 1 class 14 ("Database 'probe_db' is
    already open and can only have one user at a time."), `DONE` status 2,
    CurCmd 253, and the session stays in its database;
  - a login naming the database fails with ERROR 4060 state 1 class 11, then
    18456 state 1 class 14, the same as a nonexistent database;
  - `DROP DATABASE` by another session fails with 3702 state 4;
  - ALTER DATABASE by another session fails with 5064 state 1 class 16, then 5069,
    even `WITH NO_WAIT` or `WITH ROLLBACK IMMEDIATE`.
- The holder can drop the database. When the issuer disconnects, the first
  session to enter the database becomes the holder.

### Owner sequence

The owner's four statements in one batch, from `master`, with one idle session
in `probe_db`, produce `DONE` (1, 215), (17, 193, 1 row), (1, 215), (0, 204).
Each ALTER sends the 5060 pair, and the idle session is closed.

### sys.dm_exec_sessions and @@SPID

- `sys.dm_exec_sessions` declares 52 columns. The ones relevant here are
  `session_id smallint NOT NULL`, `login_time datetime NOT NULL`,
  `host_name nvarchar(128) NULL`, `program_name nvarchar(128) NULL`,
  `host_process_id int NULL`, `client_version int NULL`,
  `client_interface_name nvarchar(32) NULL`, `login_name nvarchar(128) NOT NULL`,
  `status nvarchar(30) NOT NULL`, `database_id smallint NOT NULL`,
  `is_user_process bit NOT NULL` and `original_login_name nvarchar(128) NOT NULL`.
  These string columns use `SQL_Latin1_General_CP1_CI_AS`.
- `program_name`, `host_name` and `client_interface_name` are the LOGIN7
  application name, host name and client library name (`Tedious`); tedious
  sends `Tedious` as the application name when it is set to an empty string.
  The querying session's `status` is `running`, and an idle one's is `sleeping`.
- `@@SPID` is a non-nullable `smallint` computed column (flags 32). User
  sessions are above 50 and are listed in `sys.dm_exec_sessions`.
