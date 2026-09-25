#!/usr/bin/env node
import assert from 'node:assert/strict'
import {createHash} from 'node:crypto'
import {execFileSync, spawn} from 'node:child_process'
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs'
import {dirname, resolve} from 'node:path'

const options = Object.fromEntries(process.argv.slice(2).reduce((pairs, value, index, args) => {
  if (index % 2 === 0) {
    assert(value.startsWith('--') && args[index + 1], 'Expected --option value pairs')
    pairs.push([value.slice(2), args[index + 1]])
  }
  return pairs
}, []))
assert(Object.keys(options).every(key => ['tree', 'cpus', 'samples', 'output', 'revision'].includes(key)), 'Unknown option')
assert(/^\d+,\d+$/.test(options.cpus ?? '') && new Set(options.cpus.split(',')).size === 2,
  'Use --cpus with two distinct allowed Linux CPU IDs, e.g. --cpus 4,5')
const samples = Number(options.samples ?? 20)
assert(Number.isSafeInteger(samples) && samples >= 5 && samples <= 200, 'Use 5–200 samples')
const tree = resolve(options.tree ?? '.')
const harness = readFileSync(resolve(tree, 'tests/all_objects.rs'))
const loader = readFileSync(resolve(tree, 'src/object_catalog/system_objects.rs'))
const revision = options.revision ?? execFileSync('git', ['rev-parse', 'HEAD'], {cwd: tree, encoding: 'utf8'}).trim()
assert(/^[a-f0-9]{40}$/.test(revision), 'Expected a full source revision')
const dirty = options.revision ? null
  : execFileSync('git', ['status', '--porcelain'], {cwd: tree, encoding: 'utf8'}).trim().length > 0
const command = ['-c', options.cpus, 'cargo', 'test', '--locked', '--test', 'all_objects',
  'benchmark_builtin_catalog_startup', '--', '--ignored', '--exact', '--nocapture', '--test-threads=1']
const child = spawn('taskset', command, {
  cwd: tree,
  env: {...process.env, CARGO_BUILD_JOBS: '2', MSDUCK_CATALOG_BENCH_SAMPLES: String(samples)},
  stdio: ['ignore', 'pipe', 'pipe'],
})
let stdout = ''
child.stdout.on('data', chunk => { stdout += chunk; process.stdout.write(chunk) })
child.stderr.on('data', chunk => { process.stderr.write(chunk) })
await new Promise((fulfill, reject) => {
  child.on('error', reject)
  child.on('close', code => code === 0 ? fulfill() : reject(Error(`Benchmark exited ${code}`)))
})
const match = stdout.match(/catalog_startup_benchmark=(\{[^\n]+\})/)
assert(match, 'Benchmark output missing')
const report = {
  revision,
  dirty,
  harness_sha256: createHash('sha256').update(harness).digest('hex'),
  loader_sha256: createHash('sha256').update(loader).digest('hex'),
  cpus: options.cpus,
  ...JSON.parse(match[1]),
}
const serialized = JSON.stringify(report, null, 2) + '\n'
if (options.output) {
  const output = resolve(options.output)
  mkdirSync(dirname(output), {recursive: true})
  writeFileSync(output, serialized)
}
process.stdout.write(serialized)
