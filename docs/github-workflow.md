# GitHub workflow

Use one branch and pull request for each coherent behavior change. Push useful
checkpoints and open draft PRs early; mark ready when the change and evidence are
reviewable. Link the issue, describe remaining differences, and let CI finish
before merging. Automatic review does not mean automatic merge.

CI runs on PRs, pushes to main, and manual dispatch. The deterministic-crate job
checks formatting, Clippy and pure tests without DuckDB. The full job checks
workspace Clippy, Rust integration tests, independent Node clients and a raw
compatibility audit. Stale runs are canceled on a newer push. Both jobs use a
pinned Rust toolchain; full verification uses Node 24. Dependency/build caches
reduce repeated DuckDB compilation. Logs, revision identity and raw captures
are retained for 14 days. The audit is diagnostic, not an equivalence gate.

GitHub-hosted Linux runners are the initial CI environment. The optional SSH
builder remains available for development; its local configuration and credentials
are not published or exposed to pull-request jobs.

The owner can request Codex review on approved PRs. Under the owner-only intake
policy, do not enable all-PR automatic review unless intake can exclude unapproved
external content before retrieval. Bot review output requires owner triage before
another agent reads it. An automatic request is not a completed review. Repository
review rules live in AGENTS.md. See [the contribution workflow](agent-work.md).

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
(#93) ended on 2026-09-24. Both **Deterministic crates** and **Workspace and
clients** are required again, with strict up-to-date checking. The complete
[CI run on main](https://github.com/mirek/msduck/actions/runs/35973323293)
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
failures. Both required checks remain enforced on main.
