# BulkLoad staging interactions reference

Task `bulk-staging-interactions-reference-v1` (#878) probes the combinations
that the original `gaps-bulk` capture did not establish: multiple column-default
patterns across staging boundaries, identity assignment, trigger invocation and
membership, and constraints on tables written by a trigger.

`scripts/capture-bulk-staging-reference.mjs` captures each case in two fresh
databases in each of two independently started pinned SQL Server containers.
It retains post-login request and response packets, input rows and client
options, typed descriptors and rows, diagnostics, completion events, callbacks,
identity functions, constraint trust and transaction state. It excludes LOGIN7
and credentials. Raw packets include the generated INSERT BULK statement.

The cases span 499/500/501 parameter-boundary rows, 999/1000/1001 rows, and
1501 rows with three alternating default patterns. Listed identity cases add a
fifth column and span 399/400/401 rows. Controls include KEEP_NULLS, disabled
triggers and rows whose every listed column takes a default. A trigger records
each invocation and its inserted membership. Separate CHECK and FOREIGN KEY
probes write invalid rows to another table, with and without CHECK_CONSTRAINTS
and an explicit transaction. Readback records the actual identity mapping;
input order is not assumed to establish SQL Server's identity ordering.

Capture to a new diagnostic path with:

```sh
node scripts/capture-bulk-staging-reference.mjs artifacts/bulk-staging.json
```

The first retained fixture requires `--write-fixture`. Every output must be new;
existing files, hard links, dangling links and symlink parents are rejected
before Docker starts. Raw capture is written exclusively before validation or
comparison. Exact differences between runs are retained in the artifact.
There is no hostname, database, response-SPID, error, descriptor or row
normalization. A later capture that differs from the retained fixture writes
its raw artifact and comparison sidecar, then exits unsuccessfully so that the
differences must be inspected. A raw mismatch is not a compatibility pass.

`--replay-fixture` validates the retained framing, exact case inputs and
recomputed difference summaries without starting Docker. Run
`node --test tests/bulk_staging_capture.test.mjs` for offline regression tests.

The retained 15,034,941-byte fixture has SHA256
`3c9ab565aefd2bc9e0bf8d77dce8b9d1ce25c956079058bd2aa7195866470221`.
The four real runs report SQL Server `17.0.4065.4`; their pinned image is recorded
in the artifact. The capture source SHA256 is
`812a853b244d38a820e0e35011567a0f3d28fd7bd2977d9c881befc68706326d`.
Subsequent script changes strengthen offline EOM validation and clarify the
no-baseline message; they do not change the cases or captured requests.

In these runs, each successful load with FIRE_TRIGGERS invoked the trigger once
with the entire load, including mixed-default loads of 1501 rows and 1001
all-default rows. The recorded generated identities match source markers in
order for these inputs. Listed identities retain the supplied values;
SCOPE_IDENTITY and IDENT_CURRENT report the final target identity, while
@@IDENTITY reflects the trigger's audit identity. The no-trigger control has
no audit rows. These observations do not prove ordering for other input shapes
or SQL Server execution plans.

Every trigger constraint probe reports 547/state0/class16 and leaves no target,
audit or membership rows, even without CHECK_CONSTRAINTS. The retained readback
also records identity advancement and transaction state after failure. One
foreign-key probe without CHECK_CONSTRAINTS or an explicit transaction reports
@@ERROR=547 in the first three runs and 0 in the fourth. This difference remains
in both the raw response and typed results; it is not normalized or given a
fabricated stable expectation. Other between-run differences include generated
database names in diagnostics, server names and response packet headers.

The runtime is unchanged. This reference does not fix or accept the
default-pattern grouping, trigger constraint suspension or scope-identity gaps
documented in `gaps-bulk.md`. The earlier misconfigured capture, whose marker
nullability caused4816, is preserved separately as failed setup evidence and is
not the retained fixture.
