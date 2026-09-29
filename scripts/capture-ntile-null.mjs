#!/usr/bin/env node
// Capture SQL Server's NTILE NULL bucket binding and runtime behavior.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { TYPES } from 'tedious'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/ntile-null.json', import.meta.url)
const fixtureSha256 = 'ff22ae5d5e5b5e8c2fd8fc3abfe2fecafae36c13e7db5dcedea7459ac7c6dfb1'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-ntile-null.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/ntile-null/capture.json')

async function canonicalOutput(path) {
  try { return await realpath(path) }
  catch (error) {
    if (error.code !== 'ENOENT') throw error
    return join(await realpath(dirname(path)), basename(path))
  }
}

async function existingFile(path, inspect = stat) {
  try { return await inspect(path) }
  catch (error) {
    if (error.code === 'ENOENT') return null
    throw error
  }
}

const rows = 'SELECT n,b FROM dbo.bucket ORDER BY n'
const window = 'OVER (ORDER BY n) AS tile FROM dbo.bucket ORDER BY n'
const plan = [
  { name: 'server version', sql: "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version" },
  { name: 'create table', sql: 'CREATE TABLE dbo.bucket(n INT NOT NULL,b INT NULL)' },
  { name: 'seed rows', sql: 'INSERT dbo.bucket(n,b) VALUES(1,NULL),(2,NULL),(3,NULL)' },
  { name: 'baseline rows', sql: rows },
  { name: 'literal NULL', sql: `SELECT NTILE(NULL) ${window}` },
  { name: 'typed INT NULL', sql: `SELECT NTILE(CAST(NULL AS INT)) ${window}` },
  { name: 'typed BIGINT NULL', sql: `SELECT NTILE(CAST(NULL AS BIGINT)) ${window}` },
  { name: 'scalar subquery NULL', sql: `SELECT NTILE((SELECT MAX(b) FROM dbo.bucket)) ${window}` },
  { name: 'source column NULL', sql: `SELECT NTILE(b) ${window}` },
  { name: 'zero count', sql: `SELECT NTILE(0) ${window}` },
  { name: 'negative count', sql: `SELECT NTILE(-1) ${window}` },
  { name: 'valid count', sql: `SELECT NTILE(2) ${window}` },
  { name: 'empty typed NULL', sql: `SELECT NTILE(CAST(NULL AS INT)) OVER (ORDER BY n) AS tile FROM dbo.bucket WHERE 1=0` },
  { name: 'RPC NULL', sql: `SELECT NTILE(@b) ${window}`, mode: 'rpc', parameter: null },
  { name: 'RPC valid', sql: `SELECT NTILE(@b) ${window}`, mode: 'rpc', parameter: 2 },
  { name: 'RPC zero', sql: `SELECT NTILE(@b) ${window}`, mode: 'rpc', parameter: 0 },
  { name: 'RPC NULL again', sql: `SELECT NTILE(@b) ${window}`, mode: 'rpc', parameter: null },
  { name: 'session reusable', sql: 'SELECT 1 AS reusable' },
]

async function captureTokens(connection, sql, mode = 'batch', parameter = undefined) {
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
    const transport = mode === 'rpc' ? {
      on: (...args) => connection.on(...args),
      off: (...args) => connection.off(...args),
      execSqlBatch(request) {
        request.addParameter('b', TYPES.Int, parameter)
        connection.execSql(request)
      },
    } : connection
    const result = await capture(transport, sql)
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

async function observe(connection) {
  const records = []
  for (const { name, sql, mode, parameter } of plan) {
    const result = canonical(await captureTokens(connection, sql, mode, parameter))
    records.push({ name, sql, mode: mode ?? 'batch', ...(mode === 'rpc' ? { parameter } : {}), result })
    console.log(name)
  }
  return records
}

function validate(run) {
  assertSameCapture(run.map(({ name, sql, mode, parameter }) => ({ name, sql, mode, ...(mode === 'rpc' ? { parameter } : {}) })),
    plan.map(({ name, sql, mode, parameter }) => ({ name, sql, mode: mode ?? 'batch', ...(mode === 'rpc' ? { parameter } : {}) })), 'capture plan changed')
  const get = name => {
    const record = run.find(item => item.name === name)
    assert(record, `missing ${name}`)
    return record.result
  }
  for (const { name, result } of run) {
    for (const key of ['sets', 'done', 'doneTokens', 'events', 'errors', 'info']) assert(Array.isArray(result[key]), `${name}: missing ${key}`)
    assert(result.done.length > 0, `${name}: missing completion`)
    assert.equal(result.doneTokens.length, result.done.length, `${name}: incomplete completion`)
    for (const token of result.doneTokens) assert(Number.isInteger(token.status), `${name}: missing DONE status`)
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete result set`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
  }
  const positive = "The function 'ntile' takes only a positive int or bigint expression as its input."
  const failures = [
    ['literal NULL', 4116, positive, [], ['ERROR', 'DONE'], [['DONE', 2, 253, null]]],
    ['typed INT NULL', 4116, positive, [], ['ERROR', 'DONE'], [['DONE', 2, 253, null]]],
    ['typed BIGINT NULL', 4116, positive, [], ['ERROR', 'DONE'], [['DONE', 2, 253, null]]],
    ['scalar subquery NULL', 4116, positive, [[]], ['COLMETADATA', 'ORDER', 'ERROR', 'INFO', 'DONE'], [['DONE', 2, 193, null]]],
    ['source column NULL', 4195, 'The reference to column "b" is not allowed in an argument to the NTILE function. Only references to columns at an outer scope or standalone expressions and subqueries are allowed here.', [], ['ERROR', 'DONE'], [['DONE', 2, 253, null]]],
    ['zero count', 4116, positive, [], ['ERROR', 'DONE'], [['DONE', 2, 253, null]]],
    ['negative count', 4116, positive, [], ['ERROR', 'DONE'], [['DONE', 2, 253, null]]],
    ['empty typed NULL', 4116, positive, [], ['ERROR', 'DONE'], [['DONE', 2, 253, null]]],
    ['RPC NULL', 4116, positive, [[]], ['COLMETADATA', 'ORDER', 'ERROR', 'DONEPROC'], [['DONEPROC', 2, 224, null]]],
    ['RPC zero', 4116, positive, [[]], ['COLMETADATA', 'ORDER', 'ERROR', 'DONEPROC'], [['DONEPROC', 2, 224, null]]],
    ['RPC NULL again', 4116, positive, [[]], ['COLMETADATA', 'ORDER', 'ERROR', 'DONEPROC'], [['DONEPROC', 2, 224, null]]],
  ]
  for (const [name, number, message, rows, events, done] of failures) {
    const result = get(name)
    assertSameCapture(result.errors.map(({ number, state, class: severity, message }) => [number, state, severity, message]),
      [[number, 1, 15, message]], `${name}: diagnostic changed`)
    assertSameCapture(result.sets.map(set => set.rows), rows, `${name}: rows changed`)
    assertSameCapture(result.events.map(event => event.kind), events, `${name}: event order changed`)
    assertSameCapture(result.doneTokens.map(({ kind, status, command, rowCount }) => [kind, status, command, rowCount]), done, `${name}: completion changed`)
  }
  const shape = set => set.columns.map(({ name, type, length, flags }) => [name, type, length, flags])
  const tileShape = [['tile', 'IntN', 8, 1]]
  for (const name of ['scalar subquery NULL', 'valid count', 'RPC NULL', 'RPC valid', 'RPC zero', 'RPC NULL again']) {
    assertSameCapture(get(name).sets.map(shape), [tileShape], `${name}: descriptor changed`)
  }
  for (const name of ['valid count', 'RPC valid']) {
    assertSameCapture(get(name).sets.map(set => set.rows), [[['1'], ['1'], ['2']]], `${name}: bucket values changed`)
    assertSameCapture(get(name).errors, [], `${name}: unexpected error`)
  }
  assertSameCapture(get('baseline rows').sets.map(set => set.rows), [[[1, null], [2, null], [3, null]]], 'baseline changed')
  assertSameCapture(get('session reusable').sets.map(set => set.rows), [[[1]]], 'session not reusable')
}

async function checkFixture() {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'reference repetitions missing')
  for (const run of retained.runs) validate(run)
  assertSameCapture(retained.runs[0], retained.runs[1], 'reference runs differ')
  console.log(`Checked ${retained.runs[0].length} NTILE NULL observations in two retained runs`)
}

if (check) await checkFixture()
else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if ((await existingFile(output, lstat))?.isSymbolicLink()) {
    throw Error('capture output must not be a symbolic link')
  }
  const fixturePath = fileURLToPath(fixture)
  if (await canonicalOutput(output) === await canonicalOutput(fixturePath)) {
    throw Error('capture output must not be the retained fixture')
  }
  const [outputFile, retainedFile] = await Promise.all([existingFile(output), existingFile(fixturePath)])
  if (outputFile && retainedFile && outputFile.dev === retainedFile.dev && outputFile.ino === retainedFile.ino) {
    throw Error('capture output must not be a hard link to the retained fixture')
  }
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) {
    throw Error('reference image must be pinned')
  }
  await withReferenceContainer(async (config, container) => {
    assert.equal(container.image, referenceImage, 'reference image must be pinned')
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      const run = await isolatedReference(config, observe)
      runs.push(run)
      await writeFile(output, JSON.stringify({ image: container.image, runs }) + '\n')
      validate(run)
    }
    assertSameCapture(runs[0], runs[1], 'fresh-database captures differ')
    const actual = { image: container.image, runs }
    await writeFile(output, JSON.stringify(actual) + '\n')
    let retained
    try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    if (retained) {
      assert.equal(actual.image, retained.image, 'reference image changed')
      assertSameCapture(actual.runs, retained.runs, 'live capture differs from retained fixture')
    }
    if (writeFixture) await writeNewFixture(fixture, actual)
    console.log(`Captured ${runs[0].length} NTILE NULL observations in two fresh databases`)
  })
}
