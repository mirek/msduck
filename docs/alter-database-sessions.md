# ALTER DATABASE options, sessions and @@SPID

msduck supports the statements a client uses to prepare and tear down a probe
database:

```sql
ALTER DATABASE [x] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
SELECT program_name FROM sys.dm_exec_sessions WHERE session_id = @@SPID;
ALTER DATABASE [probe_db] SET SINGLE_USER WITH ROLLBACK IMMEDIATE;
DROP DATABASE [probe_db];
```

Expected behavior comes from `reference/alter-database-sessions.json`, captured
from SQL Server 2025 (see
[the reference notes](alter-database-sessions-reference.md)).
`tests/alter_database_sessions.test.mjs` compares msduck with that capture
through tedious.

## ALTER DATABASE

```
ALTER DATABASE { name | CURRENT }
SET option [, option ...]
[ WITH { ROLLBACK IMMEDIATE | ROLLBACK AFTER n [SECONDS] | NO_WAIT } ]

option: READ_COMMITTED_SNAPSHOT { ON | OFF } | SINGLE_USER | RESTRICTED_USER | MULTI_USER
```

- Success sends DONE CurCmd 215. The options persist in the database registry
  and appear in `sys.databases` as `user_access`/`user_access_desc` and
  `is_read_committed_snapshot_on`. DuckDB always reads from a transaction
  snapshot, so `READ_COMMITTED_SNAPSHOT` does not change how msduck executes
  queries; the setting is recorded and reported.
- Refusals follow the capture: unknown database 5011 then 5069; `master`
  5058 (state 2 for `READ_COMMITTED_SNAPSHOT`, 5 for user access); `CURRENT`
  in `master` 12104; inside a user transaction 226 (state 6, the transaction
  stays open).
- `READ_COMMITTED_SNAPSHOT` and `SINGLE_USER` need the other sessions using the
  database gone. `RESTRICTED_USER` does not, because every msduck login is
  sysadmin and SQL Server admits sysadmin members.
  - `WITH NO_WAIT` fails with 5070 then 5069.
  - `WITH ROLLBACK IMMEDIATE` interrupts the other sessions' statements, closes
    their connections, waits until they have released the database (their
    transactions roll back), and sends INFO 5060 state 2 (0%) and state 1
    (100%) before DONE.
  - `WITH ROLLBACK AFTER n` waits up to n seconds for the sessions to leave,
    then terminates the remaining ones the same way.
  - Without a clause SQL Server waits indefinitely. msduck fails explicitly
    instead, so a statement cannot block other logins and `USE` forever.
- The session that sets `SINGLE_USER` holds the database, even while its current
  database is `master`, until it disconnects, drops the database or sets another
  user access mode. RESETCONNECTION keeps the hold, as the session continues.
  While another session holds a single-user database:
  - `USE` fails with 924;
  - a login naming the database fails with 4060 then 18456;
  - `DROP DATABASE` fails with 3702 (state 4);
  - `ALTER DATABASE` fails with 5064 then 5069.

## sys.dm_exec_sessions and @@SPID

Each client session gets the lowest free SPID from 51 on, and keeps it
across RESETCONNECTION. `@@SPID` is a non-nullable `smallint`.
`sys.dm_exec_sessions` exists in every database and lists the server's
sessions with these columns, in SQL Server's order and with its declarations:

| Column | Value |
| --- | --- |
| `session_id` | the SPID |
| `login_time` | when the session was created, UTC |
| `host_name`, `program_name`, `client_interface_name` | LOGIN7 HostName, AppName and CltIntName |
| `host_process_id` | LOGIN7 ClientPID |
| `client_version` | NULL; the value SQL Server reports was not captured |
| `login_name`, `original_login_name` | the authenticated login |
| `status` | `running` while the session executes a request, otherwise `sleeping` |
| `is_user_process` | 1 |
| `database_id` | the session's current database |

A query reads a snapshot taken when it starts executing. Sessions created
inside the process, such as in Rust tests, have NULL client names.

## Differences from SQL Server

- A statement running in a terminated session stops, and its connection
  closes. SQL Server first sends it ERROR 596 (class 21) and DONE, and msduck
  does not.
- SQL Server sent the 5060 pair for a `ROLLBACK IMMEDIATE` that followed
  enabling `READ_COMMITTED_SNAPSHOT`, even when no user session was left in the
  database (the second ALTER in the owner's batch). msduck sends it only when it
  terminated a session.
- msduck ends a batch at the first ALTER DATABASE error, as it does for other
  statement errors; SQL Server continues with the next statement.
- Other ALTER DATABASE options and forms (`ALLOW_SNAPSHOT_ISOLATION`,
  `MODIFY NAME`, `COLLATE`, file options) fail as unsupported when parsed.
- The other 40 `sys.dm_exec_sessions` columns are not provided, and the server
  has no system sessions (SPIDs below 51).
