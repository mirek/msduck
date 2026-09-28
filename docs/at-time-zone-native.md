# Native Windows-zone rule adapter

The root crate registers internal `__msduck_at_time_zone_local_N` and
`__msduck_at_time_zone_instant_N` DuckDB functions for scales 0–7. The first
interprets tagged `DATETIME2` ticks as a named-zone wall time; the second
interprets tagged `DATETIMEOFFSET` UTC ticks as an instant. Both return the
tagged `DATETIMEOFFSET` representation already used by the server: UTC ticks
and offset minutes. The adapter passes an explicit immutable rule table to the
deterministic transition resolver. The native adapter on its own remains
internal. [PR #506's SQL-facing lowering](https://github.com/mirek/msduck/pull/506/files)
connects captured `DATETIME2(s)` and `DATETIMEOFFSET(s)` expressions to these
functions when that change is present in the checkout; its file changes include
the `docs/at-time-zone-lowering.md` guide.

The compact [rule table](../src/at_time_zone_rules_1900_2050.json) contains
141 Windows names and 20,414 minute-resolved transitions detected from the
[pinned SQL Server 2025 capture](../reference/at-time-zone-history-1900-2050.json).
`node scripts/generate-at-time-zone-rules.mjs --check` verifies that the
committed table derives exactly from the pinned full-content capture digest;
running the command without `--check` regenerates it. Runtime registration
validates the table's provenance, transition order, continuity and offset
bounds before accepting a query. It never reads the host's time-zone database.

The original named-zone table covers UTC instants from 1900-01-01 through
2050-12-31. The separately pinned historical and future tables extend the
combined window to 1800-01-01 through 2100-12-31. UTC itself has no offset
transitions and accepts the full SQL temporal range; other names remain
explicit outside the proven window. The
underlying daily scan can miss changes that cancel before the next midnight,
and minute refinement cannot prove subminute boundaries. The SQL Server rule
source can also change independently of the pinned image. `AT TIME ZONE` is
therefore marked volatile in DuckDB even though this particular table is
immutable.

The SQL-facing work in PR #506 chooses local-wall versus instant conversion
for captured exact temporal inputs and replays selected rows and descriptors.
It does not cover legacy `DATETIME` and `SMALLDATETIME`, columns whose temporal
declaration is unavailable to the lowering pass, or named-zone dates outside
captured rule ranges. The staged declaration binder and core resolver still need exported
crate entry points, and the error path still needs exact SQL Server 8116 and
9820 diagnostics and invalid-zone descriptor behavior. Further differential
tests must retain exact rows, metadata, errors and completion tokens for those
forms and for newly captured case and boundary probes.
