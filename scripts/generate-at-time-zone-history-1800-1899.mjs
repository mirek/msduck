// Derive a compact historical rule prefix from pinned SQL Server evidence.
import { createHash } from 'node:crypto'
import { readFile, writeFile } from 'node:fs/promises'

const source = new URL('../reference/at-time-zone-history-1800-1899.json', import.meta.url)
const following = new URL('../src/at_time_zone_rules_1900_2050.json', import.meta.url)
const output = new URL('../src/at_time_zone_rules_1800_1899.json', import.meta.url)
const pinned = 'e6683618f8d80f58dd48edfb7042dbe4296402b1f68e5d0a626ec7c4232ff3d1'
const followingSource = '2ecb9841cb43d0c0251f99e7e3ebd70c9f259473d12232fa6f6fb24e3dc226b9'
const start = 567709344000000000n // 1800-01-01 UTC
const end = 599266080000000000n // 1900-01-01 UTC, exclusive
const fixture = JSON.parse(await readFile(source, 'utf8'))
const next = JSON.parse(await readFile(following, 'utf8'))
const digest = createHash('sha256').update(JSON.stringify(fixture)).digest('hex')
if (digest !== pinned || fixture.image !== next.image || next.sourceSha256 !== followingSource || next.utcStart !== end.toString()) throw new Error('historical or following rule evidence differs from pinned captures')

function toTicks(value) {
  if (value?.kind !== 'date' || value.nanosecondsDelta) throw new Error('expected minute-aligned UTC transition')
  const ms = Date.parse(value.value)
  if (!Number.isSafeInteger(ms) || ms % 60000 !== 0) throw new Error('invalid UTC transition')
  return (BigInt(ms) + 62135596800000n) * 10000n
}

const baseline = fixture.baseline.reference.sets[0].rows
if (baseline.length !== 141 || next.zones.length !== 141 || fixture.chunks.length !== 10) throw new Error('incomplete historical zone coverage')
const histories = new Map(baseline.map(([name]) => [name, []]))
if (histories.size !== 141) throw new Error('duplicate historical zone name')
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
  const nextZone = next.zones[index]
  if (nextZone.name !== name) throw new Error(`historical name differs from following rules for ${name}`)
  const entries = histories.get(name).sort((a, b) => a[0] < b[0] ? -1 : 1)
  let current = initial
  let previousTick = start
  const transitions = entries.map(([tick, before, after]) => {
    if (tick <= previousTick || tick >= end || before !== current || before === after || ![before, after].every(Number.isInteger) || Math.abs(before) > 840 || Math.abs(after) > 840) throw new Error(`invalid historical transition history for ${name}`)
    current = after
    previousTick = tick
    return [tick.toString(), before, after]
  })
  if (current !== nextZone.initial) throw new Error(`1900 baseline differs for ${name}`)
  return { name, initial, transitions }
})
if (zones.reduce((sum, zone) => sum + zone.transitions.length, 0) !== 15200) throw new Error('unexpected historical transition count')
const rules = { version: 1, image: fixture.image, sourceSha256: digest, followingSourceSha256: followingSource, utcStart: start.toString(), utcEndExclusive: end.toString(), zones }
const serialized = `${JSON.stringify(rules)}\n`
if (process.argv[2] === '--check') {
  if (await readFile(output, 'utf8') !== serialized) throw new Error('historical rules differ; regenerate explicitly')
  console.log('checked 141 zones and 15200 historical transitions')
} else if (process.argv.length === 2) {
  await writeFile(output, serialized)
  console.log('wrote 141 zones and 15200 historical transitions')
} else throw new Error('usage: node scripts/generate-at-time-zone-history-1800-1899.mjs [--check]')
