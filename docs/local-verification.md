# Local verification

PR CI runs only the short **Deterministic crates** gate so ordinary PRs finish
well under an hour (see [the GitHub workflow](github-workflow.md)). The native
**Workspace and clients** job runs after merge to main, on owner dispatch,
`verify-*` tags and weekly. `scripts/verify-local.mjs` lets a worker produce
pre-merge evidence for the same checks on their own machine, with a summary that
can be pasted into the PR.

Local evidence complements, and does not replace, full CI on main or an
owner-dispatched run. The integrator still decides whether a PR needs a full CI
run on its branch before merging.

## Usage

```sh
node scripts/verify-local.mjs            # fast mode (mirrors the PR gate)
node scripts/verify-local.mjs --full     # fast mode plus the native full-job steps
node scripts/verify-local.mjs --full --audit
```

Options:

| Option | Effect |
| --- | --- |
| `--full` | Add the full-job steps after the fast steps. |
| `--audit` | With `--full`, also run `npm run audit:local`. The audit records diagnostic captures; it is not a SQL Server equivalence check. |
| `--cargo-jobs N` | Override `CARGO_BUILD_JOBS`. |
| `--client-jobs N` | Override client shard workers (1 to 16, the shard runner's limit). |
| `--keep-going` | Run the remaining steps after a failure. The exit status is still nonzero. |
| `--allow-dirty` | Run on a tree with uncommitted or untracked changes. The result is marked as not representing a commit. |
| `--verbose` | Stream every step's output to the terminal as well as the log. |

The command requires Node.js 24 and the Rust toolchain pinned by
`RUSTUP_TOOLCHAIN` in `.github/workflows/ci.yml`, with `rustfmt` and `clippy`.
It reads the pin from the workflow and runs every step with that toolchain. If
the toolchain or a component is missing, it prints the exact `rustup toolchain
install ...` command and exits nonzero; it never installs anything itself.

## What each mode covers

**Fast mode** runs the `run:` steps of the CI `fast` job, read from the
workflow at run time so the two cannot drift silently. Currently these are the
coordination script tests
(`node --test tests/agent-work.test.mjs tests/client-shards.test.mjs`),
`cargo fmt --all --check`, and strict Clippy and tests for `msduck-core`,
`msduck-sql` and `msduck-tds`. It does not compile DuckDB.

The command accounts for every step in that job:

- A `run:` step, named or not, runs locally. It can be a single line or a `|`
  block.
- A `uses:` action (checkout, cache, setup-node) is CI setup and is not run.
- The `rustup toolchain install` step is replaced by the local toolchain check.

Both kinds of skipped step are listed in the summary. If the job gains anything
else, such as a step with `env`, `shell` or `if`, a job-level `env` or
`defaults`, or a folded or quoted `run:` value, the command stops with an error
rather than skipping or approximating it.

**Full mode** adds, in the CI order:

1. `npm ci --no-audit --no-fund`
2. `cargo clippy --workspace --all-targets --locked -- -D warnings`
3. `cargo test --workspace --locked`
4. `cargo build --workspace --all-targets --locked`
5. `node scripts/run-client-shards.mjs --jobs N --revision HEAD --output DIR`
6. `node --test --test-concurrency=1 tests/aggregate_diagnostics.test.mjs tests/character_extrema.test.mjs`
7. `npm run audit:local` (only with `--audit`)

Differences from CI:

- Cargo and client-shard parallelism follow the host (below). CI uses two of
  each.
- Cargo output is uncolored so logs stay readable.
- The first full run in a fresh worktree compiles DuckDB from source. Later
  runs reuse that worktree's `target/`.
- The host OS, architecture and native compiler can differ from the
  `ubuntu-24.04` runner. A local pass on macOS or ARM does not show that the
  Linux job passes, and the reverse is also true.
- CI's audit step always runs in the full job. Locally it is optional.

## Parallelism

The command prints the values it chose and why. The inputs are
`os.availableParallelism()`, the one-minute load average when the run starts,
total memory, currently available memory (`os.freemem()`, which is
`MemAvailable` on Linux) and `process.constrainedMemory()`, which reports a
container or cgroup limit where Node can see one. On macOS, `os.freemem()`
leaves out reclaimable cache, so the command uses the larger of free memory and
60% of total memory.

- The memory budget is usable memory minus OS headroom. Headroom is the larger
  of 2 GiB and 15% of total (or limited) memory.
- Cargo jobs are the smaller of the CPU count and the budget divided by 3 GiB.
  DuckDB C++ units and linking test binaries against DuckDB can each need
  several GB at once.
- Client shards are the smaller of the idle CPUs (CPU count minus the load
  average) divided by 4, the budget divided by 1.5 GiB, and 16. Each shard runs
  a Node test process plus msduck servers whose DuckDB queries use several
  threads. Many client tests have 20 or 30 second deadlines, and some already
  take most of that when run alone. Packing more shards onto the CPUs turns
  those tests into timeout failures, so each shard gets whole idle CPUs.
- Neither value goes below 1.

A small laptop gets a few Cargo jobs and one or two shards. A large idle
builder gets up to one Cargo job per CPU and more shards. The load average only
shows work running at the start. If other work starts during the run, such as
another worktree's build, or if client tests time out, rerun with a lower
`--client-jobs`. A test that times out still counts as a failure. Client tests
bind ephemeral ports, so separate worktrees can run them at the same time, but
they still compete for CPU.

## Evidence

The command refuses a tree with uncommitted or untracked changes unless you
pass `--allow-dirty`. With that option, the summary says that the result does
not represent the commit and lists the changed files. The command fingerprints
`HEAD`, the index, tracked changes and the contents of untracked files before and
after the run. If `HEAD` or the fingerprint changes during a run, the run fails
and is marked as not representing a commit. This also applies to a run with
`--allow-dirty`.

Each run writes to
`artifacts/local-verify/<short-rev>/<mode>[-dirty]-<timestamp>/`, which is
gitignored:

- `<step>.log` holds each step's raw combined output, starting with its command.
- `client-shards/` holds the shard runner's plan, provenance, TAP/event logs and
  summary.
- `summary.json` records the revision, the dirty state and the start and end
  tree fingerprints, the CI setup steps not run locally, toolchain versions
  (pinned, `rustc`, `cargo`, `node`, `npm`), the host, the chosen parallelism
  with its reasons, and each step's command, status, exit code, duration and
  parsed test counts.
- `summary.md` is the PR-ready summary. It is also printed at the end.

Test counts are parsed only where the totals are unambiguous: `cargo test`
result lines, the Node test runner's final totals, and the shard runner's final
JSON line. Other steps show no count. The command stops at the first failed step
unless you pass `--keep-going`. Steps that did not run are listed as "not run".
Each failed step's last 40 log lines are printed and included in `summary.md`.
For a failed client-shard step, the summary also names each failing test with
its failure type and error, and any accounting problems from the shard runner.
Any failure, interruption or change to the tree during the run gives a nonzero
exit status.

## Adding evidence to a PR

1. Commit your work. For a Rust or behavior change, run
   `node scripts/verify-local.mjs --full` on that commit. For changes limited to
   deterministic crates or coordination scripts, fast mode may be enough;
   documentation-only changes need neither.
2. Paste `summary.md` from the run directory into the PR's verification
   section. Check that its revision is the PR head you push. A later push needs
   a new run.
3. Report failures, including unrelated baseline failures, as they are. Do not
   trim them from the summary.
4. Run `node scripts/agent-work.mjs verify TASK-ID`, push, and then
   `node scripts/agent-work.mjs status TASK-ID review`.
