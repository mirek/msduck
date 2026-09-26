# Reference capture conventions

Capture scripts under `scripts/capture-*.mjs` retain SQL Server rows,
descriptors, diagnostics and completion tokens as fixtures in `reference/`.
New scripts should follow these conventions. Existing fixtures are evidence:
never overwrite one to make a check pass.

## Shared helpers (`scripts/lib/reference.mjs`)

- `isolatedReference(config, work)` runs `work` in a freshly created database
  and drops only that database afterwards.
- `refuseExistingFixture(path)` checks for a retained fixture by existence only
  and must run before any container starts. `writeNewFixture(path, value)`
  writes compact JSON with the exclusive `wx` flag.
- `assertSameCapture(actual, expected, label)` and
  `describeFirstDifference(actual, expected)` compare whole captures with
  `isDeepStrictEqual` and report one bounded path. Never pass whole captures or
  fixtures to `node:assert`: on Node 24 a failed `equal`/`deepEqual` inspects
  both operands without a practical bound, which has driven capture processes
  to ~123 GB RSS. Small targeted assertions on individual values remain fine.
- `capturePrepared(connection, sql, declarations, valueSets, options)` captures
  one `sp_prepare`, one `sp_execute` per value set and `sp_unprepare` on a
  single reusable tedious `Request`.

## Prepared statements

`capturePrepared` returns
`{ prepare, prepared, executions: [{ values, result }], unprepare }`. Each phase
result has `sets`, `done`, `errors`, `info`, `returnStatus` and `rowCount`.
`values` are returned as given; apply `canonical()` when retaining them. When
`sp_prepare` returns no handle, `prepared` is false, no execution runs and
`unprepare` is `null`. Scripts map this to their retained record shape; for
example the HASHBYTES capture keeps `prepare.rowCount` as canonical `missing`,
while the CHARINDEX/PATINDEX capture omits `rowCount` from preparation and
unpreparation.

The helper absorbs two tedious behaviors that previously produced wrong or
hanging captures:

- `sp_prepare` completion is reported through the Request's `prepared` or
  `error` event, not its callback. Waiting on the callback hangs.
- tedious assigns `request.error` for each server error but never clears it,
  and passes it to every later callback of the same Request. A copied helper
  that recorded the callback error when no error token arrived therefore
  reported the first failure again on every later execution. The helper clears
  `request.error` before each phase and never attributes an error object that an
  earlier phase already reported.

Only `errorMessage`/`infoMessage` tokens raised while a phase is outstanding are
recorded for that phase, with number, state, class, line and message. A
callback error is recorded as `{ message, number }` only when the phase received
no server error (client-side parameter validation, cancellation, socket loss).
Rows are bounded per result set and messages/done tokens per phase
(`PREPARED_LIMITS`, overridable with `options.limits`); overflow is counted in a
`truncated: { rows, messages }` field that appears only when a bound was hit.
`tests/reference-prepared.test.mjs` covers these rules with a fake connection
driving real tedious Request objects.

Do not hand-roll another prepared helper in a capture script.

### Verification of existing fixtures

After converting both scripts to `capturePrepared`, their normal check modes
(two containers, two fresh databases each) reproduced the retained
`reference/hashbytes-checksum.json` and `reference/charindex-patindex.json`
byte for byte. In particular, `prepared charindex bigint start` execution 3
(`start = -3000000000` after execution 2 failed with `3000000000`) is a genuine
SQL Server error 8115 (state 2, class 16, line 1, return status -6), not a
stale tedious error: the fixture records the full server token, and the fixed
helper reproduces it. The HASHBYTES prepared sequence has no failing execution.

## Unique statement text for plan-dependent sequences

SQL Server caches plans by exact statement text (and, for parameterized calls,
the parameter declaration list), so identical text in different programs of the
same capture (`sp_executesql` and `sp_prepare`, or repeated batches) can share a
plan cache entry. When a sequence's outcome may depend on plan reuse,
such as parameter-sensitive typing, cached conversion paths or first-value
sniffing, give each sequence a unique text, for example a trailing comment
`/*prepared matches*/`, and record which cases intentionally share text.
Existing fixtures were captured with shared text (for example the CHARINDEX
prepared cases reuse `SELECT CHARINDEX(@find,@search,@start) AS value` from the
`sp_executesql` cases); changing their SQL requires a new fixture and an owner
decision.

## Containers

`withReferenceContainer` starts a uniquely named container with
`--label msduck.reference=1` and `--label msduck.owner=<cwd>:<pid>`, and removes
only that container. Parallel workers share one Docker daemon: list your own
containers with
`docker ps -a --filter label=msduck.owner --format '{{.Names}} {{.Label "msduck.owner"}}'`
and remove only those whose owner names your worktree. Never remove a container
another worker owns.

## Memory limits

Run capture and check modes with `node --max-old-space-size=2048` and an RSS
watchdog that kills the process above 4 GB. A normal capture stays near 100 MB
RSS; exceeding the limit indicates an unbounded comparison or collection, which
must be fixed rather than given more memory.

## Temporary files

Keep captures, logs and helper scripts in the worktree's ignored
`artifacts/<task-id>/` directory or a per-task scratch directory such as
`<scratchpad>/<task-id>/`. Do not write to shared paths like `/tmp/capture.json`
or another worker's `artifacts/` directory, and pass an explicit output path
when the script's default could collide with another worker.
