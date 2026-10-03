import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile, mkdtemp, mkdir, writeFile, symlink, link, rm} from 'node:fs/promises'
import {join} from 'node:path'
import {spawnSync} from 'node:child_process'
import {cases, rowsFor, expectedRows, compare, validate, validateRetained, guardOutput, persistCapture} from '../scripts/capture-bulk-character-encoding.mjs'

const fixture = new URL('../reference/bulk-character-encoding.json', import.meta.url)
const load = async () => JSON.parse(await readFile(fixture, 'utf8'))

test('four raw runs preserve native codepages, client decoding and distinct malformed UTF8 outcomes', async () => {
  const value = validateRetained(await readFile(fixture))
  assert.equal(cases.length, 33)
  assert.equal(value.runs.length, 4)
  for (const run of value.runs) {
    const find = name => run.observations.find(o => o.case.name === name)
    assert.deepEqual(find('CP1252-same-bounded').readback.result.sets[1].rows[4], [5, '���ÿ', {kind: 'binary', value: '818d90ff'}, '\u0081\u008d\u0090ÿ'])
    assert.deepEqual(find('CP1251-unicode-bounded').readback.result.sets[1].rows[3], [4, 'Привет', {kind: 'binary', value: '1f0440043804320435044204'}, 'Привет'])
    assert.deepEqual(find('utf-8-cp1252-bounded').readback.result.sets[1].rows[3], [4, 'éO??', {kind: 'binary', value: 'e94f3f3f'}, 'éO??'])
    assert.deepEqual(find('utf-8-invalid-eda080-same').readback.result.sets[1].rows[1], [2, '���', {kind: 'binary', value: 'eda080'}, '��'])
    assert.equal(find('utf-8-invalid-80-same').execution.result.errors[0].number, 9833)
    assert.equal(find('utf-8-invalid-f09f92-same').execution.result.errors[0].number, 7339)
    assert.equal(find('utf-8-invalid-c328-same').execution.result.error, null)
    assert.ok(find('utf-8-same-max').execution.packets.filter(p => p.direction === 'out' && p.rawHex.startsWith('07')).length > 1)
    // Actual first readback PLP boundary bisects the emoji's four UTF8 bytes.
    // The next TDS payload begins with its 4084-byte PLP chunk length.
    const reply = find('utf-8-same-max').readback.packets.filter(p => p.direction === 'in').map(p => Buffer.from(p.rawHex, 'hex'))
    assert.equal(reply[0].subarray(-3).toString('hex'), 'f09fa6')
    assert.equal(reply[1].readUInt32LE(8), 4084)
    assert.equal(reply[1][12], 0x86)
    assert.equal(Buffer.concat([reply[0].subarray(-3), reply[1].subarray(12, 13)]).toString('utf8'), '🦆')
  }
  const max = cases.find(c => c.name === 'utf-8-same-max')
  assert.equal(rowsFor(max).at(-1).valueHex, Buffer.from('éΩ🦆'.repeat(3000)).toString('hex'))
  assert.equal(expectedRows(max).at(-1)[3], 'éΩ🦆'.repeat(3000))
  const replay = spawnSync(process.execPath, ['scripts/capture-bulk-character-encoding.mjs', '--replay-fixture'], {cwd: new URL('../', import.meta.url), encoding: 'utf8', timeout: 5000, env: {...process.env, PATH: ''}})
  assert.equal(replay.status, 0, replay.stderr)
})

test('uniform semantic corruption cannot become an oracle by recomputing all four difference summaries', async () => {
  const retained = await load()
  const mutations = [
    ['utf-8-same-bounded', o => {o.readback.result.sets[1].rows[3][2].value = '80'}],
    ['CP1252-same-bounded', o => {o.readback.result.sets[1].rows[4][3] = '���ÿ'}],
    ['utf-8-invalid-eda080-same', o => {o.readback.result.sets[1].rows[1][3] = '���'}],
    ['utf-8-same-bounded', o => {o.metadata.result.sets[0].columns[0].collation.flags = 32}],
    ['utf-8-same-bounded', o => {o.setup.result.errors.push({number: 50000})}],
    ['utf-8-same-bounded', o => {o.cleanup.result.done[0].rowCount = 1}],
    ['utf-8-same-bounded', o => {o.readback.result.info.push({number: 3621})}],
    ['utf-8-invalid-80-same', o => {o.execution.result.errors[0].state = 1}],
    ['utf-8-invalid-f09f92-same', o => {o.execution.result.errors[0].message = 'changed'}],
    ['utf-8-same-bounded', o => {o.wireCollationHex = '0904d00034'}]
  ]
  for (const [name, mutate] of mutations) {
    const actual = structuredClone(retained)
    for (const run of actual.runs) mutate(run.observations.find(o => o.case.name === name))
    actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
    assert.throws(() => validate(actual), /fixed/)
  }
})

test('request bytes and retained response payloads are checked independently of framing and decoded summaries', async () => {
  const retained = await load()
  const request = structuredClone(retained)
  for (const run of request.runs) {
    const p = run.observations[0].execution.packets.find(p => p.direction === 'out' && p.rawHex.startsWith('07'))
    const b = Buffer.from(p.rawHex, 'hex')
    b[b.length - 1] ^= 1
    p.rawHex = b.toString('hex')
  }
  request.comparisons = request.runs.slice(1).map(run => compare(run, request.runs[0]))
  assert.throws(() => validate(request), /fixed actual BulkLoad/)
  const response = structuredClone(retained)
  for (const run of response.runs) {
    const p = run.observations[0].setup.packets.find(p => p.direction === 'in')
    const b = Buffer.from(p.rawHex, 'hex')
    b[b.length - 1] ^= 1
    p.rawHex = b.toString('hex')
  }
  response.comparisons = response.runs.slice(1).map(run => compare(run, response.runs[0]))
  assert.throws(() => validateRetained(Buffer.from(JSON.stringify(response) + '\n')), /fixed retained bytes/)
})

test('provenance, encoded packet limits, EOM and forged summaries fail explicitly', async () => {
  const retained = await load()
  for (const [mutate, pattern] of [
    [v => {v.containers[0].image += '-changed'}, /fixed image/],
    [v => {v.runs[0].version.result.sets[0].rows[0][0] = '17.0.9999.9'}, /fixed version/],
    [v => {v.runs[0].observations[0].execution.packets[0].rawHex = '00'.repeat(32768)}, /bounded encoded packet/],
    [v => {const p = v.runs[0].observations[0].cleanup.packets.at(-1); const b = Buffer.from(p.rawHex, 'hex'); b[1] &= ~1; p.rawHex = b.toString('hex')}, /complete EOM/],
    [v => {v.comparisons[0] = []}, /exact raw difference summaries/]
  ]) {
    const value = structuredClone(retained)
    mutate(value)
    assert.throws(() => validate(value), pattern)
  }
})

test('output aliases, existing paths, regular-file ancestors and fixture-writing flags fail before Docker', async () => {
  await mkdir(new URL('../.tmp/', import.meta.url), {recursive: true})
  const dir = await mkdtemp(new URL('../.tmp/bulk-encoding-guards-', import.meta.url))
  try {
    const file = join(dir, 'existing'), hard = join(dir, 'hard'), dangling = join(dir, 'dangling'), alias = join(dir, 'alias')
    await writeFile(file, 'original')
    await link(file, hard)
    await symlink(join(dir, 'missing'), dangling)
    await symlink(dir, alias)
    for (const path of [file, hard, dangling, join(alias, 'new'), dir, join(file, 'child.json')]) {
      await assert.rejects(guardOutput(path), /existing output|symlink|ENOTDIR/)
      const result = spawnSync(process.execPath, ['scripts/capture-bulk-character-encoding.mjs', path], {cwd: new URL('../', import.meta.url), encoding: 'utf8', timeout: 5000, env: {...process.env, PATH: ''}})
      assert.equal(result.status, 1)
      assert.match(result.stderr, /existing output|symlink|ENOTDIR/)
      assert.doesNotMatch(result.stderr, /docker/)
    }
    const write = spawnSync(process.execPath, ['scripts/capture-bulk-character-encoding.mjs', '--write-fixture'], {cwd: new URL('../', import.meta.url), encoding: 'utf8', timeout: 5000, env: {...process.env, PATH: ''}})
    assert.equal(write.status, 1)
    assert.match(write.stderr, /known flags/)
    assert.equal(await readFile(file, 'utf8'), 'original')
  } finally {await rm(dir, {recursive: true, force: true})}
})

test('unknown fresh outcomes preserve raw capture and complete comparison before gold rejection', async () => {
  const original = await readFile(fixture)
  const actual = JSON.parse(original)
  for (const run of actual.runs) run.observations[0].readback.result.sets[0].rows[0][0] = 999
  actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
  const dir = await mkdtemp(new URL('../.tmp/bulk-encoding-rejected-', import.meta.url))
  try {
    const output = join(dir, 'fresh.json')
    await assert.rejects(persistCapture(actual, output), /fixed readback rows/)
    assert.deepEqual(JSON.parse(await readFile(output, 'utf8')), actual)
    const sidecar = JSON.parse(await readFile(`${output}.comparison.json`, 'utf8'))
    assert.equal(sidecar.retained, true)
    assert.deepEqual(sidecar.differences, compare(actual, JSON.parse(original)))
    assert.ok(sidecar.differences.some(d => d.local === 999 && d.path.endsWith('/readback/result/sets/0/rows/0/0')))
    assert.deepEqual(await readFile(fixture), original)
  } finally {await rm(dir, {recursive: true, force: true})}
})
