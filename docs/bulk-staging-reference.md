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

The 29 cases span 332/333/334 rows around msduck's four-column staging boundary,
plus 499/500/501, 999/1000/1001 and 1501 rows with three alternating default
patterns. Listed identity cases add a fifth column and span 284/285/286 and
399/400/401 rows. msduck reserves two staging parameters per row, so its
`2000 / (columns + 2)` limit puts these boundaries at 333 and 285 respectively;
these are msduck boundaries, not claimed SQL Server bulk limits.
Controls include KEEP_NULLS, disabled
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

`--replay-fixture` validates the retained framing, exact case inputs, fixed
per-case readback rows/identity/trigger membership, descriptors, diagnostic
state/class/text, trust and completion events, and recomputed difference
summaries without starting Docker. Uniform changes in all four runs cannot
become their own oracle or relabel the pinned image and ProductVersion.
Encoded packet length and remaining exchange budget
are checked before regex or buffer allocation. Explicit-transaction requests
must carry a nonzero transaction descriptor in their raw ALL_HEADERS.
Run
`node --test tests/bulk_staging_capture.test.mjs` for offline regression tests.

The retained 17,788,143-byte fixture has SHA256
`37827e03eeeafde346c4776e233af2cc287781b9a9c9ac7a6edb1721d5232b34`.
The four real runs report SQL Server `17.0.4065.4`; their pinned image is recorded
in the artifact. The capture source SHA256 is
`775db11a16c64ae94a1be8d6d1ca7d6ba8afaf03263d6876887dd64b0a8a0663`.
Subsequent script changes strengthen offline semantic and allocation-bound
validation; they do not change the 29 cases or captured requests. The fixed
readback descriptor array hash is
`488a820956ff22530072a249001c6b42fb4c60f7845a452cb6a63f4a20cbeb5c`.

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
also records identity advancement and transaction state after failure. These
four 29-case runs report @@ERROR=547 after all failed loads. The earlier 23-case
capture retained at commit `df40afd8a7ad3d50a4c355720cc221026e9e4829` (SHA256
`3c9ab565aefd2bc9e0bf8d77dce8b9d1ce25c956079058bd2aa7195866470221`)
reported @@ERROR=0 in one foreign-key probe without CHECK_CONSTRAINTS or an
explicit transaction. Both original captures are preserved. The fixed gold
checks validate this retained snapshot; they do not establish a generally
stable @@ERROR expectation for runtime compatibility. Between-run differences
in the new fixture retain generated database names in diagnostics, server
names, transaction descriptors and response packet headers without normalization.

The runtime is unchanged. This reference does not fix or accept the
default-pattern grouping, trigger constraint suspension or scope-identity gaps
documented in `gaps-bulk.md`. The earlier misconfigured capture, whose marker
nullability caused4816, is preserved separately as failed setup evidence and is
not the retained fixture.
