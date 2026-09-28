// Derive a small runtime table from the pinned, raw SQL Server capture.
import { createHash } from 'node:crypto'
import { readFile, writeFile } from 'node:fs/promises'
import { isDeepStrictEqual } from 'node:util'

const source = new URL('../reference/at-time-zone-history-1900-2050.json', import.meta.url)
const output = new URL('../src/at_time_zone_rules_1900_2050.json', import.meta.url)
const pinned = '2ecb9841cb43d0c0251f99e7e3ebd70c9f259473d12232fa6f6fb24e3dc226b9'
const fixture = JSON.parse(await readFile(source, 'utf8'))
const digest = createHash('sha256').update(JSON.stringify(fixture)).digest('hex')
if (digest !== pinned) throw new Error('historical rule source differs from pinned SQL Server capture')
const toTicks = value => {
  if (value?.kind !== 'date') throw new Error('expected captured UTC date')
  const ms = Date.parse(value.value)
  if (!Number.isSafeInteger(ms) || ms % 60000 !== 0 || value.nanosecondsDelta) throw new Error('transition is not minute-aligned')
  return ((BigInt(ms) + 62135596800000n) * 10000n).toString()
}
const baseline = fixture.baseline.reference.sets[0].rows
const names = baseline.map(row => row[0])
if (new Set(names).size !== 141) throw new Error('unexpected zone catalog')
const histories = new Map(baseline.map(([name]) => [name, []]))
for (const chunk of fixture.chunks) {
  for (const refinement of chunk.refinements) {
    for (const [name, , , , instant, before, after] of refinement.minute.reference.sets[0].rows) {
      if (!histories.has(name)) throw new Error(`unknown zone ${name}`)
      histories.get(name).push([toTicks(instant), before, after])
    }
  }
}
const zones = baseline.map(([name, initial]) => {
  if (!Number.isInteger(initial) || Math.abs(initial) > 840) throw new Error(`invalid baseline for ${name}`)
  const transitions = histories.get(name).sort((a, b) => BigInt(a[0]) < BigInt(b[0]) ? -1 : 1)
  let previous = initial
  let previousTick = 0n
  for (const [tickText, before, after] of transitions) {
    const tick = BigInt(tickText)
    if (tick <= previousTick || before !== previous || before === after || ![before, after].every(Number.isInteger) || Math.abs(before) > 840 || Math.abs(after) > 840) throw new Error(`invalid transition history for ${name}`)
    previousTick = tick
    previous = after
  }
  return { name, initial, transitions }
})
if (zones.reduce((sum, zone) => sum + zone.transitions.length, 0) !== 20414) throw new Error('unexpected transition count')
const rules = { version: 1, image: fixture.image, sourceSha256: digest, utcStart: '599266080000000000', utcEndExclusive: '646917408000000000', zones }
const serialized = `${JSON.stringify(rules)}\n`
if (process.argv[2] === '--check') {
  if (!isDeepStrictEqual(await readFile(output, 'utf8'), serialized)) throw new Error('derived rules differ; regenerate explicitly')
  console.log(`checked ${zones.length} zones and 20414 transitions`)
} else if (process.argv.length === 2) {
  await writeFile(output, serialized)
  console.log(`wrote ${zones.length} zones and 20414 transitions`)
} else throw new Error('usage: node scripts/generate-at-time-zone-rules.mjs [--check]')
