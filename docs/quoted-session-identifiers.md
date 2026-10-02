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
for quoted columns. A complete msduck replay at
`ed164392af87e3e6c3249c33820dc0160f082705` retained 267 raw differences with
no transport errors. Qualified projections match completely in both modes,
but unqualified `[@p]` is treated as a parameter: a bound 42 produces `[42,42]`
instead of `[42,7]`. Quoted counters likewise become live values and lose their
stored-column types. Missing quoted names report variable/global errors instead
of 207. The raw replay SHA-256 is
`87eb09d02cdf33bed7c0f0a5e13564096df4990a5060f417a69ea650d89e9e69`.
The replay used a private copy of the exact verified executable, with binary
SHA-256 `5928be6fac7127e801b2a939e2a7327474c35217c0e2c2b844ea315c5cd333c3`;
it did not modify the shared builder source or executable.

[Runtime follow-up #792](https://github.com/mirek/msduck/issues/792) retains
these failures and the separate SET completion and unquoted-variable diagnostic
differences. It is unreserved backlog, and this reference task reserves no
engine files or claims runtime fidelity.

To reproduce without changing the retained fixture:

```sh
node scripts/capture-quoted-session-identifiers.mjs artifacts/compatibility/quoted-session-identifiers/reproduce.json
```

The initial `--write-fixture` operation refuses an existing fixture before
starting a container. Existing output files are also refused, new outputs use
exclusive creation, and the output cannot be the retained fixture path.
The generator restores its temporary typed-parameter
hook, closes both fresh database connections and removes only its owned
random-port reference container.
