import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile} from 'node:fs/promises'
import {command} from '../scripts/run-client-tests.mjs'
import {npmFiles, ciExtras, suiteFiles, clientJobs, strictResult} from '../scripts/lib/client-suite.mjs'

test('npm adapter retains a single serial invocation by default and passes opt-in workers as data', () => {
  for (const value of [undefined, '1']) {
    const c = command({MSDUCK_CLIENT_JOBS: value, NODE_TEST_CONTEXT: 'child-v8', KEEP: 'yes'})
    assert(c.args.includes('--serial'))
    assert.equal(c.env.NODE_TEST_CONTEXT, undefined)
    assert.equal(c.env.KEEP, 'yes')
  }
  const c = command({MSDUCK_CLIENT_JOBS: '4'})
  assert(!c.args.includes('--serial'))
  assert.deepEqual(c.args.slice(1), ['--suite', 'npm', '--jobs', '4'])
  for (const value of ['', '0', '17', '1.5', '01', ' 4', '4 ', '2; touch exploit', '$(id)', '-1']) assert.throws(() => command({MSDUCK_CLIENT_JOBS: value}), /integer/)
  assert.equal(clientJobs('16'), 16)
  assert.throws(() => command({}, ['--jobs', '4']), /configuration/)
})

test('one manifest retains six npm entry points and every additional CI test', async () => {
  assert.equal(npmFiles.length, 6)
  assert(npmFiles.includes('tests/reference-prepared.test.mjs'))
  assert.deepEqual(await suiteFiles(), [...npmFiles])
  const ci = await suiteFiles('ci')
  for (const file of [...npmFiles, ...ciExtras]) assert(ci.includes(file))
  assert(ci.some(x => x.startsWith('tests/compat/')))
  assert.equal(new Set(ci).size, ci.length)
  await assert.rejects(suiteFiles('bad'), /suite/)
  const pkg = JSON.parse(await readFile('package.json'))
  assert.equal(pkg.scripts.test, 'cargo build --workspace --all-targets && node scripts/run-client-tests.mjs')
  const workflow = await readFile('.github/workflows/ci.yml', 'utf8')
  assert(workflow.includes('--suite ci --jobs 1 --serial'))
  assert(workflow.includes('tests/client-test-command.test.mjs'))
})

test('strict accounting rejects assigned skips TODO cancellations and nested failures', () => {
  const passed = {ok: true, expected: 1, passed: 1, failed: 0, skipped: 0, todo: 0, problems: []}
  assert(strictResult(passed, [{type: 'test:pass', nesting: 0}]).ok)
  for (const events of [[{type:'test:fail',nesting:1}], [{type:'test:pass',skip:true}], [{type:'test:pass',todo:true}]]) assert(!strictResult(passed, events).ok)
  for (const overrides of [{passed:0, skipped:1}, {passed:0, todo:1}, {passed:0}, {ok:false, problems:['Missing test']}]) assert(!strictResult({...passed,...overrides}, []).ok)
})

// Private harness snapshot, with a labelled inert executable marker. This never
// substitutes for the actual server/build in full-suite benchmark evidence.
import {mkdtemp, mkdir, writeFile, cp, rm} from 'node:fs/promises'
import {join, resolve} from 'node:path'
import {execFile} from 'node:child_process'
import {promisify} from 'node:util'
const exec = promisify(execFile)
async function snapshot(t, body = "test('pass',()=>{})") {
  await mkdir('.tmp', {recursive:true})
  const dir = await mkdtemp(resolve('.tmp/client-command-'))
  t.after(() => rm(dir, {recursive:true,force:true}))
  for (const sub of ['scripts/lib','tests','src','crates','vendor','reference','target/debug']) await mkdir(join(dir,sub),{recursive:true})
  for (const file of ['scripts/run-client-tests.mjs','scripts/run-client-shards.mjs','scripts/lib/client-suite.mjs','scripts/lib/client-shards.mjs']) await cp(file,join(dir,file))
  for (const file of ['Cargo.toml','Cargo.lock','package.json','package-lock.json']) await writeFile(join(dir,file),'{}')
  await writeFile(join(dir,'target/debug/msduck'),'INERT HARNESS MARKER, NOT A SERVER')
  for (const [i,file] of npmFiles.entries()) await writeFile(join(dir,file),`import {test} from 'node:test';\n${i === 0 ? body : "test('pass',()=>{})"}\n`)
  return dir
}
async function reports(dir) {
  const {readdir} = await import('node:fs/promises')
  const base = join(dir,'artifacts/client-shards')
  const directories = await readdir(base)
  return Promise.all(directories.map(x => readFile(join(base,x,'summary.json'),'utf8').then(JSON.parse)))
}

test('actual adapter clears inherited worker context and full serial/parallel accounting covers every manifest identity', async t => {
  const dir = await snapshot(t, "test('parent',async t=>{await t.test('nested',()=>{})})")
  for (const jobs of [undefined,'4']) {
    const env = {...process.env,NODE_TEST_CONTEXT:'child-v8'}
    delete env.MSDUCK_CLIENT_JOBS
    if (jobs) env.MSDUCK_CLIENT_JOBS = jobs
    await exec(process.execPath,['scripts/run-client-tests.mjs'],{cwd:dir,env})
  }
  const results = await reports(dir)
  assert.equal(results.length,2)
  for (const r of results) {
    assert.equal(r.ok,true); assert.equal(r.expected,6); assert.equal(r.passed,6)
    assert.equal(r.skipped,0); assert.equal(r.todo,0)
    assert.equal(r.changed.length,0)
  }
  assert.deepEqual(results.map(r=>r.results.length).sort(),[1,6])
})

test('actual full command rejects assigned skip TODO cancellation and changed transitive source', async t => {
  for (const body of [
    "test('skip',{skip:true},()=>{})",
    "test('todo',{todo:true},()=>{})",
    "test('cancel',{timeout:50},async()=>{await new Promise(()=>{})})",
    "test('mutation',async()=>{await(await import('node:fs/promises')).writeFile('src/added.rs','changed')})",
  ]) {
    const dir = await snapshot(t,body)
    await assert.rejects(exec(process.execPath,['scripts/run-client-tests.mjs'],{cwd:dir,env:{...process.env,MSDUCK_CLIENT_JOBS:'1'}}))
    const [report] = await reports(dir)
    assert.equal(report.ok,false)
    if (body.includes('mutation')) assert(report.changed.includes('source file inventory'))
  }
})
