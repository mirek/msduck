# `sys.databases` result descriptors

`reference/sys-databases.json` retains six SQL Server 2025 requests in each of
two independent pinned containers and fresh databases. The complete fixture
SHA-256 is `e7da8332bd616f55370339602200cf06b75cf1f06839ba1bc481e7d8249c3705`.
`node scripts/capture-sys-databases.mjs --check` verifies its checksum, the two
complete runs, fixed descriptor controls and capture plan. The fixture retains
`sys.all_columns` declarations, empty and nonempty TDS result descriptors,
rows, diagnostics and completion events. A separate fresh replay is retained at
`artifacts/remote/linux.local/sys-databases-reference/replay.json`.

The pinned SQL Server build has **98** columns in `sys.databases`; msduck
currently publishes **13**. The result metadata adapter in
`src/query_catalog.rs` declares exactly those 13 and does not invent the
remaining columns. Empty results, `SELECT *`, explicit projections and aliases
use the same logical declarations. The root regression compares actual
COLMETADATA bytes with the independent capture, including names, widths,
nullable/computed flags and encoded collations.

| Published column | SQL Server logical type | Nullable | TDS flag | Collation |
| --- | --- | --- | --- | --- |
| `name` | `sysname` (`nvarchar(128)`) | no | 8 | database |
| `database_id` | `int` | no | 8 | — |
| `source_database_id` | `int` | yes | 9 | — |
| `create_date` | `datetime` | no | 8 | — |
| `compatibility_level` | `tinyint` | no | 8 | — |
| `collation_name` | `sysname` (`nvarchar(128)`) | yes | 33 | database |
| `user_access` | `tinyint` | yes | 33 | — |
| `user_access_desc` | `nvarchar(60)` | yes | 33 | resource |
| `is_read_only` | `bit` | yes | 33 | — |
| `state` | `tinyint` | yes | 33 | — |
| `state_desc` | `nvarchar(60)` | yes | 33 | resource |
| `recovery_model` | `tinyint` | yes | 33 | — |
| `recovery_model_desc` | `nvarchar(60)` | yes | 33 | resource |

The database collation captured here is `SQL_Latin1_General_CP1_CI_AS` (TDS
sort ID 52); the resource collation is `Latin1_General_CI_AS_KS_WS` (sort ID
0). The `sys.all_columns.is_computed` flag is false for these fields, while
some TDS descriptors still carry computed-result flag 32. The adapter follows
the captured result descriptor; it does not infer its wire origin from the
catalog's `is_computed` value.

This change aligns descriptors, not all catalog values. The pinned SQL Server
master row reports compatibility level **170**; msduck's current database view
reports **160**. Msduck does not yet expose the other 85 SQL Server columns or
all system databases. Those gaps remain explicit. The copied `sys` skill
contains mssqlite and SQLite status notes; they are not msduck implementation
claims. SQL Server's retained capture and msduck's root test are the evidence
for this mapping.
