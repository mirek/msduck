import assert from 'node:assert/strict'
import {test} from 'node:test'
import {mkdtemp, writeFile, readFile, rm, access} from 'node:fs/promises'
import {tmpdir} from 'node:os'
import {join, resolve} from 'node:path'
import {execFile, spawn} from 'node:child_process'
import {promisify} from 'node:util'
import {partition, selection, assess, key} from '../scripts/lib/client-shards.mjs'
const exec = promisify(execFile)
const runner = resolve('scripts/run-client-shards.mjs')
const delay = ms => new Promise(resolve => setTimeout(resolve, ms))
async function fixture(t, sources) {
  const dir = await mkdtemp(join(tmpdir(), 'msduck-shards-'))
  t.after(() => rm(dir, {recursive: true, force: true}))
  const files = []
  for (const [name, source] of Object.entries(sources)) {
    const file = join(dir, name)
    await writeFile(file, `import {test} from 'node:test';\n${source}`)
    files.push(file)
  }
  return {dir, files, args: ['--jobs', '2', '--output', join(dir, 'out'), '--', ...files]}
}
async function summary(dir) { return JSON.parse(await readFile(join(dir, 'out/summary.json'), 'utf8')) }

test('partition covers identities once and selection handles metacharacters and newlines', () => {
  const names = ['a', 'a\n', 'a.*[x]$', 'a\\b', '😀']
  const tests = ['one', 'two'].flatMap(file => names.map(name => ({file, name})))
  const plan = partition(tests, 3)
  assert.deepEqual(plan, partition([...tests].reverse(), 3))
  assert.deepEqual(new Set(plan.flatMap(job => job.tests.map(key))), new Set(tests.map(key)))
  assert.equal(plan.flatMap(job => job.tests).length, tests.length)
  for (const sample of tests) {
    const pattern = new RegExp(selection([sample]))
    for (const name of [...names, 'ax', 'a\r', 'a\nextra']) assert.equal(pattern.test(name), name === sample.name)
  }
  assert.throws(() => partition([tests[0], tests[0]], 2), /Duplicate/)
  assert.throws(() => partition(tests, 0))
})

test('accounting rejects missing repeated unexpected tests and distinguishes skips', () => {
  const tests = [{file: 'f', name: 'a'}], event = {type: 'test:pass', file: 'f', name: 'a', nesting: 0}
  assert.equal(assess(tests, [event], 0).ok, true)
  assert.equal(assess(tests, [], 0).ok, false)
  assert.equal(assess(tests, [event, event], 0).ok, false)
  assert.equal(assess(tests, [event, {...event, name: 'extra'}], 0).ok, false)
  assert.equal(assess(tests, [event], 1).ok, false)
  const skipped = assess(tests, [{...event, skip: 'intentional'}], 0)
  assert.equal(skipped.passed, 0); assert.equal(skipped.skipped, 1)
})

test('runner preserves escaped duplicate cross-file names nested children and explicit skips', async t => {
  const marker = join(tmpdir(), `msduck-child-${process.pid}-${Date.now()}`)
  t.after(() => rm(marker, {force: true}))
  const {dir, args} = await fixture(t, {
    'a.mjs': `test('same',()=>{});test('regex.*[x]$',()=>{});test('nested',async t=>{await t.test('child',async()=>{await (await import('node:fs/promises')).writeFile(${JSON.stringify(marker)},'ran')})});test('skip',{skip:true},()=>{throw Error('must not run')});`,
    'b.mjs': "test('same',()=>{});test('line\\n',()=>{});",
  })
  await exec(process.execPath, [runner, ...args])
  const result = await summary(dir)
  assert.equal(result.ok, true); assert.equal(result.expected, 6)
  assert.equal(result.passed, 5); assert.equal(result.skipped, 1)
  assert.equal(await readFile(marker, 'utf8'), 'ran')
})

test('discovery never invokes test callbacks and unsupported APIs fail closed', async t => {
  const {dir, args} = await fixture(t, {'a.mjs': "test('never',()=>{throw Error('body executed')})"})
  await exec(process.execPath, [runner, '--plan-only', ...args])
  const plan = JSON.parse(await readFile(join(dir, 'out/plan.json'), 'utf8'))
  assert.equal(plan.plan.flatMap(job => job.tests).length, 1)
  const unsupported = await fixture(t, {'b.mjs': "test.only('unsupported',()=>{})"})
  await assert.rejects(exec(process.execPath, [runner, '--plan-only', ...unsupported.args]), /Discovery failed/)
})

test('assertion failures and worker exits cannot produce successful summaries', async t => {
  for (const body of ["throw Error('retained assertion')", 'process.exit(23)']) {
    const {dir, args} = await fixture(t, {'a.mjs': `test('bad',()=>{${body}});test('good',()=>{});`})
    await assert.rejects(exec(process.execPath, [runner, ...args]))
    const result = await summary(dir)
    assert.equal(result.ok, false)
    assert.ok(result.results.some(job => !job.ok))
    const logs = await Promise.all(result.results.map(job => readFile(job.log, 'utf8')))
    assert.ok(logs.some(log => log.includes(body.includes('retained') ? 'retained assertion' : '23')))
  }
})

test('cancellation terminates shard processes and records unfinished work', {timeout: 15000}, async t => {
  const {dir, files} = await fixture(t, {'a.mjs': ''})
  const marker = join(dir, 'ready')
  await writeFile(files[0], `import {test} from 'node:test';import {writeFileSync} from 'node:fs';test('wait',async()=>{writeFileSync(${JSON.stringify(marker)},'ready');setInterval(()=>{},1000);await new Promise(()=>{});});`)
  const child = spawn(process.execPath, [runner, '--output', join(dir, 'out'), '--', ...files], {stdio: ['ignore', 'pipe', 'pipe']})
  t.after(() => child.kill('SIGKILL'))
  const closed = new Promise(resolve => child.once('close', (code, signal) => resolve({code, signal})))
  let ready = false
  for (let i = 0; i < 200 && !ready; i++) { try { await access(marker); ready = true } catch { await delay(20) } }
  assert.ok(ready)
  child.kill('SIGTERM')
  const result = await closed
  assert.notEqual(result.code, 0)
  const report = await summary(dir)
  assert.equal(report.ok, false); assert.equal(report.aborted, true)
})


test('cancellation during module discovery cannot leave its process running', {timeout: 10000}, async t => {
  const {dir, files} = await fixture(t, {'a.mjs': ''})
  const marker = join(dir, 'discovery-ready')
  await writeFile(files[0], `import {writeFileSync} from 'node:fs';writeFileSync(${JSON.stringify(marker)},String(process.pid));setInterval(()=>{},1000);await new Promise(()=>{});`)
  const child = spawn(process.execPath, [runner, '--plan-only', '--output', join(dir, 'out'), '--', ...files], {stdio: ['ignore', 'pipe', 'pipe']})
  t.after(() => child.kill('SIGKILL'))
  const closed = new Promise(resolve => child.once('close', code => resolve(code)))
  let pid
  for (let i = 0; i < 200 && !pid; i++) { try { pid = Number(await readFile(marker, 'utf8')) } catch { await delay(20) } }
  assert.ok(pid)
  child.kill('SIGTERM')
  assert.notEqual(await closed, 0)
  assert.match(await readFile(join(dir, 'out/discovery-0/discovery.stderr'), 'utf8'), /cancelled/)
  assert.throws(() => process.kill(pid, 0), {code: 'ESRCH'})
})


test('late asynchronous registration fails discovery instead of omitting a test', async t => {
  const {dir, args} = await fixture(t, {'a.mjs': "test('now',()=>{});setTimeout(()=>test('later',()=>{}),20);"})
  await assert.rejects(exec(process.execPath, [runner, '--plan-only', ...args]), /Discovery failed/)
  assert.match(await readFile(join(dir, 'out/discovery-0/discovery.stderr'), 'utf8'), /Late test registration/)
})
