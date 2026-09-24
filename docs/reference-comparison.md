# SQL Server reference comparison

`npm run audit:local` records the 17 copied mssqlite corpus cases plus the
focused msduck probes in `scripts/compatibility.mjs`.
Local execution is diagnostic evidence, not a compatibility pass.

`npm run audit:compare` uses the same tedious capture path on msduck and an
explicitly configured SQL Server endpoint. Supply these environment variables
through your normal secret-management mechanism:

- `MSSQL_REFERENCE_HOST`, `MSSQL_REFERENCE_USER`, `MSSQL_REFERENCE_PASSWORD`
- Optional `MSSQL_REFERENCE_PORT` (default 1433)
- Optional `MSSQL_REFERENCE_ENCRYPT` (default `true`)
- Optional `MSSQL_REFERENCE_TRUST_CERTIFICATE` (default `false`)

The account must be able to create/drop databases. This command executes the
corpus's DDL and DML in a fresh random `msduck_audit_<uuid>` database for each
case, then closes that connection and drops only the database it created.
It never reuses an existing user database. Failed creation does not trigger a
DROP. Cleanup errors are reported as incomplete cases. Abrupt process termination
can leave a temporary database, whose generated name is visible on the server.
Use a development/reference server intended for this workload.

The capture includes ordered result sets and rows, type/length/precision/scale,
flags and collation, DONE-family events, errors, informational messages, return
status and a session-reuse probe. Binary, date and bigint values have explicit
JSON representations. The comparison retains exact JSON-pointer differences;
it does not apply the copied SQLite expected exceptions or normalize away
error/metadata differences. Setup, transport, cleanup or reuse failures cannot
be reported as matches. SQL Server's version is recorded separately.

Output: `artifacts/compatibility/comparison.json`. Any different or incomplete
case makes comparison mode exit nonzero. A match means only that the captured
observations agree for that case; it does not prove broader compatibility.
The diagnostic artifact remains `artifacts/compatibility/local.json`.

## Isolated Docker reference

`npm run audit:docker` builds msduck and runs the same complete comparison against
an ephemeral SQL Server 2025 Developer container. It accepts the Developer EULA,
binds a random port on loopback, generates a temporary SA password, waits for
login readiness and removes only its own randomly named container. Credentials
are passed through child environments, not command arguments or output files.
SIGINT/SIGTERM stop the comparison child and trigger container cleanup. A forced
kill or daemon failure can still require removal of the generated container.

The default image digest is inherited from mirek/mssqlite's differential runner:
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
`MSSQL_REFERENCE_IMAGE` can select another image. Docker's normal environment
and context configuration apply. This command does not change VM settings or
Docker contexts. Allow at least 4 GB VM memory and enough disk for the image,
SQL Server and Rust linking. ARM hosts require working amd64 emulation.

The local container connects as `localhost`, since recent Node TLS rejects an
IP address as the TLS server name. Certificate trust is enabled only for this
owned local development container. The external-endpoint command retains its
stricter default configuration.

Lifecycle unit tests cover retries, successful work, startup/work failures,
invalid bindings, timeouts and cancellation. A live SQL Server 2025 RTM-CU7
(17.0.4065.4) smoke capture is retained in
`artifacts/compatibility/sql-server-currency-smoke.json`. It demonstrated that
MONEY division truncates a half-unit toward zero, while multiplication rounds;
msduck's division was corrected accordingly. AVG retained the expected
truncated scale-four result. Full comparison results and remaining differences
are recorded separately; these probes do not establish full compatibility.

## First complete live comparison — 2026-09-22

The pinned image above reported SQL Server 2025 RTM-CU7, version 17.0.4065.4.
All 273 cases produced complete local/reference capture pairs: zero exact
matches, 273 differences, no incomplete cases. The comparison exited 1 as
specified for differences. Its container was removed, the dedicated reference
VM was stopped, and the original Docker context remained `colima`.

The authoritative raw observations and JSON-pointer differences are in
[`comparison.json`](../artifacts/compatibility/comparison.json). A derived
triage snapshot is in
[`comparison-summary.json`](../artifacts/compatibility/comparison-summary.json).
These generated artifacts describe this run and can be replaced by later runs.

| Difference category | Cases affected |
| --- | ---: |
| Session-reuse metadata | 273 |
| Execution column metadata | 157 |
| Execution row/result contents | 30 |
| Execution completion events | 42 |
| Execution errors | 47 |
| Setup capture | 37 |

Categories overlap. The summary also records 67 cases with other execution
structure differences; counts are not independent failures. All reuse
differences were column type, length or flags: the reuse rows themselves agreed.
For example, SQL Server reports the literal `1` and `@@TRANCOUNT` as `Int`
with flags 32, while msduck reports `IntN`, length 4, flags 1. XACT_STATE is
`IntN`, length 2, flags 33 on the reference, versus length 4, flags 1 locally.
These observations require declaration/nullability and codec work, not removal
of metadata from the comparator.

Priorities established by the captures:

1. Preserve result declaration widths and nullability through planning and TDS
   encoding, including literal string lengths and the shared reuse probe.
2. Match result-set boundaries around failures. Some apparent row mismatches
   are shifted result sets from an extra empty set before a caught error;
   inspect the complete event sequence before changing expression semantics.
3. Correct observed scalar/serialization differences: trailing-space string
   comparison, numeric-to-CHAR alignment, and MONEY emitted as a quoted string
   inside FOR JSON instead of a JSON number.
4. Reconcile error numbers/states and completion behavior against individual
   reference cases, preserving setup and cleanup diagnostics separately.

Both currency arithmetic probes now have identical local/reference rows.
Signed half-unit MONEY division returns zero; two-thirds returns 0.6666 and
negative two-thirds returns -0.6666. Multiplication still rounds a positive
half-unit to 0.0001. Their metadata differences remain, so neither complete
case is an exact match.

Verification after the division correction: formatting and strict Clippy passed,
323 workspace Rust tests passed, and all 337 client/harness tests passed with
no failures, cancellations or skips. All 273 local audit cases completed.
Against the preceding 272-case local capture, 271 executions were unchanged;
the sole existing change was the expected division result 0.6667 to 0.6666.
The signed-fraction probe is new. Full SQL Server compatibility remains open.
