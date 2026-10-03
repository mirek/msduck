import assert from 'node:assert/strict'
import { spawn, spawnSync } from 'node:child_process'
import test from 'node:test'

const script = 'scripts/compatibility.mjs'
const run = (...args) => spawnSync(process.execPath, [script, ...args], { encoding: 'utf8' })
const runDocker = (...args) => spawnSync(process.execPath, ['scripts/docker-compare.mjs', ...args], { encoding: 'utf8' })

test('focused audit lists exact names and plans a separate artifact without a server', () => {
  const listed = run('--list-cases')
  assert.equal(listed.status, 0, listed.stderr)
  const names = listed.stdout.trim().split('\n')
  assert.ok(names.includes('numeric literal descriptors'))
  assert.ok(names.includes('logical variable declaration defaults'))

  const full = run('--plan')
  assert.equal(full.status, 0, full.stderr)
  const fullPlan = JSON.parse(full.stdout)
  assert.equal(fullPlan.output, 'artifacts/compatibility/local.json')
  assert.equal(fullPlan.selectedCases.length, names.length)

  const selected = run('--case', 'logical variable declaration defaults', '--case', 'numeric literal descriptors', '--plan')
  assert.equal(selected.status, 0, selected.stderr)
  const plan = JSON.parse(selected.stdout)
  assert.deepEqual(plan.selectedCases, ['logical variable declaration defaults', 'numeric literal descriptors'])
  assert.equal(plan.totalCases, names.length)
  assert.match(plan.output, /^artifacts\/compatibility\/selected-local-[0-9a-f]{12}\.json$/)

  const reference = run('--compare', '--case', 'numeric literal descriptors', '--plan')
  assert.equal(reference.status, 0, reference.stderr)
  assert.deepEqual(JSON.parse(reference.stdout).selectedCases, ['numeric literal descriptors'])
  assert.match(JSON.parse(reference.stdout).output, /^artifacts\/compatibility\/selected-comparison-[0-9a-f]{12}\.json$/)
})

test('invalid case selections fail before server configuration or connection', () => {
  for (const args of [
    ['--compare', '--case', 'unknown case', '--plan'],
    ['--case', 'numeric literal descriptors', '--case', 'numeric literal descriptors', '--plan'],
    ['--case'],
    ['--list-cases', '--case', 'numeric literal descriptors']
  ]) {
    const result = run(...args)
    assert.notEqual(result.status, 0, args.join(' '))
    assert.equal(result.stdout, '')
  }
})

test('Docker wrapper lists, plans and rejects unknown cases before starting Docker', () => {
  const listed = runDocker('--list-cases')
  assert.equal(listed.status, 0, listed.stderr)
  assert.ok(listed.stdout.includes('numeric literal descriptors'))

  const planned = runDocker('--case', 'numeric literal descriptors', '--plan')
  assert.equal(planned.status, 0, planned.stderr)
  assert.equal(JSON.parse(planned.stdout).mode, 'comparison')

  const unknown = runDocker('--case', 'unknown case')
  assert.notEqual(unknown.status, 0)
  assert.match(unknown.stderr, /Unknown case: unknown case/)
})


// Add bounded bytes to the same real stdout pipe just before the CLI's first
// write. Delay consumption from that signal so the test establishes actual
// backpressure independently of module load or machine scheduling speed.
const pressureBytes = 2 * 1024 * 1024
const pressureImport = 'data:text/javascript,' + encodeURIComponent(`
  import { writeSync } from 'node:fs';
  const write = process.stdout.write;
  let first = true;
  process.stdout.write = function (...args) {
    if (first) {
      first = false;
      write.call(this, Buffer.alloc(${pressureBytes}, 0x78));
      writeSync(3, 'ready');
    }
    return write.apply(this, args);
  };
`)
const delayedOutput = (path, args) => new Promise((resolve, reject) => {
  const child = spawn(process.execPath, ['--import', pressureImport, path, ...args], {
    stdio: ['ignore', 'pipe', 'pipe', 'pipe']
  })
  const chunks = [], errors = []
  child.stdout.on('data', chunk => chunks.push(chunk))
  child.stdout.pause()
  child.stderr.on('data', chunk => errors.push(chunk))
  let resumeTimer
  child.stdio[3].once('data', () => {
    resumeTimer = setTimeout(() => child.stdout.resume(), 100)
  })
  const timeout = setTimeout(() => {
    child.kill('SIGKILL')
    child.stdout.resume()
    reject(new Error('CLI did not complete under bounded output backpressure'))
  }, 10000)
  child.once('error', reject)
  child.once('close', (status, signal) => {
    clearTimeout(timeout)
    clearTimeout(resumeTimer)
    resolve({status, signal, stdout: Buffer.concat(chunks), stderr: Buffer.concat(errors).toString()})
  })
})
for (const path of [script, 'scripts/docker-compare.mjs']) {
  for (const mode of ['--list-cases', '--plan']) {
    test(`${path} ${mode} preserves complete output with a delayed pipe reader`, async () => {
      const expected = spawnSync(process.execPath, [path, mode], {encoding: 'utf8'})
      assert.equal(expected.status, 0, expected.stderr)
      if (mode === '--list-cases') assert.ok(expected.stdout.endsWith('numeric literal descriptors\n'))
      else assert.ok(JSON.parse(expected.stdout).selectedCases.includes('numeric literal descriptors'))
      const actual = await delayedOutput(path, [mode])
      assert.equal(actual.status, 0, actual.stderr)
      assert.equal(actual.signal, null)
      assert.equal(actual.stdout.length, pressureBytes + Buffer.byteLength(expected.stdout), 'complete prefix and CLI output byte count')
      assert.ok(actual.stdout.subarray(0, pressureBytes).equals(Buffer.alloc(pressureBytes, 0x78)), 'unchanged backpressure prefix')
      assert.equal(actual.stdout.subarray(pressureBytes).toString(), expected.stdout)
    })
  }
}
