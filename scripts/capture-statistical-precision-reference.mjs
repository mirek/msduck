#!/usr/bin/env node
// Independent SQL Server evidence for statistical accumulation and rounding.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/statistical-precision.json', import.meta.url)
const fixtureSha256 = '381131fd141da52a49839b17d7a607b9e90c7cb147b5c16e0802f1e9c66ba9e9'
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-statistical-precision-reference.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/statistical-precision-reference/capture.json')
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const functions = ['STDEV', 'STDEVP', 'VAR', 'VARP']
const value = (item, type) => item === null ? `CAST(NULL AS ${type})` : `CAST('${item}' AS ${type})`
const source = (type, values) => `(VALUES ${values.map((item, index) => `(${index + 1},${value(item, type)})`).join(',')}) s(id,n)`
const aggregate = (from, distinct = false, suffix = '') => `SELECT ${functions.map(name => `${name}(${distinct ? 'DISTINCT ' : ''}n) AS ${name.toLowerCase()}`).join(',')} FROM ${from} ${suffix}`
const window = (from, over) => `SELECT id,${functions.map(name => `${name}(n) OVER (${over}) AS ${name.toLowerCase()}`).join(',')} FROM ${from} ORDER BY id`
const grouped = `(VALUES (1,1,CAST('1' AS DECIMAL(20,0))),(2,1,CAST('2' AS DECIMAL(20,0))),(3,1,CAST('2' AS DECIMAL(20,0))),(4,2,CAST('1000000000000' AS DECIMAL(20,0))),(5,2,CAST('1000000000001' AS DECIMAL(20,0))),(6,2,CAST('1000000000002' AS DECIMAL(20,0)))) s(id,g,n)`
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['small repeated int', aggregate(source('INT', [1, 2, 2]))],
  ['small reversed int', aggregate(source('INT', [2, 2, 1]))],
  ['small mixed signs int', aggregate(source('INT', [-9, -2, 1, 7]))],
  ['large adjacent bigint', aggregate(source('BIGINT', ['9007199254740992', '9007199254740993', '9007199254740994']))],
  ['large adjacent decimal', aggregate(source('DECIMAL(20,0)', ['9007199254740992', '9007199254740993', '9007199254740994']))],
  ['large shifted decimal', aggregate(source('DECIMAL(20,0)', ['1000000000000', '1000000000001', '1000000000002']))],
  ['balanced magnitudes decimal', aggregate(source('DECIMAL(20,0)', ['1000000000000', '-1000000000000', 3, -3]))],
  ['mixed exponents float', aggregate(source('FLOAT', ['1e20', '1e-20', '-1e20', 3]))],
  ['adjacent float', aggregate(source('FLOAT', ['1', '1.0000000000000002', '1.0000000000000004']))],
  ['real source', aggregate(source('REAL', ['1.125', '2.25', '2.25', '4.5']))],
  ['fractional decimal', aggregate(source('DECIMAL(20,10)', ['0.1', '0.2', '0.3']))],
  ['tiny decimal', aggregate(source('DECIMAL(38,10)', ['0.0000000001', '0.0000000002', '0.0000000003']))],
  ['nulls int', aggregate(source('INT', [1, null, 2, null]))],
  ['singleton int', aggregate(source('INT', [42]))],
  ['all null int', aggregate(source('INT', [null, null]))],
  ['empty int', aggregate(source('INT', [1, 2]), false, 'WHERE 1=0')],
  ['distinct adjacent decimal', aggregate(source('DECIMAL(20,0)', ['9007199254740992', '9007199254740993', '9007199254740993']), true)],
  ['distinct float with null', aggregate(source('FLOAT', [1, 1, 2, null]), true)],
  ['grouped decimal', `SELECT g,${functions.map(name => `${name}(n) AS ${name.toLowerCase()}`).join(',')} FROM ${grouped} GROUP BY g ORDER BY g`],
  ['ordered ascending int', window(source('INT', [1, 2, 2, 9]), 'ORDER BY id ROWS UNBOUNDED PRECEDING')],
  ['ordered reversed int', window(source('INT', [9, 2, 2, 1]), 'ORDER BY id ROWS UNBOUNDED PRECEDING')],
  ['bounded window with null', window(source('INT', [null, 1, 2, 2, 9]), 'ORDER BY id ROWS BETWEEN 2 PRECEDING AND CURRENT ROW')],
  ['partitioned decimal', `SELECT id,${functions.map(name => `${name}(n) OVER (PARTITION BY g ORDER BY id ROWS UNBOUNDED PRECEDING) AS ${name.toLowerCase()}`).join(',')} FROM ${grouped} ORDER BY id`],
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
    assert(typeof value === 'number' && Number.isFinite(value), 'nonfinite or nonnumeric FLOAT(53) result')
    const bytes = Buffer.alloc(8)
    bytes.writeDoubleBE(value)
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
    assertSameCapture(bits, floatBits(result), `${name}: FLOAT(53) bits changed`)
  }
  const get = name => run.find(item => item.name === name).result
  const bits = name => run.find(item => item.name === name).bits[0][0]
  assertSameCapture(get('session reusable').sets[0].rows, [[1]], 'session unusable after errors')
  assertSameCapture(get('server version').sets[0].rows, [['17.0.4065.4']], 'server version changed')
  assertSameCapture(bits('small repeated int'), ['3fe279a74590331a', '3fde2b7dddfefa62', '3fd5555555555550', '3fcc71c71c71c715'], 'small input bits changed')
  assertSameCapture(bits('small reversed int'), bits('small repeated int'), 'small input permutation changed')
  assertSameCapture(bits('large adjacent bigint'), ['41a0000000000000', '419a20bd700c2c3e', '4350000000000000', '4345555555555555'], 'large adjacent bits changed')
  assertSameCapture(bits('large adjacent decimal'), bits('large adjacent bigint'), 'large source families diverged')
  assertSameCapture(bits('large shifted decimal'), Array(4).fill('0000000000000000'), 'cancellation result changed')
  assertSameCapture(bits('singleton int'), [null, '0000000000000000', null, '0000000000000000'], 'singleton policy changed')
  assertSameCapture(bits('all null int'), Array(4).fill(null), 'all-NULL policy changed')
  assertSameCapture(bits('distinct adjacent decimal'), Array(4).fill('0000000000000000'), 'typed DISTINCT identity changed')
  for (const name of ['nulls int', 'all null int', 'distinct float with null', 'bounded window with null']) {
    assertSameCapture(get(name).info.map(info => info.number), [8153], `${name}: NULL warning changed`)
  }
  assertSameCapture(get('empty int').info, [], 'empty input emitted a NULL warning')
  for (const { name, result } of run.slice(1, -1)) {
    if (result.errors.length) continue
    assert.equal(result.sets.length, 1, `${name}: expected one result set`)
    const columns = result.sets[0].columns
    const offset = name.includes('window') || name.startsWith('ordered ') || name === 'grouped decimal' || name === 'partitioned decimal' ? 1 : 0
    assert.equal(columns.length, offset + functions.length, `${name}: unexpected result columns`)
    for (const column of columns.slice(offset)) {
      assert.equal(column.type, 'FloatN', `${name}: statistical output type changed`)
      assert.equal(column.length, 8, `${name}: statistical output width changed`)
    }
  }
}

function candidateComparison(run) {
  const byName = new Map(run.map(record => [record.name, record]))
  const large = ['9007199254740992', '9007199254740993', '9007199254740994'].map(Number)
  const shifted = [1e12, 1e12 + 1, 1e12 + 2]
  const aggregates = {
    'small repeated int': [1, 2, 2], 'small reversed int': [2, 2, 1],
    'small mixed signs int': [-9, -2, 1, 7],
    'large adjacent bigint': large, 'large adjacent decimal': large,
    'large shifted decimal': shifted,
    'balanced magnitudes decimal': [1e12, -1e12, 3, -3],
    'mixed exponents float': [1e20, 1e-20, -1e20, 3],
    'adjacent float': [1, 1.0000000000000002, 1.0000000000000004],
    'real source': [1.125, 2.25, 2.25, 4.5],
    'fractional decimal': [0.1, 0.2, 0.3],
    'tiny decimal': [1e-10, 2e-10, 3e-10],
    'nulls int': [1, null, 2, null], 'singleton int': [42],
    'all null int': [null, null], 'empty int': [],
    // These are two distinct DECIMAL inputs before both round to the same
    // binary64 value. Do not deduplicate the converted values.
    'distinct adjacent decimal': [Number('9007199254740992'), Number('9007199254740993')],
    'distinct float with null': [1, 2],
  }
  const frames = {
    'grouped decimal': [[1, 2, 2], shifted],
    'ordered ascending int': [[1], [1, 2], [1, 2, 2], [1, 2, 2, 9]],
    'ordered reversed int': [[9], [9, 2], [9, 2, 2], [9, 2, 2, 1]],
    'bounded window with null': [[null], [null, 1], [null, 1, 2], [1, 2, 2], [2, 2, 9]],
    'partitioned decimal': [[1], [1, 2], [1, 2, 2], [1e12], [1e12, 1e12 + 1], shifted],
  }
  const bits = number => {
    if (number === null) return null
    const bytes = Buffer.alloc(8)
    bytes.writeDoubleBE(number)
    return bytes.toString('hex')
  }
  const candidate = values => {
    const present = values.filter(value => value !== null)
    const count = present.length
    if (!count) return [null, null, null, null]
    const sum = present.reduce((total, value) => total + value, 0)
    const squares = present.reduce((total, value) => total + value * value, 0)
    const numerator = Math.max(0, squares - sum * sum / count)
    const sample = count > 1 ? numerator / (count - 1) : null
    const population = numerator / count
    return [sample === null ? null : Math.sqrt(sample), Math.sqrt(population), sample, population].map(bits)
  }
  let compared = 0
  let matched = 0
  const compare = (name, row, values, offset) => {
    const actual = byName.get(name).bits[0][row].slice(offset)
    const predicted = candidate(values)
    for (let index = 0; index < functions.length; index++) {
      compared++
      if (actual[index] === predicted[index]) matched++
    }
  }
  for (const [name, values] of Object.entries(aggregates)) compare(name, 0, values, 0)
  for (const [name, rows] of Object.entries(frames)) rows.forEach((values, index) => compare(name, index, values, 1))
  return { compared, matched }
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
  console.log(`Checked ${retained.runs[0].length} statistical observations in two retained runs`)
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
  if (retained) assertSameCapture(actual, retained, 'fresh capture differs from retained fixture')
  await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} statistical observations in two fresh containers${retained ? '; matched fixture' : ''}`)
}
