# DDL completion reference

`reference/ddl-completion.json` retains a 15-statement SQL Server sequence in
both SQL batch and RPC modes. Each mode ran in two fresh isolated databases
with identical results. The pinned SQL Server image digest is in the fixture.
Observations include canonical result/error streams, raw decoded DONE token
fields, and a subsequent query of `@@ROWCOUNT` and `@@ERROR`.

Reproduce it on a host with Docker after `npm ci`:

```sh
node scripts/capture-ddl-completion.mjs
```

An optional first argument selects the output directory. The command saves both
raw runs before checking equality, then compares the result with the retained
fixture. It does not overwrite the reference. The raw token hook uses the pinned
tedious driver's debug token callback because ordinary request completion events
do not expose `curCmd`. Optional absent fields retain their JSON representation.

| Statement | Captured command | Completion row count |
| --- | ---: | --- |
| CREATE/DROP SCHEMA | 253 | absent |
| CREATE TABLE, including IDENTITY | 198 | absent |
| DROP TABLE | 199 | absent |
| CREATE INDEX | 200 | absent |
| DROP INDEX … ON table | 201 | absent |
| CREATE/ALTER VIEW | 207 | absent |
| DROP VIEW | 208 | absent |
| ALTER TABLE ADD/DROP COLUMN | 216 | absent |
| TRUNCATE TABLE | 234 | absent |
| INSERT of two rows | 195 | 2 |

DDL resets `@@ROWCOUNT` to zero in these captures. For RPC calls, schema DDL
emits only the final DONEPROC (command 224). The other statements emit an
intermediate DONEINPROC followed by DONEPROC. SQL batches emit DONE with the
statement's command identity. This fixture records the default session settings;
it does not establish every NOCOUNT, error, transactional or stored-procedure path.

The current msduck backend does not lower `DROP INDEX … ON table`; forwarding
that syntax to DuckDB fails. Task #107 tracks deterministic index binding and
its remaining reference coverage. Removing ON without validating the target
would risk dropping an index belonging to another table.

The separate completion fix has a focused native regression for the 14 executable
cases in both modes. It creates the index after ALTER/TRUNCATE and then drops its
table because index-drop lowering is incomplete. That regression is not an exact
replay of this full sequence and must not be reported as all 30 cases matching.
The retained fixture includes the original index-drop operation without filtering.
