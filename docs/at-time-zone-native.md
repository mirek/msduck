# Native Windows-zone rule adapter

The root crate registers internal `__msduck_at_time_zone_local_N` and
`__msduck_at_time_zone_instant_N` DuckDB functions for scales 0–7. The first
interprets tagged `DATETIME2` ticks as a named-zone wall time; the second
interprets tagged `DATETIMEOFFSET` UTC ticks as an instant. Both return the
tagged `DATETIMEOFFSET` representation already used by the server: UTC ticks
and offset minutes. The adapter passes an explicit immutable rule table to the
deterministic transition resolver. SQL expression lowering does not yet call
these functions, so `AT TIME ZONE` is still not a supported public feature.

The compact [rule table](../src/at_time_zone_rules_1900_2050.json) contains
141 Windows names and 20,414 minute-resolved transitions detected from the
[pinned SQL Server 2025 capture](../reference/at-time-zone-history-1900-2050.json).
`node scripts/generate-at-time-zone-rules.mjs --check` verifies that the
committed table derives exactly from the pinned full-content capture digest;
running the command without `--check` regenerates it. Runtime registration
validates the table's provenance, transition order, continuity and offset
bounds before accepting a query. It never reads the host's time-zone database.

The adapter accepts UTC instants from 1900-01-01 through 2050-12-31. It
rejects dates outside that captured range instead of extrapolating. The
underlying daily scan can miss changes that cancel before the next midnight,
and minute refinement cannot prove subminute boundaries. The SQL Server rule
source can also change independently of the pinned image. `AT TIME ZONE` is
therefore marked volatile in DuckDB even though this particular table is
immutable.

Before public support, the staged SQL declaration binder and core resolver
need exported crate entry points, the root expression pass needs to choose
the local or instant function while preserving legacy `DATETIME` and
`SMALLDATETIME` scales, and the error path needs SQL Server's 9820 invalid-zone
diagnostic and exact descriptor behavior. Public differential tests should
then replay the retained rows, metadata, errors and completion tokens, plus
newly captured case and boundary probes. Native tests here establish only the
internal adapter behavior.
