# SQL Server BulkLoad wire reference

`reference/bulk-load-wire.json` retains first-party post-login TDS traffic from
tedious 20.0.0 against pinned SQL Server 2025 image
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`
(ProductVersion `17.0.4065.4`). The script used two fresh containers and two
fresh databases in each container on `linux.local`. Every case retained its
outgoing SQL Batch (`0x01`), incoming tabular response (`0x04`), outgoing
BulkLoad (`0x07`), and final tabular response (`0x04`), with complete packet
headers and payloads. Table setup, follow-up SELECT, login, credentials and TLS
records were outside the packet capture; readback rows and typed descriptors are
retained separately.

Tedious sends generated `INSERT BULK dbo.BulkWireProbe (...)` as a SQL Batch,
waits for its completion, then sends the type-7 BCP stream. A zero-row
`execBulkLoad([])` sends only a 13-byte DONE token
(`fd000000000000000000000000`), without COLMETADATA. SQL Server rejects it
with error 4804/state 2/class 16 and inserts no rows. The current pure codec's
`COLMETADATA ROW* DONE` grammar therefore correctly rejects this particular
empty client stream; it does not establish what SQL Server accepts from other
clients. The NULL row succeeds with one row and typed NULLs. The Unicode/binary
case preserves `é😀` and bytes `00ff80`. The multirow case succeeds with three
rows, distinguishing NULL from empty binary. These are observations of the
pinned image and client, not a claim that msduck executes BulkLoad yet.

The two database runs within the first container matched byte for byte. Runs
in the second container differed in the raw zero-row error response, which
contains the container-specific server name. The JSON retains all four raw
responses and each exact comparison result; it does not erase this difference
or rewrite the server-name bytes. The second fresh two-container run again
matched the semantic assertions but differed from the retained raw fixture at
that server-name field. `--replay-fixture` performs offline exact-byte replay
of the retained packet headers/payloads and cross-run comparison record.

```sh
node scripts/capture-bulk-load-wire.mjs --replay-fixture
node scripts/capture-bulk-load-wire.mjs [diagnostic-output.json]
```

The second command needs Docker and starts two disposable pinned containers.
`--write-fixture` is only for initial fixture creation: it refuses an existing
fixture before container startup. A diagnostic output path resolving to or
hard-linking the fixture is rejected before any write or container startup,
including symlinked parent paths. The script caps each TDS packet at 32,767
bytes, each case at 512 KiB and 128 packets, and records only the four controlled
post-login message kinds. It uses a fixed four-case workload; arbitrary large
rows, multiple packet boundaries, cancellation, transactions, constraint
failures, identity/default columns, BCP options, FreeTDS, and other TDS versions
remain uncaptured. The pure codec does not implement target binding, execution
atomicity or server transport integration; those require separate root-side work.

The wire shape follows the local [TDS protocol skill](../.agents/skills/tds-protocol/SKILL.md),
the current [codec contract](bulk-load-codec.md), and the installed tedious
BulkLoad implementation. Raw packet bytes, callback errors/counts, SELECT rows
and metadata are all retained so later adapters can be checked without hiding
differences.
