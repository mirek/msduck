import {spawn, execFile} from 'node:child_process'
import {createHash, randomUUID} from 'node:crypto'
import {createReadStream, createWriteStream} from 'node:fs'
import {mkdir, readFile, writeFile, realpath} from 'node:fs/promises'
import {registerHooks} from 'node:module'
import {resolve} from 'node:path'
import {fileURLToPath, pathToFileURL} from 'node:url'
import {promisify} from 'node:util'
import {partition, selection, assess} from './lib/client-shards.mjs'
const exec = promisify(execFile)
const script = fileURLToPath(import.meta.url)
const reporter = fileURLToPath(new URL('./lib/client-shards.mjs', import.meta.url))
const childEnvironment = {...process.env}
// A parent node:test runner marks its workers with this internal variable.
// Passing it onward makes a new --test process silently skip its files.
delete childEnvironment.NODE_TEST_CONTEXT
const defaults = ['tests/tedious.test.mjs', 'tests/compatibility.test.mjs', 'tests/tls.test.mjs']

async function digest(path) {
  const hash = createHash('sha256')
  for await (const data of createReadStream(path)) hash.update(data)
  return hash.digest('hex')
}
async function discover(files) {
  // Module initialization runs normally; only registration is intercepted.
  // Unknown node:test APIs fail module loading instead of hiding tests.
  const source = 'export const cases=[];export function test(name,...args){if(typeof name!=="string"||!name)throw Error("Named tests required");cases.push(name);return Promise.resolve()}export default test;'
  const url = 'data:text/javascript,' + encodeURIComponent(source)
  registerHooks({resolve(specifier, context, next) {
    return specifier === 'node:test' ? {url, shortCircuit: true} : next(specifier, context)
  }})
  const {cases} = await import('node:test')
  let emitted = 0
  process.once('beforeExit', () => {
    if (cases.length !== emitted) {
      console.error('Late test registration is unsupported')
      process.exitCode = 1
    }
  })
  for (const file of files) {
    const before = cases.length
    await import(pathToFileURL(file).href)
    for (const name of cases.slice(before)) console.log(JSON.stringify({file, name}))
    emitted = cases.length
  }
}

async function inventoryProcess(inputs, output) {
  const child = spawn(process.execPath, [script, '--internal-discover', ...inputs], {
    detached: true, stdio: ['ignore', 'pipe', 'pipe'], env: childEnvironment,
  })
  const chunks = {stdout: [], stderr: []}
  let size = 0, failure
  const kill = message => {
    failure ??= message
    if (Number.isInteger(child.pid)) { try { process.kill(-child.pid, 'SIGKILL') } catch {} }
  }
  const cancel = () => kill('Discovery cancelled')
  process.on('SIGINT', cancel); process.on('SIGTERM', cancel)
  const timeout = setTimeout(() => kill('Discovery timed out'), 30000)
  for (const stream of ['stdout', 'stderr']) child[stream].on('data', data => {
    size += data.length
    if (size > 16 * 1024 * 1024) kill('Discovery output exceeded limit')
    else chunks[stream].push(data)
  })
  try {
    const status = await new Promise(resolve => {
      child.once('error', error => { failure = error.message })
      child.once('close', (code, signal) => resolve({code, signal}))
    })
    const stdout = Buffer.concat(chunks.stdout).toString('utf8')
    await writeFile(resolve(output, 'discovery.stdout'), stdout)
    await writeFile(resolve(output, 'discovery.stderr'), Buffer.concat(chunks.stderr).toString('utf8') + (failure ?? ''))
    if (failure || status.code !== 0) throw Error(`Discovery failed; see ${output}/discovery.stderr`)
    return stdout.trim().split('\n').filter(Boolean).map(line => JSON.parse(line))
  } finally {
    clearTimeout(timeout)
    process.off('SIGINT', cancel); process.off('SIGTERM', cancel)
  }
}

async function main(args) {
  let jobs = 1, output = resolve('artifacts/client-shards', randomUUID()), planOnly = false, revision
  const files = []
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--jobs') jobs = Number(args[++i])
    else if (args[i] === '--output') { if (!args[i + 1]) throw Error('Missing output directory'); output = resolve(args[++i]) }
    else if (args[i] === '--revision') revision = args[++i]
    else if (args[i] === '--plan-only') planOnly = true
    else if (args[i] === '--') { files.push(...args.slice(i + 1)); break }
    else throw Error(`Unknown argument: ${args[i]}`)
  }
  if (revision !== undefined && !/^[a-f0-9]{40}$/.test(revision)) throw Error('revision must be a full commit SHA')
  if (process.platform === 'win32') throw Error('Process-group cancellation currently requires POSIX')
  const inputs = await Promise.all((files.length ? files : defaults).map(file => realpath(resolve(file))))
  if (new Set(inputs).size !== inputs.length) throw Error('Duplicate input files')
  if (inputs.some(file => !file.endsWith('.mjs'))) throw Error('Discovery currently supports ESM .mjs tests')
  await mkdir(output, {recursive: true})
  const inventory = []
  for (const [index, file] of inputs.entries()) {
    const directory = resolve(output, `discovery-${index}`)
    await mkdir(directory, {recursive: true})
    inventory.push(...await inventoryProcess([file], directory))
  }
  const plan = partition(inventory, jobs)
  const hashes = Object.fromEntries(await Promise.all([...new Set([...inputs, script, reporter])].map(async file => [file, await digest(file)])))
  const binary = resolve('target/debug/msduck')
  let binaryHash = null
  try { binaryHash = await digest(binary) } catch (error) { if (error.code !== 'ENOENT') throw error }
  if (!planOnly && !files.length && !binaryHash) throw Error('Build the server first; this runner does not compile')
  let checkoutRevision = null, checkoutDirty = null
  try {
    checkoutRevision = (await exec('git', ['rev-parse', 'HEAD'])).stdout.trim()
    checkoutDirty = Boolean((await exec('git', ['status', '--porcelain'])).stdout.trim())
  } catch { /* Private verified source snapshots need not contain .git. */ }
  const provenance = {node: process.version, declaredRevision: revision ?? null, checkoutRevision, checkoutDirty, inputs: hashes, binary: binaryHash ? {path: binary, sha256: binaryHash} : null}
  await writeFile(resolve(output, 'plan.json'), JSON.stringify({provenance, jobs, plan}, null, 2) + '\n')
  if (planOnly) { console.log(JSON.stringify({output, tests: inventory.length, processes: plan.length, jobs})); return }

  const active = new Set(), results = []
  let aborted = false, next = 0, cancellation
  const stop = () => {
    aborted = true
    if (cancellation) return
    const groups = [...active].map(child => child.pid).filter(Number.isInteger)
    for (const pid of groups) { try { process.kill(-pid, 'SIGTERM') } catch {} }
    cancellation = new Promise(resolve => setTimeout(() => {
      for (const pid of groups) { try { process.kill(-pid, 'SIGKILL') } catch {} }
      resolve()
    }, 2000))
  }
  process.on('SIGINT', stop); process.on('SIGTERM', stop)
  const started = performance.now()
  async function run(job, index) {
    const prefix = resolve(output, `job-${index}`)
    const argv = ['--test', `--test-name-pattern=${selection(job.tests)}`, '--test-reporter=tap', `--test-reporter=${reporter}`, `--test-reporter-destination=${prefix}.tap`, `--test-reporter-destination=${prefix}.jsonl`, job.file]
    const log = createWriteStream(`${prefix}.console.log`)
    const child = spawn(process.execPath, argv, {detached: true, stdio: ['ignore', 'pipe', 'pipe'], env: childEnvironment})
    active.add(child)
    child.stdout.pipe(log, {end: false}); child.stderr.pipe(log, {end: false})
    const status = await new Promise(resolve => {
      let error
      child.once('error', cause => { error = cause.message })
      child.once('close', (code, signal) => resolve({code, signal, error}))
    })
    active.delete(child)
    await new Promise(resolve => log.end(resolve))
    let events = []
    try { events = (await readFile(`${prefix}.jsonl`, 'utf8')).trim().split('\n').filter(Boolean).map(line => JSON.parse(line)) }
    catch (error) { status.reportError = error.message }
    const result = {...assess(job.tests, events, status.code), index, shard: job.shard, status, log: `${prefix}.tap`}
    if (status.reportError || status.error) result.ok = false
    results.push(result)
  }
  try {
    await Promise.all(Array.from({length: jobs}, async () => {
      while (!aborted && next < plan.length) { const index = next++; await run(plan[index], index) }
    }))
  } finally {
    if (active.size) stop()
    if (cancellation) await cancellation
    process.off('SIGINT', stop); process.off('SIGTERM', stop)
  }
  const changed = []
  for (const [file, hash] of Object.entries(hashes)) if (await digest(file) !== hash) changed.push(file)
  if (binaryHash && await digest(binary) !== binaryHash) changed.push(binary)
  const summary = {ok: !aborted && !changed.length && results.length === plan.length && results.every(r => r.ok), provenance, elapsedMs: performance.now() - started, aborted, changed, expected: inventory.length, passed: results.reduce((n, r) => n + r.passed, 0), failed: results.reduce((n, r) => n + r.failed, 0), skipped: results.reduce((n, r) => n + r.skipped, 0), todo: results.reduce((n, r) => n + r.todo, 0), results: results.sort((a, b) => a.index - b.index)}
  await writeFile(resolve(output, 'summary.json'), JSON.stringify(summary, null, 2) + '\n')
  console.log(JSON.stringify({output, ...Object.fromEntries(['ok', 'expected', 'passed', 'failed', 'skipped', 'todo', 'aborted', 'elapsedMs'].map(key => [key, summary[key]]))}))
  if (!summary.ok) process.exitCode = 1
}

if (process.argv[2] === '--internal-discover') await discover(process.argv.slice(3))
else await main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 1 })
