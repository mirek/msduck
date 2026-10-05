#!/usr/bin/env node
// Owner-controlled SQL Server 2025 evidence for DATEADD BIGINT amounts.
import assert from 'node:assert/strict'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/dateadd-bigint.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/dateadd-bigint/capture.json')

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

const date = "CAST('2024-01-01' AS DATE)"
const time = "CAST('12:34:56.1234567' AS TIME(7))"
const datetime2 = "CAST('2024-01-01T12:34:56.1234567' AS DATETIME2(7))"
const offset = "CAST('2024-01-01T12:34:56.1234567+14:00' AS DATETIMEOFFSET(7))"
const cases = [
  ['date int max', `SELECT DATEADD(day,2147483647,${date}) AS value`],
  ['date above int', `SELECT DATEADD(day,2147483648,${date}) AS value`],
  ['date bigint typed zero', `SELECT DATEADD(day,CAST(0 AS BIGINT),${date}) AS value`],
  ['date bigint fractional', `SELECT DATEADD(day,CAST(-1.9 AS DECIMAL(20,1)),${date}) AS value`],
  ['date bigint NULL', `SELECT DATEADD(day,CAST(NULL AS BIGINT),${date}) AS value`],
  ['date bigint empty', `SELECT DATEADD(day,CAST(2147483648 AS BIGINT),${date}) AS value WHERE 1=0`],
  ['date unsupported ns', `SELECT DATEADD(ns,2147483648,${date}) AS value`],
  ['time above int nanos', `SELECT DATEADD(ns,2147483648,${time}) AS value`],
  ['time below int nanos', `SELECT DATEADD(ns,-2147483649,${time}) AS value`],
  ['time max bigint nanos', `SELECT DATEADD(ns,CAST(9223372036854775807 AS BIGINT),${time}) AS value`],
  ['time min bigint nanos', `SELECT DATEADD(ns,CAST(-9223372036854775808 AS BIGINT),${time}) AS value`],
  ['time above int hours', `SELECT DATEADD(hour,2147483648,${time}) AS value`],
  ['time bigint NULL', `SELECT DATEADD(ns,CAST(NULL AS BIGINT),${time}) AS value`],
  ['time bigint empty', `SELECT DATEADD(ns,2147483648,${time}) AS value WHERE 1=0`],
  ['time unsupported day', `SELECT DATEADD(day,2147483648,${time}) AS value`],
  ['datetime2 above int nanos', `SELECT DATEADD(ns,2147483648,${datetime2}) AS value`],
  ['datetime2 below int nanos', `SELECT DATEADD(ns,-2147483649,${datetime2}) AS value`],
  ['datetime2 max bigint nanos', `SELECT DATEADD(ns,CAST(9223372036854775807 AS BIGINT),${datetime2}) AS value`],
  ['datetime2 min bigint nanos', `SELECT DATEADD(ns,CAST(-9223372036854775808 AS BIGINT),${datetime2}) AS value`],
  ['datetime2 min bare nanos', `SELECT DATEADD(ns,-9223372036854775808,${datetime2}) AS value`],
  ['datetime2 above int millis', `SELECT DATEADD(millisecond,2147483648,${datetime2}) AS value`],
  ['datetime2 above int days', `SELECT DATEADD(day,2147483648,${datetime2}) AS value`],
  ['datetime2 bigint NULL', `SELECT DATEADD(ns,CAST(NULL AS BIGINT),${datetime2}) AS value`],
  ['datetime2 bigint empty', `SELECT DATEADD(ns,2147483648,${datetime2}) AS value WHERE 1=0`],
  ['offset above int nanos', `SELECT DATEADD(ns,2147483648,${offset}) AS value`],
  ['offset below int nanos', `SELECT DATEADD(ns,-2147483649,${offset}) AS value`],
  ['offset max bigint nanos', `SELECT DATEADD(ns,CAST(9223372036854775807 AS BIGINT),${offset}) AS value`],
  ['offset min bigint nanos', `SELECT DATEADD(ns,CAST(-9223372036854775808 AS BIGINT),${offset}) AS value`],
  ['offset above int days', `SELECT DATEADD(day,2147483648,${offset}) AS value`],
  ['offset bigint NULL', `SELECT DATEADD(ns,CAST(NULL AS BIGINT),${offset}) AS value`],
  ['offset bigint empty', `SELECT DATEADD(ns,2147483648,${offset}) AS value WHERE 1=0`],
  ['beyond bigint', `SELECT DATEADD(ns,9223372036854775808,${datetime2}) AS value`],
]

function rpc(connection, sql, parameters) {
  const transport = {
    on: (...args_) => connection.on(...args_), off: (...args_) => connection.off(...args_),
    execSqlBatch: request => {
      for (const [name, type, value] of parameters) request.addParameter(name, type, value)
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    const result = canonical(await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql)))
    records.push({ name, sql, ...(parameters ? { parameters: parameters.map(([name_, type, value]) => ({ name: name_, type: type.name, value })) } : {}), result })
  }
  await record('server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version")
  for (const [name, sql] of cases) await record(name, sql)
  await record('create stored amounts', 'CREATE TABLE dbo.dateadd_bigint(n BIGINT,d DATE,t TIME(7),dt DATETIME2(7),dto DATETIMEOFFSET(7))')
  await record('insert stored amounts', `INSERT dbo.dateadd_bigint VALUES(2147483648,${date},${time},${datetime2},${offset}),(-2147483649,NULL,NULL,NULL,NULL)`)
  for (const [name, column, part] of [['date stored','d','day'],['time stored','t','ns'],['datetime2 stored','dt','ns'],['offset stored','dto','ns']]) {
    await record(name, `SELECT DATEADD(${part},n,${column}) AS value FROM dbo.dateadd_bigint ORDER BY n DESC`)
  }
  for (const [name, amount] of [['rpc positive','2147483648'],['rpc negative','-2147483649'],['rpc minimum','-9223372036854775808'],['rpc NULL',null]]) {
    for (const [family, value, part] of [['date',date,'day'],['time',time,'ns'],['datetime2',datetime2,'ns'],['offset',offset,'ns']]) {
      await record(`${family} ${name}`, `SELECT DATEADD(${part},@n,${value}) AS value`, [['n',TYPES.BigInt,amount]])
    }
  }
  return records
}

function validate(run) {
  assert.equal(run.length, 1 + cases.length + 2 + 4 + 16)
  for (const item of run) assert.ok(item.result.done.length > 0, `${item.name}: missing DONE`)
  const get = name => run.find(item => item.name === name)?.result
  assertSameCapture(get('server version').sets[0].rows, [['17.0.4065.4']], 'pinned SQL Server build')
  for (const name of ['date above int','datetime2 above int days','offset above int days']) {
    assert.equal(get(name).errors[0]?.number, 517, name)
  }
  assert.equal(get('beyond bigint').errors[0]?.number, 8115)
  const original = { kind: 'date', value: '2024-01-01T12:34:56.123Z', nanosecondsDelta: 0.0004567 }
  for (const name of ['datetime2 min bigint nanos', 'datetime2 min bare nanos', 'datetime2 rpc minimum']) {
    assertSameCapture(get(name).sets[0].rows, [[original]], `${name}: SQL Server 2025 min BIGINT no-op`)
  }
  for (const name of ['time max bigint nanos', 'time min bigint nanos', 'time rpc minimum']) {
    assert.equal(get(name).sets[0].columns[0].scale, 7, name)
  }
  for (const name of ['date bigint NULL','time bigint NULL','datetime2 bigint NULL','offset bigint NULL']) {
    assertSameCapture(get(name).sets[0].rows, [[null]], name)
  }
  for (const name of ['date bigint empty','time bigint empty','datetime2 bigint empty','offset bigint empty']) {
    assertSameCapture(get(name).sets[0].rows, [], name)
    assert.equal(get(name).sets[0].columns.length, 1, name)
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
      if (captures.length) assertSameCapture(run, captures[0], 'fresh BIGINT DATEADD captures differ')
      captures.push(run)
    }
  })
}
const actual = { image, freshDatabases: captures.length, independentContainers: 2, results: captures[0] }
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (retained) assertSameCapture(actual, retained, 'BIGINT DATEADD capture differs from fixture')
await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${actual.results.length} BIGINT DATEADD observations in four fresh databases and two containers${retained ? '; matched retained fixture' : ''}`)
