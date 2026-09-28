# SQL Server Windows-zone rules observed from 1900 through 2050

The [capture script](../scripts/capture-at-time-zone-history.mjs) ran against
the pinned SQL Server 2025 image and retained its raw TDS evidence in
[the fixture](../reference/at-time-zone-history-1900-2050.json). It recorded
the offset at 1900-01-01 00:00 UTC for all 141 `sys.time_zone_info` names,
then scanned every UTC day through 2050-12-31 in bounded decade chunks. Each
detected daily change was refined to the first changed hour and minute, with
offsets and rendered local times immediately before and at that minute. The
fixture preserves the SQL, result descriptors, rows, diagnostics, completion
events and `@@VERSION` for the baseline, seven catalog samples, every decade
scan and every refinement batch.

This image produced **20,414 detected transitions across 98 zones**; 43 zones
had no change detected in the range. The 1900s through 1990s each yielded
1,520 detected transitions per decade, while 2020–2029 yielded 846. Nearly a
thousand captured boundaries were not on a UTC hour. The 2011 `Samoa Standard
Time` change advanced its offset by 1,440 minutes, from `-10:00` to `+14:00`.
These are SQL Server outputs, including its historical rule extrapolation;
they are not assertions about civil time as actually observed in those years.

The source/fixture check validates the 141-name catalog set, offset bounds and
continuous per-zone histories. It replays the seven retained 1900–2050
local-noon catalog samples through the captured history and compares all 83
minute-resolved 2024 transitions with the [separate 2024 capture](at-time-zone-transitions.md).
The two independent SQL Server runs of this capture produced the same full
JSON digest. A new capture retains changed sample values and reports the
differences rather than rewriting this fixture.

This is a versioned rule **snapshot**, not complete `AT TIME ZONE` support.
A daily scan can miss changes that cancel before the next sampled midnight;
hourly and minute refinement cannot establish subminute boundaries. The
snapshot does not cover dates before 1900 or after 2050, and SQL Server's
external time-zone rules may change with updates. A runtime adapter should
feed explicit transitions into the deterministic resolver, identify this
snapshot's provenance, reject unproven ranges rather than inventing rules,
and retain exact offset-aware versus local-wall semantics. [Microsoft documents
`AT TIME ZONE` as nondeterministic because its rule source is external](https://learn.microsoft.com/en-us/sql/t-sql/queries/at-time-zone-transact-sql?view=sql-server-ver17).

Run `node scripts/capture-at-time-zone-history.mjs --check reference/at-time-zone-history-1900-2050.json`
to validate the committed fixture without SQL Server. On a Docker-capable
x86_64 host with Node dependencies, run
`node scripts/capture-at-time-zone-history.mjs path/to/new.json` to create a
fresh capture; the script refuses to overwrite an existing file. Compare
the new raw result with the retained fixture if the digest changes.
