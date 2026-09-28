# SQL Server DATEFORMAT reference

`SET DATEFORMAT` controls how SQL Server interprets date character strings at
execution time. The [pinned SQL Server 2025 capture](../reference/dateformat.json)
records 43 observations from each of two independent containers. It checks every
documented order (`mdy`, `dmy`, `ymd`, `ydm`, `myd`, `dym`) against ambiguous text and
an order-specific spelling. It retains `DATE`, `DATETIME`, `DATETIME2(7)` and
`DATETIMEOFFSET(7)` descriptors, rows, conversion errors and ordered wire-token
events, including raw DONE status words, decoded bits and command codes.

For `03/04/2024`, `mdy` returns March 4 and `dmy` returns April 3 in all four
target types. `ymd` returns March 4 for this input. Under `ydm`, `myd` and `dym`,
the newer `DATE`, `DATETIME2` and `DATETIMEOFFSET` conversions return NULL through
`TRY_CAST`, while legacy `DATETIME` still produces a date. The `ydm` spelling
`2024/05/04` is particularly important: `DATETIME` returns April 5, but the
newer types return May 4. For `2024/31/12` under `ydm`, strict `DATE` conversion
emits error 241 (state 1, severity 16) after a `Date` descriptor and no row;
strict `DATETIME` returns December 31. The failed `DATE` conversion emits a
`DONE_ERROR` token with command 193; invalid DATEFORMAT uses the same error bit
with command 249. Neither has an attention or server-error bit. Their ordered
wire events are `COLMETADATA, ERROR, DONE` and `ERROR, DONE`, respectively.

The same ISO timestamp results under `dmy`, `ydm` and `mdy`. The capture retains
the typed DATETIMEOFFSET value, a text rendering with its original `+02:00`
offset, and `DATEPART(TZOFFSET)` of 120 minutes. The order probes likewise
include offset text and minutes alongside the typed temporal results. A single batch
changes from `dmy` to `mdy` and returns April 3 followed by March 4 for the
same slash text. Another connection retains its own format: `24/04/05` is NULL
under its default `mdy`, then April 5 under `ymd`, while the first connection
keeps its `dmy` interpretation. `SET LANGUAGE
us_english` resets the first connection to `mdy`, and a later `SET DATEFORMAT
dmy` overrides it. Invalid `SET DATEFORMAT xyz` emits error 2741 (state 1,
severity 16) without changing the preceding format. A variable can supply a
valid format. One `sp_prepare` handle emits an empty metadata result when
prepared, then produces April 3 and March 4 when executed under successive
`dmy` and `mdy` settings; the fixture preserves all three result boundaries.

Run `node scripts/capture-dateformat.mjs --check` to validate the pinned capture
without starting SQL Server. The check pins the fixture's SHA-256 as well as
the named case plan and key observed results; a changed capture needs a new
two-container replay before updating that digest. A new capture uses two
independent containers and fresh private databases, comparing their complete
records before writing. The script uses exclusive writes to refuse replacing
either the output or the retained fixture; `--write-fixture` only creates a
missing fixture. It writes the requested output before comparing with the
retained fixture, so drift remains available for inspection. If the two fresh
containers disagree or a run fails validation, it writes the available complete
runs under `divergentRuns` before failing. The retained capture was made on
`linux.local`; the local Docker VM exposed only 2 GiB and SQL Server exited
before login readiness. This is a resource limit of that VM, not a DATEFORMAT
result.

The current server has no explicit DATEFORMAT session setting in its engine
source. This fixture is reference evidence for follow-up binding and runtime
work; it does not claim implemented msduck behavior. Tedious normalizes temporal
values to JavaScript dates, including DATETIMEOFFSET to a UTC instant. The
separate text and TZOFFSET columns retain the original SQL offset, but this
fixture is still not a raw temporal wire-payload capture. It does
not establish behavior for every date spelling, language, style code, RPC
temporal parameter, or locale-specific month name.

Microsoft's [SET DATEFORMAT reference](https://learn.microsoft.com/en-us/sql/t-sql/statements/set-dateformat-transact-sql?view=sql-server-ver17)
documents the six orders, runtime application, the `ydm` exception for newer
types, and precedence over `SET LANGUAGE`. The retained SQL Server 2025 rows and
wire descriptors, rather than those general statements alone, are the source for
future msduck compatibility assertions.
