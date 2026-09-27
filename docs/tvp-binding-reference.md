# SQL Server TVP procedure binding reference

`reference/tvp-binding.json` retains bounded post-login Tedious 20 RPC packets
and complete callback observations against pinned SQL Server 2025 image
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`
(product version `17.0.4065.4`). `scripts/capture-tvp-binding.mjs` ran the
same 12 cases in four fresh databases across two independent containers. All
four raw runs matched exactly; the fixture retains every packet, parsed TVP
field, result descriptor, row, error, INFO event, DONE event and return status.
The retained file's SHA-256 is
`9f68eff950f6512b2d87ad9f667f8484ebcb2204b67b3be8f643c212b3eecb67`.

Each database defined `dbo.TvpProbe` and `dbo.TvpOther`, plus `app.TvpProbe`,
with nullable `INT`, `NVARCHAR(10)`, and `VARBINARY(10)` columns. The procedure
`dbo.TvpProbeEcho` expects `dbo.TvpProbe READONLY` and selects all three fields
from `@rows`. Every request contains one row with integer 7, Unicode `Hi`, and
binary `00ff`, except the two-column case and the overflow case. Type names and
column metadata below are client-supplied, not inferred from the fixture.

| Client TVP metadata | SQL Server observation |
| --- | --- |
| `dbo.TvpProbe` | Row returned; `doneInProc`, `doneProc`, return status 0. |
| Empty schema and type name | Same row and completion stream. The procedure's declared type supplies the identity. |
| Empty schema, `TvpProbe` name | Same row and completion stream under this database's default schema. |
| `dbo` schema, empty type name | Error 8049/state 2: invalid TVP type name. |
| Existing `dbo.TvpOther` with identical columns | Error 206/state 3: declared and supplied type identities clash. |
| Existing `app.TvpProbe` with identical columns | Error 206/state 3: schema identity matters. |
| Nonexistent `dbo.Missing` | Error 2715/state 3: type cannot be found. |
| Two columns for the three-column type | Error 500/state 1: wrong TVP column count. |
| `NVARCHAR(5)` metadata for declared `NVARCHAR(10)` | Row accepted and projected with declared `NVARCHAR(10)` result metadata. |
| `NVARCHAR(12)` metadata for declared `NVARCHAR(10)` | Same accepted row and declared result metadata. |
| `BIGINT` metadata and value 7 for declared `INT` | Accepted; result is typed `IntN(4)` with value 7. |
| `BIGINT` value 2147483648 for declared `INT` | Errors 8115/state 2, then 8061/state 1; no result set. |

Successful cases expose `IntN(4)`, `NVarChar(20 bytes)`, and
`VarBinary(10 bytes)` result descriptors with nullable flags, independent of
the client TVP widths. Failure cases emit one final `done` and no procedure
return status. The two errors in the overflow case are separate ordered
diagnostics and must not be collapsed into one. Client metadata is therefore
not an exact equality test against `sys.columns`: the server resolves a type
identity, checks column count, and converts eligible values to the declared
column types.

The [MS-TDS TVP metadata specification](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/0dfc5367-a388-4c92-9ba4-4d28e775acbc)
permits zero-length TVP type-name fields for a stored procedure or function
whose parameter metadata is available server-side. It requires sufficient
client type information for ad-hoc SQL, where that metadata is unavailable.
This capture tests only the procedure path and does not establish ad-hoc TVP
binding behavior.

Run `node scripts/capture-tvp-binding.mjs` on a Docker host to write a separate
diagnostic capture and compare it with the retained fixture. `--write-fixture`
creates a missing fixture and refuses to overwrite an existing one. The
generator rejects direct, symlinked and hard-linked output aliases to the
fixture, bounds post-login request captures at 512 KiB, and requires complete
TDS packets before decoding. An independent four-database replay matched the
retained fixture.

The cases cover one three-column nullable table type, one procedure, one row,
bounded non-PLP cells, and Tedious-generated RPC metadata. They do not cover
multiple parameters, permissions, defaulted table columns, constraints,
ordering/unique metadata, arbitrary implicit conversions, PLP cells, prepared
RPCs, or TVP execution in msduck. The existing root RPC dispatcher and stored
procedure executor do not yet consume TVPs.
