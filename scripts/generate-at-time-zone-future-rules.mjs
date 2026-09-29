// Derive a compact future rule extension from pinned SQL Server evidence.
import { createHash } from 'node:crypto'
import { readFile, writeFile } from 'node:fs/promises'

const source = new URL('../reference/at-time-zone-history-2051-2100.json', import.meta.url)
const previous = new URL('../src/at_time_zone_rules_1900_2050.json', import.meta.url)
const output = new URL('../src/at_time_zone_rules_2051_2100.json', import.meta.url)
const pinned = 'e7f7798940c1664048626907a4e9b52357053ae9c72e41582e29c58a1d2d3f6c'
const priorSource = '2ecb9841cb43d0c0251f99e7e3ebd70c9f259473d12232fa6f6fb24e3dc226b9'
const start = 646917408000000000n // 2051-01-01 UTC
const end = 662695776000000000n // 2101-01-01 UTC, exclusive
const fixture = JSON.parse(await readFile(source, 'utf8'))
const prior = JSON.parse(await readFile(previous, 'utf8'))
const digest = createHash('sha256').update(JSON.stringify(fixture)).digest('hex')
if (digest !== pinned || fixture.image !== prior.image || prior.sourceSha256 !== priorSource || prior.utcEndExclusive !== start.toString()) throw new Error('future or previous rule evidence differs from pinned captures')

function toTicks(value) {
  if (value?.kind !== 'date' || value.nanosecondsDelta) throw new Error('expected minute-aligned UTC transition')
  const ms = Date.parse(value.value)
  if (!Number.isSafeInteger(ms) || ms % 60000 !== 0) throw new Error('invalid UTC transition')
  return (BigInt(ms) + 62135596800000n) * 10000n
}

const baseline = fixture.baseline.reference.sets[0].rows
if (baseline.length !== 141 || prior.zones.length !== 141 || fixture.chunks.length !== 5) throw new Error('incomplete future zone coverage')
const histories = new Map(baseline.map(([name]) => [name, []]))
if (histories.size !== 141) throw new Error('duplicate future zone name')
for (const chunk of fixture.chunks) {
  for (const refinement of chunk.refinements) {
    for (const [name, , , , instant, before, after] of refinement.minute.reference.sets[0].rows) {
      const history = histories.get(name)
      if (!history) throw new Error(`unknown zone ${name}`)
      history.push([toTicks(instant), before, after])
    }
  }
}
const zones = baseline.map(([name, initial], index) => {
  const priorZone = prior.zones[index]
  const terminal = priorZone.transitions.at(-1)?.[2] ?? priorZone.initial
  if (priorZone.name !== name || initial !== terminal) throw new Error(`2051 offset differs from previous rules for ${name}`)
  const entries = histories.get(name).sort((a, b) => a[0] < b[0] ? -1 : 1)
  let current = initial
  let previousTick = start
  const transitions = entries.map(([tick, before, after]) => {
    if (tick <= previousTick || tick >= end || before !== current || before === after || ![before, after].every(Number.isInteger) || Math.abs(before) > 840 || Math.abs(after) > 840) throw new Error(`invalid future transition history for ${name}`)
    current = after
    previousTick = tick
    return [tick.toString(), before, after]
  })
  return { name, initial, transitions }
})
if (zones.reduce((sum, zone) => sum + zone.transitions.length, 0) !== 4000) throw new Error('unexpected future transition count')
const rules = { version: 1, image: fixture.image, sourceSha256: digest, priorSourceSha256: priorSource, utcStart: start.toString(), utcEndExclusive: end.toString(), zones }
const serialized = `${JSON.stringify(rules)}\n`
if (process.argv[2] === '--check') {
  if (await readFile(output, 'utf8') !== serialized) throw new Error('future rules differ; regenerate explicitly')
  console.log('checked 141 zones and 4000 future transitions')
} else if (process.argv.length === 2) {
  await writeFile(output, serialized)
  console.log('wrote 141 zones and 4000 future transitions')
} else throw new Error('usage: node scripts/generate-at-time-zone-future-rules.mjs [--check]')
