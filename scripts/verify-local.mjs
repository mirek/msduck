#!/usr/bin/env node
// Local pre-merge verification that mirrors the CI fast gate and, with --full,
// the native workspace/client job. It records evidence; it never installs tools
// and never hides a failure. See docs/local-verification.md.
import { spawn, execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { createWriteStream, lstatSync, readFileSync, readlinkSync } from 'node:fs'
import { mkdir, readdir, readFile, writeFile } from 'node:fs/promises'
import { availableParallelism, cpus, freemem, loadavg, totalmem, platform, arch, release } from 'node:os'
import { resolve, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../', import.meta.url))
const GiB = 1024 ** 3
// Planning constants, not machine measurements. A DuckDB-linked Rust/C++ job
// group can transiently need several GB (C++ units, rustc, test-binary links).
// A client shard runs one node:test process plus msduck servers whose DuckDB
// queries use several threads; many client tests have 20-30 s deadlines, so
// shards get whole idle CPUs rather than a share of a busy machine.
const CARGO_JOB_BYTES = 3 * GiB
const CLIENT_JOB_BYTES = 1.5 * GiB
const CLIENT_JOB_CPUS = 4
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

// Read the pinned toolchain and the fast job's steps from the workflow so the
// local gate cannot silently drift from CI. Every step must be understood:
// `uses:` actions are CI setup (checkout, caches, setup-node), the rustup
// install step is replaced by checkToolchain, and each other `run:` step, named
// or not, single-line or a `|` block, runs locally. Anything else in the job
// (step env/shell/if, job env/defaults/services, folded scalars) is rejected.
const JOB_KEYS = new Set(['name', 'if', 'needs', 'runs-on', 'timeout-minutes', 'steps'])
const STEP_KEYS = new Set(['name', 'uses', 'with', 'run', 'timeout-minutes'])
function readWorkflow() {
  const text = readFileSync(resolve(root, '.github/workflows/ci.yml'), 'utf8')
  const toolchain = text.match(/^ {2}RUSTUP_TOOLCHAIN:\s*['"]?([A-Za-z0-9._-]+)['"]?\s*$/m)?.[1]
  if (!toolchain) throw Error('Could not find the top-level RUSTUP_TOOLCHAIN in .github/workflows/ci.yml')
  const unsupported = why => Error(`Unsupported CI fast job (${why}); update scripts/verify-local.mjs so it mirrors the job`)
  const lines = text.split('\n')
  const start = lines.findIndex(line => line === '  fast:')
  if (start < 0) throw Error('Could not find the fast job in .github/workflows/ci.yml')
  let end = lines.findIndex((line, i) => i > start && /^ {2}[A-Za-z0-9_-]+:/.test(line))
  if (end < 0) end = lines.length
  const job = lines.slice(start + 1, end)
  const indent = line => line.match(/^ */)[0].length
  const blank = line => !line.trim() || /^\s*#/.test(line)
  for (const line of job) {
    const key = line.match(/^ {4}([A-Za-z0-9_-]+):/)?.[1]
    if (key && !JOB_KEYS.has(key)) throw unsupported(`job key "${key}"`)
  }
  const stepsAt = job.findIndex(line => /^ {4}steps:\s*$/.test(line))
  if (stepsAt < 0) throw unsupported('no steps list')
  const body = job.slice(stepsAt + 1)
  const itemIndent = indent(body.find(line => !blank(line)) ?? '')
  const items = []
  for (const line of body) {
    if (blank(line)) { items.at(-1)?.push(line); continue }
    if (indent(line) <= 4) break
    if (indent(line) === itemIndent && line.slice(itemIndent).startsWith('- ')) items.push([' '.repeat(itemIndent + 2) + line.slice(itemIndent + 2)])
    else if (indent(line) < itemIndent + 2 || !items.length) throw unsupported(`unexpected line "${line.trim()}"`)
    else items.at(-1).push(line)
  }
  const keyIndent = itemIndent + 2
  const commands = [], setup = [], substituted = []
  for (const item of items) {
    const step = {}
    for (let i = 0; i < item.length; i++) {
      const line = item[i]
      if (blank(line) || indent(line) > keyIndent) continue
      const m = line.match(/^\s*([A-Za-z0-9_-]+):\s?(.*)$/)
      if (!m) throw unsupported(`unexpected line "${line.trim()}"`)
      const [, key, value] = m
      if (!STEP_KEYS.has(key)) throw unsupported(`step key "${key}"`)
      if (key !== 'run') { step[key] = value.trim(); continue }
      if (/^\|[-+]?\s*$/.test(value)) {
        const block = []
        while (i + 1 < item.length && (blank(item[i + 1]) || indent(item[i + 1]) > keyIndent)) block.push(item[++i])
        const depth = Math.min(...block.filter(l => l.trim()).map(indent))
        step.run = block.map(l => l.slice(depth)).join('\n').trim()
      } else if (!value.trim() || /^[>'"]/.test(value.trim())) {
        throw unsupported('folded, quoted or empty run value')
      } else step.run = value.trim()
      if (!step.run) throw unsupported('empty run step')
    }
    const label = step.name || step.run || step.uses
    if (step.run && step.uses) throw unsupported(`step "${label}" has both run and uses`)
    if (step.uses) setup.push(step.name ? `${step.name} (${step.uses})` : step.uses)
    else if (!step.run) throw unsupported(`step "${label ?? '?'}" has neither run nor uses`)
    else if (/^rustup toolchain install\b/.test(step.run) && !step.run.includes('\n')) substituted.push(`${step.name ?? step.run}: replaced by the local toolchain check`)
    else commands.push(step.run)
  }
  if (!commands.length) throw Error('No run steps found in the CI fast job')
  // Every setup-node step in the workflow (fast and full jobs) must agree on one
  // plain version pin such as '24', '24.x' or '24.13.0'.
  const pins = [...text.matchAll(/^\s+node-version:\s*['"]?([^'"\s#]+)['"]?\s*(?:#.*)?$/gm)].map(m => m[1])
  if (!pins.length) throw unsupported('no setup-node node-version pin')
  if (new Set(pins).size !== 1) throw unsupported(`conflicting node-version pins ${[...new Set(pins)].join(', ')}`)
  if (!/^\d+(?:\.x|\.\d+\.\d+)?$/.test(pins[0])) throw unsupported(`node-version "${pins[0]}" is not a plain major or exact version`)
  return { toolchain, node: pins[0], fastCommands: commands, ciSetup: { actions: setup, substituted } }
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

// Both the Node running this script and the `node` on PATH that the step
// commands (and npm) resolve must match the CI setup-node pin.
function checkNode(pin, env) {
  const matches = version => pin.includes('.') && !pin.endsWith('.x')
    ? version === `v${pin}` : version?.match(/^v(\d+)\./)?.[1] === pin.replace(/\.x$/, '')
  const child = quiet('node', ['--version'], env)
  const childPath = quiet('bash', ['-c', 'command -v node'], env)
  const found = [`this script: ${process.version} (${process.execPath})`, `node on PATH: ${child ?? 'not found'}${childPath ? ` (${childPath})` : ''}`]
  if (matches(process.version) && matches(child)) return { ok: true, child }
  return { ok: false, message: `CI pins Node ${pin} (setup-node node-version in .github/workflows/ci.yml), but found\n  ${found.join('\n  ')}\nInstall Node ${pin} and put it first on PATH (for example with your Node version manager), then rerun.` }
}

// Parallelism from explicit host inputs. Linux freemem() reports MemAvailable;
// macOS reports only free pages (reclaimable cache excluded), so a fraction of
// total memory is the better estimate there. process.constrainedMemory() reports
// cgroup/container limits where Node can see them.
// Client timeouts are sensitive to other work on the host, so client shards are
// sized from CPUs left idle by the one-minute load average at the start.
function plan({ cpuCount, load, total, free, constrained, os, full, cargoJobs, clientJobs }) {
  const limit = constrained > 0 ? Math.min(constrained, total) : total
  const reclaimable = os === 'darwin' ? Math.max(free, total * 0.6) : free
  const usable = Math.min(reclaimable, limit)
  const headroom = Math.max(2 * GiB, limit * 0.15)
  const budget = Math.max(0, usable - headroom)
  const gb = bytes => `${(bytes / GiB).toFixed(1)} GiB`
  const byMemCargo = Math.floor(budget / CARGO_JOB_BYTES)
  const autoCargo = Math.max(1, Math.min(cpuCount, byMemCargo))
  const idle = Math.max(1, Math.min(cpuCount, Math.round(cpuCount - load)))
  const byCpuClient = Math.floor(idle / CLIENT_JOB_CPUS)
  const byMemClient = Math.floor(budget / CLIENT_JOB_BYTES)
  const autoClient = Math.max(1, Math.min(byCpuClient, byMemClient, CLIENT_JOB_LIMIT))
  const basis = `${cpuCount} CPUs (load ${load.toFixed(1)}, ~${idle} idle); ${gb(usable)} usable memory${os === 'darwin' ? ' (macOS: max(free, 60% of total))' : ''}` +
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
      : `min(${byCpuClient} by ${idle} idle CPUs at ${CLIENT_JOB_CPUS}/shard, ${byMemClient} by memory at ${gb(CLIENT_JOB_BYTES)}/shard, runner limit ${CLIENT_JOB_LIMIT}), at least 1`,
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

// Top-level TAP failures plus the per-test failure type and error, and any
// accounting problems (missing/unexpected tests, process exits) the runner saw.
async function shardFailures(directory) {
  const lines = []
  try {
    const summary = JSON.parse(await readFile(resolve(root, directory, 'summary.json'), 'utf8'))
    for (const r of summary.results ?? []) {
      for (const problem of r.problems ?? []) lines.push(`- job ${r.index} (${r.log ? relative(root, r.log) : 'no log'}): ${problem}`)
    }
    for (const name of (await readdir(resolve(root, directory))).filter(f => /^job-\d+\.tap$/.test(f)).sort()) {
      const tap = (await readFile(resolve(root, directory, name), 'utf8')).split('\n')
      tap.forEach((line, i) => {
        const failed = line.match(/^not ok \d+ - (.*)$/)
        if (!failed) return
        const detail = tap.slice(i + 1, i + 30).map(l => l.trim()).filter(l => /^(failureType|error):/.test(l)).slice(0, 2).join('; ')
        lines.push(`- ${name}: ${failed[1]}${detail ? ` (${detail})` : ''}`)
      })
    }
  } catch (error) { lines.push(`- could not read shard results: ${error.message}`) }
  return lines.slice(0, 60)
}

// Content fingerprint of HEAD, the index, tracked changes and untracked files.
// Comparing it at the end detects edits during a run even when the tree was
// already dirty at the start (--allow-dirty).
function treeFingerprint() {
  const hash = createHash('sha256')
  const run = (...args) => execFileSync('git', args, { cwd: root, maxBuffer: 1024 ** 3 })
  hash.update(run('rev-parse', 'HEAD'))
  hash.update(run('status', '--porcelain=v1', '-z', '--untracked-files=all'))
  hash.update(run('diff', '--binary', '--no-ext-diff', 'HEAD'))
  hash.update(run('diff', '--binary', '--no-ext-diff', '--cached', 'HEAD'))
  for (const file of run('ls-files', '--others', '--exclude-standard', '-z').toString('utf8').split('\0').filter(Boolean).sort()) {
    hash.update(`\0${file}\0`)
    try {
      const path = resolve(root, file)
      hash.update(lstatSync(path).isSymbolicLink() ? `link:${readlinkSync(path)}` : readFileSync(path))
    } catch (error) { hash.update(`unreadable:${error.code}`) }
  }
  return hash.digest('hex')
}

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
    `- Toolchain: ${toolchain.rustc ?? 'unknown rustc'} (CI pin ${toolchain.pinned}); ${toolchain.cargo ?? 'unknown cargo'}; node ${toolchain.node} (CI pin ${toolchain.nodePinned})${toolchain.npm ? `; npm ${toolchain.npm}` : ''}`,
    `- Host: ${host.platform}/${host.arch}, ${host.cpuModel}, ${host.availableParallelism} CPUs, ${host.totalMemoryGiB} GiB RAM`,
    `- Parallelism: cargo ${parallelism.cargo.jobs} (${parallelism.cargo.reason})${parallelism.client ? `; client shards ${parallelism.client.jobs} (${parallelism.client.reason})` : ''}`,
    `- CI setup not run locally: ${[...summary.ciSetup.substituted, ...summary.ciSetup.actions.map(a => a.replace(/@[0-9a-f]{40}/, ''))].join('; ')}`,
    `- Total: ${duration(summary.durationMs)}; started ${summary.startedAt}`,
    '',
    '| Step | Result | Time | Tests |',
    '| --- | --- | --- | --- |',
    ...summary.steps.map(s => `| \`${s.command.replaceAll('\n', '; ').replaceAll('|', '\\|')}\` | ${icon[s.status]}${s.status === 'failed' ? ` (exit ${s.exitCode ?? s.signal})` : ''} | ${s.durationMs === null ? '' : duration(s.durationMs)} | ${describeTests(s.tests)} |`),
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
  const { toolchain, node: nodePin, fastCommands, ciSetup } = readWorkflow()
  const toolcheck = checkToolchain(toolchain)
  if (!toolcheck.ok) { console.error(toolcheck.message); return 1 }
  const nodecheck = checkNode(nodePin, process.env)
  if (!nodecheck.ok) { console.error(nodecheck.message); return 1 }

  const head = git('rev-parse', 'HEAD')
  const status = git('status', '--porcelain', '--untracked-files=normal')
  if (status && !options.allowDirty) {
    console.error('Refusing to verify an uncommitted tree; the evidence would not match any commit.')
    console.error('Commit (or stash) first, or pass --allow-dirty to run anyway with the result marked as not representing a commit.')
    console.error(status.split('\n').slice(0, 20).join('\n'))
    return 1
  }
  const dirty = Boolean(status)
  const fingerprint = treeFingerprint()

  const host = {
    platform: platform(), arch: arch(), release: release(), cpuModel: cpus()[0]?.model?.trim() ?? 'unknown CPU',
    availableParallelism: availableParallelism(), loadAverage1m: +loadavg()[0].toFixed(2), totalMemoryGiB: +(totalmem() / GiB).toFixed(1), freeMemoryGiB: +(freemem() / GiB).toFixed(1),
    memoryLimitGiB: process.constrainedMemory?.() > 0 && process.constrainedMemory() < totalmem() ? +(process.constrainedMemory() / GiB).toFixed(1) : null,
  }
  const parallelism = plan({
    cpuCount: availableParallelism(), load: loadavg()[0], total: totalmem(), free: freemem(), constrained: process.constrainedMemory?.() ?? 0,
    os: platform(), full: options.full, cargoJobs: options.cargoJobs, clientJobs: options.clientJobs,
  })

  const env = { ...process.env, RUSTUP_TOOLCHAIN: toolchain, CARGO_BUILD_JOBS: String(parallelism.cargo.jobs), CARGO_TERM_COLOR: 'never', NO_COLOR: '1' }
  // A parent node:test runner's marker would make child --test runs skip files.
  delete env.NODE_TEST_CONTEXT
  const versions = {
    pinned: toolchain, rustc: quiet('rustc', ['--version'], env), cargo: quiet('cargo', ['--version'], env),
    node: process.version, nodeOnPath: nodecheck.child, nodePinned: nodePin, npm: options.full ? quiet('npm', ['--version'], env) : null,
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
      { id: 'full-5-clients', job: 'full', command: `node scripts/run-client-shards.mjs --jobs ${parallelism.client.jobs} --revision ${head} --output ${relative(root, resolve(output, 'client-shards'))}`, count: 'shards', shards: relative(root, resolve(output, 'client-shards')) },
      { id: 'full-6-aggregate-diagnostics', job: 'full', command: 'node --test --test-concurrency=1 tests/aggregate_diagnostics.test.mjs tests/character_extrema.test.mjs', count: 'node' },
    )
    if (options.audit) steps.push({ id: 'full-7-audit', job: 'full', command: 'npm run audit:local', count: 'none' })
  }

  console.log(`Local verification (${mode}) of ${head}${dirty ? ' WITH UNCOMMITTED CHANGES' : ''}`)
  console.log(`Toolchain: ${versions.rustc} (CI pin ${toolchain}), node ${versions.node} (CI pin ${nodePin})`)
  console.log(`Host: ${host.platform}/${host.arch} ${host.cpuModel}; ${parallelism.basis}`)
  console.log(`Cargo jobs: ${parallelism.cargo.jobs} (${parallelism.cargo.reason})`)
  console.log(`CI setup not run locally: ${[...ciSetup.substituted, ...ciSetup.actions].join('; ')}`)
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
      // The shard runner prints only totals; name the failing tests from its logs.
      if (step.shards) {
        const failures = await shardFailures(step.shards)
        if (failures.length) record.tail += `\n\nFailing client tests (${step.shards}):\n${failures.join('\n')}`
      }
      console.log(`--- last lines of ${record.log} ---\n${record.tail}\n--- end ---`)
      if (!options.keepGoing) stop = true
    }
  }

  const problems = []
  if (dirty) problems.push('tree had uncommitted changes at start')
  const endHead = git('rev-parse', 'HEAD')
  const endFingerprint = treeFingerprint()
  if (endHead !== head) problems.push(`HEAD moved during the run to ${endHead}`)
  if (endFingerprint !== fingerprint) problems.push('tree changed during the run')
  if (interrupted) problems.push('run interrupted')
  const allPassed = results.every(s => s.status === 'passed')
  const summary = {
    ok: allPassed && !interrupted && endHead === head && endFingerprint === fingerprint,
    mode, representsCommit: !problems.length, problems, startedAt, durationMs: performance.now() - started,
    revision: { head, short, dirtyAtStart: dirty, dirtyFiles: dirty ? status.split('\n') : [], treeFingerprint: { start: fingerprint, end: endFingerprint } },
    ciSetup,
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
