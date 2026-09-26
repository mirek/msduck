#!/usr/bin/env node
// Retain first-party SQL Server temporal constructor rows, descriptors and tokens.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'

const fixture = new URL('../reference/temporal-parts.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/compatibility/temporal-parts/capture.json')
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (writeFixture && retained) throw new Error('refusing to overwrite retained fixture')

const time = (hour = 12, minute = 34, second = 56, fraction = 123, precision = 3) =>
  `TIMEFROMPARTS(${hour},${minute},${second},${fraction},${precision})`
const date2 = (year = 2024, month = 2, day = 29, hour = 12, minute = 34, second = 56, fraction = 123, precision = 3) =>
  `DATETIME2FROMPARTS(${year},${month},${day},${hour},${minute},${second},${fraction},${precision})`
const offset = (year = 2024, month = 2, day = 29, hour = 12, minute = 34, second = 56, fraction = 123, zoneHour = 5, zoneMinute = 30, precision = 3) =>
  `DATETIMEOFFSETFROMPARTS(${year},${month},${day},${hour},${minute},${second},${fraction},${zoneHour},${zoneMinute},${precision})`

const cases = []
const add = (name, expression) => cases.push([name, `SELECT ${expression} AS value`])
const addEmpty = (name, expression) => cases.push([name, `SELECT ${expression} AS value WHERE 1=0`])

for (let precision = 0; precision <= 7; precision++) {
  const maximum = 10 ** precision - 1
  for (const [name, constructor] of [
    ['time', fraction => time(23, 59, 59, fraction, precision)],
    ['datetime2', fraction => date2(2000, 2, 29, 23, 59, 59, fraction, precision)],
    ['datetimeoffset', fraction => offset(2000, 2, 29, 23, 59, 59, fraction, -5, -30, precision)],
  ]) {
    add(`${name} scale ${precision} maximum fraction`, constructor(maximum))
    addEmpty(`${name} scale ${precision} empty metadata`, constructor(maximum))
    add(`${name} scale ${precision} invalid fraction`, constructor(10 ** precision))
  }
}

for (const [name, expression] of [
  ['time midnight', time(0, 0, 0, 0, 7)],
  ['time maximum tick', time(23, 59, 59, 9999999, 7)],
  ['time null hour', time('NULL')],
  ['time null minute', time(1, 'NULL')],
  ['time null second', time(1, 2, 'NULL')],
  ['time null fraction', time(1, 2, 3, 'NULL')],
  ['time invalid hour', time(24)],
  ['time invalid minute', time(1, 60)],
  ['time invalid second', time(1, 2, 60)],
  ['time negative hour', time(-1)],
  ['time null and invalid', time('NULL', 60)],
  ['datetime2 first instant', date2(1, 1, 1, 0, 0, 0, 0, 7)],
  ['datetime2 last instant', date2(9999, 12, 31, 23, 59, 59, 9999999, 7)],
  ['datetime2 non-leap day', date2(1900, 2, 29)],
  ['datetime2 leap day', date2(2000, 2, 29)],
  ['datetime2 year zero', date2(0)],
  ['datetime2 year ten thousand', date2(10000)],
  ['datetime2 month zero', date2(2024, 0)],
  ['datetime2 day zero', date2(2024, 1, 0)],
  ['datetime2 day beyond month', date2(2024, 4, 31)],
  ['datetime2 null year', date2('NULL')],
  ['datetime2 null month', date2(2024, 'NULL')],
  ['datetime2 null day', date2(2024, 1, 'NULL')],
  ['datetime2 null hour', date2(2024, 1, 1, 'NULL')],
  ['datetime2 null fraction', date2(2024, 1, 1, 0, 0, 0, 'NULL')],
  ['datetime2 null and invalid', date2('NULL', 13)],
  ['offset positive fourteen', offset(2024, 1, 1, 14, 0, 0, 0, 14, 0, 7)],
  ['offset negative fourteen', offset(2024, 1, 1, 0, 0, 0, 0, -14, 0, 7)],
  ['offset negative zero hour', offset(2024, 1, 1, 0, 0, 0, 0, 0, -30, 7)],
  ['offset positive zero hour', offset(2024, 1, 1, 0, 0, 0, 0, 0, 30, 7)],
  ['offset positive fourteen plus minute', offset(2024, 1, 1, 14, 0, 0, 0, 14, 1, 7)],
  ['offset negative fourteen minus minute', offset(2024, 1, 1, 0, 0, 0, 0, -14, -1, 7)],
  ['offset mixed positive negative', offset(2024, 1, 1, 0, 0, 0, 0, 5, -30, 7)],
  ['offset mixed negative positive', offset(2024, 1, 1, 0, 0, 0, 0, -5, 30, 7)],
  ['offset hour fifteen', offset(2024, 1, 1, 0, 0, 0, 0, 15, 0, 7)],
  ['offset minute sixty', offset(2024, 1, 1, 0, 0, 0, 0, 0, 60, 7)],
  ['offset null hour', offset(2024, 1, 1, 0, 0, 0, 0, 'NULL', 0, 7)],
  ['offset null minute', offset(2024, 1, 1, 0, 0, 0, 0, 0, 'NULL', 7)],
  ['offset null day', offset(2024, 1, 'NULL', 0, 0, 0, 0, 0, 0, 7)],
  ['offset invalid UTC before year one', offset(1, 1, 1, 0, 0, 0, 0, 14, 0, 7)],
  ['offset invalid UTC after year 9999', offset(9999, 12, 31, 23, 59, 59, 9999999, -14, 0, 7)],
  ['offset valid UTC lower boundary', offset(1, 1, 1, 14, 0, 0, 0, 14, 0, 7)],
  ['offset valid UTC upper boundary', offset(9999, 12, 31, 9, 59, 59, 9999999, -14, 0, 7)],
]) add(name, expression)

for (const [name, expression] of [
  ['offset positive fourteen text', offset(2024, 1, 1, 14, 0, 0, 0, 14, 0, 7)],
  ['offset negative fourteen text', offset(2024, 1, 1, 0, 0, 0, 0, -14, 0, 7)],
  ['offset negative zero hour text', offset(2024, 1, 1, 0, 0, 0, 0, 0, -30, 7)],
  ['offset positive zero hour text', offset(2024, 1, 1, 0, 0, 0, 0, 0, 30, 7)],
  ['offset maximum fraction text', offset(2000, 2, 29, 23, 59, 59, 9999999, -5, -30, 7)],
]) {
  add(name, `CONVERT(VARCHAR(40),${expression},127)`)
  add(name.replace(' text', ' zone'), `DATENAME(tz,${expression})`)
}

for (const [name, expression] of [
  ['time invalid precision and component', time(24, 0, 0, 0, 8)],
  ['datetime2 invalid precision and component', date2(2024, 13, 1, 0, 0, 0, 0, 8)],
  ['offset invalid precision and component', offset(2024, 1, 1, 0, 0, 0, 0, 15, 0, 8)],
  ['time invalid conversion and precision', time("'nope'", 0, 0, 0, 8)],
  ['datetime2 invalid conversion and precision', date2("'nope'", 1, 1, 0, 0, 0, 0, 8)],
  ['offset invalid conversion and precision', offset("'nope'", 1, 1, 0, 0, 0, 0, 0, 0, 8)],
]) add(name, expression)

for (const [name, make] of [
  ['time', precision => time(1, 2, 3, 0, precision)],
  ['datetime2', precision => date2(2024, 1, 1, 1, 2, 3, 0, precision)],
  ['datetimeoffset', precision => offset(2024, 1, 1, 1, 2, 3, 0, 0, 0, precision)],
]) {
  for (const [label, precision] of [
    ['arithmetic', '1+2'], ['division', '8/2'], ['bitwise', '7&3'],
    ['parentheses', '(2+3)'], ['cast', 'CAST(3 AS INT)'],
    ['null', 'NULL'], ['negative', '-1'], ['out of range', '8'],
    ['decimal', '3.0'], ['float', '3E0'], ['string', "'3'"],
    ['divide by zero', '1/0'], ['overflow', '2147483647+1'],
  ]) add(`${name} precision ${label}`, make(precision))
  cases.push([`${name} precision variable`, `DECLARE @p INT=3; SELECT ${make('@p')} AS value`])
  cases.push([`${name} precision missing arguments`, `SELECT ${name === 'time' ? 'TIMEFROMPARTS(1,2,3,0)' : name === 'datetime2' ? 'DATETIME2FROMPARTS(2024,1,1,1,2,3,0)' : 'DATETIMEOFFSETFROMPARTS(2024,1,1,1,2,3,0,0,0)'} AS value`])
  cases.push([`${name} precision extra arguments`, `SELECT ${name === 'time' ? 'TIMEFROMPARTS(1,2,3,0,3,4)' : name === 'datetime2' ? 'DATETIME2FROMPARTS(2024,1,1,1,2,3,0,3,4)' : 'DATETIMEOFFSETFROMPARTS(2024,1,1,1,2,3,0,0,0,3,4)'} AS value`])
}

for (const [name, expression] of [
  ['time string integer', time("'12'", "'34'", "'56'", "'123'")],
  ['time decimal integer', time('12.9', '34.9', '56.9', '123.9')],
  ['time float integer', time('CAST(12.9 AS FLOAT)', 34, 56, 123)],
  ['time invalid string', time("'nope'")],
  ['time bigint overflow', time('CAST(2147483648 AS BIGINT)')],
  ['time argument conversion before null', time("'nope'", 'NULL')],
  ['datetime2 string integer', date2("'2024'", "'2'", "'29'", "'12'", "'34'", "'56'", "'123'")],
  ['datetime2 decimal integer', date2('2024.9', '2.9', '29.9', '12.9', '34.9', '56.9', '123.9')],
  ['datetime2 invalid string', date2("'nope'")],
  ['datetime2 argument conversion before null', date2("'nope'", 'NULL')],
  ['offset string integer', offset("'2024'", "'2'", "'29'", "'12'", "'34'", "'56'", "'123'", "'5'", "'30'")],
  ['offset decimal integer', offset('2024.9', '2.9', '29.9', '12.9', '34.9', '56.9', '123.9', '5.9', '30.9')],
  ['offset invalid string', offset("'nope'")],
  ['offset argument conversion before null', offset("'nope'", 'NULL')],
]) add(name, expression)

function rpc(connection, sql, parameters) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch: request => {
      for (const [name, type, value] of parameters) request.addParameter(name, type, value)
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

async function preparedSeries(connection, name, sql, parameterName, values) {
  const records = []
  let current
  let complete = () => {}
  const request = new Request(sql, (error, rowCount) => complete(error, rowCount))
  request.addParameter(parameterName, TYPES.Int)
  const diagnostic = item => ({ number: item.number, state: item.state, class: item.class, lineNumber: item.lineNumber, message: item.message })
  const onError = item => current?.errors.push(diagnostic(item))
  const onInfo = item => current?.info.push(diagnostic(item))
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  request.on('columnMetadata', columns => current?.sets.push({
    columns: columns.map(column => ({ name: column.colName, type: column.type.name,
      length: column.dataLength ?? null, precision: column.precision ?? null,
      scale: column.scale ?? null, flags: column.flags, collation: canonical(column.collation ?? null) })),
    rows: [],
  }))
  request.on('row', row => current.sets.at(-1).rows.push(row.map(column => column.value)))
  for (const kind of ['done', 'doneInProc', 'doneProc']) {
    request.on(kind, (rowCount, more) => current?.done.push({ kind, rowCount: rowCount ?? null, more }))
  }
  request.on('doneProc', (_rowCount, _more, status) => { if (current) current.returnStatus = status })
  try {
    await new Promise((resolve, reject) => {
      complete = error => { if (error) reject(error) }
      request.once('prepared', resolve)
      request.once('error', reject)
      connection.prepare(request)
    })
    for (const [label, value] of values) {
      current = { sets: [], done: [], errors: [], info: [], returnStatus: null }
      await new Promise(resolve => {
        complete = (error, rowCount) => {
          current.rowCount = rowCount
          if (error && !current.errors.length) current.errors.push({ message: error.message, number: error.number ?? null })
          resolve()
        }
        request.error = undefined
        connection.execute(request, { [parameterName]: value })
      })
      records.push({ name: `${name} prepared ${label}`, sql, parameters: [{ name: parameterName, type: 'Int', value }], result: canonical(current) })
      current = undefined
    }
    await new Promise((resolve, reject) => {
      complete = error => error ? reject(error) : resolve()
      connection.unprepare(request)
    })
  } finally {
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
  return records
}

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    const result = canonical(await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql)))
    records.push({ name, sql, ...(parameters ? {
      parameters: parameters.map(([parameter, type, value]) => ({ name: parameter, type: type.name, value })),
    } : {}), result })
  }
  await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
  for (const [name, sql] of cases) await record(name, sql)
  for (const [name, expression] of [
    ['time', time('@hour', 2, 3, 123)],
    ['datetime2', date2('@year', 2, 29, 1, 2, 3, 123)],
    ['datetimeoffset', offset('@year', 2, 29, 1, 2, 3, 123, 5, 30)],
  ]) {
    const parameterName = name === 'time' ? 'hour' : 'year'
    for (const [label, value] of [['first', name === 'time' ? 12 : 2024], ['null', null], ['invalid', name === 'time' ? 24 : 1900], ['reuse', name === 'time' ? 12 : 2024]]) {
      await record(`${name} bound ${label}`, `SELECT ${expression} AS value`, [[parameterName, TYPES.Int, value]])
    }
    await record(`${name} bound precision`, `SELECT ${name === 'time' ? time(1, 2, 3, 0, '@p') : name === 'datetime2' ? date2(2024, 1, 1, 1, 2, 3, 0, '@p') : offset(2024, 1, 1, 1, 2, 3, 0, 0, 0, '@p')} AS value`, [['p', TYPES.Int, 3]])
    const prepared = await preparedSeries(connection, name,
      `SELECT ${expression} AS value`, parameterName,
      [['first', name === 'time' ? 12 : 2024], ['null', null], ['invalid', name === 'time' ? 24 : 1900], ['reuse', name === 'time' ? 12 : 2024]])
    records.push(...prepared)
  }
  return records
}

function validate(run) {
  assert.equal(run.length, cases.length + 1 + 27)
  const get = name => {
    const result = run.find(record => record.name === name)?.result
    assert(result, `missing ${name}`)
    return result
  }
  assert.equal(get('server identity').sets[0].rows[0][0], '17.0.4065.4')
  for (const [name, type, state] of [['time', 'Time', 2], ['datetime2', 'DateTime2', 5], ['datetimeoffset', 'DateTimeOffset', 6]]) {
    for (let scale = 0; scale <= 7; scale++) {
      const value = get(`${name} scale ${scale} maximum fraction`)
      const empty = get(`${name} scale ${scale} empty metadata`)
      assert.deepEqual(value.errors, [], `${name} scale ${scale}`)
      assert.equal(value.sets[0].columns[0].type, type)
      assert.equal(value.sets[0].columns[0].scale, scale)
      assert.equal(empty.sets[0].columns[0].type, type)
      assert.equal(empty.sets[0].columns[0].scale, scale)
      assert.deepEqual(empty.sets[0].rows, [])
      assert.equal(get(`${name} scale ${scale} invalid fraction`).errors[0]?.state, state)
    }
    assert.equal(get(`${name} precision null`).errors[0]?.number, 10760)
    assert.equal(get(`${name} precision out of range`).errors[0]?.number, 1002)
    assert.deepEqual(get(`${name} bound first`).sets, get(`${name} bound reuse`).sets)
    assert.deepEqual(get(`${name} prepared first`).sets, get(`${name} prepared reuse`).sets)
  }
  assert.deepEqual(get('offset negative zero hour zone').sets[0].rows, [['-00:30']])
  assert.deepEqual(get('offset positive zero hour zone').sets[0].rows, [['+00:30']])
  for (const record of run) assert(record.result.done.length > 0, `${record.name}: no completion`)
}

await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
      const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, observe)
      validate(run)
      runs.push(run)
    }
    if (!oneDatabase) assert.deepEqual(runs[0], runs[1], 'temporal constructor observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) assert.deepEqual(containers[0].runs[0], containers[1].runs[0], 'temporal constructor observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
if (retained && !oneDatabase) assert.deepEqual(actual, retained, 'temporal constructor observations differ from retained fixture')
if (writeFixture) {
  await writeFile(fixture, JSON.stringify(actual) + '\n')
}
console.log(`Captured ${containers[0].runs[0].length} temporal constructor observations` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained && !oneDatabase ? ' and matched retained fixture' : ''))
