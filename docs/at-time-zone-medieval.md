# Captured Windows-zone rules from 1000 through 1499

The [raw SQL Server 2025 capture](../reference/at-time-zone-history-1000-1499.json)
retains the pinned image digest and `@@VERSION`, a baseline for all 141
`sys.time_zone_info` names at 1000-01-01 UTC, six seasonal samples, 50 bounded
decade scans, and hour/minute refinements of every detected daily change.
Every query's TDS descriptors, rows, diagnostics and completion events remain
in the fixture. The capture found 76,000 changes, 1,520 in each decade.
These are SQL Server's observed outputs, including its historical Windows-rule
extrapolation; they do not assert actual civil time in these centuries.

`node scripts/capture-at-time-zone-history-1000-1499.mjs --check` validates
the full fixture checksum, catalog order, offset bounds, per-zone continuity,
six seasonal samples replayed through the transitions, and the 1500 boundary
against the next rule table without starting SQL Server. A fresh capture writes
a new file and must be compared raw before replacing retained evidence.
`node scripts/generate-at-time-zone-history-1000-1499.mjs --check` verifies
that the compact [runtime prefix](../src/at_time_zone_rules_1000_1499.json)
derives exactly from the pinned fixture and joins continuously to the existing
1500–1799 table.

The native adapter combines this prefix with the 0001–0499, 0500–0999,
1500–1799, 1800–1899, 1900–2050 and 2051–2100 tables. Named-zone UTC
instants from 0001-01-01 through 2100-12-31 are in the captured range;
`UTC` retains the full SQL temporal range. Dates from 2101 onward remain
explicitly uncaptured for other names. The
[SQL-facing regression](../tests/at_time_zone_medieval.rs)
checks seasonal offsets for Pacific, Central European and Samoa zones and the
1000 and 1500 joins. The 0500–0999 regression checks the year-0500 join.

A daily scan could miss changes that cancel between sampled midnights; minute
refinement does not prove subminute boundaries. Some year-1 local-wall inputs
fail; exact error 9813 token parity remains a separate gap. Exact invalid-zone
error 9820 and some TDS result flags remain separate gaps. This extension
preserves them as differences rather
than treating a partial snapshot as full parity.
