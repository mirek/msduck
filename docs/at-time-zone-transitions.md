# SQL Server 2024 named-zone transition capture

The [capture script](../scripts/capture-at-time-zone-transitions.mjs) ran against
the pinned SQL Server 2025 image recorded in the
[raw fixture](../reference/at-time-zone-transitions-2024.json). It queried all
141 names from `sys.time_zone_info`, saved the UTC offset at 2024-01-01 00:00,
and compared the January and July 2024 local-noon offsets to the earlier
[catalog capture](at-time-zone-catalog.md). Every name and both sets of offsets
matched that retained capture.

For each month, SQL Server evaluated the offset at every UTC hour. The scan
included the previous month's final hour so a change at the month boundary was
visible; December also included 2025-01-01 00:00 to cover its final hour. For
each changed hourly pair, a second SQL query evaluated every minute between
the pair and retained the first minute with the new offset, plus the rendered
wall time and offset one minute before and at that instant. The fixture keeps
the raw TDS descriptors, rows, diagnostics and completion events for the
baseline, seasonal samples, twelve hourly queries and all nonempty minute
queries. It also keeps the exact generated SQL and `@@VERSION` result.

The scan detected **83 changes across 41 zones** in 2024; 100 catalog zones
had no change detected. Five UTC transitions fell at a half-hour rather than
an hour boundary. Offset changes were 40 decreases of 60 minutes, 41 increases
of 60 minutes, and one decrease and one increase of 30 minutes. `Greenland
Standard Time` had three detected changes, including one at 2024-01-01
02:00 UTC. These are captured observations, not inferred recurrence rules.

This is a minute-precision reference for a future versioned Windows-rule
adapter. An hourly scan can miss two offset changes within one hour that
return to the same sampled offset; minute refinement cannot prove a boundary
at subminute precision. The capture covers 2024 only and does not establish
historical or future transitions, or make the SQL expression available in
msduck. The adapter must identify its rule source and compare its output to
this and the older catalog fixture; it must not silently replace historical
Windows behavior with IANA rules. SQL Server documents `AT TIME ZONE` as
[dependent on external time-zone rules and nondeterministic](https://learn.microsoft.com/en-us/sql/t-sql/queries/at-time-zone-transact-sql?view=sql-server-ver17).

To reproduce on an x86_64 Docker host with Node dependencies installed, run
`node scripts/capture-at-time-zone-transitions.mjs path/to/new.json`. The script
refuses to overwrite existing evidence. Use
`node scripts/capture-at-time-zone-transitions.mjs --check reference/at-time-zone-transitions-2024.json`
to validate the committed fixture without starting SQL Server. The check pins
the full JSON content digest, including rows and metadata, and independently
checks catalog names, sample offsets and every detected minute boundary. A fresh
capture reports any changed sample offsets and still writes their raw evidence
for review; it does not replace or normalize the committed fixture. The check
also verifies continuity of each zone's captured offset sequence and replays
both local-noon samples through that sequence.
