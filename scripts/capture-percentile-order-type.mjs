#!/usr/bin/env node
// Capture percentile ORDER BY type eligibility and descriptors.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, open, readFile, realpath } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/percentile-order-type.json', import.meta.url)
const fixtureSha256 = '878046d815625a00225430c816b2b16f822290b2b1f014c289881d64f6b010e0'
const args = process.argv.slice(2)
const mode = args[0]?.startsWith('--') ? args.shift() : undefined
if (![undefined, '--check', '--write-fixture'].includes(mode) || args.length > 1 || args[0]?.startsWith('--')) throw Error('usage: capture-percentile-order-type.mjs [--check | --write-fixture] [output]')
const output = resolve(args[0] ?? 'artifacts/compatibility/percentile-order-type/capture.json')
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const families = [
  ['BIT', '0', '1'], ['TINYINT', '1', '3'], ['SMALLINT', '-2', '4'],
  ['INT', '-2', '4'], ['BIGINT', '9007199254740993', '9007199254740995'],
  ['DECIMAL(10,2)', '1.00', '1.01'], ['DECIMAL(38,8)', '12345678901234567890.12345678', '12345678901234567890.12345680'],
  ['SMALLMONEY', '1.0000', '1.0002'], ['MONEY', '1.0000', '1.0002'],
  ['REAL', '0.1', '0.2'], ['FLOAT(24)', '0.1', '0.2'], ['FLOAT(53)', '0.1', '0.2'],
  ['CHAR(8)', "'a'", "'b'"], ['VARCHAR(8)', "'a'", "'b'"],
  ['NCHAR(8)', "N'a'", "N'b'"], ['NVARCHAR(8)', "N'a'", "N'b'"],
  ['VARCHAR(MAX)', "'a'", "'b'"], ['NVARCHAR(MAX)', "N'a'", "N'b'"],
  ['DATE', "'2024-01-01'", "'2024-01-03'"], ['TIME(3)', "'01:02:03.004'", "'02:03:04.005'"],
  ['SMALLDATETIME', "'2024-01-01T01:02:00'", "'2024-01-03T01:02:00'"],
  ['DATETIME', "'2024-01-01T01:02:03.003'", "'2024-01-03T01:02:03.007'"],
  ['DATETIME2(7)', "'2024-01-01T01:02:03.1234567'", "'2024-01-03T01:02:03.1234568'"],
  ['DATETIMEOFFSET(7)', "'2024-01-01T01:02:03.1234567+02:00'", "'2024-01-03T01:02:03.1234568-03:00'"],
  ['BINARY(4)', '0x01', '0x03'], ['VARBINARY(4)', '0x01', '0x03'],
  ['UNIQUEIDENTIFIER', "'00000000-0000-0000-0000-000000000001'", "'00000000-0000-0000-0000-000000000003'"],
  ['XML', "N'<a/>'", "N'<b/>'"], ['SQL_VARIANT', '1', '3'],
]
const ordered = (kind, expression, source, suffix = '') =>
  `SELECT id,PERCENTILE_${kind}(.5) WITHIN GROUP (ORDER BY ${expression}) OVER () AS p FROM ${source}${suffix} ORDER BY id`
const plan = [{ name: 'server version', sql: "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version" }]
for (const [type, first, second] of families) {
  for (const kind of ['CONT', 'DISC']) {
    for (const shape of ['ordinary', 'all NULL', 'empty']) {
      const a = shape === 'all NULL' ? 'NULL' : first
      const b = shape === 'all NULL' ? 'NULL' : second
      const source = `(VALUES (1,CAST(${a} AS ${type})),(2,CAST(${b} AS ${type})),(3,CAST(NULL AS ${type}))) AS sample(id,n)`
      plan.push({ name: `${kind} ${type} ${shape}`, sql: ordered(kind, 'n', source, shape === 'empty' ? ' WHERE 1=0' : '') })
    }
  }
}
plan.push({ name: 'session reusable', sql: 'SELECT 1 AS reusable' })
const preparedTypes = [
  { label: 'Int', type: TYPES.Int, values: [2, null, 4, 2] },
  { label: 'BigInt', type: TYPES.BigInt, values: ['9007199254740993', null, '9007199254740995'] },
  { label: 'Decimal', type: TYPES.Decimal, options: { precision: 10, scale: 2 }, values: [1.01, null, 2.02] },
  { label: 'Real', type: TYPES.Real, values: [0.1, null, 0.2] },
  { label: 'Float', type: TYPES.Float, values: [0.1, null, 0.2] },
  { label: 'VarChar', type: TYPES.VarChar, options: { length: 8 }, values: ['a', null, 'b'] },
  { label: 'NVarChar', type: TYPES.NVarChar, options: { length: 8 }, values: ['a', null, 'b'] },
  { label: 'VarBinary', type: TYPES.VarBinary, options: { length: 4 }, values: [Buffer.from([1]), null, Buffer.from([3])] },
  { label: 'UniqueIdentifier', type: TYPES.UniqueIdentifier, values: ['00000000-0000-0000-0000-000000000001', null, '00000000-0000-0000-0000-000000000003'] },
]
const preparedPlan = preparedTypes.flatMap(({ label, ...entry }) => ['CONT', 'DISC'].map(kind => ({
  ...entry, name: `prepared ${kind} ${label}`, sql: ordered(kind, '@b', '(VALUES (1),(2)) AS sample(id)'),
})))

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
  for (const { name, sql, type, options, values } of preparedPlan) {
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
          executions.push({ value: canonical(value), result })
        }
      } finally {
        unpreparation = canonical(await capturePreparedPhase(connection, request,
          () => connection.unprepare(request), setComplete))
      }
    }
    records.push({ name, sql, mode: 'prepared', type: type.name, options: options ?? null,
      values: canonical(values), preparation, executions, unpreparation })
    console.log(name)
  }
  return records
}


function validate(run) {
  assertSameCapture(run.map(({ name, sql, mode, type, options, values }) => ({ name, sql, mode,
    ...(mode === 'prepared' ? { type, options, values } : {}) })),
    [...plan.map(({ name, sql }) => ({ name, sql, mode: 'batch' })),
      ...preparedPlan.map(({ name, sql, type, options, values }) =>
        ({ name, sql, mode: 'prepared', type: type.name, options: options ?? null, values: canonical(values) }))], 'capture plan changed')
  for (const record of run) {
    const phases = record.mode === 'batch' ? [record.result] :
      [record.preparation, ...record.executions.map(entry => entry.result), record.unpreparation].filter(Boolean)
    for (const result of phases) {
      for (const field of ['sets', 'errors', 'info', 'done', 'doneTokens', 'events']) assert(Array.isArray(result[field]), 'missing capture field')
      assert(result.done.length > 0, 'missing completion')
      assert.equal(result.done.length, result.doneTokens.length)
      for (const token of result.doneTokens) assert(Number.isInteger(token.status), 'missing raw DONE status')
    }
    if (record.mode === 'prepared') assert.equal(record.executions.length,
      record.preparation.errors.length ? 0 : record.values.length, 'missing bindings')
  }
  assertSameCapture(run.find(entry => entry.name === 'server version').result.sets[0].rows, [['17.0.4065.4']], 'reference version changed')
  const reuse = run.find(entry => entry.name === 'session reusable').result
  assertSameCapture(reuse.sets.map(set => set.rows), [[[1]]], 'reuse failed')
  assert.equal(reuse.errors.length, 0)
}
async function canonicalOutput(path) {
  try { return await realpath(path) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return resolve(await realpath(dirname(path)), basename(path))
  }
}
async function retained() {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const value = JSON.parse(bytes)
  assert.equal(value.image, referenceImage, 'reference image changed')
  assert.equal(value.runs.length, 2, 'independent runs missing')
  for (const run of value.runs) validate(run)
  assertSameCapture(value.runs[0], value.runs[1], 'independent runs differ')
  return value
}
if (mode === '--check') {
  const value = await retained()
  console.log(`Checked ${value.runs[0].length} order-type records in two retained runs`)
} else {
  if (mode === '--write-fixture') await refuseExistingFixture(fixture)
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) throw Error('reference image must be pinned')
  await mkdir(dirname(output), { recursive: true })
  if (await canonicalOutput(output) === await canonicalOutput(fileURLToPath(fixture))) throw Error('output must not alias the retained fixture')
  const file = await open(output, 'wx') // refuse existing paths and fixture aliases
  try {
    await file.writeFile(JSON.stringify({ status: 'incomplete' }) + '\n')
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      await withReferenceContainer(async (config, container) => {
        assert.equal(container.image, referenceImage)
        const run = await isolatedReference(config, observe)
        validate(run)
        if (runs.length) assertSameCapture(run, runs[0], 'independent reference containers differ')
        runs.push(run)
      })
    }
    const value = { image: referenceImage, runs }
    if (mode !== '--write-fixture') assertSameCapture(value, await retained(), 'fresh reference differs')
    const bytes = Buffer.from(JSON.stringify(value) + '\n')
    await file.truncate(0)
    let written = 0
    while (written < bytes.length) written += (await file.write(bytes, written, bytes.length - written, written)).bytesWritten
    if (mode === '--write-fixture') await writeNewFixture(fixture, value)
    console.log(`Captured ${runs[0].length} order-type records in two fresh containers`)
  } finally { await file.close() }
}
