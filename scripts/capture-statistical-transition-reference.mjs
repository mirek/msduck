#!/usr/bin/env node
// Independent SQL Server evidence for statistical transition order and extremes.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/statistical-transition.json', import.meta.url)
const fixtureSha256 = 'b8cec3ef18bb56562b6b167c716a1e4e1f4433dd4c4a389510819e1a430d3000'
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-statistical-transition-reference.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/statistical-transition-reference/capture.json')
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const functions = ['STDEV', 'STDEVP', 'VAR', 'VARP']
const value = (item, type) => item === null ? `CAST(NULL AS ${type})` : `CAST('${item}' AS ${type})`
const source = (type, values) => `(VALUES ${values.map((item, index) => `(${index + 1},${value(item, type)})`).join(',')}) s(id,n)`
const aggregate = (from, distinct = false, suffix = '') => `SELECT ${functions.map(name => `${name}(${distinct ? 'DISTINCT ' : ''}n) AS ${name.toLowerCase()}`).join(',')} FROM ${from} ${suffix}`
const window = (from, over) => `SELECT id,${functions.map(name => `${name}(n) OVER (${over}) AS ${name.toLowerCase()}`).join(',')} FROM ${from} ORDER BY id`
const ordered = Array.from({ length: 96 }, (_, i) => i % 2 ? '1000000000001' : '999999999999')
const clustered = [...ordered.filter(x => x === '999999999999'), ...ordered.filter(x => x === '1000000000001')]
const extrema = ['1e150', '-1e150', '1e150', '-1e150']
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['long alternating decimal', aggregate(source('DECIMAL(20,0)', ordered))],
  ['long clustered decimal', aggregate(source('DECIMAL(20,0)', clustered))],
  ['long reversed decimal', aggregate(source('DECIMAL(20,0)', [...clustered].reverse()))],
  ['long ordered window decimal', window(source('DECIMAL(20,0)', ordered), 'ORDER BY id ROWS UNBOUNDED PRECEDING')],
  ['long reverse window decimal', window(source('DECIMAL(20,0)', [...ordered].reverse()), 'ORDER BY id ROWS UNBOUNDED PRECEDING')],
  ['signed zero float', aggregate(source('FLOAT', ['-0.0', '0.0', '-0.0']))],
  ['signed zero real', aggregate(source('REAL', ['-0.0', '0.0', '-0.0']))],
  ['signed zero scalar', "SELECT CAST('-0.0' AS FLOAT) AS negative_zero, -CAST('0.0' AS FLOAT) AS negated_zero"],
  ['large finite float', aggregate(source('FLOAT', extrema))],
  ['overflow square float', aggregate(source('FLOAT', ['1e308', '-1e308']))],
  ['near overflow square float', aggregate(source('FLOAT', ['1e154', '-1e154']))],
  ['near clamp decimal 1e8', aggregate(source('DECIMAL(20,0)', ['100000000', '100000001', '100000002']))],
  ['near clamp decimal 1e10', aggregate(source('DECIMAL(20,0)', ['10000000000', '10000000001', '10000000002']))],
  ['near clamp decimal 1e11', aggregate(source('DECIMAL(20,0)', ['100000000000', '100000000001', '100000000002']))],
  ['near clamp decimal 1e12', aggregate(source('DECIMAL(20,0)', ['1000000000000', '1000000000001', '1000000000002']))],
  ['near clamp decimal 1e13', aggregate(source('DECIMAL(20,0)', ['10000000000000', '10000000000001', '10000000000002']))],
  ['session reusable', 'SELECT 1 AS reusable'],
]

async function existingFile(path, inspect = stat) {
  try { return await inspect(path) } catch (error) { if (error.code === 'ENOENT') return null; throw error }
}

async function canonicalOutput(path) {
  try { return await realpath(path) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return join(await realpath(dirname(path)), basename(path))
  }
}

async function captureTokens(connection, sql) {
  const doneTokens = []
  const raw = []
  const events = []
  const createParser = connection.createTokenStreamParser
  const readToken = StreamParser.prototype.readToken
  const recordRaw = (parser, type) => {
    assert(parser.options.tdsVersion >= '7_2', 'unexpected TDS version')
    const at = parser.position
    if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordRaw(parser, type))
    raw.push({ kind: doneKinds.get(type), status: parser.buffer.readUInt16LE(at), command: parser.buffer.readUInt16LE(at + 2) })
    return readToken.call(parser, type)
  }
  StreamParser.prototype.readToken = function (type) {
    return doneKinds.has(type) ? recordRaw(this, type) : readToken.call(this, type)
  }
  connection.createTokenStreamParser = function (message, handler) {
    const parser = createParser.call(this, message, handler)
    assert.equal(typeof parser.parser?.prependListener, 'function', 'Tedious token stream unavailable')
    parser.parser.prependListener('data', token => {
      if (typeof token.name === 'string') events.push({ kind: token.name })
      if (['DONE', 'DONEINPROC', 'DONEPROC'].includes(token.name)) doneTokens.push({
        kind: token.name, more: token.more, sqlError: token.sqlError,
        attention: token.attention, serverError: token.serverError,
        rowCount: token.rowCount ?? null, command: token.curCmd,
      })
    })
    return parser
  }
  try {
    const result = canonical(await capture(connection, sql))
    assert.equal(doneTokens.length, result.done.length, 'incomplete decoded DONE tokens')
    assert.equal(raw.length, doneTokens.length, 'incomplete raw DONE words')
    for (let index = 0; index < doneTokens.length; index++) {
      assert.equal(raw[index].kind, doneTokens[index].kind, 'raw and decoded DONE kinds differ')
      assert.equal(raw[index].command, doneTokens[index].command, 'raw and decoded DONE commands differ')
      doneTokens[index].status = raw[index].status
    }
    return { ...result, doneTokens, events }
  } finally {
    connection.createTokenStreamParser = createParser
    StreamParser.prototype.readToken = readToken
  }
}

function floatBits(result) {
  return result.sets.map(set => set.rows.map(row => row.map((value, index) => {
    const column = set.columns[index]
    if (column.type !== 'FloatN' || column.length !== 8 || value === null) return null
    const number = typeof value === 'number' ? value : value?.kind === 'number' ? Number(value.value) : null
    assert(typeof number === 'number', 'nonnumeric FLOAT(53) result')
    const bytes = Buffer.alloc(8)
    bytes.writeDoubleBE(number)
    return bytes.toString('hex')
  })))
}

function validate(run) {
  assertSameCapture(run.map(({ name, sql }) => [name, sql]), plan, 'capture plan changed')
  for (const { name, result, bits } of run) {
    for (const field of ['sets', 'done', 'doneTokens', 'events', 'errors', 'info']) assert(Array.isArray(result[field]), `${name}: missing ${field}`)
    assert(result.done.length > 0, `${name}: missing completion`)
    assert.equal(result.doneTokens.length, result.done.length, `${name}: completion mismatch`)
    for (const token of result.doneTokens) assert(Number.isInteger(token.status), `${name}: missing raw DONE status`)
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete result`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
    // JSON stores -0 as 0. Keep its exact wire-decoded bits separately, then
    // compare all other bits against the serialized row values.
    const withoutSignOfZero = sets => sets.map(set => set.map(row => row.map(bit =>
      bit === '8000000000000000' ? '0000000000000000' : bit)))
    assertSameCapture(withoutSignOfZero(bits), withoutSignOfZero(floatBits(result)), `${name}: FLOAT(53) bits changed`)
    if (!['server version', 'session reusable', 'signed zero scalar'].includes(name) && !result.errors.length) {
      assert.equal(result.sets.length, 1, `${name}: expected one result set`)
      const columns = result.sets[0].columns
      const offset = name.includes('window') ? 1 : 0
      assert.equal(columns.length, offset + functions.length, `${name}: unexpected result columns`)
      for (const column of columns.slice(offset)) {
        assert.equal(column.type, 'FloatN', `${name}: statistical output type changed`)
        assert.equal(column.length, 8, `${name}: statistical output width changed`)
      }
    }
  }
  const get = name => run.find(item => item.name === name).result
  assertSameCapture(get('session reusable').sets[0].rows, [[1]], 'session unusable after errors')
  assertSameCapture(get('server version').sets[0].rows, [['17.0.4065.4']], 'server version changed')
  assert.equal(get('signed zero scalar').sets[0].columns.length, 2, 'signed-zero scalar control missing')
  assertSameCapture(run.find(item => item.name === 'signed zero scalar').bits[0][0],
    ['8000000000000000', '8000000000000000'], 'signed-zero scalar bits changed')
  for (const name of ['signed zero float', 'signed zero real', 'near clamp decimal 1e8', 'near clamp decimal 1e11', 'near clamp decimal 1e12']) {
    assertSameCapture(run.find(item => item.name === name).bits[0][0],
      Array(4).fill('0000000000000000'), `${name}: zero-clamp bits changed`)
  }
  for (const name of ['overflow square float', 'near overflow square float']) {
    assertSameCapture(get(name).errors.map(error => error.number), [8115], `${name}: overflow diagnostic changed`)
    assertSameCapture(get(name).doneTokens.map(token => token.status), [2], `${name}: overflow DONE status changed`)
  }
  assertSameCapture(run.find(item => item.name === 'long alternating decimal').bits[0][0],
    ['40dd5d7ea914b937', '40dd363d1848dcbf', '41caf286bca1af28', '41caaaaaaaaaaaab'],
    'alternating transition bits changed')
  assertSameCapture(run.find(item => item.name === 'long clustered decimal').bits[0][0],
    ['40e2927b2cd320f5', '40e279a74590331c', '41d58ed2308158ed', '41d5555555555555'],
    'clustered transition bits changed')
  assertSameCapture(run.find(item => item.name === 'long reversed decimal').bits[0][0],
    ['40ca43da0adc6899', '40ca20bd700c2c3e', '41a58ed2308158ed', '41a5555555555555'],
    'reversed transition bits changed')
  assert.equal(get('long ordered window decimal').sets[0].rows.length, ordered.length, 'ordered window rows missing')
  assert.equal(get('long reverse window decimal').sets[0].rows.length, ordered.length, 'reverse window rows missing')
}

function candidateComparison(run) {
  const byName = new Map(run.map(record => [record.name, record]))
  const values = {
    'long alternating decimal': ordered.map(Number),
    'long clustered decimal': clustered.map(Number),
    'long reversed decimal': [...clustered].reverse().map(Number),
    'signed zero float': [-0, 0, -0],
    'signed zero real': [-0, 0, -0],
    'large finite float': extrema.map(Number),
    'overflow square float': [1e308, -1e308],
    'near overflow square float': [1e154, -1e154],
    'near clamp decimal 1e8': [1e8, 1e8 + 1, 1e8 + 2],
    'near clamp decimal 1e10': [1e10, 1e10 + 1, 1e10 + 2],
    'near clamp decimal 1e11': [1e11, 1e11 + 1, 1e11 + 2],
    'near clamp decimal 1e12': [1e12, 1e12 + 1, 1e12 + 2],
    'near clamp decimal 1e13': [1e13, 1e13 + 1, 1e13 + 2],
  }
  const bits = number => {
    if (number === null) return null
    const bytes = Buffer.alloc(8)
    bytes.writeDoubleBE(number)
    return bytes.toString('hex')
  }
  const candidate = values => {
    const count = values.length
    const sum = values.reduce((total, value) => total + value, 0)
    const squares = values.reduce((total, value) => total + value * value, 0)
    const numerator = Math.max(0, squares - sum * sum / count)
    const sample = count > 1 ? numerator / (count - 1) : null
    const population = numerator / count
    return [sample === null ? null : Math.sqrt(sample), Math.sqrt(population), sample, population].map(bits)
  }
  const differences = []
  let compared = 0
  const compare = (name, row, input, offset) => {
    const record = byName.get(name)
    if (record.result.errors.length) return
    const actual = record.bits[0][row].slice(offset)
    const predicted = candidate(input)
    for (let index = 0; index < functions.length; index++) {
      compared++
      if (actual[index] !== predicted[index]) differences.push({ name, row, function: functions[index], actual: actual[index], predicted: predicted[index] })
    }
  }
  for (const [name, input] of Object.entries(values)) compare(name, 0, input, 0)
  for (const [name, input] of [
    ['long ordered window decimal', ordered.map(Number)],
    ['long reverse window decimal', [...ordered].reverse().map(Number)],
  ]) input.forEach((_, row) => compare(name, row, input.slice(0, row + 1), 1))
  return { compared, matched: compared - differences.length, differences }
}

async function observe(connection) {
  const run = []
  for (const [name, sql] of plan) {
    const result = await captureTokens(connection, sql)
    run.push({ name, sql, result, bits: floatBits(result) })
    console.log(name)
  }
  validate(run)
  return run
}

if (check) {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'independent runs missing')
  assert.equal(createHash('sha256').update(JSON.stringify(retained.runs[0])).digest('hex'), retained.captureSha256, 'capture checksum changed')
  for (const run of retained.runs) validate(run)
  assertSameCapture(retained.runs[0], retained.runs[1], 'independent runs differ')
  const candidate = candidateComparison(retained.runs[0])
  assert.equal(candidate.compared, 812, 'candidate coverage changed')
  assert.equal(candidate.matched, candidate.compared, `candidate no longer fits captured cells: ${JSON.stringify(candidate.differences.slice(0, 8))}`)
  console.log(`Checked ${retained.runs[0].length} statistical transition observations in two retained runs`)
  console.log(`Candidate zero-clamped sum-of-squares calculation matched ${candidate.matched}/${candidate.compared} captured cells`)
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if ((await existingFile(output, lstat))?.isSymbolicLink()) throw Error('capture output must not be a symbolic link')
  const fixturePath = fileURLToPath(fixture)
  if (await canonicalOutput(output) === await canonicalOutput(fixturePath)) throw Error('capture output must not be the retained fixture')
  const [outputFile, retainedFile] = await Promise.all([existingFile(output), existingFile(fixturePath)])
  if (outputFile && retainedFile && outputFile.dev === retainedFile.dev && outputFile.ino === retainedFile.ino) throw Error('capture output must not be a hard link to the retained fixture')
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) throw Error('reference image must be pinned')
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    await withReferenceContainer(async (config, container) => {
      assert.equal(container.image, referenceImage, 'reference image must be pinned')
      const run = await isolatedReference(config, observe)
      if (runs.length) assertSameCapture(run, runs[0], 'independent reference containers differ')
      runs.push(run)
    })
  }
  const actual = { image: referenceImage, captureSha256: createHash('sha256').update(JSON.stringify(runs[0])).digest('hex'), runs }
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) } catch (error) { if (error.code !== 'ENOENT') throw error }
  // JSON round-trip removes the sign from -0 in the row representation; the
  // separate `bits` field still compares its exact binary64 representation.
  if (retained) assertSameCapture(JSON.parse(JSON.stringify(actual)), retained, 'fresh capture differs from retained fixture')
  await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} statistical transition observations in two fresh containers${retained ? '; matched fixture' : ''}`)
}
