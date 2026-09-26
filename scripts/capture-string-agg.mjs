#!/usr/bin/env node
// Retain SQL Server STRING_AGG rows, descriptors, diagnostics and completions.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/string-agg.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/string-agg/capture.json')

const cases = [
  ['unicode null elimination', "SELECT STRING_AGG(v,N'|') AS value FROM (VALUES(N'a'),(CAST(NULL AS NVARCHAR(10))),(N'b')) d(v)"],
  ['ansi null elimination', "SELECT STRING_AGG(v,'|') AS value FROM (VALUES('a'),(CAST(NULL AS VARCHAR(10))),('b')) d(v)"],
  ['unicode empty source', "SELECT STRING_AGG(v,N'|') AS value FROM (VALUES(N'a')) d(v) WHERE 1=0"],
  ['unicode all null', "SELECT STRING_AGG(v,N'|') AS value FROM (VALUES(CAST(NULL AS NVARCHAR(10))),(NULL)) d(v)"],
  ['unicode null separator', "SELECT STRING_AGG(v,CAST(NULL AS NVARCHAR(10))) AS value FROM (VALUES(N'a'),(N'b')) d(v)"],
  ['unicode empty separator', "SELECT STRING_AGG(v,N'') AS value FROM (VALUES(N'a'),(N'b')) d(v)"],
  ['ansi spaces', "SELECT STRING_AGG(v,' | ') AS value FROM (VALUES(' a '),('b ')) d(v)"],
  ['unicode source null', "SELECT STRING_AGG(CAST(NULL AS NVARCHAR(10)),N'|') AS value"],
  ['ordered ascending', "SELECT STRING_AGG(txt,N'|') WITHIN GROUP (ORDER BY seq) AS value FROM dbo.string_agg_src WHERE g=1"],
  ['ordered descending', "SELECT STRING_AGG(txt,N'|') WITHIN GROUP (ORDER BY seq DESC) AS value FROM dbo.string_agg_src WHERE g=1"],
  ['ordered null keys', "SELECT STRING_AGG(txt,N'|') WITHIN GROUP (ORDER BY rank_key,seq) AS value FROM dbo.string_agg_src WHERE g=1"],
  ['ordered ties', "SELECT STRING_AGG(txt,N'|') WITHIN GROUP (ORDER BY rank_key) AS value FROM dbo.string_agg_src WHERE g=2"],
  ['grouped', "SELECT g,STRING_AGG(txt,N'|') WITHIN GROUP (ORDER BY seq) AS value FROM dbo.string_agg_src GROUP BY g ORDER BY g"],
  ['outer empty group', "SELECT groups.g,STRING_AGG(src.txt,N'|') AS value FROM (VALUES(1),(2),(3)) groups(g) LEFT JOIN dbo.string_agg_src src ON src.g=groups.g GROUP BY groups.g ORDER BY groups.g"],
  ['ansi source column', "SELECT STRING_AGG(ansi,'|') WITHIN GROUP (ORDER BY seq) AS value FROM dbo.string_agg_src WHERE g=1"],
  ['ansi with unicode separator', "SELECT STRING_AGG(ansi,N'|') AS value FROM dbo.string_agg_src"],
  ['unicode with ansi separator', "SELECT STRING_AGG(txt,'|') AS value FROM dbo.string_agg_src"],
  ['int expression', "SELECT STRING_AGG(v,'|') AS value FROM (VALUES(1),(2),(CAST(NULL AS INT))) d(v)"],
  ['decimal expression', "SELECT STRING_AGG(v,'|') AS value FROM (VALUES(CAST(1.20 AS DECIMAL(8,2))),(CAST(-2.50 AS DECIMAL(8,2)))) d(v)"],
  ['datetime expression', "SELECT STRING_AGG(v,'|') AS value FROM (VALUES(CAST('2024-01-02T03:04:05' AS DATETIME2(0))),(CAST('2024-01-03T00:00:00' AS DATETIME2(0)))) d(v)"],
  ['binary expression', "SELECT STRING_AGG(v,'|') AS value FROM (VALUES(CAST(0x4142 AS VARBINARY(2))),(CAST(0x43 AS VARBINARY(2)))) d(v)"],
  ['varchar max source', "SELECT STRING_AGG(CAST(v AS VARCHAR(MAX)),'|') AS value FROM (VALUES('a'),('b')) d(v)"],
  ['nvarchar max source', "SELECT STRING_AGG(CAST(v AS NVARCHAR(MAX)),N'|') AS value FROM (VALUES(N'a'),(N'b')) d(v)"],
  ['varchar cast separator rejected', "SELECT STRING_AGG(CAST(v AS VARCHAR(20)),CAST('|' AS VARCHAR(MAX))) AS value FROM (VALUES('a'),('b')) d(v)"],
  ['nvarchar cast separator rejected', "SELECT STRING_AGG(CAST(v AS NVARCHAR(20)),CAST(N'|' AS NVARCHAR(MAX))) AS value FROM (VALUES(N'a'),(N'b')) d(v)"],
  ['varchar bounded with max separator variable', "DECLARE @sep VARCHAR(MAX)='|'; SELECT STRING_AGG(CAST(v AS VARCHAR(20)),@sep) AS value FROM (VALUES('a'),('b')) d(v)"],
  ['nvarchar bounded with max separator variable', "DECLARE @sep NVARCHAR(MAX)=N'|'; SELECT STRING_AGG(CAST(v AS NVARCHAR(20)),@sep) AS value FROM (VALUES(N'a'),(N'b')) d(v)"],
  ['varchar bounded overflow', "SELECT STRING_AGG(CAST(REPLICATE('x',3000) AS VARCHAR(3000)),'') AS value FROM (VALUES(1),(2),(3)) d(i)"],
  ['nvarchar bounded overflow', "SELECT STRING_AGG(CAST(REPLICATE(N'x',2000) AS NVARCHAR(2000)),N'') AS value FROM (VALUES(1),(2),(3)) d(i)"],
  ['varchar max beyond bounded limit', "SELECT STRING_AGG(CAST(REPLICATE('x',3000) AS VARCHAR(MAX)),'') AS value FROM (VALUES(1),(2),(3)) d(i)"],
  ['nvarchar max beyond bounded limit', "SELECT STRING_AGG(CAST(REPLICATE(N'x',2000) AS NVARCHAR(MAX)),N'') AS value FROM (VALUES(1),(2),(3)) d(i)"],
  ['distinct unsupported', "SELECT STRING_AGG(DISTINCT txt,N'|') AS value FROM dbo.string_agg_src"],
  ['window unsupported', "SELECT STRING_AGG(txt,N'|') OVER(PARTITION BY g) AS value FROM dbo.string_agg_src"],
  ['incompatible order lists', "SELECT STRING_AGG(txt,N'|') WITHIN GROUP (ORDER BY seq) AS a,STRING_AGG(txt,N'|') WITHIN GROUP (ORDER BY seq DESC) AS b FROM dbo.string_agg_src"],
  ['integer separator', "SELECT STRING_AGG(txt,1) AS value FROM dbo.string_agg_src"],
]

function rpc(connection, sql, parameters) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      for (const [name, type, value, options] of parameters) {
        request.addParameter(name, type, value, options)
      }
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    const result = canonical(await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql)))
    records.push({
      name, sql,
      ...(parameters ? { parameters: parameters.map(([parameter, type, value, options]) => ({
        name: parameter, type: type.name, value, ...(options ? { options } : {}),
      })) } : {}),
      result,
    })
  }
  await record('create source', 'CREATE TABLE dbo.string_agg_src(g INT,seq INT,rank_key INT,txt NVARCHAR(20),ansi VARCHAR(20))')
  await record('insert source', "INSERT dbo.string_agg_src VALUES (1,2,2,N'b','b'),(1,1,1,N'a','a'),(1,3,NULL,NULL,NULL),(2,1,1,N'x','x'),(2,2,1,N'x','x')")
  for (const [name, sql] of cases) await record(name, sql)
  const bound = "SELECT STRING_AGG(txt,@separator) WITHIN GROUP (ORDER BY seq) AS value FROM dbo.string_agg_src WHERE g=@group"
  for (const [name, type, value] of [
    ['unicode separator parameter', TYPES.NVarChar, ':'],
    ['unicode null separator parameter', TYPES.NVarChar, null],
    ['ansi separator parameter', TYPES.VarChar, ':'],
    ['unicode separator replay', TYPES.NVarChar, ':'],
  ]) await record(name, bound, [['separator', type, value, { length: 10 }], ['group', TYPES.Int, 1]])
  await record('bound source expression', "SELECT STRING_AGG(@value,N'|') AS value FROM (VALUES(1),(2)) d(i)", [['value', TYPES.NVarChar, 'bound', { length: 20 }]])
  return records
}

function validate(run) {
  assert.equal(run.length, cases.length + 2 + 5)
  const get = name => run.find(record => record.name === name)?.result
  assert.deepEqual(get('unicode null elimination').sets[0].rows, [['a|b']])
  assert.deepEqual(get('unicode empty source').sets[0].rows, [[null]])
  assert.deepEqual(get('ordered ascending').sets[0].rows, [['a|b']])
  assert.deepEqual(get('ordered descending').sets[0].rows, [['b|a']])
  assert.deepEqual(get('unicode separator parameter').sets[0].rows, [['a:b']])
  for (const [name, type, length] of [
    ['ansi null elimination', 'VarChar', 8000],
    ['unicode null elimination', 'NVarChar', 8000],
    ['int expression', 'NVarChar', 8000],
    ['varchar max source', 'VarChar', 65535],
    ['nvarchar max source', 'NVarChar', 65535],
  ]) {
    assert.equal(get(name).sets[0].columns[0].type, type, name)
    assert.equal(get(name).sets[0].columns[0].length, length, name)
  }
  for (const [name, number] of [
    ['ansi with unicode separator', 8116],
    ['varchar cast separator rejected', 8733],
    ['nvarchar cast separator rejected', 8733],
    ['varchar bounded with max separator variable', 8734],
    ['nvarchar bounded with max separator variable', 8734],
    ['varchar bounded overflow', 9829],
    ['nvarchar bounded overflow', 9829],
    ['distinct unsupported', 102],
    ['window unsupported', 4113],
    ['incompatible order lists', 8711],
  ]) assert.equal(get(name).errors[0]?.number, number, name)
  for (const record of run) assert(record.result.done.length > 0, record.name + ': no completion')
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < 2; containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let databaseIndex = 0; databaseIndex < 2; databaseIndex++) {
      const run = await isolatedReference({
        ...config, options: { ...config.options, requestTimeout: 120000 },
      }, observe)
      validate(run)
      runs.push(run)
    }
    assertSameCapture(runs[0], runs[1], 'STRING_AGG observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
assertSameCapture(containers[0].runs[0], containers[1].runs[0], 'STRING_AGG observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (retained) assertSameCapture(actual, retained, 'STRING_AGG observations differ from retained fixture')
if (writeFixture) {
  await writeNewFixture(fixture, actual)
}
console.log('Captured ' + containers[0].runs[0].length + ' STRING_AGG observations in four fresh databases across two containers' + (retained ? ' and matched retained fixture' : ''))
