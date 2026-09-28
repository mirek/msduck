# UTC `AT TIME ZONE` beyond the captured transition window

The [pinned SQL Server 2025 capture](../reference/at-time-zone-utc-range.json)
retains 16 direct `AT TIME ZONE` cases at years 0001, 1899, 1900, 2050, 2051
and 9999. The [capture script](../scripts/capture-at-time-zone-utc-range.mjs)
stores the image digest, `@@VERSION`, TDS descriptors, rows, errors and
completion events. Its `--check` mode verifies the retained fixture without a
container. A fresh capture writes a new path and must be compared raw before
the pinned evidence is replaced.

[SQL Server documents](https://learn.microsoft.com/en-us/sql/t-sql/data-types/datetimeoffset-transact-sql)
the `DATETIMEOFFSET` range as year 0001 through 9999. The pinned capture
confirms `AT TIME ZONE 'UTC'` accepts `DATETIME2(7)` values at both endpoints,
plus offset-bearing inputs before 1900 and after 2050. An offset-bearing
`+02:00` input preserves its UTC instant when converted to UTC.

The native adapter's transition snapshot covers 1900-01-01 through 2050-12-31
for 141 named zones. UTC has offset zero and no transitions in that snapshot.
The adapter now skips the snapshot-range guard **only** for the exact `UTC`
catalog key after the previously captured case/NUL lookup normalization. The
validated temporal core still checks the full type range and source offset.
Other names retain a guard based on their separately captured rule range. The
reference successfully converts Pacific Standard Time in 1899 and 2051; the
former remains outside the original snapshot, and the latter needs a future
rule extension. No offset should be invented for uncaptured dates.

The SQL-facing test checks the supported UTC rows, type and scale, plus NULL
and scale-zero boundary values. Two differences remain explicit: SQL Server's
direct result descriptor has flags 33 while this server currently emits 1;
the 1899 Pacific reference rows remain outside captured named-zone history.
The test retains the 2051 Pacific reference rows but leaves their positive
server assertion to the future-rule regression. It checks explicit rejection
in 2101, beyond that planned extension. None of these differences is treated
as a compatibility pass.
