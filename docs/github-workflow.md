# GitHub workflow

Use one branch and pull request for each coherent behavior change. Push useful
checkpoints and open draft PRs early; mark ready when the change and evidence are
reviewable. Link the issue, describe remaining differences, and let CI finish
before merging. Automatic review does not mean automatic merge.

The required PR check, **Deterministic crates**, checks formatting,
coordination scripts, strict Clippy and tests for the three deterministic crates
without DuckDB. It is intentionally short. The **Workspace and clients** job
runs on owner manual dispatch, `verify-*` tags and the weekly schedule. It does
not start after each merge to main. It checks strict workspace Clippy, Rust
integration tests, independent Node clients and a raw compatibility audit.
Stale runs on the same ref are canceled on a newer push. Both jobs use a pinned
Rust toolchain; full verification uses Node 24. Dependency/build caches reduce
repeated DuckDB compilation. Full-run logs, revision identity and raw captures
are retained for 14 days. The audit is diagnostic, not an equivalence gate.

For a PR that changes Rust or behavior, the worker still runs the full relevant
`AGENTS.md` checks locally or on the isolated Linux builder and reports the
exact tested revision, results and any unrelated baseline failures. The
integrator reviews that evidence and may start the full CI job on the PR branch
with `gh workflow run ci.yml --ref work/TASK-ID`; the run records its actual
checkout SHA in `artifacts/ci/revision.txt`. A manual run on an earlier SHA does
not verify a later push. A `verify-*` tag pins a revision for a repeatable full
run. The weekly run checks the then-current main revision; workers should
request a manual run or create a `verify-*` tag when a specific merge needs
full hosted verification. A green fast check alone does not establish SQL Server
compatibility.

GitHub-hosted Linux runners are the initial CI environment. The optional SSH
builder remains available for development; its local configuration and credentials
are not published or exposed to pull-request jobs.

The owner can request Codex review on approved PRs. Under the owner-only intake
policy, do not enable all-PR automatic review unless intake can exclude unapproved
external content before retrieval. Output from owner-initiated agents is readable
after verifying its origin and owner-approved inputs. Third-party submissions,
replies and unverified automation still require owner triage. An automatic request
is not a completed review. Repository review rules live in AGENTS.md. See
[the contribution workflow](agent-work.md).

Use issues for actionable gaps with reference evidence and acceptance criteria;
use the roadmap tracking issue to group larger work. The long-term scope remains
in ROADMAP.md. Close an issue only when the promised behavior and evidence exist.
Keep verification progress in PR checks and linked artifacts rather than creating
an issue for every successful test run.

The [development project](https://github.com/users/mirek/projects/2) now groups
workstreams and bounded tasks. Readiness is informational: workers discover only
the owner-approved registry and claim through `scripts/agent-work.mjs`.
See [exclusive claims and owner-only intake](agent-work.md).

The temporary merge exception for the owner-requested integration checkpoint
(#93) ended on 2026-09-24. The full job was then required on every PR, but its
native compilation and client/audit work made ordinary PRs wait more than an
hour. The current required check is **Deterministic crates**, with strict
up-to-date checking; full verification remains a separate owner-controlled run.
The complete [CI run on main](https://github.com/mirek/msduck/actions/runs/35973323293)
passed at `6016b8605189b347e81fe48f3f17bee1f127314f`, including Rust tests,
independent clients and diagnostic capture. Passing capture is not full SQL
Server equivalence.

[The checkpoint](integration-checkpoint.md) records the earlier consolidation,
temporary gate and retained compatibility gaps. Owner-only intake and permanent
claim protections remain unchanged.

## Client verification in CI

The full job builds the current checkout with the workspace all-target feature
graph, then runs the complete standard client inventory with two workers through
`scripts/run-client-shards.mjs`. No build runs concurrently with those clients.
The runner requires every discovered test identity to execute exactly once and
propagates assertion failures, process failures and incomplete runs. It retains
skips separately from passes and checks source/executable hashes after execution.
The fast job runs its failure, cancellation and coverage-accounting regressions.

The existing verification artifact includes `artifacts/ci/client-shards/`: the
plan, revision/hash provenance, per-job TAP/console/event logs and final summary.
The local `npm test` command remains available for the serial baseline. The
owner-run two-CPU benchmark passed all 408 tests in about 14.4 minutes after
splitting the oversized BIT matrix without removing assertions. This measurement
is not a guarantee of GitHub runner performance or a diagnosis of earlier CI
failures. It is why client verification remains available on demand and runs
weekly, while the short PR gate stays responsive.
