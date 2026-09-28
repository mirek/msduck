# `AT TIME ZONE` reference capture

`AT TIME ZONE` is not implemented in msduck. The retained [SQL Server 2025 capture](../reference/at-time-zone.json) is evidence for a future implementation, not a compatibility claim. It was produced by [the capture script](../scripts/capture-at-time-zone.mjs) against the pinned container image recorded in the fixture. `@@VERSION` identifies SQL Server 2025 RTM-CU7, build 17.0.4065.4, on Linux. The fixture contains 25 batches and five executions of one prepared handle.

The fixture retains TDS column descriptors, decoded rows, errors, informational messages, and completion events. Tedious represents a `datetimeoffset` row as a UTC `Date`, which does not itself expose the original offset. Each successful query therefore also selects SQL Server's default string rendering and `DATEPART(TZOFFSET, value)` in minutes. The rendered column is needed to distinguish wall time and offset from the UTC instant; style 127 conversion normalized the display to `Z` in an exploratory capture and was not retained. Parameter values in the fixture are ISO strings for reproducibility; the script binds them as `DateTime2(7)` `Date` objects through Tedious.

Observed behavior in this image:

| Input | Reference result |
| --- | --- |
| `datetime2(7)` in `Central European Standard Time` | Preserves scale 7 and local wall time; 2024-01-02 03:04:05.1234567 becomes `+01:00`. |
| `datetime`, `smalldatetime` in `UTC` | Result descriptor is `DateTimeOffset`, with scale 3 and 0 respectively. |
| Offset-bearing `datetimeoffset` | Converts the instant to the named zone; chained conversions retain that instant. |
| 2022-03-27 02:30 Central European local time | Spring gap moves to `03:30:00 +02:00`. |
| 2022-10-30 02:30 Central European local time | Autumn overlap selects `02:30:00 +02:00`, the pre-change offset. |
| 2024-03-10 02:30 Pacific local time | Spring gap moves to `03:30:00 -07:00`. |
| 2024-11-03 01:30 Pacific local time | Autumn overlap selects `01:30:00 -07:00`. |
| Typed NULL input or NULL zone | Typed nullable `DateTimeOffset` result with a NULL row. |
| Invalid zone | Error 9820; the result descriptor is sent before the error. |
| `date` or `int` input | Error 8116 during binding, without a result descriptor. |

The successful scalar results and the empty result all report `DateTimeOffset` metadata with flags 33 in this capture. Prepared executions reuse one handle with different timestamp/zone bindings and retain `doneInProc` plus `doneProc` events; batch queries use `done`. The fixture preserves the exact details, including fractional precision, messages, and error text. Do not normalize them for future comparisons.

Windows time-zone names and rules come from the host operating system's time-zone data. SQL Server [documents `AT TIME ZONE` as nondeterministic](https://learn.microsoft.com/en-us/sql/t-sql/queries/at-time-zone-transact-sql?view=sql-server-ver17) because those rules can change outside SQL Server. The pinned SQL Server image alone does not freeze future behavior on a different host or after an OS rule update. A future root adapter should acquire or package a versioned zone-rule source and pass the selected rule snapshot or resolved transition data explicitly into deterministic conversion code. `msduck-sql` can type-check and normalize the syntax, but must not read host clocks, environment variables, or mutable time-zone state. Treat Windows-name mapping, ambiguous/gap rules, historical transitions, and rule-version drift as explicit implementation work.

To reproduce on a Docker-capable host with Node dependencies installed, run `node scripts/capture-at-time-zone.mjs path/to/new.json`. The script refuses to overwrite an existing fixture. Run `node scripts/capture-at-time-zone.mjs --check reference/at-time-zone.json` for a container-free check that the committed case list, prepared bindings, image digest, and capture envelopes match this source. A new SQL Server run should be compared as raw evidence against the retained file; a changed time-zone rule is a material difference, not a result to silently update.
