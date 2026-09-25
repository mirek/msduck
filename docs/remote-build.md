# Optional remote Linux builds

`scripts/remote-build.mjs` synchronizes the current working tree to a dedicated
SSH workspace and runs builds or tests there. Configure `.env` from `.env.example`:

```dotenv
MSDUCK_BUILD_HOST=linux.local
MSDUCK_BUILD_DIR=/home/mirek/.cache/msduck/remote-build
MSDUCK_BUILD_JOBS=16
MSDUCK_BUILD_TOOLCHAIN=1.95.0
```

`.env` is ignored by Git and excluded from synchronization. Environment variables
already set in the invoking process take precedence. Configuration is parsed as
data, not executed as shell code. Use Node 24 or newer on both hosts, with SSH
key authentication, rsync, Rust (including rustfmt and Clippy), a C++ toolchain,
and `flock` on the Linux host. The runner adds `$HOME/.cargo/bin` to the remote
PATH for noninteractive SSH sessions. The optional toolchain setting exports
`RUSTUP_TOOLCHAIN` remotely; install that version first, for example:

```sh
ssh linux.local '~/.cargo/bin/rustup toolchain install 1.95.0 --profile minimal --component rustfmt --component clippy'
```

Matching toolchains avoids interpreting newly introduced Clippy lints as platform
regressions. The runner does not change the host's default Rust toolchain.

```sh
npm run remote -- fast   # deterministic core, SQL and TDS tests; no DuckDB
npm run remote -- build  # complete workspace/all-targets build
npm run remote -- rust   # formatting, strict Clippy, workspace Rust tests
npm run remote -- test   # build and complete Node/tedious test suite
npm run remote -- audit  # local diagnostic corpus on the remote server
npm run remote -- verify # all Rust/client/audit checks after one synchronization
```

`verify` holds the same remote workspace lock throughout all checks and uses
one synchronized source snapshot. This avoids a later phase picking up local
edits made while an earlier phase was running.

Ordinary `npm test` and `npm run audit:local` continue to run locally. Each remote
command syncs uncommitted and untracked source edits too. Avoid editing sources
while synchronization is in progress. Linux executables stay on Linux; the runner
does not copy them into the local macOS target directory.

The remote directory must initially be empty or already owned by this runner.
A marker records ownership. A remote lock covers synchronization and execution;
a second invocation against that workspace fails rather than changing files
beneath an active run. Source deletions are mirrored only inside its `source`
subdirectory. `.git`, `.env` files, `.msduck/` credentials, database files, local `target`, `node_modules`
and artifacts are excluded. Remote target and dependency caches survive syncs.
`npm ci` runs when the package lock changes or dependencies are absent.

Source sync compares file content (`rsync --checksum`) and does not preserve
source mtimes on the receiver. A changed file therefore receives a fresh Linux
mtime for Cargo's fingerprint check, while unchanged files and their cached
build artifacts remain untouched. The previous timestamp-preserving sync could
copy changed bytes with an older mtime, leaving Cargo to reuse a stale binary.
On the first run after this sync change, the runner performs a one-time
`cargo clean` in an existing remote target and records a marker outside the
source tree. This cold rebuild clears artifacts that may already have been
compiled from stale bytes; later unchanged-source runs reuse the target.
The marker and reset are protected by the same workspace lock as synchronization.

`node --test tests/remote-build.test.mjs` exercises a same-size source change
with an old mtime in a small offline Cargo crate. It checks that the changed
binary rebuilds, an unchanged rerun leaves the binary untouched, and private
`.env` and `.msduck` files stay excluded. On `linux.local`, the test passed and
the runner's first migrated workspace build completed in 1m 31s of Cargo time;
an unchanged second invocation finished in 0.05s of Cargo time (0.77s wall
clock). These are build-cache checks, not full workspace or compatibility test
results for the current revision.

Remote audit captures are copied to
`artifacts/remote/<host>/compatibility/`, leaving local captures intact. Compare
raw values, metadata, errors and completion events before treating results from
different operating systems as interchangeable. Audit execution alone is not
SQL Server compatibility proof.

The initial `linux.local` inspection found 32 logical CPUs, 122 GiB RAM and an
x86_64 Linux environment. The default configuration uses 16 build jobs to leave
headroom for native C++ compilation. A cold DuckDB build and cached incremental
builds have different costs; measure representative runs before claiming a speedup.


Initial verification: the deterministic suite passed all 127 tests on Linux,
including with the configured Rust 1.95.0 toolchain. The first complete Linux
workspace/all-targets build finished successfully in Cargo's reported 1m 15s;
that timing excludes synchronization and earlier dependency work. A concurrent
runner invocation was rejected while the build held the workspace lock. Remote
inspection confirmed that `.env` and `.git` were absent from the synchronized
source directory. Remote formatting, strict Clippy and all 329 workspace Rust tests also passed
with Rust 1.95.0. All 341 remote client/harness tests passed, and all 277 remote audit cases
completed. The raw comparison with macOS matches except for two rows appearing
in reverse order in the unordered `derived table apply` query. The four cell
changes remain recorded in
`artifacts/remote/linux.local/grouping-baseline-comparison.json`; no ordering
normalization was applied. This evidence covers the grouping-metadata baseline
before fixed scalar wire encoding was introduced.


Fixed-scalar baseline verification completed with formatting, strict Clippy,
332 Rust tests, 343 client/harness tests and 279 audit captures. The remote
captures match the preserved macOS baseline exactly; raw comparison is retained
in `artifacts/remote/linux.local/fixed-scalars-baseline-comparison.json`.
This snapshot precedes the XACT_STATE declaration/preflight changes.
