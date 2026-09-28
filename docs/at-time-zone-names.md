# `AT TIME ZONE` name lookup capture

`reference/at-time-zone-names.json` retains 24 raw TDS captures from the pinned
SQL Server 2025 RTM-CU7 image identified in the fixture. Each case records the
query, column descriptors, rows, errors, information messages, and completion
events. `scripts/capture-at-time-zone-names.mjs --check` validates the retained
image, version, ordered probes, capture shape, and SHA-256 without starting SQL
Server. A fresh capture requires the same pinned image and writes only a new
fixture path; compare its raw output before replacing retained evidence.

The [SQL Server `AT TIME ZONE` documentation](https://learn.microsoft.com/en-us/sql/t-sql/queries/at-time-zone-transact-sql)
describes Windows time-zone names and the corresponding
[`sys.time_zone_info` catalog](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-time-zone-info-transact-sql).
The retained probes establish the more precise behavior for this SQL Server
build:

| Input | Captured outcome |
| --- | --- |
| `UTC`, `utc`, `uTc`; three cases of `Pacific Standard Time` | Same named-zone result, with `DateTimeOffset(7)` column flags 33 |
| Leading/trailing spaces; empty or unknown name | Error 9820, state 1; result descriptor precedes the error |
| `Etc/UTC`, `Europe/Zurich`, `GMT`, fullwidth `ＵＴＣ`, combining accent, tab | Error 9820; these are not aliases or normalization variants |
| Leading, embedded, trailing NUL in `UTC` | Succeeds as UTC; captured `DATALENGTH` and binary bytes prove the NUL is present |
| `UTC` + NUL + `garbage` | Error 9820; lookup does not stop at NUL |
| Dynamic lower-case, trailing-space, and NULL `NVARCHAR` | Same success, error, and NULL outcomes as literals |

The root native adapter removes NUL code points only when building its bounded
catalog lookup key, then applies ASCII case folding. It keeps the original
input for eventual diagnostics. Its catalog remains the pinned 141 Windows
names; no IANA or GMT alias is introduced. `tests/at_time_zone_names.rs`
executes the retained SQL through the server and checks supported results and
metadata.

The current server still differs from SQL Server for invalid names: it reports
a generic DuckDB-backed error instead of exact TDS error 9820 with the supplied
name, state 1, class 16, and the captured descriptor/completion sequence.
For successful names the current server also emits first-column flags 1 where
the direct SQL Server capture emits 33; the SQL-facing regression records both
values explicitly while still checking the type, scale, row, and offset.
That remains a separate result/error-alignment task. The fixture retains these
differences instead of treating a rejected query as an exact compatibility pass.
