# SQL Server legacy DATEADD reference

`reference/dateadd-legacy.json` retains 48 raw observations from SQL Server
2025 (`17.0.4065.4`) using the pinned image
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
`scripts/capture-dateadd-legacy.mjs` captured the same records in four fresh
databases across two independent containers, then checked the retained fixture
in a separate run. Each record keeps the actual Tedious rows, column type and
width, flags, errors with number/state/class/message, information messages and
DONE events. The script refuses an output alias, existing output or a retained
fixture overwrite. These are SQL Server reference observations, not msduck
compatibility results.

## Observed contract

- DATEADD retains its typed legacy input family. DATETIME results use `DateTimeN`
  width 8; SMALLDATETIME uses `DateTimeN` width 4. The same metadata appears for
  NULL and empty results, stored columns and RPC parameters. A direct string
  literal returns width-8 DATETIME, including a NULL date argument.
- Month-end arithmetic clamps the day: `2024-01-31` plus one month is
  `2024-02-29`. A DATETIME leap-day value plus one year becomes `2025-02-28`.
- DATETIME uses its legacy 1/300-second grid. Adding 1 millisecond to midnight
  leaves it unchanged; adding 2 or 3 milliseconds produces `.003`. Adding -1
  millisecond to midnight also leaves it unchanged. A stored `.997` tick survives
  month-end and second additions.
- SMALLDATETIME stores minutes. Seconds `-30..29` and milliseconds
  `-30001..29998` leave a minute unchanged; `-31` seconds or `-30002`
  milliseconds move back a minute, while `30` seconds or `29999` milliseconds
  move forward. Stored and RPC forms retain the same four-byte result metadata.
- Microsecond and nanosecond additions to DATETIME, and microsecond addition to
  SMALLDATETIME, return error 9810 with the named type/datepart. Range overflow
  returns 517, state 1 for DATETIME and state 2 for SMALLDATETIME. A literal
  with four fractional second digits or a timezone suffix returns 241/state 1.
- On this pinned SQL Server 2025 build, an amount of `2147483648` is accepted
  as BIGINT and then fails with date overflow 517. An amount one above BIGINT
  maximum fails conversion with 8115/state 2. A fractional `-1.9` day amount
  truncates toward zero to `-1`.
- RPC responses have `doneInProc` followed by `doneProc`; direct batches have
  `done`. The fixture preserves full completion records, not just these names.

The [Microsoft DATEADD reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/dateadd-transact-sql?view=sql-server-ver17)
specifies the dynamic return type, legacy SMALLDATETIME thresholds, unavailable
microsecond/nanosecond units and SQL Server 2025 BIGINT extension. The capture
pins exact results and diagnostics for this particular server build.

The owner-controlled
[mssqlite date-functions implementation](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/engine/src/date-functions.ts)
was inspected. Its `dateadd` parses a date string into calendar parts, clamps
calendar months, adds subday 100ns ticks and formats non-offset results at scale
3. It does not distinguish DATETIME from SMALLDATETIME or encode their separate
rounding grids, widths, ranges and errors. Its calendar arithmetic is useful as
a model, but its formatter cannot be reused as the full legacy result adapter.

## Implementation gates

The root `src/dateadd.rs` currently dispatches DATE, TIME, DATETIME2 and
DATETIMEOFFSET. Legacy DATETIME/SMALLDATETIME and direct string-literal returns
still need source-aware binding, typed output metadata, native tick/minute
arithmetic and exact overflow/unsupported-unit diagnostics. BIGINT amounts also
need a separate, bounded conversion path. Do not implement these by coercing
legacy inputs through DATETIME2: doing so loses the 1/300-second grid and
SMALLDATETIME minute thresholds.

An implementation should replay every applicable raw record and preserve any
remaining metadata, error or completion differences in a local comparison.
Typed NULL, empty result sets, stored values and RPC parameter reuse must be
covered alongside scalar literals. The existing `docs/dateadd.md` describes
current msduck behavior; this file records the independent SQL Server target.
