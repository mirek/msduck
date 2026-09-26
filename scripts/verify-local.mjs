#!/usr/bin/env node
// Local pre-merge verification that mirrors the CI fast gate and, with --full,
// the native workspace/client job. It records evidence; it never installs tools
// and never hides a failure. See docs/local-verification.md.
import { spawn, execFileSync } from 'node:child_process'
import { createWriteStream, readFileSync } from 'node:fs'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { availableParallelism, cpus, freemem, totalmem, platform, arch, release } from 'node:os'
import { resolve, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../', import.meta.url))
const GiB = 1024 ** 3
// Planning constants, not machine measurements. A DuckDB-linked Rust/C++ job
// group can transiently need several GB (C++ units, rustc, test-binary links).
// A client shard runs one node:test process plus short-lived msduck servers.
const CARGO_JOB_BYTES = 3 * GiB
const CLIENT_JOB_BYTES = 1.5 * GiB
const CLIENT_JOB_CPUS = 2
const CLIENT_JOB_LIMIT = 16 // run-client-shards.mjs accepts 1..16

const usage = `Usage: node scripts/verify-local.mjs [options]

  (default)          fast mode: mirror the CI "Deterministic crates" job
  --full             also run the CI "Workspace and clients" native steps
  --audit            with --full, also run npm run audit:local (diagnostic)
  --cargo-jobs N     override CARGO_BUILD_JOBS (positive integer)
  --client-jobs N    override client shard workers (1..${CLIENT_JOB_LIMIT})
  --keep-going       continue after a failed step (still exits nonzero)
  --allow-dirty      run on an uncommitted tree; result does not represent a commit
  --verbose          stream step output to the terminal as well as the log
  --help             show this help`

function parseArgs(argv) {
  const options = { full: false, audit: false, keepGoing: false, allowDirty: false, verbose: false, cargoJobs: null, clientJobs: null }
  const positive = (flag, value, max = Infinity) => {
    if (value === undefined || !/^[1-9][0-9]*$/.test(value) || Number(value) > max) {
      throw new UsageError(`${flag} needs a positive integer${max < Infinity ? ` up to ${max}` : ''}, got ${JSON.stringify(value ?? '')}`)
    }
    return Number(value)
  }
  for (let i = 0; i < argv.length; i++) {
    const [flag, inline] = argv[i].split(/=(.*)/s, 2)
    const value = () => inline ?? argv[++i]
    if (flag === '--full') options.full = true
    else if (flag === '--audit') options.audit = true
    else if (flag === '--keep-going') options.keepGoing = true
    else if (flag === '--allow-dirty') options.allowDirty = true
    else if (flag === '--verbose') options.verbose = true
    else if (flag === '--cargo-jobs') options.cargoJobs = positive(flag, value())
    else if (flag === '--client-jobs') options.clientJobs = positive(flag, value(), CLIENT_JOB_LIMIT)
    else if (flag === '--help' || flag === '-h') { console.log(usage); process.exit(0) }
    else throw new UsageError(`Unknown argument: ${argv[i]}`)
  }
  if (options.audit && !options.full) throw new UsageError('--audit requires --full (the audit needs the native build)')
  if (options.clientJobs !== null && !options.full) throw new UsageError('--client-jobs applies only to --full')
  return options
}
class UsageError extends Error {}

const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim()
const quiet = (command, args, env) => {
  try { return execFileSync(command, args, { cwd: root, env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim() }
  catch { return null }
}

// Read the pinned toolchain and the fast job's commands from the workflow so the
// local gate cannot silently drift from CI. Only single-line `- run:` steps are
// accepted; anything else in the fast job needs this script to be updated.
function readWorkflow() {
  const text = readFileSync(resolve(root, '.github/workflows/ci.yml'), 'utf8')
  const toolchain = text.match(/^ {2}RUSTUP_TOOLCHAIN:\s*['"]?([A-Za-z0-9._-]+)['"]?\s*$/m)?.[1]
  if (!toolchain) throw Error('Could not find the top-level RUSTUP_TOOLCHAIN in .github/workflows/ci.yml')
  const lines = text.split('\n')
  const start = lines.findIndex(line => line === '  fast:')
  if (start < 0) throw Error('Could not find the fast job in .github/workflows/ci.yml')
  let end = lines.findIndex((line, i) => i > start && /^ {2}[A-Za-z0-9_-]+:/.test(line))
  if (end < 0) end = lines.length
  const commands = []
  for (const line of lines.slice(start, end)) {
    const run = line.match(/^\s+- run:\s*(.*)$/)
    if (!run) continue
    if (!run[1] || /^[|>]/.test(run[1])) throw Error('Unsupported multi-line run step in the CI fast job; update scripts/verify-local.mjs')
    commands.push(run[1])
  }
  if (!commands.length) throw Error('No run steps found in the CI fast job')
  return { toolchain, fastCommands: commands }
}

function checkToolchain(toolchain) {
  const install = `rustup toolchain install ${toolchain} --profile minimal --component rustfmt --component clippy`
  let installed
  try { installed = execFileSync('rustup', ['toolchain', 'list'], { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }) }
  catch (error) {
    const reason = error.code === 'ENOENT' ? 'rustup was not found on PATH' : `rustup toolchain list failed: ${String(error.stderr ?? error.message).trim()}`
    return { ok: false, message: `${reason}. Fix rustup, then make sure the CI-pinned toolchain is installed:\n  ${install}` }
  }
  if (!installed.split('\n').some(line => line.split(/\s/)[0] === toolchain || line.startsWith(`${toolchain}-`))) {
    return { ok: false, message: `The CI-pinned Rust toolchain ${toolchain} is not installed. Run:\n  ${install}` }
  }
  const components = quiet('rustup', ['component', 'list', '--installed', '--toolchain', toolchain]) ?? ''
  const missing = ['rustfmt', 'clippy'].filter(name => !components.split('\n').some(line => line.startsWith(`${name}-`) || line === name))
  if (missing.length) return { ok: false, message: `Toolchain ${toolchain} lacks ${missing.join(', ')}. Run:\n  ${install}` }
  return { ok: true }
}

// Parallelism from explicit host inputs. Linux freemem() reports MemAvailable;
// macOS reports only free pages (reclaimable cache excluded), so a fraction of
// total memory is the better estimate there. process.constrainedMemory() reports
// cgroup/container limits where Node can see them.
function plan({ cpuCount, total, free, constrained, os, full, cargoJobs, clientJobs }) {
  const limit = constrained > 0 ? Math.min(constrained, total) : total
  const reclaimable = os === 'darwin' ? Math.max(free, total * 0.6) : free
  const usable = Math.min(reclaimable, limit)
  const headroom = Math.max(2 * GiB, limit * 0.15)
  const budget = Math.max(0, usable - headroom)
  const gb = bytes => `${(bytes / GiB).toFixed(1)} GiB`
  const byMemCargo = Math.floor(budget / CARGO_JOB_BYTES)
  const autoCargo = Math.max(1, Math.min(cpuCount, byMemCargo))
  const byCpuClient = Math.floor(cpuCount / CLIENT_JOB_CPUS)
  const byMemClient = Math.floor(budget / CLIENT_JOB_BYTES)
  const autoClient = Math.max(1, Math.min(byCpuClient, byMemClient, CLIENT_JOB_LIMIT))
  const basis = `${cpuCount} CPUs; ${gb(usable)} usable memory${os === 'darwin' ? ' (macOS: max(free, 60% of total))' : ''}` +
    `${limit < total ? ' (process/container limit)' : ''} minus ${gb(headroom)} OS headroom = ${gb(budget)} budget`
  const result = {
    basis,
    cargo: {
      jobs: cargoJobs ?? autoCargo,
      reason: cargoJobs ? 'explicit --cargo-jobs override'
        : `min(${cpuCount} CPUs, ${byMemCargo} by memory at ${gb(CARGO_JOB_BYTES)}/job), at least 1`,
    },
  }
  if (full) result.client = {
    jobs: clientJobs ?? autoClient,
    reason: clientJobs ? 'explicit --client-jobs override'
      : `min(${byCpuClient} by CPU at ${CLIENT_JOB_CPUS} CPUs/shard, ${byMemClient} by memory at ${gb(CLIENT_JOB_BYTES)}/shard, runner limit ${CLIENT_JOB_LIMIT}), at least 1`,
  }
  return result
}

// Test counts are parsed only from formats whose totals are unambiguous.
function countCargo(text) {
  let found = false
  const counts = { passed: 0, failed: 0, ignored: 0 }
  for (const m of text.matchAll(/^test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored;/gm)) {
    found = true
    counts.passed += Number(m[1]); counts.failed += Number(m[2]); counts.ignored += Number(m[3])
  }
  return found ? counts : null
}
function countNodeTest(text) {
  const counts = {}
  for (const m of text.matchAll(/^(?:ℹ|#) (tests|pass|fail|cancelled|skipped|todo) (\d+)\s*$/gm)) counts[m[1]] = Number(m[2])
  if (!('tests' in counts)) return null
  return { passed: counts.pass ?? 0, failed: (counts.fail ?? 0) + (counts.cancelled ?? 0), skipped: counts.skipped ?? 0, todo: counts.todo ?? 0 }
}
function countShards(text) {
  const line = text.trim().split('\n').reverse().find(l => l.startsWith('{') && l.includes('"expected"'))
  if (!line) return null
  try {
    const s = JSON.parse(line)
    return { expected: s.expected, passed: s.passed, failed: s.failed, skipped: s.skipped, todo: s.todo }
  } catch { return null }
}
const counters = { cargo: countCargo, node: countNodeTest, shards: countShards, none: () => null }

let current = null
let interrupted = false
function runStep(step, env, directory, verbose) {
  return new Promise(done => {
    const log = resolve(directory, `${step.id}.log`)
    const out = createWriteStream(log)
    out.write(`$ ${step.command}\n`)
    const started = performance.now()
    // Each step gets its own process group so an interrupt stops the whole tree.
    const child = spawn('bash', ['-o', 'pipefail', '-c', step.command], { cwd: root, env, detached: true, stdio: ['ignore', 'pipe', 'pipe'] })
    current = child
    const sink = data => { out.write(data); if (verbose) process.stdout.write(data) }
    child.stdout.on('data', sink); child.stderr.on('data', sink)
    child.once('error', error => sink(`\nspawn error: ${error.message}\n`))
    child.once('close', (code, signal) => {
      current = null
      const durationMs = performance.now() - started
      out.end(async () => {
        const text = await readFile(log, 'utf8')
        done({ code, signal, durationMs, log, tests: counters[step.count](text), text })
      })
    })
  })
}
function stopCurrent(signal) {
  interrupted = true
  if (current?.pid) { try { process.kill(-current.pid, signal) } catch {} }
}

const duration = ms => {
  const s = Math.round(ms / 1000)
  return s >= 60 ? `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, '0')}s` : `${s}s`
}
function describeTests(t) {
  if (!t) return ''
  const parts = [`${t.passed} passed`, `${t.failed} failed`]
  if (t.ignored) parts.push(`${t.ignored} ignored`)
  if (t.skipped) parts.push(`${t.skipped} skipped`)
  if (t.todo) parts.push(`${t.todo} todo`)
  if (t.expected !== undefined) parts.push(`${t.expected} expected`)
  return parts.join(', ')
}

function markdown(summary) {
  const { revision, host, toolchain, parallelism } = summary
  const icon = { passed: 'pass', failed: '**FAIL**', skipped: 'not run', interrupted: '**INTERRUPTED**' }
  const lines = [
    `### Local verification (${summary.mode}): ${summary.ok ? 'PASSED' : '**FAILED**'}`,
    '',
    `- Revision: \`${revision.head}\`${revision.dirtyAtStart ? ' **with uncommitted changes (does not represent this commit)**' : ''}`,
    `- Represents commit: ${summary.representsCommit ? 'yes' : '**no**'}${summary.problems.length ? ` (${summary.problems.join('; ')})` : ''}`,
    ...(revision.dirtyFiles.length ? [`- Uncommitted: ${revision.dirtyFiles.slice(0, 20).map(f => `\`${f.trim()}\``).join(', ')}${revision.dirtyFiles.length > 20 ? `, and ${revision.dirtyFiles.length - 20} more` : ''}`] : []),
    `- Toolchain: ${toolchain.rustc ?? 'unknown rustc'} (CI pin ${toolchain.pinned}); ${toolchain.cargo ?? 'unknown cargo'}; node ${toolchain.node}${toolchain.npm ? `; npm ${toolchain.npm}` : ''}`,
    `- Host: ${host.platform}/${host.arch}, ${host.cpuModel}, ${host.availableParallelism} CPUs, ${host.totalMemoryGiB} GiB RAM`,
    `- Parallelism: cargo ${parallelism.cargo.jobs} (${parallelism.cargo.reason})${parallelism.client ? `; client shards ${parallelism.client.jobs} (${parallelism.client.reason})` : ''}`,
    `- Total: ${duration(summary.durationMs)}; started ${summary.startedAt}`,
    '',
    '| Step | Result | Time | Tests |',
    '| --- | --- | --- | --- |',
    ...summary.steps.map(s => `| \`${s.command.replaceAll('|', '\\|')}\` | ${icon[s.status]}${s.status === 'failed' ? ` (exit ${s.exitCode ?? s.signal})` : ''} | ${s.durationMs === null ? '' : duration(s.durationMs)} | ${describeTests(s.tests)} |`),
    '',
    `Local evidence complements, and does not replace, full CI on main or an owner-dispatched run. Logs: \`${summary.output}\``,
  ]
  const failed = summary.steps.filter(s => s.status === 'failed' || s.status === 'interrupted')
  if (failed.length) {
    lines.push('', 'Failure output (last lines of each failed step):')
    for (const s of failed) lines.push('', `\`${s.command}\` (${s.log}):`, '', '```', s.tail, '```')
  }
  return lines.join('\n') + '\n'
}

async function main() {
  const options = parseArgs(process.argv.slice(2))
  const { toolchain, fastCommands } = readWorkflow()
  const toolcheck = checkToolchain(toolchain)
  if (!toolcheck.ok) { console.error(toolcheck.message); return 1 }

  const head = git('rev-parse', 'HEAD')
  const status = git('status', '--porcelain', '--untracked-files=normal')
  if (status && !options.allowDirty) {
    console.error('Refusing to verify an uncommitted tree; the evidence would not match any commit.')
    console.error('Commit (or stash) first, or pass --allow-dirty to run anyway with the result marked as not representing a commit.')
    console.error(status.split('\n').slice(0, 20).join('\n'))
    return 1
  }
  const dirty = Boolean(status)

  const host = {
    platform: platform(), arch: arch(), release: release(), cpuModel: cpus()[0]?.model?.trim() ?? 'unknown CPU',
    availableParallelism: availableParallelism(), totalMemoryGiB: +(totalmem() / GiB).toFixed(1), freeMemoryGiB: +(freemem() / GiB).toFixed(1),
    memoryLimitGiB: process.constrainedMemory?.() > 0 && process.constrainedMemory() < totalmem() ? +(process.constrainedMemory() / GiB).toFixed(1) : null,
  }
  const parallelism = plan({
    cpuCount: availableParallelism(), total: totalmem(), free: freemem(), constrained: process.constrainedMemory?.() ?? 0,
    os: platform(), full: options.full, cargoJobs: options.cargoJobs, clientJobs: options.clientJobs,
  })

  const env = { ...process.env, RUSTUP_TOOLCHAIN: toolchain, CARGO_BUILD_JOBS: String(parallelism.cargo.jobs), CARGO_TERM_COLOR: 'never', NO_COLOR: '1' }
  // A parent node:test runner's marker would make child --test runs skip files.
  delete env.NODE_TEST_CONTEXT
  const versions = {
    pinned: toolchain, rustc: quiet('rustc', ['--version'], env), cargo: quiet('cargo', ['--version'], env),
    node: process.version, npm: options.full ? quiet('npm', ['--version'], env) : null,
  }

  const mode = options.full ? (options.audit ? 'full+audit' : 'full') : 'fast'
  const startedAt = new Date().toISOString()
  const short = head.slice(0, 12)
  const output = resolve(root, 'artifacts/local-verify', short, `${mode}${dirty ? '-dirty' : ''}-${startedAt.replace(/[:.]/g, '-')}`)
  await mkdir(output, { recursive: true })

  const count = command => /^cargo test\b/.test(command) ? 'cargo' : /^node --test\b/.test(command) ? 'node' : 'none'
  const steps = fastCommands.map((command, i) => ({ id: `fast-${i + 1}`, job: 'fast', command, count: count(command) }))
  if (options.full) {
    steps.push(
      { id: 'full-1-npm-ci', job: 'full', command: 'npm ci --no-audit --no-fund', count: 'none' },
      { id: 'full-2-clippy', job: 'full', command: 'cargo clippy --workspace --all-targets --locked -- -D warnings', count: 'none' },
      { id: 'full-3-rust-tests', job: 'full', command: 'cargo test --workspace --locked', count: 'cargo' },
      { id: 'full-4-build', job: 'full', command: 'cargo build --workspace --all-targets --locked', count: 'none' },
      { id: 'full-5-clients', job: 'full', command: `node scripts/run-client-shards.mjs --jobs ${parallelism.client.jobs} --revision ${head} --output ${relative(root, resolve(output, 'client-shards'))}`, count: 'shards' },
      { id: 'full-6-aggregate-diagnostics', job: 'full', command: 'node --test --test-concurrency=1 tests/aggregate_diagnostics.test.mjs tests/character_extrema.test.mjs', count: 'node' },
    )
    if (options.audit) steps.push({ id: 'full-7-audit', job: 'full', command: 'npm run audit:local', count: 'none' })
  }

  console.log(`Local verification (${mode}) of ${head}${dirty ? ' WITH UNCOMMITTED CHANGES' : ''}`)
  console.log(`Toolchain: ${versions.rustc} (CI pin ${toolchain}), node ${versions.node}`)
  console.log(`Host: ${host.platform}/${host.arch} ${host.cpuModel}; ${parallelism.basis}`)
  console.log(`Cargo jobs: ${parallelism.cargo.jobs} (${parallelism.cargo.reason})`)
  if (parallelism.client) console.log(`Client shards: ${parallelism.client.jobs} (${parallelism.client.reason})`)
  console.log(`Logs: ${relative(root, output)}\n`)

  process.on('SIGINT', () => stopCurrent('SIGINT'))
  process.on('SIGTERM', () => stopCurrent('SIGTERM'))
  const started = performance.now()
  const results = []
  let stop = false
  for (const step of steps) {
    const record = { id: step.id, job: step.job, command: step.command, status: 'skipped', exitCode: null, signal: null, durationMs: null, tests: null, log: null }
    results.push(record)
    if (stop || interrupted) continue
    process.stdout.write(`[${step.job}] ${step.command} ... `)
    const r = await runStep(step, env, output, options.verbose)
    Object.assign(record, { exitCode: r.code, signal: r.signal, durationMs: r.durationMs, tests: r.tests, log: relative(root, r.log) })
    record.status = interrupted ? 'interrupted' : r.code === 0 ? 'passed' : 'failed'
    // A zero exit with parsed failures is still a failure.
    if (record.status === 'passed' && r.tests?.failed) record.status = 'failed'
    console.log(`${record.status} in ${duration(r.durationMs)}${r.tests ? ` (${describeTests(r.tests)})` : ''}`)
    if (record.status !== 'passed') {
      // Tools such as rustfmt colour diffs regardless of CARGO_TERM_COLOR.
      record.tail = r.text.replace(/\x1b(?:\[[0-9;?]*[A-Za-z]|\([A-Z0-9])/g, '').trimEnd().split('\n').slice(-40).join('\n')
      console.log(`--- last lines of ${record.log} ---\n${record.tail}\n--- end ---`)
      if (!options.keepGoing) stop = true
    }
  }

  const problems = []
  if (dirty) problems.push('tree had uncommitted changes at start')
  const endHead = git('rev-parse', 'HEAD')
  const endStatus = git('status', '--porcelain', '--untracked-files=normal')
  if (endHead !== head) problems.push(`HEAD moved during the run to ${endHead}`)
  if (!dirty && endStatus) problems.push('tree changed during the run')
  if (interrupted) problems.push('run interrupted')
  const allPassed = results.every(s => s.status === 'passed')
  const summary = {
    ok: allPassed && !interrupted && endHead === head && (dirty || !endStatus),
    mode, representsCommit: !problems.length, problems, startedAt, durationMs: performance.now() - started,
    revision: { head, short, dirtyAtStart: dirty, dirtyFiles: dirty ? status.split('\n') : [] },
    toolchain: versions, host, parallelism, options, output: relative(root, output),
    steps: results.map(({ tail, ...s }) => s),
  }
  const md = markdown({ ...summary, steps: results })
  await writeFile(resolve(output, 'summary.json'), JSON.stringify(summary, null, 2) + '\n')
  await writeFile(resolve(output, 'summary.md'), md)
  console.log('\n' + md)
  return summary.ok ? 0 : 1
}

try {
  process.exitCode = await main()
} catch (error) {
  console.error(error instanceof UsageError ? `${error.message}\n\n${usage}` : `verify-local: ${error.message}`)
  process.exitCode = error instanceof UsageError ? 2 : 1
}
