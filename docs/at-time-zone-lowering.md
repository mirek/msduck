# SQL-facing `AT TIME ZONE` lowering for captured exact temporal types

The existing root expression visitor now lowers `AT TIME ZONE` over a declared
`DATETIME2(s)` or `DATETIMEOFFSET(s)` input into the native adapter. The choice
uses the input's declared expression shape, never its value. `DATETIME2` is a
local wall time, while `DATETIMEOFFSET` already identifies a UTC instant.
The returned tagged value retains scale `s` from 0 through 7. Result inference
recognizes both the original expression and the lowered native function, so a
NULL or empty result can still carry `DATETIMEOFFSET(s)` metadata.

The [root tests](../tests/at_time_zone.rs) exercise the SQL parser, root
translator, DuckDB function, TDS descriptor and encoded result through the
server. They check the retained 2025 reference's UTC, Central European and
Pacific gap/overlap cases, chained conversion, typed NULL and empty results.
For the replayed successful cases they compare the reference's first-column
nullable flags, type, scale and encoded instant/offset. A prepared test reuses
one declared `DATETIME2(7)` binding across changing values and NULL, and a
6,000-row test checks that volatile timestamp and zone inputs each run once.
The native rule table remains the pinned 1900–2050 snapshot described in
[the adapter notes](at-time-zone-native.md).

This is a captured-family increment, not full `AT TIME ZONE` compatibility.
Legacy `DATETIME` and `SMALLDATETIME` lose their distinct declaration before
this visitor, and column expressions without an available temporal declaration
remain unsupported here. The staged declaration binder has not been exported;
the public error path still needs exact 8116 for invalid source types and 9820
with the SQL Server descriptor for invalid names. The snapshot cannot establish
rules outside 1900–2050 or changes missed by its daily/minute capture. New
reference probes should settle zone-name case handling and boundary behavior
before those gaps are closed.
