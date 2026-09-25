#!/usr/bin/env node
// Retain SQL Server STRING_SPLIT rows, descriptors, diagnostics and completions.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'

const fixture = new URL('../reference/string-split.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/compatibility/string-split/capture.json')

const cases = [
  ['ansi basic', "SELECT value FROM STRING_SPLIT('alpha,beta',',') ORDER BY value"],
  ['unicode basic', "SELECT value FROM STRING_SPLIT(N'alpha,beta',N',') ORDER BY value"],
  ['unordered observation', "SELECT value FROM STRING_SPLIT('c,a,b',',')"],
  ['empty input', "SELECT value FROM STRING_SPLIT(CAST('' AS VARCHAR(12)),',')"],
  ['unicode empty input', "SELECT value FROM STRING_SPLIT(CAST(N'' AS NVARCHAR(12)),N',')"],
  ['null ansi input', "SELECT value FROM STRING_SPLIT(CAST(NULL AS VARCHAR(12)),',')"],
  ['null unicode input', "SELECT value FROM STRING_SPLIT(CAST(NULL AS NVARCHAR(12)),N',')"],
  ['repeated separator', "SELECT value FROM STRING_SPLIT('a,,b',',') ORDER BY value"],
  ['edge separators', "SELECT value FROM STRING_SPLIT(',a,',',') ORDER BY value"],
  ['space tokens', "SELECT value FROM STRING_SPLIT(' a | b | ', '|') ORDER BY value"],
  ['empty ansi separator', "SELECT value FROM STRING_SPLIT('abc','')"],
  ['empty unicode separator', "SELECT value FROM STRING_SPLIT(N'abc',N'')"],
  ['null ansi separator', "SELECT value FROM STRING_SPLIT('a,b',CAST(NULL AS VARCHAR(1)))"],
  ['null unicode separator', "SELECT value FROM STRING_SPLIT(N'a,b',CAST(NULL AS NVARCHAR(1)))"],
  ['two character separator', "SELECT value FROM STRING_SPLIT('a::b','::')"],
  ['unicode two character separator', "SELECT value FROM STRING_SPLIT(N'a::b',N'::')"],
  ['supplementary separator', "SELECT value FROM STRING_SPLIT(N'a😀b',N'😀')"],
  ['varchar max input', "SELECT value FROM STRING_SPLIT(CAST('a,b' AS VARCHAR(MAX)),',') ORDER BY value"],
  ['nvarchar max input', "SELECT value FROM STRING_SPLIT(CAST(N'a,b' AS NVARCHAR(MAX)),N',') ORDER BY value"],
  ['varchar with unicode separator', "SELECT value FROM STRING_SPLIT(CAST('a,b' AS VARCHAR(12)),N',') ORDER BY value"],
  ['nvarchar with ansi separator', "SELECT value FROM STRING_SPLIT(CAST(N'a,b' AS NVARCHAR(12)),',') ORDER BY value"],
  ['explicit binary collation', "SELECT value FROM STRING_SPLIT(N'a,A' COLLATE Latin1_General_100_BIN2,N',') ORDER BY value"],
  ['integer input', "SELECT value FROM STRING_SPLIT(123,',')"],
  ['binary input', "SELECT value FROM STRING_SPLIT(0x414243,',')"],
  ['integer separator', "SELECT value FROM STRING_SPLIT('a,b',44)"],
  ['ordinal one ordered', "SELECT value,ordinal FROM STRING_SPLIT('b,a,b',',',1) ORDER BY ordinal"],
  ['ordinal zero', "SELECT value FROM STRING_SPLIT('b,a',',',0) ORDER BY value"],
  ['ordinal empty input', "SELECT value,ordinal FROM STRING_SPLIT(CAST('' AS VARCHAR(12)),',',1)"],
  ['ordinal null input', "SELECT value,ordinal FROM STRING_SPLIT(CAST(NULL AS NVARCHAR(12)),N',',1)"],
  ['ordinal two', "SELECT value,ordinal FROM STRING_SPLIT('a,b',',',2) ORDER BY ordinal"],
  ['ordinal negative', "SELECT value,ordinal FROM STRING_SPLIT('a,b',',',-1) ORDER BY ordinal"],
  ['ordinal null', "SELECT value,ordinal FROM STRING_SPLIT('a,b',',',NULL) ORDER BY ordinal"],
  ['ordinal cast constant', "SELECT value,ordinal FROM STRING_SPLIT('a,b',',',CAST(1 AS BIT)) ORDER BY ordinal"],
  ['ordinal variable', "DECLARE @ordinal BIT=1; SELECT value,ordinal FROM STRING_SPLIT('a,b',',',@ordinal) ORDER BY ordinal"],
  ['ordinal decimal', "SELECT value,ordinal FROM STRING_SPLIT('a,b',',',1.0) ORDER BY ordinal"],
  ['missing separator', "SELECT value FROM STRING_SPLIT('a,b')"],
  ['too many arguments', "SELECT value FROM STRING_SPLIT('a,b',',',1,0)"],
  ['apply source column', "SELECT s.id,x.value,x.ordinal FROM dbo.string_split_src AS s CROSS APPLY STRING_SPLIT(s.utf,N'|',1) AS x ORDER BY s.id,x.ordinal"],
  ['apply null source', "SELECT s.id,x.value FROM dbo.string_split_src AS s OUTER APPLY STRING_SPLIT(s.utf,N'|') AS x ORDER BY s.id,x.value"],
  ['apply ansi column', "SELECT s.id,x.value FROM dbo.string_split_src AS s CROSS APPLY STRING_SPLIT(s.ansi,'|') AS x ORDER BY s.id,x.value"],
]

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
    records.push({
      name, sql,
      ...(parameters ? { parameters: parameters.map(([parameter, type, value, options]) => ({
        name: parameter, type: type.name, value, ...(options ? { options } : {}),
      })) } : {}),
      result,
    })
  }
  await record('create source', 'CREATE TABLE dbo.string_split_src(id INT,utf NVARCHAR(20),ansi VARCHAR(20))')
  await record('insert source', "INSERT dbo.string_split_src VALUES (1,N'a|b','a|b'),(2,N'|x|','|x|'),(3,NULL,NULL)")
  for (const [name, sql] of cases) await record(name, sql)
  const bound = 'SELECT value,ordinal FROM STRING_SPLIT(@input,@separator,1) ORDER BY ordinal'
  for (const [name, inputType, input, separatorType, separator] of [
    ['bound ansi', TYPES.VarChar, 'a|b', TYPES.VarChar, '|'],
    ['bound unicode', TYPES.NVarChar, 'a|b', TYPES.NVarChar, '|'],
    ['bound null input', TYPES.NVarChar, null, TYPES.NVarChar, '|'],
    ['bound null separator', TYPES.NVarChar, 'a|b', TYPES.NVarChar, null],
    ['bound ansi replay', TYPES.VarChar, 'a|b', TYPES.VarChar, '|'],
  ]) await record(name, bound, [
    ['input', inputType, input, { length: 20 }],
    ['separator', separatorType, separator, { length: 1 }],
  ])
  await record('bound ordinal parameter',
    'SELECT value,ordinal FROM STRING_SPLIT(@input,@separator,@ordinal) ORDER BY ordinal', [
      ['input', TYPES.VarChar, 'a|b', { length: 20 }],
      ['separator', TYPES.VarChar, '|', { length: 1 }],
      ['ordinal', TYPES.Bit, true],
    ])
  return records
}

function validate(run) {
  assert.equal(run.length, cases.length + 2 + 6)
  const get = name => run.find(record => record.name === name)?.result
  assert.deepEqual(get('ansi basic').sets[0].rows, [['alpha'], ['beta']])
  assert.deepEqual(get('unicode basic').sets[0].rows, [['alpha'], ['beta']])
  assert.deepEqual(get('empty input').sets[0].rows, [['']])
  assert.deepEqual(get('null ansi input').sets[0].rows, [])
  assert.deepEqual(get('ordinal one ordered').sets[0].rows, [['b', '1'], ['a', '2'], ['b', '3']])
  assert.deepEqual(get('bound ansi').sets[0].rows, [['a', '1'], ['b', '2']])
  for (const record of run) assert(record.result.done.length > 0, `${record.name}: no completion`)
}

await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
      const run = await isolatedReference({
        ...config, options: { ...config.options, requestTimeout: 120000 },
      }, observe)
      validate(run)
      runs.push(run)
    }
    if (!oneDatabase) assert.deepEqual(runs[0], runs[1], 'STRING_SPLIT observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) assert.deepEqual(containers[0].runs[0], containers[1].runs[0], 'STRING_SPLIT observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (retained && !oneDatabase) assert.deepEqual(actual, retained, 'STRING_SPLIT observations differ from retained fixture')
if (writeFixture) {
  assert.equal(retained, undefined, 'refusing to overwrite retained fixture')
  await writeFile(fixture, JSON.stringify(actual) + '\n')
}
console.log(`Captured ${containers[0].runs[0].length} STRING_SPLIT observations` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained && !oneDatabase ? ' and matched retained fixture' : ''))
