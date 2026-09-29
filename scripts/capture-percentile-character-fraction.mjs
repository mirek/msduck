#!/usr/bin/env node
// Capture character percentile fraction conversion before implementing it.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, open, readFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/percentile-character-fraction.json', import.meta.url)
const fixtureSha256 = 'PENDING'
const args = process.argv.slice(2)
const mode = args[0]?.startsWith('--') ? args.shift() : undefined
if (![undefined, '--check', '--write-fixture'].includes(mode) || args.length > 1 || args[0]?.startsWith('--')) throw Error('usage: capture-percentile-character-fraction.mjs [--check | --write-fixture] [output]')
const output = resolve(args[0] ?? 'artifacts/compatibility/percentile-character-fraction/capture.json')
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const literal = text => "'" + text.replaceAll("'", "''") + "'"
const source = '(VALUES (1,1),(2,2),(3,3),(4,4)) AS sample(id,n)'
const query = (kind, fraction, descending = false, order = 'n') =>
  `SELECT id,PERCENTILE_${kind}(${fraction}) WITHIN GROUP (ORDER BY ${order}${descending ? ' DESC' : ''}) OVER () AS p FROM ${source} ORDER BY id`
const texts = ['0.5', ' 0.5 ', '+.5', '-.0', '-0', '1e-1', '1E+0',
  '1.00000000000000000001', '1.0000000000000002', '0.99999999999999999',
  '1e-400', '-1e-400', '1e309', '-0.1', '1.1', '', ' ', '.', '+', '-',
  'abc', 'NaN', 'Infinity', '0x1', '0,5', '0.5junk', '0.5\t', '0.5\n',
  '\v0.5', '\f0.5', '\r0.5', '\u00a00.5', '\u20030.5', '0.5\0']
const plan = [{ name: 'server version', sql: "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version" }]
for (const kind of ['CONT', 'DISC']) {
  for (const text of texts) plan.push({ name: `${kind} text ${JSON.stringify(text)}`, sql: query(kind, literal(text)) })
  for (const text of ['0.5', ' 0.5 ', '-0', '１', '\u202f0.5', 'abc', '1.1']) {
    plan.push({ name: `${kind} Unicode ${JSON.stringify(text)}`, sql: query(kind, 'N' + literal(text)) })
  }
  for (const fraction of ['0', '.5', '1.00000000000000000001', 'NULL', "CAST('0.5' AS VARCHAR(8))", "('0.5')", "+'0.5'"]) {
    plan.push({ name: `${kind} expression ${fraction}`, sql: query(kind, fraction) })
  }
  for (const text of ['-0', '0.5', '1']) plan.push({ name: `${kind} descending ${text}`, sql: query(kind, literal(text), true) })
}
plan.push({ name: 'session reusable', sql: 'SELECT 1 AS reusable' })
const preparedPlan = [
  { name: 'prepared CONT character fraction', sql: query('CONT', "'0.5'", false, '@b'), type: TYPES.Int, values: [2, null, 4, 2] },
  { name: 'prepared DISC Unicode fraction', sql: query('DISC', "N'0.5'", false, '@b'), type: TYPES.NVarChar, options: { length: 8 }, values: ['a', null, 'b'] },
  { name: 'prepared bad character fraction', sql: query('CONT', "'abc'", false, '@b'), type: TYPES.Int, values: [2] },
  { name: 'prepared out-of-range fraction', sql: query('CONT', "'1.1'", false, '@b'), type: TYPES.Int, values: [2] },
  { name: 'prepared dynamic fraction', sql: query('CONT', '@b'), type: TYPES.NVarChar, options: { length: 8 }, values: ['0.5'] },
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
  assertSameCapture(run.map(({ name, sql, mode, type, options, values }) => ({ name, sql, mode,
    ...(mode === 'prepared' ? { type, options, values } : {}) })),
    [...plan.map(({ name, sql }) => ({ name, sql, mode: 'batch' })),
      ...preparedPlan.map(({ name, sql, type, options, values }) =>
        ({ name, sql, mode: 'prepared', type: type.name, options: options ?? null, values }))], 'capture plan changed')
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
  console.log(`Checked ${value.runs[0].length} character-fraction records in two retained runs`)
} else {
  if (mode === '--write-fixture') await refuseExistingFixture(fixture)
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) throw Error('reference image must be pinned')
  await mkdir(dirname(output), { recursive: true })
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
    console.log(`Captured ${runs[0].length} character-fraction records in two fresh containers`)
  } finally { await file.close() }
}
