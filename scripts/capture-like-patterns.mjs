#!/usr/bin/env node
// Retain SQL Server LIKE rows, descriptors, diagnostics and completion events.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'

const fixture = new URL('../reference/like-patterns.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/compatibility/like-patterns/capture.json')
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (writeFixture && retained) throw new Error('refusing to overwrite retained fixture')

const cases = []
const add = (name, sql) => cases.push([name, sql])
const predicate = (name, expression) =>
  add(name, `SELECT CASE WHEN ${expression} THEN 1 WHEN NOT (${expression}) THEN 0 ELSE NULL END AS matches`)

for (const [name, expression] of [
  ['percent wildcard', "'alpha' LIKE 'a%'"],
  ['underscore wildcard', "'abc' LIKE 'a_c'"],
  ['underscore rejects empty', "'ac' LIKE 'a_c'"],
  ['not like true', "'abc' NOT LIKE 'z%'"],
  ['not like false', "'abc' NOT LIKE 'a%'"],
  ['bracket range', "'b' LIKE '[a-c]'"],
  ['bracket range outside', "'z' LIKE '[a-c]'"],
  ['bracket negation', "'z' LIKE '[^a-c]'"],
  ['bracket negation false', "'b' LIKE '[^a-c]'"],
  ['literal opening bracket', "'[' LIKE '[[]'"],
  ['literal closing bracket', "']' LIKE '[]]'"],
  ['hyphen first in class', "'-' LIKE '[-a]'"],
  ['hyphen last in class', "'-' LIKE '[a-]'"],
  ['unclosed bracket', "'[' LIKE '['"],
  ['empty class', "'a' LIKE '[]'"],
  ['reversed range', "'a' LIKE '[z-a]'"],
  ['escaped percent', "'%' LIKE '!%' ESCAPE '!'"],
  ['escaped underscore', "'_' LIKE '!_' ESCAPE '!'"],
  ['escaped opening bracket', "'[' LIKE '![' ESCAPE '!'"],
  ['escaped escape character', "'!' LIKE '!!' ESCAPE '!'"],
  ['escape absent from pattern', "'abc' LIKE 'abc' ESCAPE '!'"],
  ['escape at pattern end', "'a!' LIKE 'a!' ESCAPE '!'"],
  ['two character escape', "'a' LIKE 'a' ESCAPE 'xx'"],
  ['empty escape', "'a' LIKE 'a' ESCAPE ''"],
  ['null escape', "'a' LIKE 'a' ESCAPE CAST(NULL AS VARCHAR(1))"],
  ['null source', "CAST(NULL AS VARCHAR(8)) LIKE 'a%'"],
  ['null pattern', "'abc' LIKE CAST(NULL AS VARCHAR(8))"],
  ['null not like', "'abc' NOT LIKE CAST(NULL AS VARCHAR(8))"],
  ['ansi pattern trailing blank', "'a' LIKE 'a '"],
  ['ansi source trailing blank', "'a ' LIKE 'a'"],
  ['unicode pattern trailing blank', "N'a' LIKE N'a '"],
  ['unicode source trailing blank', "N'a ' LIKE N'a'"],
  ['mixed unicode pattern trailing blank', "'a' LIKE N'a '"],
  ['mixed unicode source trailing blank', "N'a ' LIKE 'a'"],
  ['varchar fixed source trailing blank', "CAST('a' AS CHAR(3)) LIKE 'a'"],
  ['nvarchar fixed source trailing blank', "CAST(N'a' AS NCHAR(3)) LIKE N'a'"],
  ['bin2 case mismatch', "N'B' COLLATE Latin1_General_100_BIN2 LIKE N'[a-c]'"],
  ['bin2 case exact', "N'b' COLLATE Latin1_General_100_BIN2 LIKE N'[a-c]'"],
  ['ci case match', "N'B' COLLATE Latin1_General_100_CI_AS LIKE N'[a-c]'"],
  ['ci accent mismatch', "N'É' COLLATE Latin1_General_100_CI_AS LIKE N'[e]'"],
  ['ci ai accent match', "N'É' COLLATE Latin1_General_100_CI_AI LIKE N'[e]'"],
  ['bin2 supplementary underscore', "N'🦆' COLLATE Latin1_General_100_BIN2 LIKE N'_'"],
  ['ci supplementary underscore', "N'🦆' COLLATE Latin1_General_100_CI_AS LIKE N'_'"],
]) predicate(name, expression)

add('default database collation', "SELECT CONVERT(NVARCHAR(128),DATABASEPROPERTYEX(DB_NAME(),'Collation')) AS collation")
add('empty integer descriptor', "SELECT CASE WHEN N'a' LIKE N'[a]' THEN 1 ELSE 0 END AS matches WHERE 1=0")
add('empty nullable descriptor', "SELECT CASE WHEN CAST(NULL AS NVARCHAR(8)) LIKE N'a%' THEN 1 WHEN NOT (CAST(NULL AS NVARCHAR(8)) LIKE N'a%') THEN 0 ELSE NULL END AS matches WHERE 1=0")

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

async function prepared(connection, name, sql, values, withEscape = false) {
  let complete = () => {}
  const request = new Request(sql, (...args) => complete(...args))
  request.addParameter('value', TYPES.NVarChar, undefined, { length: 20 })
  request.addParameter('pattern', TYPES.NVarChar, undefined, { length: 20 })
  if (withEscape) request.addParameter('escape', TYPES.NVarChar, undefined, { length: 2 })
  await new Promise((resolve, reject) => {
    request.once('prepared', resolve)
    request.once('error', reject)
    connection.prepare(request)
  })
  const executions = []
  try {
    for (const values_ of values) {
      const bindings = { value: values_[0], pattern: values_[1], ...(withEscape ? { escape: values_[2] } : {}) }
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
          connection.execute(request, bindings)
        })
      } finally { detach() }
      executions.push({ bindings, result: canonical(result) })
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
  await record('create source', 'CREATE TABLE dbo.like_source(id INT PRIMARY KEY, ansi VARCHAR(20) NULL, utf NVARCHAR(20) NULL, pattern VARCHAR(20) NULL, utf_pattern NVARCHAR(20) NULL)')
  await record('insert source', "INSERT dbo.like_source VALUES(1,'b',N'b','[a-c]',N'[a-c]'),(2,'z',N'É','[^a-c]',N'[e]'),(3,'a ',N'a ', 'a',N'a'),(4,NULL,NULL,NULL,NULL)")
  await record('ansi source and pattern columns', 'SELECT id,CASE WHEN ansi LIKE pattern THEN 1 WHEN ansi NOT LIKE pattern THEN 0 ELSE NULL END AS matches FROM dbo.like_source ORDER BY id')
  await record('unicode source and pattern columns', 'SELECT id,CASE WHEN utf LIKE utf_pattern THEN 1 WHEN utf NOT LIKE utf_pattern THEN 0 ELSE NULL END AS matches FROM dbo.like_source ORDER BY id')
  await record('source with explicit bin2 collation', "SELECT id,CASE WHEN utf COLLATE Latin1_General_100_BIN2 LIKE N'[a-c]' THEN 1 WHEN utf COLLATE Latin1_General_100_BIN2 NOT LIKE N'[a-c]' THEN 0 ELSE NULL END AS matches FROM dbo.like_source ORDER BY id")
  await record('source empty descriptor', 'SELECT id,CASE WHEN utf LIKE utf_pattern THEN 1 ELSE 0 END AS matches FROM dbo.like_source WHERE 1=0')
  const bound = 'SELECT CASE WHEN @value LIKE @pattern THEN 1 WHEN @value NOT LIKE @pattern THEN 0 ELSE NULL END AS matches'
  for (const [name, type, value, pattern] of [
    ['rpc ansi range', TYPES.VarChar, 'b', '[a-c]'],
    ['rpc unicode range', TYPES.NVarChar, 'b', '[a-c]'],
    ['rpc unicode null', TYPES.NVarChar, null, '[a-c]'],
    ['rpc unicode replay', TYPES.NVarChar, 'b', '[a-c]'],
  ]) await record(name, bound, [
    ['value', type, value, { length: 20 }], ['pattern', type, pattern, { length: 20 }],
  ])
  const preparedResults = []
  preparedResults.push(await prepared(connection, 'prepared pattern reuse', bound,
    [['b', '[a-c]'], ['z', '[a-c]'], [null, '[a-c]'], ['b', '[a-c]']]))
  preparedResults.push(await prepared(connection, 'prepared escape recovery',
    "SELECT CASE WHEN @value LIKE @pattern ESCAPE @escape THEN 1 ELSE 0 END AS matches",
    [['%', '!%', '!'], ['%', '!%', 'xx'], ['%', '!%', null], ['%', '!%', '!']], true))
  return { records, prepared: preparedResults }
}

function validate(run) {
  assert.equal(run.records.length, cases.length + 2 + 4 + 4)
  assert.equal(new Set(run.records.map(record => record.name)).size, run.records.length)
  for (const record of run.records) assert(record.result.done.length > 0, `${record.name}: no completion`)
  const get = name => run.records.find(record => record.name === name)?.result
  assert.deepEqual(get('bracket range').sets[0].rows, [[1]])
  assert.deepEqual(get('bracket range outside').sets[0].rows, [[0]])
  assert.deepEqual(get('null source').sets[0].rows, [[null]])
  assert.equal(get('two character escape').errors[0]?.number, 506)
  assert.equal(get('two character escape').errors[0]?.state, 1)
  assert.equal(get('two character escape').sets[0].columns[0].type, 'IntN')
  assert.equal(get('two character escape').done[0].rowCount, null)
  assert.equal(get('empty escape').errors[0]?.number, 506)
  assert.deepEqual(get('null escape').sets[0].rows, [[1]])
  assert.deepEqual(get('ansi source trailing blank').sets[0].rows, [[1]])
  assert.deepEqual(get('unicode source trailing blank').sets[0].rows, [[0]])
  assert.deepEqual(get('bin2 case mismatch').sets[0].rows, [[0]])
  assert.deepEqual(get('ci case match').sets[0].rows, [[1]])
  assert.deepEqual(get('ci accent mismatch').sets[0].rows, [[0]])
  assert.deepEqual(get('ci ai accent match').sets[0].rows, [[1]])
  assert.deepEqual(get('bin2 supplementary underscore').sets[0].rows, [[0]])
  assert.deepEqual(get('empty integer descriptor').sets[0].rows, [])
  assert.equal(get('empty integer descriptor').sets[0].columns[0].type, 'Int')
  assert.equal(get('empty nullable descriptor').sets[0].columns[0].type, 'IntN')
  assert.deepEqual(get('source empty descriptor').sets[0].rows, [])
  for (const entry of run.prepared) {
    assert.equal(entry.executions.length, 4)
    for (const execution of entry.executions) assert(execution.result.done.length > 0, `${entry.name}: no completion`)
    assert.deepEqual(entry.executions[0].result, entry.executions[3].result, `${entry.name}: replay differs`)
  }
  const escape = run.prepared.find(entry => entry.name === 'prepared escape recovery')
  assert.equal(escape.executions[1].result.errors[0]?.number, 506)
  assert.equal(escape.executions[1].result.errors[0]?.state, 2)
  assert.equal(escape.executions[1].result.returnStatus, -6)
  assert.deepEqual(escape.executions[2].result.sets[0].rows, [[0]])
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
    if (!oneDatabase) assert.deepEqual(runs[0], runs[1], 'LIKE observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) assert.deepEqual(containers[0].runs[0], containers[1].runs[0], 'LIKE observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
if (retained && !oneDatabase) assert.deepEqual(actual, retained, 'LIKE observations differ from retained fixture')
if (writeFixture) {
  if (retained) throw new Error('refusing to overwrite retained fixture')
  await writeFile(fixture, JSON.stringify(actual) + '\n')
}
console.log(`Captured ${containers[0].runs[0].records.length} LIKE programs and ${containers[0].runs[0].prepared.length} prepared programs` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained && !oneDatabase ? ' and matched retained fixture' : ''))
