#!/usr/bin/env node
// Owner-controlled SQL Server evidence for legacy DATEADD inputs and literals.
import assert from 'node:assert/strict'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/dateadd-legacy.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/dateadd-legacy/capture.json')

async function canonicalTarget(path) {
  let current = path
  const missing = []
  while (true) {
    try { return resolve(await realpath(current), ...missing.reverse()) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    const parent = dirname(current)
    if (parent === current) throw Error('cannot resolve output path')
    missing.push(basename(current)); current = parent
  }
}
async function assertSeparateOutput() {
  for (let part = output; ; part = dirname(part)) {
    let info
    try { info = await lstat(part) } catch (error) { if (error.code !== 'ENOENT') throw error }
    assert.ok(!info?.isSymbolicLink(), 'output path contains a symlink')
    if (dirname(part) === part) break
  }
  const retained = fileURLToPath(fixture)
  assert.notEqual(await canonicalTarget(output), await canonicalTarget(retained), 'output aliases retained fixture')
  let outputFile, retainedFile
  try { outputFile = await stat(output) } catch (error) { if (error.code !== 'ENOENT') throw error }
  try { retainedFile = await stat(retained) } catch (error) { if (error.code !== 'ENOENT') throw error }
  assert.ok(!outputFile || !retainedFile || outputFile.dev !== retainedFile.dev || outputFile.ino !== retainedFile.ino, 'output hard-links retained fixture')
  assert.ok(!outputFile, 'refusing to overwrite capture output')
}

const literalCases = [
  ['datetime month end', "SELECT DATEADD(month,1,CAST('2024-01-31T23:59:59.997' AS DATETIME)) AS value"],
  ['datetime leap day', "SELECT DATEADD(year,1,CAST('2024-02-29T12:34:56.003' AS DATETIME)) AS value"],
  ['datetime millisecond one', "SELECT DATEADD(millisecond,1,CAST('2024-01-01T00:00:00.000' AS DATETIME)) AS value"],
  ['datetime millisecond two', "SELECT DATEADD(millisecond,2,CAST('2024-01-01T00:00:00.000' AS DATETIME)) AS value"],
  ['datetime millisecond three', "SELECT DATEADD(millisecond,3,CAST('2024-01-01T00:00:00.000' AS DATETIME)) AS value"],
  ['datetime negative millisecond', "SELECT DATEADD(millisecond,-1,CAST('2024-01-01T00:00:00.000' AS DATETIME)) AS value"],
  ['datetime second rollover', "SELECT DATEADD(second,1,CAST('2024-01-01T23:59:59.997' AS DATETIME)) AS value"],
  ['datetime microsecond rejected', "SELECT DATEADD(microsecond,1,CAST('2024-01-01' AS DATETIME)) AS value"],
  ['datetime nanosecond rejected', "SELECT DATEADD(nanosecond,100,CAST('2024-01-01' AS DATETIME)) AS value"],
  ['datetime NULL', 'SELECT DATEADD(day,1,CAST(NULL AS DATETIME)) AS value'],
  ['datetime empty', "SELECT DATEADD(day,1,CAST('2024-01-01' AS DATETIME)) AS value WHERE 1=0"],
  ['datetime upper overflow', "SELECT DATEADD(day,1,CAST('9999-12-31T23:59:59.997' AS DATETIME)) AS value"],
  ['datetime lower overflow', "SELECT DATEADD(day,-1,CAST('1753-01-01' AS DATETIME)) AS value"],
  ['datetime bigint amount', "SELECT DATEADD(day,2147483648,CAST('2024-01-01' AS DATETIME)) AS value"],
  ['datetime beyond bigint amount', "SELECT DATEADD(day,9223372036854775808,CAST('2024-01-01' AS DATETIME)) AS value"],
  ['datetime fractional amount', "SELECT DATEADD(day,-1.9,CAST('2024-01-01' AS DATETIME)) AS value"],
  ['smalldatetime month end', "SELECT DATEADD(month,1,CAST('2024-01-31T23:59:00' AS SMALLDATETIME)) AS value"],
  ['smalldatetime seconds -31', "SELECT DATEADD(second,-31,CAST('2024-01-01T12:00:00' AS SMALLDATETIME)) AS value"],
  ['smalldatetime seconds -30', "SELECT DATEADD(second,-30,CAST('2024-01-01T12:00:00' AS SMALLDATETIME)) AS value"],
  ['smalldatetime seconds 29', "SELECT DATEADD(second,29,CAST('2024-01-01T12:00:00' AS SMALLDATETIME)) AS value"],
  ['smalldatetime seconds 30', "SELECT DATEADD(second,30,CAST('2024-01-01T12:00:00' AS SMALLDATETIME)) AS value"],
  ['smalldatetime ms -30002', "SELECT DATEADD(millisecond,-30002,CAST('2024-01-01T12:00:00' AS SMALLDATETIME)) AS value"],
  ['smalldatetime ms -30001', "SELECT DATEADD(millisecond,-30001,CAST('2024-01-01T12:00:00' AS SMALLDATETIME)) AS value"],
  ['smalldatetime ms 29998', "SELECT DATEADD(millisecond,29998,CAST('2024-01-01T12:00:00' AS SMALLDATETIME)) AS value"],
  ['smalldatetime ms 29999', "SELECT DATEADD(millisecond,29999,CAST('2024-01-01T12:00:00' AS SMALLDATETIME)) AS value"],
  ['smalldatetime microsecond rejected', "SELECT DATEADD(microsecond,1,CAST('2024-01-01' AS SMALLDATETIME)) AS value"],
  ['smalldatetime NULL', 'SELECT DATEADD(day,1,CAST(NULL AS SMALLDATETIME)) AS value'],
  ['smalldatetime empty', "SELECT DATEADD(day,1,CAST('2024-01-01' AS SMALLDATETIME)) AS value WHERE 1=0"],
  ['smalldatetime upper overflow', "SELECT DATEADD(day,1,CAST('2079-06-06T23:59:00' AS SMALLDATETIME)) AS value"],
  ['smalldatetime lower overflow', "SELECT DATEADD(day,-1,CAST('1900-01-01' AS SMALLDATETIME)) AS value"],
  ['literal datetime return', "SELECT DATEADD(day,1,'2024-01-31T12:34:56.123') AS value"],
  ['literal month end', "SELECT DATEADD(month,1,'2024-01-31') AS value"],
  ['literal four fractional digits', "SELECT DATEADD(day,1,'2024-01-01T12:34:56.1234') AS value"],
  ['literal offset rejected', "SELECT DATEADD(day,1,'2024-01-01T12:34:56+01:00') AS value"],
  ['literal NULL', 'SELECT DATEADD(day,1,NULL) AS value'],
  ['literal empty', "SELECT DATEADD(day,1,'2024-01-01') AS value WHERE 1=0"],
  ['literal upper overflow', "SELECT DATEADD(day,1,'9999-12-31') AS value"],
]

function rpc(connection, sql, parameters) {
  const transport = {
    on: (...args_) => connection.on(...args_), off: (...args_) => connection.off(...args_),
    execSqlBatch: request => {
      for (const [name, type, value, options] of parameters) request.addParameter(name, type, value, options)
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    const result = canonical(await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql)))
    records.push({ name, sql, ...(parameters ? { parameters: parameters.map(([name_, type, value, options]) => ({ name: name_, type: type.name, value: canonical(value), ...(options ? { options } : {}) })) } : {}), result })
  }
  await record('server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version")
  for (const [name, sql] of literalCases) await record(name, sql)
  await record('create legacy sources', 'CREATE TABLE dbo.legacy_dateadd(id INT,d DATETIME,s SMALLDATETIME)')
  await record('insert legacy sources', "INSERT dbo.legacy_dateadd VALUES(1,'2024-01-31T23:59:59.997','2024-01-31T23:59:00'),(2,NULL,NULL),(3,'1753-01-01','1900-01-01')")
  await record('datetime stored source', 'SELECT id,DATEADD(month,1,d) AS value FROM dbo.legacy_dateadd ORDER BY id')
  await record('smalldatetime stored source', 'SELECT id,DATEADD(second,30,s) AS value FROM dbo.legacy_dateadd ORDER BY id')
  await record('datetime stored empty', 'SELECT DATEADD(day,1,d) AS value FROM dbo.legacy_dateadd WHERE 1=0')
  await record('smalldatetime stored empty', 'SELECT DATEADD(day,1,s) AS value FROM dbo.legacy_dateadd WHERE 1=0')
  for (const [name, type, value] of [
    ['rpc datetime', TYPES.DateTime, new Date('2024-01-31T23:59:59.997Z')],
    ['rpc datetime null', TYPES.DateTime, null],
    ['rpc smalldatetime', TYPES.SmallDateTime, new Date('2024-01-01T12:00:00.000Z')],
    ['rpc smalldatetime null', TYPES.SmallDateTime, null],
  ]) await record(name, 'SELECT DATEADD(month,1,@d) AS month_value,DATEADD(second,30,@d) AS seconds_value', [['d', type, value]])
  return records
}

function validate(run) {
  assert.equal(run.length, 1 + literalCases.length + 2 + 4 + 4)
  for (const item of run) assert.ok(item.result.done.length > 0, `${item.name}: missing completion`)
  const get = name => {
    const item = run.find(entry => entry.name === name)
    assert.ok(item, `missing ${name}`)
    return item.result
  }
  for (const name of ['datetime month end', 'smalldatetime month end', 'literal datetime return']) {
    assert.equal(get(name).errors.length, 0, name)
    assert.equal(get(name).sets[0].rows.length, 1, name)
  }
  const date = value => ({ kind: 'date', value })
  for (const [name, value] of [
    ['datetime millisecond one', '2024-01-01T00:00:00.000Z'],
    ['datetime millisecond two', '2024-01-01T00:00:00.003Z'],
    ['datetime millisecond three', '2024-01-01T00:00:00.003Z'],
    ['datetime negative millisecond', '2024-01-01T00:00:00.000Z'],
    ['smalldatetime seconds -31', '2024-01-01T11:59:00.000Z'],
    ['smalldatetime seconds -30', '2024-01-01T12:00:00.000Z'],
    ['smalldatetime seconds 29', '2024-01-01T12:00:00.000Z'],
    ['smalldatetime seconds 30', '2024-01-01T12:01:00.000Z'],
    ['smalldatetime ms -30002', '2024-01-01T11:59:00.000Z'],
    ['smalldatetime ms -30001', '2024-01-01T12:00:00.000Z'],
    ['smalldatetime ms 29998', '2024-01-01T12:00:00.000Z'],
    ['smalldatetime ms 29999', '2024-01-01T12:01:00.000Z'],
  ]) assertSameCapture(get(name).sets[0].rows, [[date(value)]], name)
  for (const [name, length] of [
    ['datetime empty', 8], ['smalldatetime empty', 4], ['literal empty', 8],
    ['datetime stored empty', 8], ['smalldatetime stored empty', 4],
    ['rpc datetime null', 8], ['rpc smalldatetime null', 4],
  ]) assert.equal(get(name).sets[0].columns[0].length, length, `${name}: legacy wire width`)
  for (const name of ['datetime NULL', 'smalldatetime NULL', 'literal NULL']) {
    assertSameCapture(get(name).sets[0].rows, [[null]], name)
  }
  for (const name of ['datetime empty', 'smalldatetime empty', 'literal empty']) {
    assert.equal(get(name).sets[0].rows.length, 0, name)
    assert.equal(get(name).sets[0].columns.length, 1, name)
  }
  for (const name of ['datetime microsecond rejected', 'datetime nanosecond rejected', 'smalldatetime microsecond rejected']) {
    assert.equal(get(name).errors[0]?.number, 9810, name)
  }
  for (const name of ['datetime upper overflow', 'datetime lower overflow', 'smalldatetime upper overflow', 'smalldatetime lower overflow', 'literal upper overflow']) {
    assert.equal(get(name).errors[0]?.number, 517, name)
  }
  assert.equal(get('datetime bigint amount').errors[0]?.number, 517)
  assert.equal(get('datetime beyond bigint amount').errors[0]?.number, 8115)
  assert.equal(get('literal four fractional digits').errors[0]?.number, 241)
  assert.equal(get('literal offset rejected').errors[0]?.number, 241)
  assertSameCapture(get('datetime stored source').sets[0].rows[1], [2, null], 'stored DATETIME NULL')
  assertSameCapture(get('smalldatetime stored source').sets[0].rows[1], [2, null], 'stored SMALLDATETIME NULL')
  for (const name of ['rpc datetime', 'rpc datetime null', 'rpc smalldatetime', 'rpc smalldatetime null']) {
    assertSameCapture(get(name).done.map(item => item.kind), ['doneInProc', 'doneProc'], `${name}: RPC completions`)
  }
}

await assertSeparateOutput()
if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(dirname(output), { recursive: true })
const captures = []
let image
for (let container = 0; container < 2; container++) {
  await withReferenceContainer(async (config, info) => {
    image ??= info.image
    assert.equal(info.image, image)
    for (let database = 0; database < 2; database++) {
      const run = await isolatedReference(config, observe)
      validate(run)
      if (captures.length) assertSameCapture(run, captures[0], 'fresh legacy DATEADD captures differ')
      captures.push(run)
    }
  })
}
const actual = { image, freshDatabases: captures.length, independentContainers: 2, results: captures[0] }
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (retained) assertSameCapture(actual, retained, 'legacy DATEADD capture differs from fixture')
await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${actual.results.length} legacy DATEADD observations in four fresh databases and two containers${retained ? '; matched retained fixture' : ''}`)
