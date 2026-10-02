# Quoted session and parameter identifiers

The pinned SQL Server reference distinguishes delimited column names from
session counters and scalar variables. Bracket names and double quotes with
QUOTED_IDENTIFIER ON behave alike for `@@OPTIONS`, `@@TRANCOUNT` and `@p`.
The same source declarations survive qualification, parentheses, derived
queries, CTEs and empty results.

`scripts/capture-quoted-session-identifiers.mjs` captures 30 complete batch/RPC
observations twice in fresh databases. It uses the existing full-token observer
over a fixed two-row source and checks per-observation bounds before retention.
It retains rows, descriptors, errors and completion/event order, and
compares the entire runs without normalizing fields. The fixture image digest
is recorded in `reference/quoted-session-identifiers.json`; fixture SHA-256:
`8cf62bb9fc3962f9b6eeb5729f63210123cbd2f5c7dbfbe1e51700e548edb5e3`.

The captured source columns have these declarations, also when no rows return:

| Quoted column | Wire declaration | Flags | Stored value |
| --- | --- | --- | --- |
| `[@@OPTIONS]` | NVARCHAR(20), 40-byte metadata capacity | 9 | `stored-options` or NULL |
| `[@p]` | nullable INTN, 4-byte capacity | 9 | 7 or NULL |
| `[@@TRANCOUNT]` | fixed SMALLINT | 8 | 12 or 13 |

An actual `@@OPTIONS` or `@@TRANCOUNT` expression instead has fixed INT
metadata and flags 32. A local or bound `@p` projected alongside `[@p]` has
nullable INTN metadata and flags 33; its value can be 42 or NULL while the
column remains 7. Empty parameterized results preserve both descriptors.

Missing `[@@MISSING]` and `[@missing]` report column error 207, state 1,
class 16. Unquoted `@missing` instead reports variable error 137, state 2,
class 15. Complete diagnostic text and completion tokens remain in the fixture.

These are SQL Server observations. PR #786 verifies bounded logical inference
for quoted columns; this capture does not establish msduck runtime fidelity.
Root session substitution, variable binding and physical descriptor propagation
must be replayed against every retained observation before claiming support.
No engine scope is reserved by this reference task.

To reproduce without changing the retained fixture:

```sh
node scripts/capture-quoted-session-identifiers.mjs artifacts/compatibility/quoted-session-identifiers/reproduce.json
```

The initial `--write-fixture` operation refuses an existing fixture before
starting a container. The generator restores its temporary typed-parameter
hook, closes both fresh database connections and removes only its owned
random-port reference container.
