# `AT TIME ZONE` declaration binding

The staged SQL-crate binder in `crates/msduck-sql/src/at_time_zone.rs` maps the
captured temporal input declarations to a nullable `DATETIMEOFFSET` result.
`DATETIME2(s)` and `DATETIMEOFFSET(s)` preserve `s`; `DATETIME` produces scale
3 and `SMALLDATETIME` scale 0. The retained SQL Server 2025 capture reports
error 8116, state 1, class 16 for `DATE` and `INT` inputs, with no result
descriptor. Other input families remain explicitly unsupported until captured.

The fixture-backed test compares result descriptors and the two captured
diagnostics, including empty, NULL-zone and prepared executions. It does not
derive a declaration from a runtime parameter value. Zone-name validation,
invalid-zone error 9820, transition-rule acquisition, native execution and TDS
emission are separate adapter work. `msduck-sql/src/lib.rs` remains reserved by
another worker, so the test imports the binder by path; the public SQL expression
is not available yet. The rule-source divergence recorded in
`docs/at-time-zone-catalog.md` still applies.
