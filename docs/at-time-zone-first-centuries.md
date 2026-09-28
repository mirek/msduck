# SQL Server named-zone evidence from 0001 through 0499

The [raw SQL Server 2025 capture](../reference/at-time-zone-history-0001-0499.json)
records `@@VERSION`, the pinned container image, and all 141 ordered
`sys.time_zone_info` names. It retains four all-zone year-1 boundary probes,
423 individual local-wall boundary probes (89 midnight errors, five noon
errors, and none at 14:00), a UTC baseline at the first representable
instant, six seasonal samples, 50 bounded scans, and hour/minute refinements
of every detected daily offset change. Each query's TDS descriptors, rows,
diagnostics, and completion remain in the fixture. The scan and individual
local probes used separate containers of the same pinned image and `@@VERSION`.
The scan found 75,895
changes, including 1,567 in 0001–0010 and 47 on the first day alone.

The lower boundary is not a simple extrapolation of later rules. At
`0001-01-01T00:00:00+00:00`, SQL Server returns offset `+00:00` for Pacific
and Samoa. Their observed offsets change to `-08:00` at 08:00 UTC and
`-11:00` at 11:00 UTC, respectively. The 47 first-day changes occur when
those negative offsets first produce a representable local date. An
all-zone local-wall query at midnight produces error 9813. The individual
probes show which zone and local time succeeds or fails, without inferring
unreturned rows from a partially completed all-zone query. This capture
preserves the observed behavior; it does not assign civil-time meaning to
these early Windows-rule outputs.

`node scripts/capture-at-time-zone-history-0001-0499.mjs --check` validates
the retained content checksum, catalog order, boundary outcomes, offset
bounds, per-zone transition continuity, sample replay, and the year-0500 join
to the next rule table. The script uses `setUTCFullYear` for years 1–99;
`Date.UTC(year, ...)` would silently remap those years into 1901–1999. A
fresh capture writes a new file rather than replacing pinned evidence, so
changed SQL Server output can be compared raw.

The runtime adapter still accepts named-zone UTC instants only from
0500-01-01 through 2100-12-31; `UTC` retains the full SQL temporal range.
This reference task does not extend runtime behavior. A separately claimed
successor should generate and integrate rules only after accounting for the
year-1 boundary and SQL Server's local-wall error behavior.

Daily sampling can miss changes that cancel between sampled midnights, and
minute refinement does not prove subminute boundaries. These limitations
remain visible even when every detected transition passes continuity checks.
