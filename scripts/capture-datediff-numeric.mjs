#!/usr/bin/env node
// Owner-controlled SQL Server evidence for numeric DATEDIFF input binding.
import assert from 'node:assert/strict'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/datediff-numeric.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/datediff-numeric/capture.json')

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

const literals = [
  ['integer day', 'SELECT DATEDIFF(day,0,1) AS n'],
  ['integer negative day', 'SELECT DATEDIFF(day,-1,1) AS n'],
  ['bit day', 'SELECT DATEDIFF(day,CAST(0 AS BIT),CAST(1 AS BIT)) AS n'],
  ['decimal half day', 'SELECT DATEDIFF(hour,CAST(0 AS DECIMAL(10,4)),CAST(0.5 AS DECIMAL(10,4))) AS n'],
  ['numeric negative half', 'SELECT DATEDIFF(hour,CAST(-0.5 AS NUMERIC(10,4)),CAST(0 AS NUMERIC(10,4))) AS n'],
  ['bare decimal half', 'SELECT DATEDIFF(hour,0,0.5) AS n'],
  ['float half', 'SELECT DATEDIFF(hour,CAST(0 AS FLOAT),CAST(0.5 AS FLOAT)) AS n'],
  ['real half', 'SELECT DATEDIFF(hour,CAST(0 AS REAL),CAST(0.5 AS REAL)) AS n'],
  ['money half', 'SELECT DATEDIFF(hour,CAST(0 AS MONEY),CAST(0.5 AS MONEY)) AS n'],
  ['smallmoney half', 'SELECT DATEDIFF(hour,CAST(0 AS SMALLMONEY),CAST(0.5 AS SMALLMONEY)) AS n'],
  ['decimal single tick', 'SELECT DATEDIFF_BIG(nanosecond,CAST(0 AS DECIMAL(20,10)),CAST(0.0000000386 AS DECIMAL(20,10))) AS n'],
  ['decimal half tick below', 'SELECT DATEDIFF_BIG(nanosecond,CAST(0 AS DECIMAL(20,10)),CAST(0.0000000192 AS DECIMAL(20,10))) AS n'],
  ['decimal half tick above', 'SELECT DATEDIFF_BIG(nanosecond,CAST(0 AS DECIMAL(20,10)),CAST(0.0000000194 AS DECIMAL(20,10))) AS n'],
  ['scale38 below', 'SELECT DATEDIFF_BIG(nanosecond,CAST(0 AS DECIMAL(38,38)),CAST(0.00000001929012345679012345679012345678 AS DECIMAL(38,38))) AS n'],
  ['scale38 above', 'SELECT DATEDIFF_BIG(nanosecond,CAST(0 AS DECIMAL(38,38)),CAST(0.00000001929012345679012345679012345680 AS DECIMAL(38,38))) AS n'],
  ['bare scale38 above', 'SELECT DATEDIFF_BIG(nanosecond,0,0.00000001929012345679012345679012345680) AS n'],
  ['decimal NULL', 'SELECT DATEDIFF(hour,CAST(NULL AS DECIMAL(10,4)),CAST(0.5 AS DECIMAL(10,4))) AS n'],
  ['float NULL', 'SELECT DATEDIFF_BIG(nanosecond,CAST(NULL AS FLOAT),CAST(0.5 AS FLOAT)) AS n'],
  ['decimal lower bound', 'SELECT DATEDIFF(day,CAST(-53690 AS DECIMAL(12,4)),CAST(0 AS DECIMAL(12,4))) AS n'],
  ['decimal out of range', 'SELECT DATEDIFF(day,CAST(-53691 AS DECIMAL(12,4)),CAST(0 AS DECIMAL(12,4))) AS n'],
  ['float out of range', 'SELECT DATEDIFF(day,CAST(2958464 AS FLOAT),CAST(0 AS FLOAT)) AS n'],
  ['integer millisecond overflow', 'SELECT DATEDIFF(millisecond,0,30) AS n'],
  ['integer big millisecond', 'SELECT DATEDIFF_BIG(millisecond,0,30) AS n'],
  ['mixed decimal and datetime2', "SELECT DATEDIFF(hour,CAST(0.5 AS DECIMAL(10,4)),CAST('1900-01-02T00:00:00' AS DATETIME2(7))) AS n"],
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
    records.push({ name, sql, ...(parameters ? { parameters: parameters.map(([name_, type, value, options]) => ({ name: name_, type: type.name, value, ...(options ? { options } : {}) })) } : {}), result })
  }
  await record('server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version")
  for (const [name, sql] of literals) await record(name, sql)
  await record('create sources', 'CREATE TABLE dbo.numeric_diff(id INT,d DECIMAL(20,12),f FLOAT,r REAL,m MONEY,b BIT)')
  await record('insert sources', 'INSERT dbo.numeric_diff VALUES (1,0.5,0.5,0.5,0.5,1),(2,-0.5,-0.5,-0.5,-0.5,0),(3,0.0000000386,0.0000000386,0.0000000386,0.0001,NULL),(4,NULL,NULL,NULL,NULL,NULL)')
  for (const [name, column] of [['decimal column','d'],['float column','f'],['real column','r'],['money column','m'],['bit column','b']]) {
    await record(name, `SELECT id,DATEDIFF(hour,0,${column}) AS hours,DATEDIFF_BIG(nanosecond,0,${column}) AS nanos FROM dbo.numeric_diff ORDER BY id`)
  }
  for (const [name, type, value, options] of [
    ['rpc decimal half',TYPES.Decimal,0.5,{precision:20,scale:12}],
    ['rpc decimal null',TYPES.Decimal,null,{precision:20,scale:12}],
    ['rpc float half',TYPES.Float,0.5], ['rpc real half',TYPES.Real,0.5],
    ['rpc money half',TYPES.Money,0.5], ['rpc bit one',TYPES.Bit,true],
  ]) await record(name,'SELECT DATEDIFF(hour,0,@n) AS hours,DATEDIFF_BIG(nanosecond,0,@n) AS nanos',[['n',type,value,options]])
  return records
}

function validate(run) {
  assert.equal(run.length, 1 + literals.length + 2 + 5 + 6)
  for (const item of run) assert.ok(item.result.done.length > 0, `${item.name}: missing completion`)
  const get = name => {
    const entry = run.find(item => item.name === name)
    assert.ok(entry, `missing ${name}`)
    return entry.result
  }
  for (const name of ['integer day', 'bit day']) assertSameCapture(get(name).sets[0].rows, [[1]], name)
  for (const name of ['decimal half day', 'bare decimal half', 'float half', 'real half', 'money half', 'smallmoney half']) {
    assertSameCapture(get(name).sets[0].rows, [[12]], name)
  }
  for (const name of ['decimal single tick', 'decimal half tick above', 'scale38 above', 'bare scale38 above']) {
    assertSameCapture(get(name).sets[0].rows, [['3333333']], name)
  }
  for (const name of ['decimal half tick below', 'scale38 below']) assertSameCapture(get(name).sets[0].rows, [['0']], name)
  for (const name of ['decimal out of range', 'float out of range', 'integer millisecond overflow']) {
    assert.equal(get(name).errors.length, 1, name)
    assert.equal(get(name).errors[0].number, name === 'integer millisecond overflow' ? 535 : 8115, name)
  }
  assertSameCapture(get('decimal column').sets[0].rows[2], [3, 0, '3333333'], 'stored DECIMAL tick')
  assertSameCapture(get('rpc decimal half').sets[0].rows, [[12, '43200000000000']], 'bound DECIMAL')
  assertSameCapture(get('rpc decimal null').sets[0].rows, [[null, null]], 'bound NULL')
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
      if (captures.length) assertSameCapture(run, captures[0], 'fresh numeric DATEDIFF captures differ')
      captures.push(run)
    }
  })
}
const actual = { image, freshDatabases: captures.length, independentContainers: 2, results: captures[0] }
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (retained) assertSameCapture(actual, retained, 'numeric DATEDIFF capture differs from fixture')
await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${actual.results.length} numeric DATEDIFF observations in four fresh databases and two containers${retained ? '; matched retained fixture' : ''}`)
