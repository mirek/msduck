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

Enable Code review and Automatic reviews for `mirek/msduck` in
[Codex settings](https://chatgpt.com/codex/settings/code-review).
The native integration reviews new PRs opened for review. Use `@codex review`
for a follow-up pass after fixes. Repository review rules live in AGENTS.md.
See [official configuration instructions](https://learn.chatgpt.com/docs/third-party/github).
An enabled workflow or posted request is not proof that Codex actually reviewed a PR;
check for its reaction/review and resolve substantive findings.

Use issues for actionable gaps with reference evidence and acceptance criteria;
use the roadmap tracking issue to group larger work. The long-term scope remains
in ROADMAP.md. Close an issue only when the promised behavior and evidence exist.
Keep verification progress in PR checks and linked artifacts rather than creating
an issue for every successful test run.

A GitHub Projects board can be added after the CLI has `project` authorization
(`gh auth refresh -s project`). It is optional: issues, labels, milestones and PR
links provide the initial work queue without that additional token scope.
