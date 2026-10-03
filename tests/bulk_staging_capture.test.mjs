import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile, mkdir, mkdtemp, writeFile, symlink, link, rm} from 'node:fs/promises'
import {join} from 'node:path'
import {spawnSync} from 'node:child_process'
import {cases, rowsFor, compare, validate, validateRetained, guardOutput, persistCapture} from '../scripts/capture-bulk-staging-reference.mjs'

const fixture = new URL('../reference/bulk-staging-reference.json', import.meta.url)
const load = async () => JSON.parse(await readFile(fixture, 'utf8'))

test('captured cases preserve threshold inputs and three independent default patterns', () => {
  assert.deepEqual(cases.filter(c => c.name.startsWith('defaults-')).map(c => c.count), [1, 332, 333, 334, 499, 500, 501, 999, 1000, 1001, 1501])
  assert.deepEqual(cases.filter(c => c.listedIdentity).map(c => c.count), [284, 285, 286, 399, 400, 401, 1001])
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
  const value = validateRetained(await readFile(fixture))
  assert.deepEqual(compare(value, structuredClone(value)), [])
  assert.equal(value.runs.length, 4)
  assert.equal(value.runs[0].observations.length, cases.length)
})

test('retained byte pin rejects framed payload corruption with matching semantic summaries', async () => {
  const bytes = await readFile(fixture)
  validateRetained(bytes)
  for (const direction of ['out', 'in']) {
    const actual = JSON.parse(bytes)
    for (const run of actual.runs) {
      const packet = run.observations[0].setup.packets.find(p => p.direction === direction)
      const raw = Buffer.from(packet.rawHex, 'hex')
      raw[raw.length - 1] ^= 1 // Payload only: retain length, type, direction and EOM.
      packet.rawHex = raw.toString('hex')
    }
    actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
    // General capture validation still accepts independently observed raw drift;
    // only the previously reviewed retained artifact must match its byte pin.
    validate(actual)
    assert.throws(() => validateRetained(Buffer.from(JSON.stringify(actual) + '\n')), /fixed retained fixture bytes/)
  }
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
    assert.throws(() => validate(actual), /comparisons exactly reflect|complete successful bulk load|accidental metadata|fixed captured|fixed whole-load|fixed trigger/)
  }
})

test('malformed packet frames and forged comparison summaries are rejected', async () => {
  const retained = await load()
  const malformed = structuredClone(retained)
  malformed.runs[0].observations[0].execution.packets[0].rawHex = '07010009'
  assert.throws(() => validate(malformed), /exact packet frame|bounded encoded packet/)
  const forged = structuredClone(retained)
  forged.comparisons[0] = []
  assert.throws(() => validate(forged), /comparisons exactly reflect/)
})

test('uniform corruption is rejected even after recomputing all four-run summaries', async () => {
  const retained = await load()
  const mutations = [
    o => {o.readback.result.sets[1].rows[0][0] += 100},
    o => {o.readback.result.sets[3].rows[0][2] = -123},
    o => {o.readback.result.sets[1].columns[0].flags ^= 1},
    o => {o.readback.result.done[0].rowCount += 1},
    o => {o.execution.result.errors[0].state += 1},
    o => {o.execution.result.errors[0].class += 1}
  ]
  for (const [index, mutate] of mutations.entries()) {
    const actual = structuredClone(retained)
    for (const run of actual.runs) mutate(run.observations.find(o => o.case.name === (index >= 4 ? 'trigger-check-check0-tran0' : 'defaults-501')))
    actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
    assert.throws(() => validate(actual), /fixed captured|fixed trigger|fixed readback/)
  }
})

test('oversized encoded packets and unfinished EOM are rejected before acceptance', async () => {
  const retained = await load()
  const oversized = structuredClone(retained)
  oversized.runs[0].observations[0].execution.packets[0].rawHex = '00'.repeat(32768)
  assert.throws(() => validate(oversized), /bounded encoded packet before decoding/)
  const budget = structuredClone(retained)
  const fullPacket = Buffer.alloc(32767)
  fullPacket[0] = 7
  fullPacket[1] = 1
  fullPacket.writeUInt16BE(32767, 2)
  budget.runs[0].observations[0].execution.packets = Array(257).fill({direction: 'out', rawHex: fullPacket.toString('hex')})
  assert.throws(() => validate(budget), /bounded encoded packet before decoding/)
  const incomplete = structuredClone(retained)
  const packet = incomplete.runs[0].observations[0].execution.packets.at(-1)
  const bytes = Buffer.from(packet.rawHex, 'hex')
  bytes[1] &= ~1
  packet.rawHex = bytes.toString('hex')
  assert.throws(() => validate(incomplete), /complete EOM framing/)
})

test('uniform setup, trigger, transaction and cleanup failures cannot become gold evidence', async () => {
  const retained = await load()
  const mutations = [
    result => {result.errors.push({number: 50000, state: 1, class: 16, message: 'failed step'})},
    result => {result.info.push({number: 3621, message: 'unexpected diagnostic'})},
    result => {result.done[0].more = !result.done[0].more},
    result => {result.rowCount += 1},
    result => {result.returnStatus = 99},
    result => {result.sets.push({columns: [], rows: [[]]})}
  ]
  for (const step of ['setup', 'trigger', 'begin', 'rollback', 'cleanup']) {
    for (const mutate of mutations) {
      const actual = structuredClone(retained)
      for (const run of actual.runs) mutate(run.observations.find(o => o.case.transaction)[step].result)
      actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
      assert.throws(() => validate(actual), new RegExp(`fixed ${step} result`))
    }
  }
})

test('version and readback diagnostics and descriptors are pinned independently of comparisons', async () => {
  const retained = await load()
  for (const mutate of [
    run => {run.version.result.errors.push({number: 50000})},
    run => {run.version.result.done[0].rowCount = 0},
    run => {run.version.result.sets[0].columns[0].flags ^= 1},
    run => {run.observations[0].readback.result.info.push({number: 3621})},
    run => {run.observations[0].readback.result.returnStatus = 99},
    run => {run.observations[0].begin = structuredClone(run.observations.find(o => o.case.transaction).begin)}
  ]) {
    const actual = structuredClone(retained)
    for (const run of actual.runs) mutate(run)
    actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
    assert.throws(() => validate(actual), /fixed version|fixed readback|no unrequested begin/)
  }
})

test('uniformly changed image and server version cannot relabel retained provenance', async () => {
  const retained = await load()
  const image = structuredClone(retained)
  for (const container of image.containers) container.image = `mcr.microsoft.com/mssql/server:2025-latest@sha256:${'0'.repeat(64)}`
  assert.throws(() => validate(image), /fixed captured server image/)
  const version = structuredClone(retained)
  for (const run of version.runs) run.version.result.sets[0].rows[0][0] = '17.0.9999.9'
  version.comparisons = version.runs.slice(1).map(run => compare(run, version.runs[0]))
  assert.throws(() => validate(version), /fixed captured server version/)
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

test('a rejected changed observation retains both raw capture and full comparison sidecar', async () => {
  const originalBytes = await readFile(fixture)
  const actual = JSON.parse(originalBytes)
  for (const run of actual.runs) {
    run.observations.find(o => o.case.name === 'trigger-foreign-key-check0-tran0').readback.result.sets[0].rows[0][0] = 0
  }
  actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
  await mkdir(new URL('../.tmp/', import.meta.url), {recursive: true})
  const directory = await mkdtemp(new URL('../.tmp/bulk-staging-rejected-', import.meta.url))
  try {
    const output = join(directory, 'changed.json')
    await assert.rejects(persistCapture(actual, output), /fixed failed-load identity/)
    assert.deepEqual(JSON.parse(await readFile(output, 'utf8')), actual)
    const sidecar = JSON.parse(await readFile(`${output}.comparison.json`, 'utf8'))
    assert.equal(sidecar.retained, true)
    assert.deepEqual(sidecar.differences, compare(actual, JSON.parse(originalBytes)))
    assert.ok(sidecar.differences.some(d => d.path.includes('/readback/result/sets/0/rows/0/0') && d.local === 0 && d.reference === 547))
    assert.deepEqual(await readFile(fixture), originalBytes)
  } finally { await rm(directory, {recursive: true, force: true}) }
})
