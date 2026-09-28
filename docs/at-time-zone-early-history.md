# Captured Windows-zone rules from 1500 through 1799

The [raw SQL Server 2025 capture](../reference/at-time-zone-history-1500-1799.json)
retains the pinned image digest and `@@VERSION`, a baseline for all 141
`sys.time_zone_info` names at 1500-01-01 UTC, six seasonal samples, 30 bounded
decade scans, and hour/minute refinements of every detected daily change.
Every query's TDS descriptors, rows, diagnostics and completion events remain
in the fixture. The capture found 45,600 changes, 1,520 in each decade.
These are SQL Server's observed outputs, including its historical Windows-rule
extrapolation; they do not assert actual civil time in these centuries.

`node scripts/capture-at-time-zone-history-1500-1799.mjs --check` validates
the full fixture checksum, catalog order, offset bounds, per-zone continuity,
six seasonal samples replayed through the transitions, and the 1800 boundary
against the next rule table without starting SQL Server. A fresh capture writes
a new file and must be compared raw before replacing retained evidence.
`node scripts/generate-at-time-zone-history-1500-1799.mjs --check` verifies
that the compact [runtime prefix](../src/at_time_zone_rules_1500_1799.json)
derives exactly from the pinned fixture and joins continuously to the existing
1800–1899 table.

The native adapter combines this prefix with the 1800–1899, 1900–2050 and
2051–2100 tables. Named-zone UTC instants from 1500-01-01 through 2100-12-31
are in the captured range; `UTC` retains the full SQL temporal range. Dates
before 1500 and from 2101 onward remain explicitly uncaptured for other names.
The [SQL-facing regression](../tests/at_time_zone_early_history.rs) checks
seasonal offsets for Pacific, Central European and Samoa zones, both ends of
the new range, and explicit rejection before it.

A daily scan could miss changes that cancel between sampled midnights; minute
refinement does not prove subminute boundaries. The pinned SQL Server image
accepts named-zone inputs as early as year 1, so 0001–1499 remains a genuine
compatibility gap. Exact invalid-zone error 9820 and some TDS result flags
remain separate gaps. This extension preserves them as differences rather
than treating a partial snapshot as full parity.
