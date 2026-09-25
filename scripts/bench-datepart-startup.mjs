#!/usr/bin/env node
// Run the ignored Rust startup probe under a fixed two-core Linux affinity.
import { spawnSync } from 'node:child_process'

const cpus = process.argv[2]
if (!/^\d+,\d+$/.test(cpus ?? '') || cpus.split(',')[0] === cpus.split(',')[1]) {
  console.error('Usage: node scripts/bench-datepart-startup.mjs CPU,CPU')
  process.exit(2)
}
const result = spawnSync('taskset', ['-c', cpus, 'cargo', 'test', '--lib',
  'datepart::tests::startup_benchmark', '--locked', '--', '--ignored', '--nocapture'], {
  encoding: 'utf8',
  env: {...process.env, MSDUCK_DATEPART_BENCH: '1'},
  maxBuffer: 4 * 1024 * 1024,
})
if (result.error || result.status !== 0) {
  process.stderr.write(result.stderr ?? '')
  process.stderr.write(result.stdout ?? '')
  console.error(result.error?.message ?? `benchmark exited ${result.status}`)
  process.exit(1)
}
const lines = result.stdout.split('\n')
const samples = Object.fromEntries(['DATEPART', 'SERVER'].map(name => {
  const values = lines.flatMap(line => {
    const match = line.match(new RegExp(`MSDUCK_${name}_MS (\\d+\\.\\d+)`))
    return match ? [Number(match[1])] : []
  })
  if (values.length !== 20) throw new Error(`Expected 20 ${name} samples; received ${values.length}`)
  const sorted = [...values].sort((a, b) => a - b)
  return [name.toLowerCase(), {
    values_ms: values,
    p50_ms: (sorted[9] + sorted[10]) / 2,
    p95_ms: sorted[18],
  }]
}))
console.log(JSON.stringify({revision: process.env.MSDUCK_BENCH_REVISION ?? 'working-tree', cpus, samples}, null, 2))
