#!/usr/bin/env node
// Preserve SQL Server offset-function values, descriptors, diagnostics and completions.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'

const fixture = new URL('../reference/offset-functions.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/offset-functions/capture.json')

const cases = []
// Tedious exposes DATETIMEOFFSET as a UTC Date, which loses its local offset.
// Retain the wire-typed value, server-rendered UTC text and original offset.
const withRendered = sql => `SELECT value,CONVERT(NVARCHAR(50),value,127) AS rendered,DATEPART(tzoffset,value) AS offset_minutes FROM (${sql}) AS probe`
const add = (name, sql) => cases.push([name, withRendered(sql)])
for (let scale = 0; scale <= 7; scale++) {
  const fraction = scale ? '.' + '1234567'.slice(0, scale) : ''
  const local = `2024-02-29T12:34:56${fraction}`
  add(`switch scale ${scale} integer`, `SELECT SWITCHOFFSET(CAST('${local}+02:30' AS DATETIMEOFFSET(${scale})), -300) AS value`)
  add(`switch scale ${scale} text`, `SELECT SWITCHOFFSET(CAST('${local}+02:30' AS DATETIMEOFFSET(${scale})), '+05:45') AS value`)
  add(`attach scale ${scale} integer`, `SELECT TODATETIMEOFFSET(CAST('${local}' AS DATETIME2(${scale})), -300) AS value`)
  add(`attach scale ${scale} text`, `SELECT TODATETIMEOFFSET(CAST('${local}' AS DATETIME2(${scale})), '+05:45') AS value`)
}
for (const [name, expression] of [
  ['zero integer', '0'], ['positive maximum integer', '840'], ['negative maximum integer', '-840'],
  ['positive one minute integer', '1'], ['negative one minute integer', '-1'],
  ['positive maximum text', "'+14:00'"], ['negative maximum text', "'-14:00'"],
  ['zero text', "'+00:00'"], ['negative zero text', "'-00:00'"],
  ['positive short text', "'+1:00'"], ['text with spaces', "' +01:30 '"],
  ['positive 841 integer', '841'], ['negative 841 integer', '-841'],
  ['text invalid minute', "'+01:60'"], ['text invalid hour', "'+15:00'"],
  ['text invalid syntax', "'UTC+01:00'"], ['text empty', "''"],
  ['text null', 'CAST(NULL AS VARCHAR(10))'], ['integer null', 'CAST(NULL AS INT)'],
  ['smallint value', 'CAST(90 AS SMALLINT)'], ['bigint value', 'CAST(-90 AS BIGINT)'],
  ['tinyint value', 'CAST(45 AS TINYINT)'], ['bit value', 'CAST(1 AS BIT)'],
  ['decimal whole', 'CAST(90.0 AS DECIMAL(8,1))'],
  ['decimal fraction', 'CAST(90.5 AS DECIMAL(8,1))'],
  ['numeric negative fraction', 'CAST(-90.5 AS NUMERIC(8,1))'],
  ['float fraction', 'CAST(90.5 AS FLOAT)'], ['real fraction', 'CAST(-90.5 AS REAL)'],
  ['money fraction', 'CAST(90.5 AS MONEY)'],
  ['unicode text', "CAST(N'+01:30' AS NVARCHAR(12))"],
  ['varchar numeric text', "CAST('90' AS VARCHAR(12))"],
  ['unicode numeric text', "CAST(N'90' AS NVARCHAR(12))"],
]) {
  add(`switch ${name}`, `SELECT SWITCHOFFSET(CAST('2024-02-29T12:34:56.1234567+02:30' AS DATETIMEOFFSET(7)), ${expression}) AS value`)
  add(`attach ${name}`, `SELECT TODATETIMEOFFSET(CAST('2024-02-29T12:34:56.1234567' AS DATETIME2(7)), ${expression}) AS value`)
}
for (const [name, local, oldZone, newZone] of [
  ['minimum valid local and UTC', '0001-01-01T14:00:00', '+14:00', '+14:00'],
  ['minimum switch local overflow', '0001-01-01T14:00:00', '+14:00', '-14:00'],
  ['maximum valid local and UTC', '9999-12-31T09:59:59.9999999', '-14:00', '-14:00'],
  ['maximum switch local overflow', '9999-12-31T09:59:59.9999999', '-14:00', '+14:00'],
  ['midnight previous day', '2024-01-01T00:00:00', '+14:00', '-14:00'],
  ['midnight next day', '2024-12-31T23:59:59.9999999', '-14:00', '+14:00'],
]) add(`switch boundary ${name}`, `SELECT SWITCHOFFSET(CAST('${local}${oldZone}' AS DATETIMEOFFSET(7)), '${newZone}') AS value`)
for (const [name, local, zone] of [
  ['minimum valid', '0001-01-01T14:00:00', '+14:00'],
  ['minimum UTC overflow', '0001-01-01T00:00:00', '+14:00'],
  ['maximum valid', '9999-12-31T09:59:59.9999999', '-14:00'],
  ['maximum UTC overflow', '9999-12-31T23:59:59.9999999', '-14:00'],
  ['cross previous day', '2024-01-01T00:00:00', '+14:00'],
  ['cross next day', '2024-12-31T23:59:59.9999999', '-14:00'],
]) add(`attach boundary ${name}`, `SELECT TODATETIMEOFFSET(CAST('${local}' AS DATETIME2(7)), '${zone}') AS value`)
for (const [name, sql] of [
  ['switch null source valid zone', "SELECT SWITCHOFFSET(CAST(NULL AS DATETIMEOFFSET(7)), '+01:00') AS value"],
  ['switch null source invalid zone', "SELECT SWITCHOFFSET(CAST(NULL AS DATETIMEOFFSET(7)), '+15:00') AS value"],
  ['attach null source valid zone', "SELECT TODATETIMEOFFSET(CAST(NULL AS DATETIME2(7)), '+01:00') AS value"],
  ['attach null source invalid zone', "SELECT TODATETIMEOFFSET(CAST(NULL AS DATETIME2(7)), '+15:00') AS value"],
  ['switch bad source and zone', "SELECT SWITCHOFFSET(CAST('not-a-date' AS DATETIMEOFFSET(7)), '+15:00') AS value"],
  ['attach bad source and zone', "SELECT TODATETIMEOFFSET(CAST('not-a-date' AS DATETIME2(7)), '+15:00') AS value"],
  ['switch bad source and type', "SELECT SWITCHOFFSET(CAST('not-a-date' AS DATETIMEOFFSET(7)), CAST(1.25 AS DECIMAL(8,2))) AS value"],
  ['attach bad source and type', "SELECT TODATETIMEOFFSET(CAST('not-a-date' AS DATETIME2(7)), CAST(1.25 AS DECIMAL(8,2))) AS value"],
  ['switch source varchar', "SELECT SWITCHOFFSET('2024-02-29T12:34:56+02:30', '+01:00') AS value"],
  ['attach source varchar', "SELECT TODATETIMEOFFSET('2024-02-29T12:34:56', '+01:00') AS value"],
]) add(name, sql)

function rpc(connection, sql, parameters) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      for (const [name, type, value, options] of parameters) request.addParameter(name, type, value, options)
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

function resultListeners(connection, request, result) {
  const onError = error => result.errors.push({ number: error.number, state: error.state, class: error.class, lineNumber: error.lineNumber, message: error.message })
  const onInfo = info => result.info.push({ number: info.number, state: info.state, class: info.class, lineNumber: info.lineNumber, message: info.message })
  const onMetadata = metadata => result.sets.push({
    columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })), rows: [],
  })
  const onRow = row => result.sets.at(-1).rows.push(row.map(c => c.value))
  const onDone = Object.fromEntries(['done', 'doneInProc', 'doneProc'].map(kind => [kind, (rowCount, more, status) => {
    result.done.push({ kind, rowCount: rowCount ?? null, more })
    if (kind === 'doneProc') result.returnStatus = status
  }]))
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  request.on('columnMetadata', onMetadata)
  request.on('row', onRow)
  for (const [kind, listener] of Object.entries(onDone)) request.on(kind, listener)
  return () => {
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
    request.off('columnMetadata', onMetadata)
    request.off('row', onRow)
    for (const [kind, listener] of Object.entries(onDone)) request.off(kind, listener)
  }
}

async function prepared(connection, name, sql, values) {
  let complete = () => {}
  const request = new Request(sql, (...args) => complete(...args))
  request.addParameter('source', TYPES.NVarChar, undefined, { length: 40 })
  request.addParameter('zone', TYPES.Int)
  await new Promise((resolve, reject) => {
    request.once('prepared', resolve)
    request.once('error', reject)
    connection.prepare(request)
  })
  const executions = []
  try {
    for (const [source, zone] of values) {
      const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
      const detach = resultListeners(connection, request, result)
      try {
        await new Promise(resolve => {
          complete = (error, rowCount) => {
            result.rowCount = rowCount
            if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          }
          request.error = undefined
          connection.execute(request, { source, zone })
        })
      } finally { detach() }
      executions.push({ source, zone, result: canonical(result) })
    }
  } finally {
    await new Promise((resolve, reject) => {
      complete = error => error ? reject(error) : resolve()
      request.error = undefined
      connection.unprepare(request)
    })
  }
  return { name, sql, executions }
}

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    const result = canonical(await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql)))
    records.push({ name, sql, ...(parameters ? { parameters: parameters.map(([parameter, type, value, options]) => ({
      name: parameter, type: type.name, value, ...(options ? { options } : {}),
    })) } : {}), result })
  }
  for (const [name, sql] of cases) await record(name, sql)
  await record('create source', 'CREATE TABLE dbo.offset_source (id INT PRIMARY KEY, dto DATETIMEOFFSET(7) NULL, dt2 DATETIME2(3) NULL, minutes INT NULL, zone NVARCHAR(12) NULL)')
  await record('insert source', "INSERT dbo.offset_source VALUES (1,'2024-02-29T12:34:56.1234567+02:30','2024-02-29T12:34:56.123',-300,N'+05:45'),(2,'2024-01-01T00:00:00-14:00','2024-01-01T00:00:00',840,N'-14:00'),(3,NULL,NULL,NULL,NULL)")
  await record('switch source integer column', 'SELECT id,SWITCHOFFSET(dto,minutes) AS value,CONVERT(NVARCHAR(50),SWITCHOFFSET(dto,minutes),127) AS rendered,DATEPART(tzoffset,SWITCHOFFSET(dto,minutes)) AS offset_minutes FROM dbo.offset_source ORDER BY id')
  await record('switch source text column', 'SELECT id,SWITCHOFFSET(dto,zone) AS value,CONVERT(NVARCHAR(50),SWITCHOFFSET(dto,zone),127) AS rendered,DATEPART(tzoffset,SWITCHOFFSET(dto,zone)) AS offset_minutes FROM dbo.offset_source ORDER BY id')
  await record('attach source integer column', 'SELECT id,TODATETIMEOFFSET(dt2,minutes) AS value,CONVERT(NVARCHAR(50),TODATETIMEOFFSET(dt2,minutes),127) AS rendered,DATEPART(tzoffset,TODATETIMEOFFSET(dt2,minutes)) AS offset_minutes FROM dbo.offset_source ORDER BY id')
  await record('attach source text column', 'SELECT id,TODATETIMEOFFSET(dt2,zone) AS value,CONVERT(NVARCHAR(50),TODATETIMEOFFSET(dt2,zone),127) AS rendered,DATEPART(tzoffset,TODATETIMEOFFSET(dt2,zone)) AS offset_minutes FROM dbo.offset_source ORDER BY id')
  await record('switch empty source', 'SELECT SWITCHOFFSET(dto,minutes) AS value,CONVERT(NVARCHAR(50),SWITCHOFFSET(dto,minutes),127) AS rendered,DATEPART(tzoffset,SWITCHOFFSET(dto,minutes)) AS offset_minutes FROM dbo.offset_source WHERE 1=0')
  await record('attach empty source', 'SELECT TODATETIMEOFFSET(dt2,minutes) AS value,CONVERT(NVARCHAR(50),TODATETIMEOFFSET(dt2,minutes),127) AS rendered,DATEPART(tzoffset,TODATETIMEOFFSET(dt2,minutes)) AS offset_minutes FROM dbo.offset_source WHERE 1=0')
  for (const [name, source, zone] of [
    ['switch RPC first', '2024-02-29T12:34:56.1234567+02:30', -300],
    ['switch RPC null source', null, 90],
    ['switch RPC invalid zone', '2024-02-29T12:34:56.1234567+02:30', 841],
    ['switch RPC replay', '2024-02-29T12:34:56.1234567+02:30', -300],
  ]) await record(name, withRendered('SELECT SWITCHOFFSET(CAST(@source AS DATETIMEOFFSET(7)),@zone) AS value'), [['source', TYPES.NVarChar, source, { length: 40 }], ['zone', TYPES.Int, zone]])
  for (const [name, source, zone] of [
    ['attach RPC first', '2024-02-29T12:34:56.1234567', -300],
    ['attach RPC null zone', '2024-02-29T12:34:56.1234567', null],
    ['attach RPC invalid zone', '2024-02-29T12:34:56.1234567', 841],
    ['attach RPC replay', '2024-02-29T12:34:56.1234567', -300],
  ]) await record(name, withRendered('SELECT TODATETIMEOFFSET(CAST(@source AS DATETIME2(7)),@zone) AS value'), [['source', TYPES.NVarChar, source, { length: 40 }], ['zone', TYPES.Int, zone]])
  const preparedResults = []
  preparedResults.push(await prepared(connection, 'switch prepared reuse', withRendered('SELECT SWITCHOFFSET(CAST(@source AS DATETIMEOFFSET(7)),@zone) AS value'), [
    ['2024-02-29T12:34:56.1234567+02:30', -300], [null, 90], ['2024-02-29T12:34:56.1234567+02:30', 841], ['2024-02-29T12:34:56.1234567+02:30', -300],
  ]))
  preparedResults.push(await prepared(connection, 'attach prepared reuse', withRendered('SELECT TODATETIMEOFFSET(CAST(@source AS DATETIME2(7)),@zone) AS value'), [
    ['2024-02-29T12:34:56.1234567', -300], ['2024-02-29T12:34:56.1234567', null], ['2024-02-29T12:34:56.1234567', 841], ['2024-02-29T12:34:56.1234567', -300],
  ]))
  return { records, prepared: preparedResults }
}

function validate(run) {
  assert.equal(run.records.length, cases.length + 2 + 6 + 8)
  assert.equal(new Set(run.records.map(record => record.name)).size, run.records.length)
  for (const record of run.records) assert(record.result.done.length > 0, `${record.name}: no completion`)
  for (const entry of run.prepared) {
    assert.equal(entry.executions.length, 4)
    for (const execution of entry.executions) assert(execution.result.done.length > 0, `${entry.name}: no completion`)
    assert.deepEqual(entry.executions[0].result, entry.executions[3].result, `${entry.name}: replay differs`)
  }
  const get = name => run.records.find(record => record.name === name)?.result
  for (let scale = 0; scale <= 7; scale++) {
    for (const functionName of ['switch', 'attach']) {
      const result = get(`${functionName} scale ${scale} integer`)
      assert.deepEqual(result.errors, [], `${functionName} scale ${scale}`)
      assert.equal(result.sets[0].columns[0].type, 'DateTimeOffset')
      assert.equal(result.sets[0].columns[0].scale, scale)
      assert.equal(result.sets[0].rows[0][2], -300)
    }
  }
  for (const name of ['switch positive 841 integer', 'attach positive 841 integer', 'switch text invalid minute', 'attach text invalid minute']) {
    assert.equal(get(name).errors[0]?.number, 9812, name)
  }
  for (const name of ['switch boundary minimum switch local overflow', 'switch boundary maximum switch local overflow', 'attach boundary minimum UTC overflow', 'attach boundary maximum UTC overflow']) {
    assert.equal(get(name).errors[0]?.number, 9813, name)
  }
  assert.equal(get('switch bad source and zone').errors[0]?.number, 241)
  assert.equal(get('attach bad source and zone').errors[0]?.number, 241)
  assert.deepEqual(get('switch null source invalid zone').sets[0].rows, [[null, null, null]])
  assert.deepEqual(get('attach null source invalid zone').sets[0].rows, [[null, null, null]])
  assert.equal(get('switch decimal fraction').sets[0].rows[0][2], 90)
  assert.equal(get('switch money fraction').sets[0].rows[0][2], 91)
  assert.equal(get('switch source integer column').sets[0].rows.length, 3)
  assert.equal(get('attach source integer column').sets[0].rows.length, 3)
  assert.equal(get('switch empty source').sets[0].columns[0].type, 'DateTimeOffset')
  assert.equal(get('attach empty source').sets[0].columns[0].type, 'DateTimeOffset')
}

await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < 2; containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let databaseIndex = 0; databaseIndex < 2; databaseIndex++) {
      const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, observe)
      validate(run)
      runs.push(run)
    }
    assert.deepEqual(runs[0], runs[1], 'offset-function observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
assert.deepEqual(containers[0].runs[0], containers[1].runs[0], 'offset-function observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (retained) assert.deepEqual(actual, retained, 'offset-function observations differ from retained fixture')
if (writeFixture) {
  if (retained) throw new Error('refusing to overwrite retained fixture')
  await writeFile(fixture, JSON.stringify(actual) + '\n')
}
console.log(`Captured ${containers[0].runs[0].records.length} offset-function programs and ${containers[0].runs[0].prepared.length} prepared programs in four fresh databases across two containers${retained ? ' and matched retained fixture' : ''}`)
