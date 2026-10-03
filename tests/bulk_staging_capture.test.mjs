import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile, mkdir, mkdtemp, writeFile, symlink, link, rm} from 'node:fs/promises'
import {join} from 'node:path'
import {spawnSync} from 'node:child_process'
import {cases, rowsFor, compare, validate, guardOutput} from '../scripts/capture-bulk-staging-reference.mjs'

const fixture = new URL('../reference/bulk-staging-reference.json', import.meta.url)
const load = async () => JSON.parse(await readFile(fixture, 'utf8'))

test('captured cases preserve threshold inputs and three independent default patterns', () => {
  assert.deepEqual(cases.filter(c => c.name.startsWith('defaults-')).map(c => c.count), [1, 499, 500, 501, 999, 1000, 1001, 1501])
  assert.deepEqual(cases.filter(c => c.listedIdentity).map(c => c.count), [399, 400, 401, 1001])
  assert.deepEqual(rowsFor(cases.find(c => c.name === 'defaults-501')).slice(0, 3), [
    {marker: 1, a: null, b: 'row0', c: 200},
    {marker: 2, a: 101, b: null, c: 201},
    {marker: 3, a: 102, b: 'row2', c: null}
  ])
  const identities = rowsFor(cases.find(c => c.name === 'listed-identity-401')).map(row => row.id)
  assert.equal(identities.length, 401)
  assert.equal(new Set(identities).size, 401)
  assert.equal(identities.at(-1), 10800)
})

test('four-run retained evidence validates without normalizing packet differences', async () => {
  const value = await load()
  validate(value)
  assert.deepEqual(compare(value, structuredClone(value)), [])
  assert.equal(value.runs.length, 4)
  assert.equal(value.runs[0].observations.length, cases.length)
})

test('raw packet drift cannot hide changed identity mapping, trigger membership, errors or counts', async () => {
  const retained = await load()
  const mutations = [
    observation => { observation.readback.result.sets[1].rows[0][0] += 100 },
    observation => { observation.readback.result.sets[2].rows[0][1] += 1 },
    observation => { observation.readback.result.sets[3].rows[0][2] = -123 },
    observation => { observation.execution.result.rowCount += 1 },
    observation => { observation.execution.result.errors.push({number: 547, state: 9, class: 16, message: 'changed diagnostic'}) }
  ]
  for (const mutate of mutations) {
    const actual = structuredClone(retained)
    const observation = actual.runs[0].observations.find(o => o.case.name === 'defaults-501')
    mutate(observation)
    const packet = observation.execution.packets.find(p => p.direction === 'in')
    const bytes = Buffer.from(packet.rawHex, 'hex')
    bytes[4] ^= 1 // Unnormalized response SPID change alongside semantic drift.
    packet.rawHex = bytes.toString('hex')
    const diffs = compare(actual, retained)
    assert.ok(diffs.some(d => d.category === 'raw-packet'))
    assert.ok(diffs.some(d => d.category === 'observed-field' && d.path.includes('/result/')))
    assert.throws(() => validate(actual), /comparisons exactly reflect|complete successful bulk load|accidental metadata/)
  }
})

test('malformed packet frames and forged comparison summaries are rejected', async () => {
  const retained = await load()
  const malformed = structuredClone(retained)
  malformed.runs[0].observations[0].execution.packets[0].rawHex = '07010009'
  assert.throws(() => validate(malformed), /exact packet frame/)
  const forged = structuredClone(retained)
  forged.comparisons[0] = []
  assert.throws(() => validate(forged), /comparisons exactly reflect/)
})

test('existing files, hard links, dangling links and symlink parents are refused before Docker', async () => {
  await mkdir(new URL('../.tmp/', import.meta.url), {recursive: true})
  const directory = await mkdtemp(new URL('../.tmp/bulk-staging-guards-', import.meta.url))
  try {
    const existing = join(directory, 'existing.json')
    await writeFile(existing, 'retained')
    const hard = join(directory, 'hard.json'), dangling = join(directory, 'dangling.json')
    await link(existing, hard)
    await symlink(join(directory, 'missing'), dangling)
    const alias = join(directory, 'alias')
    await symlink(directory, alias)
    for (const path of [existing, hard, dangling, join(alias, 'new.json'), directory]) {
      await assert.rejects(guardOutput(path), /existing output|symlink/)
      const result = spawnSync(process.execPath, ['scripts/capture-bulk-staging-reference.mjs', path], {
        cwd: new URL('../', import.meta.url), encoding: 'utf8', timeout: 5000,
        env: {...process.env, PATH: directory}
      })
      assert.equal(result.status, 1)
      assert.match(result.stderr, /existing output|symlink/)
      assert.doesNotMatch(result.stderr, /docker|ENOENT/)
    }
    await guardOutput(join(directory, 'new.json'))
    assert.equal(await readFile(existing, 'utf8'), 'retained')
  } finally { await rm(directory, {recursive: true, force: true}) }
})
