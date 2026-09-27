# SQL Server SELECT TOP PERCENT and WITH TIES reference

`reference/select-top-percent.json` retains 40 raw tedious observations in each
of two fresh databases on the pinned SQL Server 2025 image
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
The server reported ProductVersion `17.0.4065.4` and collation
`SQL_Latin1_General_CP1_CI_AS`. A second fresh container, again with two fresh
databases, reproduced the fixture byte for byte. The fixture SHA-256 is
`12140917db38662bb1b81a97147a8dd519e3ad965f828e9b288fc9c552a59dba`.
The two full raw captures are retained separately under ignored
`artifacts/compatibility/select-top-percent/prepared-first.json` and
`prepared-second.json` in the owner's worktree. The generator records SQL text,
ordered rows, column descriptors, errors, information events, RPC return status
and DONE-family counts. It checks stable invariants across databases and against
the retained fixture. For tie cases, only the stability comparison sorts rows
within an unspecified equal-key order; every raw run retains its actual order.

The seven-row table has scores `10,9,9,8,8,7,NULL` in ID order. Observed
`PERCENT` counts match the documented ceiling rule: `1%` returns one row,
`14.285%` one, `14.286%` two, `25%` two, `33.333%` three, and `50%` four.
`0%` returns zero rows. A query whose source has no rows still sends two typed
columns (`Int` flags 8 and nullable `IntN` flags 9) before DONE count 0.

| Case | Observed boundary |
| --- | --- |
| `TOP (2) WITH TIES` ordered by score | IDs 1, 2, 3; DONE count 3. |
| `TOP (25) PERCENT WITH TIES` | IDs 1, 2, 3; the two-row percentage cutoff expands to three. |
| `TOP (50) PERCENT WITH TIES` | IDs 1–5; the four-row cutoff expands to five. |
| `WITH TIES` without `ORDER BY` | Error 1062, state 1, class 15; no result metadata. |
| Percentage below 0 or above 100 | Error 1031, state 1, class 15; no result metadata. |
| NULL percentage | Error 1014, state 1, class 15; no result metadata in a direct batch. |
| Negative or NULL nonpercentage count with ties | Errors 127 and 1060 respectively, state 1, class 15. |
| Nonnumeric text percentage | Conversion error 8114, state 5, class 16. |

RPC-bound FLOAT `25` returns two rows with DONEINPROC count 2 followed by
DONEPROC. A prepared `25% WITH TIES` returns three; prepared `50% WITH TIES`
returns five. A prepared NULL percentage produces error 1014 **after** the
two-column metadata token, unlike the direct NULL query. Reusing that prepared
handle with `25` succeeds again and matches its first execution. The fixture
also covers variables, an aggregate, a nested TOP query, and a set-operation
branch. Errors and completion sequences remain raw in the fixture.

These observations establish this pinned SQL Server build's behavior for the
captured cases. They do not test character collations, volatile count
expressions, DISTINCT, SELECT INTO, transaction effects, concurrent writes, or
every nested query shape. `msduck` still explicitly rejects SELECT TOP PERCENT
and WITH TIES; a successor must implement and compare actual client behavior.
The [Microsoft TOP specification](https://learn.microsoft.com/en-us/sql/t-sql/queries/top-transact-sql)
documents percentage rounding and the requirement for ORDER BY with ties.

On a host with Node.js 24+ and Docker, run
`node scripts/capture-select-top-percent.mjs artifacts/compatibility/select-top-percent/recheck.json`.
The script creates and removes its own pinned container and fresh databases,
compares against the retained fixture, and refuses to overwrite that fixture.
`--write-fixture` is for initial creation only when the file is absent.
