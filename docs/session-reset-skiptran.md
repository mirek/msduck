# SQL Server RESETCONNECTIONSKIPTRAN reference

`scripts/capture-session-reset-skiptran.mjs` retains 17 ordered observations in
`reference/session-reset-skiptran.json`. Each observation includes the returned
rows, column descriptors, errors, INFO messages and DONE events available from
Tedious 20. A secondary connection observes the transaction's row independently.
This is SQL Server 2025 reference evidence, not a claim that msduck implements
the behavior. The current server implements ordinary `RESETCONNECTION` (0x08)
and rejects `RESETCONNECTIONSKIPTRAN` (0x10); see `docs/session-reset.md`.

The [MS-TDS packet status specification](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/ce398f9a-7d47-4ede-8f36-9dd6fc21ca43)
defines 0x10 as an environment reset that keeps transaction state. Tedious 20
has no public 0x10 option. After LOGIN7 completes, this capture sets the
driver's one-request reset flag and intercepts its outgoing packet stream for
one SQL Batch. The hook substitutes 0x10 for 0x08 on the first packet only and
clears 0x08 from later packets. It checks the packet length and Batch type,
then restores the original writer. The retained packet record contains only
the eight-byte header fields `type`, `status`, `length` and `packetId`; it never
contains LOGIN7, credentials or request payloads. The first and only packet
was type 1, status **0x11** (0x10 plus EOM, without 0x08), length 544,
packet ID 1. The installed driver's outgoing message reported
`resetConnection: true`; the recorded packet header establishes the bit that
was actually sent. This non-MARS Tedious connection received a normal response.

The request ran inside an open local transaction. Before the reset, the primary
session returned `@@TRANCOUNT=1`, `XACT_STATE()=1`, `@@DATEFIRST=3`, `NOCOUNT=1`
and a present local `#temp` table. The second session saw one uncommitted row
in `dbo.skiptran_marker` through `WITH (NOLOCK)`. The 0x10 request itself
returned `1, 1, 7, 0, 0` for those five fields: the transaction remained open
and committable, while DATEFIRST, NOCOUNT and the temp table reset. The second
session still saw the uncommitted row. A prepared `SELECT @p+1` handle had
returned 42 before reset; reusing it afterward raised 8179/state 1/class 16
(`Could not find prepared statement with handle 1.`), followed by DONEPROC
with no return-status token. An ordinary Batch returned 42 inside the preserved
transaction. The script rolled it back, confirmed `@@TRANCOUNT=0` and a zero
committed-row count from the second session, then reused the primary connection
for another successful Batch returning 43. Exact descriptors and completion
shapes for every step are in the fixture.

The randomly generated database name is used only to connect the secondary
session and is not retained. Tedious owns the opaque prepared handle, so no
handle-dependent request text is retained; the server's exact 8179 diagnostic,
including `handle 1`, remains in the fixture and is compared without
normalization. Two fresh databases in each of two containers of the pinned
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144…a74a` image
produced identical raw observations. A second two-container/four-database run
matched the retained fixture. The fixture and both capture outputs had SHA-256
`b4a13d77a2bed46df55b439ae628c097f77fab2bb44b5dad5f1bca40d0df08ff`.
The script refuses to overwrite the fixture or to use an output path that
aliases it, and closes the secondary connection after rolling back a preserved
transaction so its isolated database can be dropped.

This capture covers one local transaction, DATEFIRST, NOCOUNT, one local temp
table, one prepared handle and a one-packet SQL Batch. It does not capture a
distributed transaction, MARS/SMP session, multi-packet first-message rule,
transaction isolation, other SET options, raw ENVCHANGE tokens or an RPC/TM
request carrying 0x10. Tedious' DONE events omit some wire status and CurCmd
fields, so this fixture is not a byte-for-byte server response trace. The
ordinary 0x08 reset capture in `docs/session-reset-reference.md` is separate:
it rolled back its local transaction instead of preserving it.

To reproduce with a local Docker daemon and installed dependencies, run
`node scripts/capture-session-reset-skiptran.mjs`. It writes a fresh output
under `artifacts/compatibility/session-reset-skiptran/` and compares it with
the retained fixture. The output path must not already exist. The
`--write-fixture` option is only for the initial retained capture and refuses
to overwrite an existing fixture.
