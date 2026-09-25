#!/usr/bin/env node
// Numeric DATEPART/DATENAME behavior from pinned SQL Server, without normalization.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/datepart-numeric.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/datepart-numeric/capture.json')

const expressions = [
  ['integer zero', '0'], ['integer one', '1'],
  ['decimal half', '0.5'], ['decimal negative half', '-0.5'],
  ['decimal next day', '1.25'], ['decimal previous day', '-1.25'],
  ['decimal typed', 'CAST(0.5 AS DECIMAL(10,4))'],
  ['numeric typed', 'CAST(-0.5 AS NUMERIC(10,4))'],
  ['float typed', 'CAST(0.5 AS FLOAT)'],
  ['real typed', 'CAST(0.5 AS REAL)'],
  ['money typed', 'CAST(0.5 AS MONEY)'],
  ['smallmoney typed', 'CAST(-0.5 AS SMALLMONEY)'],
  ['bit zero', 'CAST(0 AS BIT)'], ['bit one', 'CAST(1 AS BIT)'],
  ['decimal null', 'CAST(NULL AS DECIMAL(10,4))'],
  ['float null', 'CAST(NULL AS FLOAT)'], ['bit null', 'CAST(NULL AS BIT)'],
  ['first date', 'CAST(-53690 AS DECIMAL(12,4))'],
  ['before first date', 'CAST(-53691 AS DECIMAL(12,4))'],
  ['last day', 'CAST(2958463 AS DECIMAL(12,4))'],
  ['after last day', 'CAST(2958464 AS DECIMAL(12,4))'],
  ['big decimal day', 'CAST(1721425.5 AS DECIMAL(10,1))'],
  ['half tick below', 'CAST(0.0000000192 AS DECIMAL(20,10))'],
  ['half tick above', 'CAST(0.0000000194 AS DECIMAL(20,10))'],
  ['scale38 below half tick', 'CAST(0.00000001929012345679012345679012345678 AS DECIMAL(38,38))'],
  ['scale38 above half tick', 'CAST(0.00000001929012345679012345679012345680 AS DECIMAL(38,38))'],
  ['bare scale38 below half tick', '0.00000001929012345679012345679012345678'],
  ['bare scale38 above half tick', '0.00000001929012345679012345679012345680'],
  ['one tick', 'CAST(0.0000000386 AS DECIMAL(20,10))'],
  ['negative one tick', 'CAST(-0.0000000386 AS DECIMAL(20,10))'],
  ['float tiny', 'CAST(0.0000000386 AS FLOAT)'],
  ['real tiny', 'CAST(0.0000000386 AS REAL)'],
  ['money tiny', 'CAST(0.0001 AS MONEY)'],
]

const parts = expr => `SELECT DATEPART(year, ${expr}) AS y,DATEPART(day, ${expr}) AS d,DATEPART(hour, ${expr}) AS h,DATEPART(minute, ${expr}) AS m,DATEPART(second, ${expr}) AS s,DATEPART(millisecond, ${expr}) AS ms,DATEPART(microsecond, ${expr}) AS us,DATEPART(nanosecond, ${expr}) AS ns`

function rpc(connection, sql, parameters) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
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
    records.push({ name, sql, ...(parameters ? { parameters: parameters.map(([name, type, value, options]) => ({name,type:type.name,value,...(options ? {options} : {})})) } : {}), result })
  }
  for (const [name, expr] of expressions) await record(name, parts(expr))
  for (const [name, expr] of [
    ['datename decimal', 'CAST(0.5 AS DECIMAL(10,4))'],
    ['datename float', 'CAST(-0.5 AS FLOAT)'],
    ['datename bit', 'CAST(1 AS BIT)'],
  ]) await record(name, `SELECT DATENAME(year, ${expr}) AS y,DATENAME(hour, ${expr}) AS h,DATENAME(millisecond, ${expr}) AS ms`)
  for (const [name, expr] of [
    ['tzoffset decimal', 'CAST(0.5 AS DECIMAL(10,4))'],
    ['tzoffset float', 'CAST(0.5 AS FLOAT)'],
    ['tzoffset bit', 'CAST(1 AS BIT)'],
  ]) await record(name, `SELECT DATEPART(tzoffset, ${expr}) AS offset`)

  await record('create sources', 'CREATE TABLE dbo.numeric_parts(id INT,d DECIMAL(20,12),f FLOAT,r REAL,m MONEY,b BIT)')
  await record('insert sources', `INSERT dbo.numeric_parts VALUES
    (1,0.5,0.5,0.5,0.5,1),
    (2,-0.5,-0.5,-0.5,-0.5,0),
    (3,0.0000000386,0.0000000386,0.0000000386,0.0001,NULL),
    (4,NULL,NULL,NULL,NULL,NULL)`)
  for (const [name, column] of [
    ['decimal column', 'd'], ['float column', 'f'], ['real column', 'r'],
    ['money column', 'm'], ['bit column', 'b'],
  ]) await record(name, `SELECT id,DATEPART(year,${column}) AS y,DATEPART(day,${column}) AS d,DATEPART(hour,${column}) AS h,DATEPART(millisecond,${column}) AS ms,DATEPART(microsecond,${column}) AS us,DATEPART(nanosecond,${column}) AS ns FROM dbo.numeric_parts ORDER BY id`)

  const rpcSql = 'SELECT DATEPART(year,@n) AS y,DATEPART(day,@n) AS d,DATEPART(hour,@n) AS h,DATEPART(millisecond,@n) AS ms,DATEPART(microsecond,@n) AS us,DATEPART(nanosecond,@n) AS ns'
  for (const [name, type, value, options] of [
    ['rpc decimal half', TYPES.Decimal, 0.5, {precision:20,scale:12}],
    ['rpc decimal negative', TYPES.Decimal, -0.5, {precision:20,scale:12}],
    ['rpc decimal null', TYPES.Decimal, null, {precision:20,scale:12}],
    ['rpc float half', TYPES.Float, 0.5],
    ['rpc real half', TYPES.Real, 0.5],
    ['rpc money half', TYPES.Money, 0.5],
    ['rpc bit one', TYPES.Bit, true],
    ['rpc decimal replay', TYPES.Decimal, 0.5, {precision:20,scale:12}],
  ]) await record(name, rpcSql, [['n', type, value, options]])
  return records
}

function validate(run) {
  assert.equal(run.length, expressions.length + 3 + 3 + 2 + 5 + 8)
  const get = name => run.find(item => item.name === name)?.result
  assert.deepEqual(get('decimal half').sets[0].rows, [[1900,1,12,0,0,0,0,0]])
  assert.deepEqual(get('decimal negative half').sets[0].rows.slice(0,1).map(row => row.slice(0,3)), [[1899,31,12]])
  assert.deepEqual(get('bit one').sets[0].rows.slice(0,1).map(row => row.slice(0,3)), [[1900,2,0]])
  assert.deepEqual(get('decimal null').sets[0].rows, [Array(8).fill(null)])
  assert.equal(get('scale38 below half tick').sets[0].rows[0][7],0)
  assert.equal(get('scale38 above half tick').sets[0].rows[0][7],3333333)
  assert.equal(get('bare scale38 below half tick').sets[0].rows[0][7],0)
  assert.equal(get('bare scale38 above half tick').sets[0].rows[0][7],3333333)
  for (const name of ['decimal column','float column','real column','money column','bit column']) assert.equal(get(name).sets[0].rows.length,4)
  for (const item of run) assert(item.result.done.length > 0, `${item.name}: no completion`)
}

await mkdir(resolve(output, '..'), {recursive:true})
await withReferenceContainer(async (config, container) => {
  const runs=[]
  for(let repeat=0;repeat<2;repeat++) {
    const run=await isolatedReference({...config,options:{...config.options,requestTimeout:120000}},observe)
    validate(run)
    runs.push(run)
  }
  assert.deepEqual(runs[0],runs[1],'numeric DATEPART observations differ across fresh databases')
  const actual={image:container.image,runs}
  await writeFile(output,JSON.stringify(actual)+'\n')
  let retained
  try {retained=JSON.parse(await readFile(fixture,'utf8'))}
  catch(error){if(error.code!=='ENOENT')throw error}
  if(retained)assert.deepEqual(actual,retained,'numeric DATEPART observations differ from retained fixture')
  if(writeFixture){assert.equal(retained,undefined,'refusing to overwrite retained fixture');await writeFile(fixture,JSON.stringify(actual)+'\n')}
  console.log(`Captured ${runs[0].length} numeric DATEPART observations twice${retained?' and matched retained fixture':''}`)
})
