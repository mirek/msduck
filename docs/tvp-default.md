# SQL Server default and null TVP RPC reference

`reference/tvp-default.json` retains four post-login RPC request/response
observations from tedious 20 against pinned SQL Server 2025 image
`sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`
(`17.0.4065.4`). Two independent containers each supplied two fresh databases.
All four runs matched byte for byte, including request packets, parsed TVP
fields, rows, errors and completion events. A second independent invocation
across four more fresh databases matched the retained fixture. Its SHA-256 is
`16ec06cde48b1b24769be930217ebe67cc383acbbae4f055467fff5dec191b7b`.

Every database defined `dbo.TvpProbe` with nullable INT, NVARCHAR(10), and
VARBINARY(10) columns, and `dbo.TvpProbeEcho` selecting `COUNT(*)` from its
TVP parameter. The observed cases were:

| RPC form | Parameter status | Result |
| --- | ---: | --- |
| Omitted TVP parameter | absent | One row containing 0; `doneInProc`, `doneProc`, return status 0 |
| Explicit null TVP | `0x00` | Error 8060, state 1; no result row; `done` |
| Null TVP with default bit | `0x02` | One row containing 0; `doneInProc`, `doneProc`, return status 0 |
| Valid empty TVP | `0x00` | One row containing 0; `doneInProc`, `doneProc`, return status 0 |

The explicit-null and default-bit requests have identical 78-byte RPC payloads
except for byte offset 69: `0x00` versus `0x02`. Tedious does not expose that
bit through `Request.addParameter`. The capture inserts a one-byte mutation
into its complete, controlled RPC packet after packetization and before TLS
encryption; the retained `rawHex` is what was sent. It attaches only after
login, never observes passwords or TLS records, and limits the request to
512 KiB. Response rows, errors and completion events are captured through
tedious; response packet bytes are not retained.

This is a **product observation**, with an important specification conflict:
the current Microsoft [MS-TDS RPC Request](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/619c43b6-9495-4a58-9e49-a4950db245b3)
rule says `fDefaultValue` **MUST be zero** for `TVP_TYPE_INFO`. This SQL Server
2025 build nonetheless treated `0x02` plus the null TVP wire body as an empty
default table. Do not generalize that to other SQL Server versions, TDS
versions or clients without another capture. The omitted-parameter form avoids
that `fDefaultValue` conflict. This fixture records a
server/client compatibility difference; it does not amend the specification.

Run `node scripts/capture-tvp-default.mjs` on a Docker host with Node
dependencies to write a separate diagnostic capture and compare it with the
retained fixture. `--write-fixture` writes only when the retained path does not
exist. Diagnostic output is refused if it aliases the fixture through a direct
path, symlink component or hard link. The generator checks that four fresh
database runs agree before fixture creation. Containers and databases are
created under unique names and cleaned up by the shared reference helpers.

The capture covers one stored procedure, one three-column table type, a
single-packet RPC request, and a row-count response. It does not establish
raw response token bytes, other TVP schemas, parameter ordering, prepared RPCs,
output parameters, NULL column cells, nonempty default TVPs, fragmented
requests, or runtime support in msduck. Preserve explicit null, omitted and
default-bit forms separately when implementing root RPC binding; the current
wire decoder alone does not execute TVPs.
