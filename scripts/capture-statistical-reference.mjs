#!/usr/bin/env node
// Independent SQL Server evidence for statistical aggregates and windows.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/statistical-aggregates.json', import.meta.url)
const fixtureSha256 = 'ddd57b260ee6db3ccb82c128bab345f5410818491d8585afd8355d2fbe150fa1'
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-statistical-reference.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/statistical-reference/capture.json')
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const source = '(VALUES (1,1,CAST(1 AS INT)),(2,1,CAST(2 AS INT)),(3,1,CAST(2 AS INT)),(4,1,NULL),(5,2,CAST(9 AS INT))) s(id,g,n)'
const functions = ['STDEV', 'STDEVP', 'VAR', 'VARP']
const aggregate = (functionName, from = source, suffix = '') => `SELECT ${functionName}(n) AS value FROM ${from} ${suffix}`
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ...functions.flatMap(functionName => [
    [`${functionName} aggregate`, aggregate(functionName)],
    [`${functionName} singleton`, aggregate(functionName, source, 'WHERE id=1')],
    [`${functionName} all NULL`, aggregate(functionName, source, 'WHERE id=4')],
    [`${functionName} empty`, aggregate(functionName, source, 'WHERE 1=0')],
    [`${functionName} DISTINCT`, `SELECT ${functionName}(DISTINCT n) AS value FROM ${source}`],
    [`${functionName} BIT`, `SELECT ${functionName}(CAST(1 AS BIT)) AS value`],
    [`${functionName} no argument`, `SELECT ${functionName}() AS value`],
  ]),
  ['partitioned windows', `SELECT id,${functions.map(name => `${name}(n) OVER (PARTITION BY g) AS ${name.toLowerCase()}`).join(',')} FROM ${source} ORDER BY id`],
  ['ordered frames', `SELECT id,${functions.map(name => `${name}(n) OVER (ORDER BY id ROWS UNBOUNDED PRECEDING) AS ${name.toLowerCase()}`).join(',')} FROM ${source} ORDER BY id`],
  ['empty preceding frame', `SELECT id,${functions.map(name => `${name}(n) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 PRECEDING) AS ${name.toLowerCase()}`).join(',')} FROM ${source} ORDER BY id`],
  ['exact decimal distinct', `SELECT ${functions.map(name => `${name}(DISTINCT n) AS ${name.toLowerCase()}`).join(',')} FROM (VALUES (CAST('9007199254740992' AS DECIMAL(20,0))),(CAST('9007199254740993' AS DECIMAL(20,0)))) s(n)`],
  ['DISTINCT window rejection', `SELECT STDEV(DISTINCT n) OVER (ORDER BY id) FROM ${source}`],
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

function validate(run) {
  assertSameCapture(run.map(({ name, sql }) => [name, sql]), plan, 'capture plan changed')
  for (const { name, result } of run) {
    for (const field of ['sets', 'done', 'doneTokens', 'events', 'errors', 'info']) assert(Array.isArray(result[field]), `${name}: missing ${field}`)
    assert(result.done.length > 0, `${name}: missing completion`)
    assert.equal(result.doneTokens.length, result.done.length, `${name}: completion mismatch`)
    for (const token of result.doneTokens) assert(Number.isInteger(token.status), `${name}: missing raw DONE status`)
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete result`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
  }
  const get = name => run.find(item => item.name === name).result
  assertSameCapture(get('session reusable').sets[0].rows, [[1]], 'session unusable after errors')
  assertSameCapture(get('server version').sets[0].rows, [['17.0.4065.4']], 'server version changed')
  const rows = name => get(name).sets[0].rows
  const done = name => get(name).doneTokens.map(({ kind, status, command, rowCount }) => [kind, status, command, rowCount])
  const kinds = name => get(name).events.map(event => event.kind)
  const descriptor = name => get(name).sets[0].columns.map(({ type, length, flags }) => [type, length, flags])
  const expected = [
    ['STDEV', 3.696845502136472, 4.358898943540674, null],
    ['STDEVP', 3.2015621187164243, 3.559026084010437, 0],
    ['VAR', 13.666666666666666, 19, null],
    ['VARP', 10.25, 12.666666666666666, 0],
  ]
  for (const [name, aggregateValue, distinctValue, singletonValue] of expected) {
    for (const [suffix, value] of [
      ['aggregate', aggregateValue], ['DISTINCT', distinctValue],
      ['singleton', singletonValue], ['all NULL', null], ['empty', null],
    ]) {
      const label = `${name} ${suffix}`
      assertSameCapture(rows(label), [[value]], `${label}: result changed`)
      assertSameCapture(descriptor(label), [['FloatN', 8, 1]], `${label}: descriptor changed`)
      assertSameCapture(done(label), [['DONE', 16, 193, 1]], `${label}: completion changed`)
    }
    for (const [suffix, number, severity, message] of [
      ['BIT', 8117, 16, `Operand data type bit is invalid for ${name.toLowerCase()} operator.`],
      ['no argument', 174, 15, `The ${name} function requires 1 argument(s).`],
    ]) {
      const label = `${name} ${suffix}`
      const result = get(label)
      assertSameCapture(result.errors.map(error => [error.number, error.state, error.class, error.message]),
        [[number, 1, severity, message]], `${label}: diagnostic changed`)
      assertSameCapture(result.sets, [], `${label}: unexpected descriptor`)
      assertSameCapture(kinds(label), ['ERROR', 'DONE'], `${label}: token order changed`)
      assertSameCapture(done(label), [['DONE', 2, 253, null]], `${label}: completion changed`)
    }
  }
  assertSameCapture(rows('exact decimal distinct'), [[0, 0, 0, 0]], 'exact distinct floating result changed')
  assertSameCapture(rows('empty preceding frame'), [
    [1, null, null, null, null], [2, null, 0, null, 0], [3, null, 0, null, 0],
    [4, null, 0, null, 0], [5, null, null, null, null],
  ], 'preceding frame changed')
  for (const name of ['partitioned windows', 'ordered frames', 'empty preceding frame']) {
    assertSameCapture(descriptor(name).slice(1), Array(4).fill(['FloatN', 8, 1]), `${name}: window descriptors changed`)
    assertSameCapture(done(name), [['DONE', 16, 193, 5]], `${name}: completion changed`)
  }
  const distinctWindow = get('DISTINCT window rejection')
  assertSameCapture(distinctWindow.errors.map(error => [error.number, error.state, error.class, error.message]),
    [[10759, 1, 15, 'Use of DISTINCT is not allowed with the OVER clause.']], 'DISTINCT window diagnostic changed')
  assertSameCapture(distinctWindow.sets, [], 'DISTINCT window unexpected descriptor')
  assertSameCapture(kinds('DISTINCT window rejection'), ['ERROR', 'DONE'], 'DISTINCT window token order changed')
  assertSameCapture(done('DISTINCT window rejection'), [['DONE', 2, 253, null]], 'DISTINCT window completion changed')
  for (const name of functions) {
    for (const suffix of ['aggregate', 'singleton', 'all NULL', 'empty', 'DISTINCT', 'BIT', 'no argument']) {
      assert(run.some(record => record.name === `${name} ${suffix}`), `missing ${name} ${suffix}`)
    }
  }
}

async function observe(connection) {
  const run = []
  for (const [name, sql] of plan) {
    run.push({ name, sql, result: await captureTokens(connection, sql) })
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
  console.log(`Checked ${retained.runs[0].length} statistical observations in two retained runs`)
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
