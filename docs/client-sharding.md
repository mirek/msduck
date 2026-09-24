# Opt-in client test sharding

The standard client suite has 404 top-level tests, including 391 in one file.
The opt-in runner partitions test identities across processes while keeping the
existing test files unchanged. CI and `npm test` are unchanged.

Build first, then run from an immutable source/executable snapshot:

```sh
node scripts/run-client-shards.mjs --plan-only --jobs 4
node scripts/run-client-shards.mjs --jobs 4 --output artifacts/client-shards/example
```

Concurrency defaults to one and is bounded to 1–16 worker processes. Each job
selects names within one file, so equal names in different files do not overlap.
Literal names are escaped and matched through the actual end of the string,
including names containing newlines. Nested tests execute with their selected
parent. The runner does not compile, replace executables or acquire the shared
builder lock. Build under that lock and copy the executable and dependencies to
a private source snapshot before releasing it for client work. Never run this
against a shared executable that another build may replace.

An optional `--revision` takes the full commit SHA of a separately verified
build snapshot. The report labels this as caller-declared provenance. It also
records the available checkout revision/dirty state, selected test and harness source hashes and
server executable hash, then checks those files again after execution. These
hashes identify inputs; they do not prove that an arbitrary binary was compiled
from a claimed revision or that every transitive input remained immutable.
Caller-owned build evidence is still required. A private snapshot need not have
Git metadata. Explicit test files may be supplied after `--` for harness checks.

Discovery loads trusted ESM `.mjs` modules in an isolated process with `node:test`
registration intercepted. It does not invoke test callbacks, but normal module
initialization still runs. Only named `test(...)` registration is supported;
unsupported APIs such as `describe`, hooks and `test.only` fail closed. Late
asynchronous registration is rejected. Duplicate names within one file and duplicate input files are rejected. Discovery is
bounded by a 30-second deadline and a 16 MiB output limit. This is not a sandbox
for external submissions: the repository's owner-only intake rule still applies.

The output directory contains the plan/provenance, discovery output, complete
TAP and console logs for every process, machine-readable events and a summary.
Every assigned top-level test must be reported exactly once. Missing, repeated
or unexpected executed tests, process failures and incomplete/cancelled runs
fail the command. Skips and TODOs retain Node's intentional status and are
reported separately, never as passes. Top-level counts are distinct from nested
tests; the complete nested results remain in TAP logs.

SIGINT/SIGTERM cancels work, terminates process groups and escalates to SIGKILL
for test workers after two seconds. Discovery cancellation kills its isolated
process group immediately. POSIX is currently required. Each subprocess clears
Node's inherited internal test-worker marker, which otherwise causes recursive
runners to silently skip files; coverage checking also detects that failure.

Run the harness regressions with:

```sh
node --test tests/client-shards.test.mjs
```

They cover exhaustive deterministic partitioning, exact name matching, duplicate
names across files, nested execution, intentional skips, missing/repeated events,
assertions, crashes, unsupported discovery and cancellation during discovery and
test execution. The two-CPU comparison below found a timeout, so CI concurrency
is not recommended yet. Balanced sums of historical test durations alone do not
establish a speedup.

Harness verification: all eight regressions pass on Node 24.13.0 (Linux) and
Node 26.5.0 (macOS). This proves runner behavior for those regression cases; the
four-worker full-suite benchmark below provides separate execution evidence.

On Linux Node 24.13.0, four workers completed all 404 unchanged client tests in
362188 ms, with zero failures, skips, cancellations, missing/repeated test
identities or changed source/executable hashes. The private executable was
built from `8a233f3ab255c7bdfa9af969fb2ec12898033e62`, with SHA-256
`7eba0d2804232fcd5bb634e5c8052dfc8ce454ed7ebf131859107940fb096c52`.
The earlier unsharded run of that runtime passed all 404 tests in 1164028 ms,
an observed wall-time ratio of 3.21. These runs shared a 32-CPU Linux host with
other verification work and did not have identical host load; this is one
measurement, not a general performance guarantee or a GitHub runner result.
The same immutable executable was then tested sequentially with affinity to
two CPUs. The unsharded baseline passed all 404 tests in 1466053 ms. The
two-worker run took 861859 ms but failed: 403 passed and the BIT aggregate
validation matrix hit its unchanged 20000 ms deadline. That matrix had taken
17813 ms in the passing baseline. Both runs accounted for all 404 tests with
no skips or missing identities, and the binary hash remained unchanged. The
runner correctly returned failure; this is not a passing speedup result. CI
integration remains pending resolution and a complete passing comparison.
