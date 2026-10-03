import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile} from 'node:fs/promises'
import {command, runCommand, windowsTreeCommand} from '../scripts/run-client-tests.mjs'
import {npmFiles, ciExtras, diagnosticFiles, suiteFiles, clientJobs, strictResult} from '../scripts/lib/client-suite.mjs'

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
  assert.deepEqual(await suiteFiles('ci-replays'), [...diagnosticFiles])
  assert(diagnosticFiles.includes('tests/aggregate_diagnostics.test.mjs'))
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

test('separately labelled CI diagnostics retain intentional skips without a full-suite pass claim', async t => {
  const dir = await snapshot(t)
  await writeFile(join(dir,diagnosticFiles[0]), "import {test} from 'node:test';test('opt-in boundary',{skip:true},()=>{});\n")
  await writeFile(join(dir,diagnosticFiles[1]), "import {test} from 'node:test';test('diagnostic',()=>{});\n")
  await exec(process.execPath,['scripts/run-client-shards.mjs','--suite','ci-replays','--serial'],{cwd:dir})
  const [report] = await reports(dir)
  assert.equal(report.ok,true)
  assert.equal(report.provenance.strictFullSuite,false)
  assert.equal(report.provenance.suite,'ci-replays')
  assert.equal(report.expected,2); assert.equal(report.passed,1); assert.equal(report.skipped,1)
})


import {EventEmitter} from 'node:events'
// A killed orphan can remain defunct until container PID 1 reaps it.
// PID existence alone must not count that non-executing state as a live worker.
async function running(pid, {platform = process.platform, probe = process.kill,
  readStat = pid => readFile(`/proc/${pid}/stat`, 'utf8')} = {}) {
  try { probe(pid, 0) } catch (error) { if (error.code === 'ESRCH') return false; throw error }
  if (platform === 'linux') {
    try {
      const stat = await readStat(pid)
      const state = stat.slice(stat.lastIndexOf(')') + 2).split(' ')[0]
      return state !== 'Z' && state !== 'X'
    } catch (error) { if (error.code === 'ENOENT') return false; throw error }
  }
  return true
}
test('Windows default and one-job commands retain the direct six-file serial invocation', () => {
  for (const value of [undefined, '1']) {
    const selected = command({MSDUCK_CLIENT_JOBS:value,NODE_TEST_CONTEXT:'child-v8'}, [], 'win32')
    assert.deepEqual(selected.args,['--test',...npmFiles])
    assert.equal(selected.env.NODE_TEST_CONTEXT,undefined)
  }
  assert.throws(()=>command({MSDUCK_CLIENT_JOBS:'4'}, [], 'win32'),/requires POSIX/)
  for (const value of [undefined,'1','4','16']) {
    assert.deepEqual(command({MSDUCK_CLIENT_JOBS:value}, [], 'linux').args,
      command({MSDUCK_CLIENT_JOBS:value}, [], 'darwin').args)
  }
})

test('actual portable serial launch propagates assertion failure and removes signal handlers', async t => {
  const dir = await snapshot(t, "test('failure',()=>{throw Error('portable retained failure')})")
  const selected = command({...process.env,MSDUCK_CLIENT_JOBS:'1',NODE_TEST_CONTEXT:'child-v8'}, [], 'win32')
  selected.args = selected.args.map(x=>npmFiles.includes(x)?join(dir,x):x)
  const signals = new EventEmitter()
  assert.notEqual(await runCommand(selected,{signals,stdio:'ignore'}),0)
  assert.equal(signals.listenerCount('SIGINT'),0)
  assert.equal(signals.listenerCount('SIGTERM'),0)
})

test('actual portable serial launch forwards cancellation and terminates its test worker', {timeout:10000}, async t => {
  const dir = await snapshot(t)
  const marker = join(dir,'ready')
  const descendantMarker = join(dir,'descendant-ready')
  await writeFile(join(dir,npmFiles[0]), `import {test} from 'node:test';import {writeFileSync} from 'node:fs';import {spawn} from 'node:child_process';test('wait',async()=>{spawn(process.execPath,['-e',${JSON.stringify("require('node:fs').writeFileSync("+JSON.stringify(descendantMarker)+",String(process.pid));process.on('SIGTERM',()=>{});setInterval(()=>{},1000)")}],{stdio:'ignore'});writeFileSync(${JSON.stringify(marker)},String(process.pid));process.on('SIGTERM',()=>{});setInterval(()=>{},1000);await new Promise(()=>{})});`)
  const selected = command({...process.env,MSDUCK_CLIENT_JOBS:'1',NODE_TEST_CONTEXT:'child-v8'}, [], 'win32')
  selected.args = selected.args.map(x=>npmFiles.includes(x)?join(dir,x):x)
  const signals = new EventEmitter()
  const terminal = runCommand(selected,{signals,stdio:'ignore'})
  let pid, descendant
  t.after(()=>{for(const candidate of [pid,descendant]){if(candidate){try{process.kill(candidate,'SIGKILL')}catch{}}}})
  for(let i=0;i<250&&!pid;i++) {
    try {pid=Number(await readFile(marker,'utf8'))}catch{await new Promise(r=>setTimeout(r,20))}
  }
  assert(pid)
  for(let i=0;i<250&&!descendant;i++) {
    try {descendant=Number(await readFile(descendantMarker,'utf8'))}catch{await new Promise(r=>setTimeout(r,20))}
  }
  assert(descendant)
  signals.emit('SIGTERM')
  assert.notEqual(await terminal,0)
  for(let i=0;i<50;i++) {
    if (!await running(pid)) {pid=undefined;break}
    await new Promise(r=>setTimeout(r,20))
  }
  assert.equal(pid,undefined,'cancelled serial test worker is terminal')
  for(let i=0;i<50;i++) {
    if (!await running(descendant)) {descendant=undefined;break}
    await new Promise(r=>setTimeout(r,20))
  }
  assert.equal(descendant,undefined,'cancelled worker descendant is terminal')
  assert.equal(signals.listenerCount('SIGTERM'),0)
})


test('Windows cancellation targets the full PID tree with bounded PID arguments', () => {
  assert.deepEqual(windowsTreeCommand(1234, 'C:\\Windows'), {file:'C:\\Windows\\System32\\taskkill.exe',args:['/PID','1234','/T','/F']})
  for (const pid of [undefined, 0, -1, '1234', 1.5]) assert.throws(()=>windowsTreeCommand(pid), /PID/)
})


test('terminal worker checks distinguish Linux zombies from executing processes', async () => {
  const probe = () => true
  for (const state of ['Z', 'X']) {
    assert.equal(await running(123, {platform:'linux', probe,
      readStat:async()=>`123 (worker name ) with spaces) ${state} 1 2 3`}), false)
  }
  for (const state of ['S','R','D','T']) {
    assert.equal(await running(123, {platform:'linux', probe,
      readStat:async()=>`123 (worker) ${state} 1 2 3`}), true)
  }
  assert.equal(await running(123, {platform:'linux',probe,
    readStat:async()=>{throw Object.assign(Error('gone'),{code:'ENOENT'})}}), false)
  assert.equal(await running(123, {probe:()=>{throw Object.assign(Error('gone'),{code:'ESRCH'})}}), false)
  await assert.rejects(running(123, {probe:()=>{throw Object.assign(Error('denied'),{code:'EPERM'})}}), /denied/)
  assert.equal(await running(123, {platform:'win32',probe,readStat:()=>{throw Error('not Linux')}}), true)
})
