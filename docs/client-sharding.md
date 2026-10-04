# Opt-in client test sharding

The standard npm client suite has six explicitly registered entry points and
520 top-level tests at the task847 base. `scripts/lib/client-suite.mjs` is the
shared manifest for npm, direct full-suite runs and CI. CI retains all its
additional session/database/compatibility files. Its separate manifest-driven
`ci-replays` group retains aggregate/character diagnostic replays and their
intentional opt-in skips; those are reported separately and never called passes.
`npm test` still builds the workspace with all targets, then invokes a small
adapter. Unset `MSDUCK_CLIENT_JOBS` or `1` retains one `node --test` invocation
across the original six files; values 2–16 opt into partitioned workers. No
SQL assertion, timeout, test callback or compile feature is changed.

```sh
npm test
MSDUCK_CLIENT_JOBS=4 npm test
```

The remote runner accepts the same validated setting from the gitignored `.env`
and exports its quoted numeric value inside the existing builder lock. Keep
that lock through builds and all client processes using its executable.
Full CI uses the shared client superset manifest with a serial fallback; its
separate diagnostic group keeps the previous one-file-at-a-time execution.
Increasing concurrency on a two-CPU runner requires a separate full proof.

Build first, then run from an immutable source/executable snapshot:

```sh
node scripts/run-client-shards.mjs --suite npm --plan-only --jobs 4
node scripts/run-client-shards.mjs --suite npm --jobs 4 --output "$TMPDIR/client-shards/example"
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
fail the command. Explicit focused runs after `--` retain intentional skips/TODOs separately,
never as passes. Strict full client manifest runs (npm/ci, including the no-file default) additionally
require passed==expected and reject assigned skips, TODOs, cancellations and
failed nested results. Any unassigned tests reported as Node filter skips remain in the raw logs
and are not assigned coverage. Node versions can omit filtered events;
coverage is checked against discovered assigned identities in either case. Source, harness,
reference and executable hashes are rechecked, including the source file list.
Added/deleted or changed inputs fail the summary. Top-level counts are distinct from nested
tests; the complete nested results remain in TAP logs.

For the POSIX runner, SIGINT/SIGTERM cancels work, terminates process groups and escalates to SIGKILL
for test workers after two seconds. Discovery cancellation kills its isolated
process group immediately. POSIX is required for sharding and strict full accounting. On Windows,
`npm test` with unset/1 client jobs retains the previous direct `node --test`
invocation over the same six manifest files and Node's ordinary exit status;
parallel jobs are explicitly unsupported. This portable serial path clears the
inherited worker marker and cancels the full process tree (Windows `taskkill /T /F`,
POSIX process groups with bounded escalation), without claiming the POSIX
runner's strict inventory/provenance guarantees on Windows. Platform-selection
and actual portable-command failure/cancellation regressions run on the available
Linux/macOS harnesses; they do not establish a Windows native-server pass.
Each subprocess clears
Node's inherited internal test-worker marker, which otherwise causes recursive
runners to silently skip files; coverage checking also detects that failure.

Run the harness regressions with:

```sh
TMPDIR=$(mktemp -d /tmp/msduck-shards-XXXXXX) && export TMPDIR || exit 1
trap 'rm -rf "$TMPDIR"' EXIT
trap 'exit 130' INT TERM HUP
node --test tests/client-shards.test.mjs tests/client-test-command.test.mjs tests/remote-build.test.mjs
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
runner correctly returned failure; this is not a passing speedup result.

The separate test-only follow-up `52142d4` splits the BIT matrix into four
aggregate cases and one widening case, retaining the assertions and unchanged
20-second deadlines. With that test file, the same executable and two CPUs
passed all 408 tests in 862726 ms, with no missing/repeated identities, skips,
cancellations or changed inputs. The assertion workload is retained, but test
granularity and startup count changed, so this is not identical test source to
the 404-test baseline. The observed wall-time ratio is 1.70 versus that baseline.
These historical results do not substitute for current task847 measurements; concurrency remains opt-in for npm.

Task847 freezes executable client sources at
`bee9c214676bf17d19ea8f2c0a3e0f8cd00ec88c`. On the 32-CPU x86_64 Linux host,
Node 24.13.0 ran complete canonical `npm test` invocations with client jobs 1
and 4. Both invoked `cargo build --workspace --all-targets`, reusing the same
feature graph/native cache (0.07 and 0.19 seconds respectively), and both
passed every one of 520 assigned identities exactly once. Failures, assigned
skips, TODOs, cancellations, omissions, repeated identities and changed inputs
were all zero. The four-worker plan used 21 subprocesses; all reported terminal
exit zero. No SQL assertions or timeouts changed.

The runner's client elapsed times were 1565199.191146 ms (26m 05.2s) for the
single Node invocation and 475381.771203 ms (7m 55.4s) for four workers. These
exclude the preceding Cargo build and discovery; the observed ratio is 3.29.
Independent native builds drove varying host load from about 0.5 at serial
startup to 40–50 during execution, and the four-worker run started around 30.
This is complete current-suite execution evidence, not a controlled speedup
experiment, a general performance guarantee or a two-CPU GitHub runner proof.
CI therefore retains the documented serial fallback.

The executable SHA-256 was
`9adfd0d2fe8ea5c8bfa2ca419027589dfd4b557d9f62f7728c7da7fb02504137`
in both plans and after both runs. All 1212 repository source, reference and
harness input hashes matched across runs; the serial plan also independently
matched the committed local source. Full plans, JSONL events, TAP/console logs
and summaries are preserved in the worker's `.tmp/client847/serial-bee9` and
`.tmp/client847/four-bee9`. Their complete summary SHA-256 values are
`9fa7593694119e05ea9ef5c97018f47d87c8a03471d31ec9ae8e5ef8d64922b4`
and `09ec822b6a9194ec6bfd552ff643806cc895d46ea5a54a30e8eae22cae2bda86`.
An independent event recount verified all 520 unique passing identities for
each run. Raw evidence was copied before the shared builder handoff. An earlier
partial run on predecessor `25eb55b` was stopped for the diagnostic-group
correction, exited 255 and remains partial evidence; it is not a pass.

All 27 command/shard/remote harness regressions pass after the portability
correction. The original 22 passed at the benchmark checkpoint. Their small
private snapshots contain explicitly inert executable markers for harness
accounting and never substitute for the real Cargo/native benchmark above.
The tiny offline Cargo fingerprint probe cleans its own temporary output.
These checks establish verification behavior, not full SQL Server compatibility.


A subsequent verified Codex finding corrected the Windows default command:
`52578c8` still routed Windows through the POSIX-only runner. The benchmark
checkpoint `bee9` remains the measured source above. The final portability
adapter changes one executable input, so those captures are not relabelled as
identical final-source proofs. A subsequent complete Linux npm validation on frozen cancellation correction
`75b602a56d1e962d9b27ff646f65fddb6b1aa1c5` passed all 520 identities with
four workers and 21 terminal subprocesses, with zero failures, assigned skips,
TODOs, cancellations, missing/repeated identities or changed inputs. Client
elapsed was 382180.040296 ms; cached all-targets build was 1m41s. The raw artifact
`6a14ecc7-fb7e-484e-9caf-e895de0e531b` is preserved in
`.tmp/client847/final-portable-75b602a`; independent recount and local comparison
verify every passing identity and all 1,212 input hashes. Summary SHA-256 is
`db982ecfe91b89d84563b6e29f288d94b8d00a8084ca7dbe3d9f35b0531ea183`;
the executable SHA-256 remains `9adfd0d2fe8ea5c8bfa2ca419027589dfd4b557d9f62f7728c7da7fb02504137`.
This validates corrected execution without repeating the original serial/four-worker
performance measurement or claiming a controlled speedup.

A further Codex finding exposed descendant leakage in `7dc045e`: cancellation
signaled only Node’s coordinator. The actual predecessor fails the strengthened
regression with a surviving test worker; the corrected adapter terminates both
a SIGTERM-ignoring worker and its child. Windows tree-command construction is
tested separately, without claiming execution on a Windows native server.

A final test-only review correction recognizes Linux defunct (`Z`/`X`) workers
as non-executing even when PID 1 has not yet reaped them. It preserves checks for
live/sleeping/stopped workers and propagates unexpected inspection errors. The
production launcher, six npm files, manifest, remote runner and executable are
byte-identical to the complete `75b602a` execution above; the amended harness
file and documentation are not relabelled as identical retained input hashes.
