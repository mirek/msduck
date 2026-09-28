# Captured Windows-zone rules from 2051 through 2100

The [raw SQL Server 2025 capture](../reference/at-time-zone-history-2051-2100.json)
extends the earlier 1900–2050 [history](at-time-zone-history.md). It retains
the pinned image digest and `@@VERSION`, baseline offsets for all 141
`sys.time_zone_info` names at 2051-01-01 UTC, six seasonal samples, five bounded
decade scans, and hour/minute refinements of every detected daily change.
Every query's TDS descriptors, rows, diagnostics and completion events remain
in the fixture. The capture found 4,000 changes: 800 in each decade.

`node scripts/capture-at-time-zone-history-2051-2100.mjs --check` validates
the full fixture checksum, catalog order, offset bounds, per-zone continuity,
six seasonal samples replayed through the captured transitions, and the
2051 boundary against the preceding rule table without starting SQL Server.
A fresh capture writes a new file and must be compared raw before replacing
retained evidence. `node scripts/generate-at-time-zone-future-rules.mjs --check`
verifies that the compact [runtime extension](../src/at_time_zone_rules_2051_2100.json)
derives exactly from the pinned raw fixture and joins continuously to the
original 20,414-transition table.

The native adapter uses this extension together with the earlier 1900–2050
table and the 0500–0999, 1000–1499, 1500–1799 and 1800–1899 historical prefixes for named-zone
UTC instants from 0500-01-01 through 2100-12-31. It still accepts `UTC` across
the full SQL temporal range because UTC has no transition history. It rejects
uncaptured named-zone instants before 0500 or from 2101 onward rather than
inventing offsets. The [SQL-facing regression](../tests/at_time_zone_future.rs)
checks retained seasonal offsets for Pacific, Central European and Samoa zones,
the 2051 boundary, and the end of the new range.

This remains a versioned observation, not complete named-zone support. A daily
scan could miss changes that cancel between sampled midnights; minute refinement
does not prove subminute boundaries. [SQL Server documents `AT TIME ZONE` as
nondeterministic](https://learn.microsoft.com/en-us/sql/t-sql/queries/at-time-zone-transact-sql)
because its external rules can change. Exact invalid-zone error 9820 and some
TDS result flags remain separate compatibility gaps; this extension does not
normalize them away.
