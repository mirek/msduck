# SQL Server named-zone evidence from 2101 through 2500

The [raw SQL Server 2025 capture](../reference/at-time-zone-history-2101-2500.json)
retains the pinned container image and `@@VERSION`, a 2101-01-01 UTC baseline
for all 141 ordered `sys.time_zone_info` names, six seasonal samples, 40
bounded decade scans, and hour/minute refinements of every detected daily
change. Each query's TDS descriptors, rows, diagnostics and completion remain
in the fixture. The scans found 32,000 changes, 800 in each decade. For
example, the retained January/July samples report Pacific offsets of
`-08:00`/`-07:00`, Central European offsets of `+01:00`/`+02:00`, and Samoa
offset `+13:00` at 2101, 2300 and 2500.

`node scripts/capture-at-time-zone-history-2101-2500.mjs --check` validates
the fixture's content digest, source image and version, catalog order, offset
bounds, per-zone continuity, sample replay and the 2101 join to the pinned
2051–2100 runtime table without starting SQL Server. A fresh capture must use
a new output path; it cannot overwrite the retained evidence.

The current native adapter still accepts captured named-zone instants only
through 2100. Integrating this evidence into runtime behavior is separate
work. The 400-year capture records SQL Server's observed Windows-zone outputs;
it does not prove that rules repeat after 2500. Daily sampling could miss
changes that cancel between sampled midnights, and minute refinement does not
prove subminute boundaries. These limits remain visible rather than treating
the snapshot as full named-zone compatibility.
