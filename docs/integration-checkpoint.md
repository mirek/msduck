# Integration checkpoint

The owner requested consolidation of the accumulated PRs on 2026-09-24,
including relaxing the CI gate if useful and then focusing on green CI.
Task #93 tracks this integration. Only owner-authored PR metadata and trusted
local branches were used; no external issue/review bodies or bot logs were read.

The integration branch starts at projection revision `30f0b1b` (PR #92).
The complete heads of PRs #1, #11, #13, #14, #16, #19, #21, #23, #25,
#27, #29, #32, #34, #36, #38, #40, #42, #44, #46, #47, #49, #50,
#53, #55, #58, #59, #64, #66, #68, #72, #74, #76, #78, #80, #82,
#84, #86, #88, #90 and #92 are retained in its ancestry.

Eight older side branches required accounting beyond the main stack. Planning
and inventory documents (#11/#14/#19), reference capture work (#21/#34), and
the SMP codec (#16) were merged normally. The #25 commit was proven patch
equivalent with `git cherry`; its ancestry was recorded without reverting later
SQL changes. The #13 metadata-alignment patch was merged with one conflict:
alignment now uses the public schema after checked projection diagnostics are
removed. Its success/error/empty-result regression tests are retained.

## Verification baseline

Before the side-branch integration, `30f0b1b` passed 457 library unit tests,
18 targeted native/planner integration tests, formatting, strict workspace
Clippy, and all 44 exact checked-projection reference cases. The 44 captures
were identical across two fresh pinned SQL Server instances. The preceding
transaction replay matched 85/87 cases, with 27 differences confined to two
constraint cases. The 132 selected existing client tests passed before the
final explicit-cast and identifier-capture fixes, which the 44-case replay covers.
These are revision-specific observations, not proof that the merged tree passes.

Full workspace/client/audit verification for `30f0b1b` was submitted to the
shared Linux builder. Earlier completed Linux runs passed their Rust suites but
retained client and compatibility failures; those older runs cannot certify this
integration. GitHub check conclusions are metadata, not a substitute for reading
owner-approved evidence or reproducing failures from trusted sources.

## CI and follow-up

The deterministic job checks coordination safeguards, formatting, Clippy and
pure Rust tests. The full job checks native workspace compilation/tests,
independent clients and diagnostic captures. Compatibility captures retain exact
mismatches and do not establish full SQL Server equivalence.

Any temporary relaxation of the full merge gate must keep the job running and
its result visible, record the affected integration revision, and retain the
owner-only intake and claim protections. Restore the full required check after
its reproducible failures are resolved. New feature work should give priority to
that baseline instead of growing another long unmerged stack.

For this owner-approved consolidation, `main` now requires **Deterministic
crates** with strict up-to-date checking. **Workspace and clients** remains
scheduled and visible, but is temporarily non-blocking. Claim-tag and registry
protections, owner-only workflow guards and external-contributor restrictions
are unchanged. This is an explicit integration exception, not permission for
workers to ignore future failures or merge unrelated work automatically.
