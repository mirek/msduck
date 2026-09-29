#!/usr/bin/env node
// Capture SQL Server's NTILE bucket type binding and conversion behavior.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { Request, TYPES } from 'tedious'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/ntile-types.json', import.meta.url)
const fixtureSha256 = '221e5848fbc16740ced1750d47971ffe3803e1687868327b6b400490951b2a06'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-ntile-types.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/ntile-types/capture.json')

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

const window = 'OVER (ORDER BY n) AS tile FROM (VALUES (1),(2),(3)) AS bucket(n) ORDER BY n'
const tile = expression => `SELECT NTILE(${expression}) ${window}`
const plan = [
  { name: 'server version', sql: "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version" },
  ...[
    ['tinyint two', 'CAST(2 AS TINYINT)'],
    ['smallint two', 'CAST(2 AS SMALLINT)'],
    ['int two', 'CAST(2 AS INT)'],
    ['bigint two', 'CAST(2 AS BIGINT)'],
    ['bit true', 'CAST(1 AS BIT)'],
    ['decimal two', 'CAST(2 AS DECIMAL(10,2))'],
    ['decimal fractional', 'CAST(2.5 AS DECIMAL(10,2))'],
    ['float fractional', 'CAST(2.5 AS FLOAT)'],
    ['varchar numeric', "CAST('2' AS VARCHAR(8))"],
    ['nvarchar numeric', "CAST(N'2' AS NVARCHAR(8))"],
    ['varchar fractional', "CAST('2.5' AS VARCHAR(8))"],
    ['varchar invalid', "CAST('bad' AS VARCHAR(8))"],
    ['binary numeric', 'CAST(0x02 AS VARBINARY(1))'],
    ['date value', "CAST('2024-01-02' AS DATE)"],
    ['guid value', "CAST('00000000-0000-0000-0000-000000000002' AS UNIQUEIDENTIFIER)"],
    ['bigint maximum', 'CAST(9223372036854775807 AS BIGINT)'],
    ['decimal zero', 'CAST(0 AS DECIMAL(10,2))'],
    ['decimal negative', 'CAST(-1.5 AS DECIMAL(10,2))'],
    ['varchar zero', "CAST('0' AS VARCHAR(8))"],
    ['tinyint NULL', 'CAST(NULL AS TINYINT)'],
    ['bit NULL', 'CAST(NULL AS BIT)'],
    ['decimal NULL', 'CAST(NULL AS DECIMAL(10,2))'],
    ['varchar NULL', 'CAST(NULL AS VARCHAR(8))'],
    ['date NULL', 'CAST(NULL AS DATE)'],
  ].map(([name, expression]) => ({ name, sql: tile(expression) })),
  { name: 'session reusable', sql: 'SELECT 1 AS reusable' },
]
const preparedPlan = [
  { name: 'prepared int', type: TYPES.Int, values: [2, null, 0, 2] },
  { name: 'prepared decimal', type: TYPES.Decimal, options: { precision: 10, scale: 2 }, values: [2.5, null, 0, 2] },
  { name: 'prepared varchar', type: TYPES.VarChar, options: { length: 8 }, values: ['2', 'bad', '0', '2'] },
]

async function captureTokens(connection, action) {
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
    const result = await action()
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

async function capturePreparedPhase(connection, request, issue, setComplete, prepared = false) {
  const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
  const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
  const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
  const onMetadata = metadata => result.sets.push({ columns: metadata.map(c => ({ name: c.colName, type: c.type.name,
    length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null,
    flags: c.flags, collation: canonical(c.collation ?? null) })), rows: [] })
  const onRow = row => result.sets.at(-1).rows.push(row.map(c => c.value))
  const done = Object.fromEntries(['done', 'doneInProc', 'doneProc'].map(kind => [kind, (rowCount, more, status) => {
    result.done.push({ kind, rowCount: rowCount ?? null, more })
    if (kind === 'doneProc') result.returnStatus = status
  }]))
  connection.on('errorMessage', onError); connection.on('infoMessage', onInfo)
  request.on('columnMetadata', onMetadata); request.on('row', onRow)
  for (const [kind, listener] of Object.entries(done)) request.on(kind, listener)
  try {
    return await captureTokens(connection, async () => {
      if (prepared) {
        await new Promise(resolve => {
          const success = () => { request.off('error', failure); resolve() }
          const failure = error => {
            request.off('prepared', success)
            if (!result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          }
          request.once('prepared', success)
          request.once('error', failure)
          issue()
        })
      } else {
        await new Promise(resolve => {
          request.error = undefined
          setComplete((error, rowCount) => {
            result.rowCount = rowCount
            if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          })
          issue()
        })
      }
      return result
    })
  } finally {
    connection.off('errorMessage', onError); connection.off('infoMessage', onInfo)
    request.off('columnMetadata', onMetadata); request.off('row', onRow)
    for (const [kind, listener] of Object.entries(done)) request.off(kind, listener)
  }
}

async function observe(connection) {
  const records = []
  for (const { name, sql } of plan) {
    const result = canonical(await captureTokens(connection, () => capture(connection, sql)))
    records.push({ name, sql, mode: 'batch', result })
    console.log(name)
  }
  for (const { name, type, options, values } of preparedPlan) {
    const sql = tile('@b')
    let complete = () => {}
    const setComplete = callback => { complete = callback }
    const request = new Request(sql, (error, rowCount) => complete(error, rowCount))
    request.addParameter('b', type, undefined, options)
    const preparation = canonical(await capturePreparedPhase(connection, request, () => connection.prepare(request), setComplete, true))
    const executions = []
    let unpreparation = null
    if (!preparation.errors.length) {
      try {
        for (const value of values) {
          const result = canonical(await capturePreparedPhase(connection, request,
            () => connection.execute(request, { b: value }), setComplete))
          executions.push({ value, result })
        }
      } finally {
        unpreparation = canonical(await capturePreparedPhase(connection, request,
          () => connection.unprepare(request), setComplete))
      }
    }
    records.push({ name, sql, mode: 'prepared', type: type.name, options: options ?? null,
      values, preparation, executions, unpreparation })
    console.log(name)
  }
  return records
}

function validate(run) {
  assertSameCapture(run.map(({ name, sql, mode, type, options, values }) =>
    ({ name, sql, mode, ...(mode === 'prepared' ? { type, options, values } : {}) })),
  [...plan.map(({ name, sql }) => ({ name, sql, mode: 'batch' })),
    ...preparedPlan.map(({ name, type, options, values }) =>
      ({ name, sql: tile('@b'), mode: 'prepared', type: type.name, options: options ?? null, values }))],
  'capture plan changed')
  const get = name => {
    const record = run.find(item => item.name === name)
    assert(record, `missing ${name}`)
    return record.result
  }
  const validateResult = (name, result) => {
    for (const key of ['sets', 'done', 'doneTokens', 'events', 'errors', 'info']) assert(Array.isArray(result[key]), `${name}: missing ${key}`)
    assert(result.done.length > 0, `${name}: missing completion`)
    assert.equal(result.doneTokens.length, result.done.length, `${name}: incomplete completion`)
    for (const token of result.doneTokens) assert(Number.isInteger(token.status), `${name}: missing DONE status`)
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete result set`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
  }
  for (const record of run) {
    if (record.mode === 'batch') validateResult(record.name, record.result)
    else {
      validateResult(`${record.name} prepare`, record.preparation)
      for (const [index, execution] of record.executions.entries()) {
        assertSameCapture(execution.value, record.values[index], `${record.name}: rebound value changed`)
        validateResult(`${record.name} execute ${index}`, execution.result)
      }
      assert.equal(record.executions.length, record.preparation.errors.length ? 0 : record.values.length,
        `${record.name}: missing prepared execution`)
      if (record.unpreparation) validateResult(`${record.name} unprepare`, record.unpreparation)
      else assert(record.preparation.errors.length, `${record.name}: missing unprepare`)
    }
  }
  assertSameCapture(get('session reusable').sets.map(set => set.rows), [[[1]]], 'session not reusable')
  assertSameCapture(get('server version').sets[0].rows, [['17.0.4065.4']], 'server version changed')
  const positive = "The function 'ntile' takes only a positive int or bigint expression as its input."
  const diagnostic = result => result.errors.map(({ number, state, class: severity, message }) =>
    [number, state, severity, message])
  const shape = result => result.sets.map(set => set.columns.map(({ type, length, flags }) =>
    [type, length, flags]))
  const rows = result => result.sets.map(set => set.rows)
  const events = result => result.events.map(event => event.kind)
  const done = result => result.doneTokens.map(({ kind, status, command }) => [kind, status, command])
  const good = new Map([
    ...['tinyint two', 'smallint two', 'int two', 'bigint two'].map(name => [name, [['1'], ['1'], ['2']]]),
    ['bigint maximum', [['1'], ['2'], ['3']]],
  ])
  for (const [name, expectedRows] of good) {
    const result = get(name)
    assertSameCapture(diagnostic(result), [], `${name}: unexpected diagnostic`)
    assertSameCapture(shape(result), [[['IntN', 8, 1]]], `${name}: descriptor changed`)
    assertSameCapture(rows(result), [expectedRows], `${name}: buckets changed`)
    assertSameCapture(events(result), ['COLMETADATA', 'ORDER', 'ROW', 'ROW', 'ROW', 'DONE'], `${name}: events changed`)
    assertSameCapture(done(result), [['DONE', 16, 193]], `${name}: completion changed`)
  }
  for (const { name } of plan.slice(1, -1)) {
    if (good.has(name)) continue
    const result = get(name)
    assertSameCapture(diagnostic(result), [[4116, 1, 15, positive]], `${name}: diagnostic changed`)
    assertSameCapture(shape(result), [], `${name}: unexpected descriptor`)
    assertSameCapture(events(result), ['ERROR', 'DONE'], `${name}: events changed`)
    assertSameCapture(done(result), [['DONE', 2, 253]], `${name}: completion changed`)
  }
  const int = run.find(record => record.name === 'prepared int')
  assertSameCapture(shape(int.preparation), [[['IntN', 8, 1]]], 'INT prepare descriptor changed')
  assertSameCapture(events(int.preparation),
    ['COLMETADATA', 'ORDER', 'DONEINPROC', 'RETURNSTATUS', 'RETURNVALUE', 'DONEPROC'],
    'INT prepare events changed')
  assertSameCapture(done(int.preparation), [['DONEINPROC', 17, 193], ['DONEPROC', 0, 224]],
    'INT prepare completion changed')
  for (const [index, execution] of int.executions.entries()) {
    const result = execution.result
    assertSameCapture(shape(result), [[['IntN', 8, 1]]], `INT execute ${index}: descriptor changed`)
    if (index === 1 || index === 2) {
      assertSameCapture(diagnostic(result), [[4116, 1, 15, positive]], `INT execute ${index}: diagnostic changed`)
      assertSameCapture(rows(result), [[]], `INT execute ${index}: rows changed`)
      assertSameCapture(events(result), ['COLMETADATA', 'ORDER', 'ERROR', 'DONEPROC'],
        `INT execute ${index}: events changed`)
      assertSameCapture(done(result), [['DONEPROC', 2, 224]], `INT execute ${index}: completion changed`)
    } else {
      assertSameCapture(diagnostic(result), [], `INT execute ${index}: unexpected diagnostic`)
      assertSameCapture(rows(result), [[['1'], ['1'], ['2']]], `INT execute ${index}: buckets changed`)
      assertSameCapture(events(result),
        ['COLMETADATA', 'ORDER', 'ROW', 'ROW', 'ROW', 'DONEINPROC', 'RETURNSTATUS', 'DONEPROC'],
        `INT execute ${index}: events changed`)
      assertSameCapture(done(result), [['DONEINPROC', 17, 193], ['DONEPROC', 0, 224]],
        `INT execute ${index}: completion changed`)
    }
  }
  assertSameCapture(events(int.unpreparation), ['RETURNSTATUS', 'DONEPROC'], 'INT unprepare events changed')
  assertSameCapture(done(int.unpreparation), [['DONEPROC', 0, 224]], 'INT unprepare completion changed')
  for (const name of ['prepared decimal', 'prepared varchar']) {
    const record = run.find(item => item.name === name)
    assertSameCapture(diagnostic(record.preparation), [
      [4116, 1, 15, positive], [8180, 1, 16, 'Statement(s) could not be prepared.'],
    ], `${name}: preparation errors changed`)
    assertSameCapture(shape(record.preparation), [], `${name}: unexpected descriptor`)
    assertSameCapture(events(record.preparation),
      ['ERROR', 'ERROR', 'RETURNSTATUS', 'RETURNVALUE', 'DONEPROC'],
      `${name}: preparation events changed`)
    assertSameCapture(done(record.preparation), [['DONEPROC', 2, 224]], `${name}: completion changed`)
  }
}

async function checkFixture() {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'reference repetitions missing')
  for (const run of retained.runs) validate(run)
  assertSameCapture(retained.runs[0], retained.runs[1], 'reference runs differ')
  console.log(`Checked ${retained.runs[0].length} NTILE type observations in two retained runs`)
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
    console.log(`Captured ${runs[0].length} NTILE type observations in two fresh databases`)
  })
}
