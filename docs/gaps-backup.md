# BACKUP, RESTORE and msdb backup history

msduck supports full database backups to disk, RESTORE HEADERONLY,
FILELISTONLY, VERIFYONLY and DATABASE, and records history in `msdb` (issue
#714). Expected results, descriptors and errors were captured from SQL Server
2022 (`mcr.microsoft.com/mssql/server@sha256:0ec7739e…`) by
`scripts/capture-gaps-backup.mjs` into `reference/gaps-backup.json`.
`node scripts/capture-gaps-backup.mjs --check` validates the fixture.

The syntax lives in `crates/msduck-sql/src/dialect/ext/backup.rs`, and it
travels through the batch as an extension carrier. The runtime lives in
`src/engine/ext/backup.rs` and its `backup/` submodules. The database catalog
(`src/database_catalog.rs`) stages restored files, creates `msdb` and publishes
the file catalogs. See [extension hooks](extension-hooks.md).

## Backup files

A backup device is a self-describing media file:

- a 16-byte magic header (`MSDUCK BACKUP`, two NUL bytes, format version 1);
- one record per backup set, each with a length-prefixed JSON header and a
  length-prefixed payload.

The JSON header records:

- the media: GUID, name, description and compression;
- the set: name, description, flags, user, server, database and its creation
  date, start and finish times;
- the recovery family, database and backup set GUIDs;
- the logical and physical file names;
- the payload's SHA-256.

The payload is a complete DuckDB database written by
`COPY FROM DATABASE <database> TO <staging file>`. That copy is one statement
that reads one MVCC snapshot. The backup therefore contains committed data
only, even while other sessions write: an open transaction's inserts and
updates are absent from it. In-memory and file-backed servers back up the
same way. The copy also includes msduck's catalog tables, such as declared
types, IDENTITY sequences with their current values, object IDs and views.
After a restore, declared metadata, IDENTITY and views work as they did in the
source.

A new file is written next to the device and then renamed over it. A failed
BACKUP therefore leaves an existing device unchanged. Backups write their media
one at a time, so concurrent appends to the same device each keep their set. The payload is staged as
a hidden `.<primary>.backup-*.duckdb` file in the database directory and is
deleted afterwards.

## BACKUP DATABASE

`BACKUP DATABASE name TO DISK = path [WITH ...]`. The database name and path
may be literals or variables.

- `FORMAT` writes a new media header. `INIT` keeps the media header but
  replaces its backup sets. `NOINIT`, the default, appends a set at the next
  position.
- `NAME`, `DESCRIPTION`, `MEDIANAME`, `MEDIADESCRIPTION`, `COPY_ONLY` and
  `CHECKSUM` are recorded in the header and in msdb.
- `COMPRESSION` marks the set and the media as compressed, as SQL Server
  reports them (`Compressed` 1, `MS_XPRESS`). Later sets on compressed media
  are compressed too, as in SQL Server. The payload itself is not compressed.
- `STATS [= n]` sends `n percent processed.` (3211) for each multiple of `n`
  up to 100. The default `n` is 10.
- These options are accepted and have no effect: `NO_COMPRESSION`,
  `NO_CHECKSUM`, `NOFORMAT`, `SKIP`, `NOSKIP`, `REWIND`, `NOREWIND`, `UNLOAD`,
  `NOUNLOAD`, `BUFFERCOUNT`, `MAXTRANSFERSIZE`, `BLOCKSIZE`,
  `CONTINUE_AFTER_ERROR`, `STOP_ON_ERROR`, `EXPIREDATE` and `RETAINDAYS`.
- A successful backup sends 4035 for the data file and for the log file, then
  3014, each followed by a DONE with the MORE bit and CurCmd 228, as captured.
  The page count is the payload size in 8 KB pages. The log file reports 0
  pages, because a DuckDB snapshot has no separate log.

Errors, with SQL Server's numbers and states, each followed by 3013 (`BACKUP
DATABASE is terminating abnormally.`):

| Case | Error |
| --- | --- |
| unknown database | 911, state 11 |
| inside a transaction | 3021, state 0 |
| directory missing or not writable | 3201, state 1, `Operating system error 5(Access is denied.)` |
| a device that is an attached database file or its WAL | 3201, state 1, operating system error 32 |
| existing device that is not a backup, without `FORMAT` | 3241, state 0 |
| `BACKUP LOG` (every msduck database uses the SIMPLE recovery model) | 4208, state 1 |
| unknown option | 155, state 1, severity 15, without 3013 |

## RESTORE HEADERONLY, FILELISTONLY and VERIFYONLY

- HEADERONLY returns SQL Server's 59 columns, one row per backup set. The
  descriptors match the capture exactly: types, widths, NUMERICN for LSNs and
  nullable flags. Values follow SQL Server:
  - `BackupType` 1 and `DeviceType` 2;
  - `Position`, `Compressed`, `Flags` (512, plus 16 with CHECKSUM and 1024
    with COPY_ONLY), `HasBackupChecksums` and `IsCopyOnly`;
  - `RecoveryModel` `SIMPLE`, `BackupTypeDescription` `Database`,
    `CompatibilityLevel` 160 and `Collation` `SQL_Latin1_General_CP1_CI_AS`;
  - `SortOrder` 52, `UnicodeLocaleId` 1033 and `UnicodeComparisonStyle`
    196609;
  - `DatabaseVersion` 957 and software version 16.0.4236 (vendor 4608), the
    captured SQL Server 2022 values;
  - `ServerName` and `MachineName` are the host name;
  - dates are local server time.
- FILELISTONLY returns SQL Server's 22 columns for the set chosen by
  `FILE = n` (default 1). The rows are the data file (`D`, `PRIMARY`) and the
  log file (`L`), with the source database's logical and physical names.
- VERIFYONLY checks the payload's SHA-256 and sends 3262 (`The backup set on
  file n is valid.`).

The completion commands are those captured: HEADERONLY sends DONE(CurCmd 230)
with the row count, then DONE(250); FILELISTONLY ends with 376; VERIFYONLY
with 377.

## RESTORE DATABASE

`RESTORE DATABASE name FROM DISK = path [WITH FILE = n, REPLACE, MOVE
'logical' TO 'physical', RECOVERY, STATS [= n], ...]`.

- The set's payload is copied to a staging file and verified against its
  SHA-256.
- A new database gets the next database ID and is attached from the staged
  file.
- An existing database is replaced in place and keeps its database ID. The
  staged file is first opened and bootstrapped on its own, so a payload this
  build cannot open leaves the existing database untouched. Then the database
  is hidden and detached, its files are deleted, and the staged file takes its
  name. If a step fails after the old file is gone, the database stays hidden,
  as SQL Server leaves a failed restore in the RESTORING state, and DROP
  DATABASE removes it.
- A restored database keeps the backup's logical file names, for example
  `foo` and `foo_log` for a clone of `foo`. Its physical names are the MOVE
  targets, or else the backup's physical names. `sys.database_files`,
  `sys.master_files`, FILELISTONLY of a later backup and msdb report these
  names. The data itself lives in msduck's own DuckDB file for the database.
- SQL Server's rules decide whether RESTORE may overwrite an existing
  database. Without REPLACE, it may only when the database belongs to the same
  recovery family, which is the case for the database that was backed up and
  for earlier restores of the same backup. Every other database is refused
  with 3154. msduck records each database's family GUID and database GUID in
  the registry, and RESTORE copies them from the backup set.
- Messages: 3211 for STATS, then 4035 for each file and 3014, with CurCmd 229.
- These options are accepted and have no effect: `RECOVERY`, `CHECKSUM`,
  `NO_CHECKSUM`, `REWIND`, `NOREWIND`, `UNLOAD`, `NOUNLOAD`, `BUFFERCOUNT`,
  `MAXTRANSFERSIZE`, `BLOCKSIZE`, `CONTINUE_AFTER_ERROR`, `STOP_ON_ERROR`,
  `KEEP_REPLICATION`, `KEEP_CDC`, `ENABLE_BROKER`,
  `ERROR_BROKER_CONVERSATIONS`, `NEW_BROKER`, `MEDIANAME` and `LOADHISTORY`.

Errors, each followed by 3013 (`RESTORE DATABASE is terminating
abnormally.`):

| Case | Error |
| --- | --- |
| missing device | 3201, state 2, `Operating system error 2(The system cannot find the file specified.)` |
| device shorter than a media header | 3254, state 1 |
| device that is not a backup | 3241, state 0 |
| `FILE = n` beyond the last set | 3287, state 1 |
| MOVE of a logical name not in the backup | 3234, state 2 (names the target database) |
| target used by this session | 3102, state 1 |
| target used by another session | 3101, state 1 |
| existing database of another family without REPLACE | 3154, state 4 |
| physical name used by another database | 1834 (state 1) and 3156 (state 4) per file, then 3119 (state 1) |
| inside a transaction | 3021, state 0 |
| payload checksum mismatch | msduck error naming the damaged set |

The 1834 rule follows SQL Server. Without MOVE, the restored files take the
backup's physical names, so a clone of an existing database needs MOVE.

## msdb

`msdb` is a real database with SQL Server's database ID 4. It is created on
first use:

- by BACKUP or RESTORE;
- by `USE msdb`;
- by a statement that names an msdb object outside a transaction.

Once it exists, it is listed in `sys.databases`, `DB_ID('msdb')` returns 4,
file-backed servers re-attach it on startup, and `DROP DATABASE msdb` fails
with 3708 (state 4), as for `master`, `model` and `tempdb`. The exception is an
msdb whose creation or recovery failed: it can be dropped, so its next use
creates it again.

The history tables have SQL Server's columns and declared types:

- `dbo.backupmediaset` and `dbo.backupmediafamily`;
- `dbo.backupset` and `dbo.backupfile`;
- `dbo.restorehistory` and `dbo.restorefile`.

`media_set_id`, `backup_set_id` and `restore_history_id` are IDENTITY
columns, as in SQL Server. The tables are created through the ordinary T-SQL
path, so their result descriptors come from those declarations.
The rows are written in the same way:

- BACKUP adds a media set and media family for a new media GUID, then a
  backup set with its two backup files;
- RESTORE adds a restore history row with its two restore files, and adds the
  backup set first if msdb does not know it yet, as SQL Server does for a
  backup taken elsewhere.

A statement in another database whose relations are all msdb objects, such as
`SELECT TOP(1) * FROM msdb..backupmediafamily` or
`SELECT * FROM msdb.dbo.backupset`, runs in msdb's context, so its descriptors
are msdb's. `msdb..name` names msdb's `dbo` schema.

## File catalogs

Every database has `sys.master_files` and `sys.database_files` with SQL
Server's columns:

- `master` reports `master` and `mastlog`;
- msdb reports `MSDBData` and `MSDBLog`;
- user databases report `<name>` and `<name>_log`, or the logical names they
  were restored with.

Physical names default to the database's DuckDB file and its `.wal` file, or
`:memory:` for an in-memory `master`. A restored database reports its MOVE
targets. `size` is DuckDB's allocated blocks in 8 KB pages. Log files report
0 pages. Growth and maximum size follow the captured defaults. GUID and LSN
columns are NULL.

## Remaining limits

- Only full database backups to one `DISK` device are supported. msduck
  refuses the following explicitly, with 3013:
  - differential backups, `BACKUP LOG`, `RESTORE LOG` and `RESTORE LABELONLY`;
  - FILE and FILEGROUP clauses;
  - URL, TAPE, logical and striped devices, and `MIRROR TO`;
  - `ENCRYPTION`, `PASSWORD`, `NORECOVERY`, `STANDBY`, `PARTIAL`, `STOPAT*`
    and `RESTRICTED_USER`.
- Backup files are msduck's format. SQL Server cannot read them, and msduck
  cannot read SQL Server `.bak` files.
- RESTORE refuses a backup of `master` and refuses to restore over `master`,
  `msdb`, `model` or `tempdb`.
- LSN columns in HEADERONLY and msdb are NULL, and FILELISTONLY reports LSNs
  as 0. `UniqueId` and the file GUIDs are NULL. Backup sizes are the
  DuckDB payload's size.
- Ordinary query results (msdb tables, `sys.master_files`) encode `numeric`
  declarations as DECIMALN, because that is the only decimal type the shared
  TDS encoder writes. `sys.master_files` and `sys.database_files` use DuckDB
  types, so their text columns are `nvarchar(max)` and `differential_base_time`
  is `datetime2`.
- msduck ends the batch at a failed BACKUP or RESTORE, with DONE CurCmd 0,
  as it does for most failed statements. SQL Server continues the batch, and
  its DONE has CurCmd 228 or 229. A later ROLLBACK must therefore be sent in
  its own batch.
- msdb objects in IF, WHILE and SET expressions, and statements that mix
  msdb objects with objects of the current database, are not routed to msdb.
  They fail with msduck's cross-database error.
- msdb does not exist until its first use, so `DB_ID('msdb')` is NULL on a
  fresh server.
- Comparisons of NVARCHAR columns with literals or parameters currently fail
  in msduck for every table, msdb included. For example,
  `WHERE database_name = N'foo'` fails with 245. msdb queries without such
  filters work.
