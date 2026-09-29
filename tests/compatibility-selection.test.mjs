import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

const script = 'scripts/compatibility.mjs'
const run = (...args) => spawnSync(process.execPath, [script, ...args], { encoding: 'utf8' })

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
