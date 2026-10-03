# Distribution windows

PERCENT_RANK and CUME_DIST share ranking signature checks: zero arguments,
mandatory OVER and ORDER BY, and no ROWS/RANGE frame. Named window inheritance
is expanded before validation. The deterministic validator now uses SQL
Server's exact uppercase function names and messages: 4114 for an argument,
10753 for missing OVER, 4112 for missing ORDER BY, and 10752 for a frame.
Other ranking functions retain their existing diagnostics.
They remain outside BIGINT ranking inference: DuckDB computes DOUBLE results,
which the existing wire encoder exposes as eight-byte FLOATN (SQL FLOAT(53)).

Tedious tests cover tied values, ascending/descending NULL ordering, partitions,
singleton and all-NULL partitions, empty-result metadata, outer aggregates,
prepared execution and invalid prepared windows. A diagnostic audit probe
captures values, metadata and missing-order diagnostics. The pinned SQL Server
2025 reference capture in `reference/distribution-reference.json` records 16
batch observations from two independent containers and fresh databases. The
replay script is `node scripts/capture-distribution-reference.mjs --check`, or
run it without `--check` against fresh containers to compare raw rows,
descriptors, errors, event order and DONE status words. The two retained runs
match exactly; the fixture's SHA-256 is pinned in the replay script.

The reference confirms that both functions expose nullable eight-byte FLOATN
descriptors even for empty results. For ascending `(NULL,10,10,20,30)`, their
values are `(0,0.2)`, `(0.25,0.6)`, `(0.25,0.6)`, `(0.75,0.8)`, `(1,1)`;
descending order places the NULL last. A singleton returns `(0,1)` and two
NULLs each return `(0,1)`. A named window works. Successful SELECT batches
end with a DONE status word of 16 and command 193; invalid calls emit ERROR
before DONE with status 2 and command 253, without column metadata.

The reference shows that a frame raises 10752/state 3/class 15 and an argument
raises 4114/state 1/class 15. Missing OVER uses 10753/state 3/class 15;
missing ORDER BY uses 4112/state 1/class 15. The deterministic validator now
matches those numbers and exact messages for these two functions. The root TDS
error adapter still needs a separate comparison and fix for state/class, and
the shared tedious test currently expects the old 4106 frame number for these
two functions. That test belongs to another active claim and must be updated
after its owner hands it off or merges. Broader floating-point expression
coercion and complete diagnostic fidelity remain unverified.
WINDOW clauses without FROM now reach named-window resolution and inherited
frame validation; both source-free and VALUES queries are covered.

The upstream mssqlite implementation has been inspected for related mappings;
its behavior is not used as SQL Server ground truth.

References: Microsoft [PERCENT_RANK](https://learn.microsoft.com/en-us/sql/t-sql/functions/percent-rank-transact-sql)
and [CUME_DIST](https://learn.microsoft.com/en-us/sql/t-sql/functions/cume-dist-transact-sql).
