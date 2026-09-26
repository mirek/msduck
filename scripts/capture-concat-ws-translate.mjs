#!/usr/bin/env node
// Retain SQL Server CONCAT_WS and TRANSLATE rows, descriptors, diagnostics and completions.
// Usage: capture-concat-ws-translate.mjs [output] [--write-fixture | --one-database | --check-fixture]
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { isDeepStrictEqual } from 'node:util'
import { resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { referenceImage, withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'

const fixture = new URL('../reference/concat-ws-translate.json', import.meta.url)
const flags = ['--write-fixture', '--one-database', '--check-fixture']
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const checkFixture = args.includes('--check-fixture')
const positional = args.filter(arg => !flags.includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
if (checkFixture && (writeFixture || oneDatabase || positional.length)) throw new Error('--check-fixture takes no other arguments')
const output = resolve(positional[0] ?? 'artifacts/compatibility/concat-ws-translate/capture.json')

const list = (count, value) => Array.from({ length: count }, () => value).join(',')

const concatWsCases = [
  ['cws ansi basic', "SELECT CONCAT_WS(',','a','b','c') AS value"],
  ['cws unicode basic', "SELECT CONCAT_WS(N',',N'a',N'b',N'c') AS value"],
  ['cws null argument skipped', "SELECT CONCAT_WS(',','a',NULL,'b') AS value"],
  ['cws typed null arguments skipped', "SELECT CONCAT_WS(',',CAST(NULL AS VARCHAR(5)),'a',CAST(NULL AS NVARCHAR(5)),'b',CAST(NULL AS INT)) AS value"],
  ['cws all null arguments', "SELECT CONCAT_WS(',',NULL,NULL) AS value"],
  ['cws all typed null arguments', "SELECT CONCAT_WS(',',CAST(NULL AS VARCHAR(5)),CAST(NULL AS VARCHAR(5))) AS value"],
  ['cws null literal separator', "SELECT CONCAT_WS(NULL,'a','b') AS value"],
  ['cws typed null separator', "SELECT CONCAT_WS(CAST(NULL AS VARCHAR(3)),'a','b') AS value"],
  ['cws all null with null separator', "SELECT CONCAT_WS(NULL,NULL,NULL) AS value"],
  ['cws empty separator', "SELECT CONCAT_WS('','a','b') AS value"],
  ['cws empty string arguments kept', "SELECT CONCAT_WS(',','a','','b','') AS value"],
  ['cws multi character separator', "SELECT CONCAT_WS(' - ','a ',' b') AS value"],
  ['cws char padding', "SELECT CONCAT_WS('|',CAST('a' AS CHAR(3)),CAST('b' AS NCHAR(2))) AS value"],
  ['cws one argument', "SELECT CONCAT_WS(',','a') AS value"],
  ['cws separator only null', "SELECT CONCAT_WS(NULL) AS value"],
  ['cws no arguments', 'SELECT CONCAT_WS() AS value'],
  ['cws 254 arguments', `SELECT CONCAT_WS('',${list(253, "'x'")}) AS value`],
  ['cws 255 arguments', `SELECT CONCAT_WS('',${list(254, "'x'")}) AS value`],
  ['cws 256 arguments', `SELECT CONCAT_WS('',${list(255, "'x'")}) AS value`],
  ['cws varchar widths', "SELECT CONCAT_WS('-',CAST('a' AS VARCHAR(10)),CAST('b' AS VARCHAR(20))) AS value"],
  ['cws varchar widths with long separator', "SELECT CONCAT_WS(CAST('--' AS VARCHAR(7)),CAST('a' AS VARCHAR(10)),CAST('b' AS VARCHAR(20)),CAST('c' AS VARCHAR(30))) AS value"],
  ['cws nvarchar widths', "SELECT CONCAT_WS(N'-',CAST(N'a' AS NVARCHAR(10)),CAST(N'b' AS NVARCHAR(20))) AS value"],
  ['cws mixed varchar nvarchar', "SELECT CONCAT_WS('-',CAST('a' AS VARCHAR(10)),CAST(N'b' AS NVARCHAR(20))) AS value"],
  ['cws unicode separator ansi arguments', "SELECT CONCAT_WS(N'-',CAST('a' AS VARCHAR(10)),CAST('b' AS VARCHAR(20))) AS value"],
  ['cws varchar width over 8000', "SELECT CONCAT_WS('-',CAST('a' AS VARCHAR(5000)),CAST('b' AS VARCHAR(5000))) AS value"],
  ['cws nvarchar width over 4000', "SELECT CONCAT_WS(N'-',CAST(N'a' AS NVARCHAR(3000)),CAST(N'b' AS NVARCHAR(3000))) AS value"],
  ['cws varchar long value', "SELECT LEN(CONCAT_WS('-',CAST(REPLICATE('x',5000) AS VARCHAR(5000)),CAST(REPLICATE('y',5000) AS VARCHAR(5000)))) AS length,DATALENGTH(CONCAT_WS('-',CAST(REPLICATE('x',5000) AS VARCHAR(5000)),CAST(REPLICATE('y',5000) AS VARCHAR(5000)))) AS bytes"],
  ['cws nvarchar long value', "SELECT LEN(CONCAT_WS(N'-',CAST(REPLICATE(N'x',3000) AS NVARCHAR(3000)),CAST(REPLICATE(N'y',3000) AS NVARCHAR(3000)))) AS length,DATALENGTH(CONCAT_WS(N'-',CAST(REPLICATE(N'x',3000) AS NVARCHAR(3000)),CAST(REPLICATE(N'y',3000) AS NVARCHAR(3000)))) AS bytes"],
  ['cws varchar max argument', "SELECT CONCAT_WS('-',CAST('a' AS VARCHAR(MAX)),'b') AS value"],
  ['cws nvarchar max argument', "SELECT CONCAT_WS('-',CAST(N'a' AS NVARCHAR(MAX)),'b') AS value"],
  ['cws varchar max separator', "SELECT CONCAT_WS(CAST('-' AS VARCHAR(MAX)),'a','b') AS value"],
  ['cws varchar max long value', "SELECT DATALENGTH(CONCAT_WS('-',CAST(REPLICATE('x',5000) AS VARCHAR(MAX)),REPLICATE('y',5000))) AS bytes"],
  ['cws integers', "SELECT CONCAT_WS(',',1,-2,CAST(3 AS TINYINT),CAST(4 AS SMALLINT),CAST(5 AS BIGINT)) AS value"],
  ['cws integer separator', "SELECT CONCAT_WS(0,'a','b') AS value"],
  ['cws decimal float money bit', "SELECT CONCAT_WS('|',CAST(1.50 AS DECIMAL(8,2)),CAST(1.5 AS FLOAT),CAST(1.5 AS REAL),CAST(2.5 AS MONEY),CAST(1 AS BIT)) AS value"],
  ['cws dates', "SELECT CONCAT_WS('|',CAST('2024-01-02' AS DATE),CAST('2024-01-02T03:04:05.123' AS DATETIME),CAST('2024-01-02T03:04:05.1234567' AS DATETIME2),CAST('2024-01-02T03:04:00' AS SMALLDATETIME),CAST('03:04:05.12' AS TIME(2)),CAST('2024-01-02T03:04:05+01:30' AS DATETIMEOFFSET(0))) AS value"],
  ['cws uniqueidentifier', "SELECT CONCAT_WS('|',CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER),'x') AS value"],
  ['cws binary argument', "SELECT CONCAT_WS('|',0x4142,'x') AS value"],
  ['cws sql_variant argument', "SELECT CONCAT_WS('|',CAST(1 AS SQL_VARIANT),'x') AS value"],
  ['cws xml argument', "SELECT CONCAT_WS('|',CAST('<a/>' AS XML),'x') AS value"],
  ['cws explicit collation', "SELECT CONCAT_WS(',','a' COLLATE Latin1_General_100_BIN2,'b') AS value"],
  ['cws conflicting explicit collations', "SELECT CONCAT_WS(',','a' COLLATE Latin1_General_100_BIN2,'b' COLLATE Latin1_General_100_CS_AS) AS value"],
  ['cws implicit column collations', 'SELECT CONCAT_WS(N\',\',bin,cs) AS value FROM dbo.cwt_src WHERE id=1'],
  ['cws supplementary unicode', "SELECT CONCAT_WS(N'😀',N'a',N'𝄞') AS value,LEN(CONCAT_WS(N'😀',N'a',N'𝄞')) AS length"],
  ['cws supplementary to varchar', "SELECT CONCAT_WS('-',N'😀','a') AS value"],
  ['cws lone surrogate', "SELECT CONCAT_WS(N'-',NCHAR(55357),N'a') AS value,DATALENGTH(CONCAT_WS(N'-',NCHAR(55357),N'a')) AS bytes"],
  ['cws column rows', 'SELECT id,CONCAT_WS(N\',\',a,u,n) AS value FROM dbo.cwt_src ORDER BY id'],
  ['cws column null separator', 'SELECT id,CONCAT_WS(sep,a,u) AS value FROM dbo.cwt_src ORDER BY id'],
  ['cws in predicate', "SELECT id FROM dbo.cwt_src WHERE CONCAT_WS(',',a,u)=N'a,u' ORDER BY id"],
]

const translateCases = [
  ['tr ansi basic', "SELECT TRANSLATE('2*[3+4]/{7-2}','[]{}','()()') AS value"],
  ['tr unicode basic', "SELECT TRANSLATE(N'2*[3+4]/{7-2}',N'[]{}',N'()()') AS value"],
  ['tr null input', "SELECT TRANSLATE(CAST(NULL AS VARCHAR(10)),'a','b') AS value"],
  ['tr null literal input', "SELECT TRANSLATE(NULL,'a','b') AS value"],
  ['tr null characters', "SELECT TRANSLATE('abc',NULL,'b') AS value"],
  ['tr null translations', "SELECT TRANSLATE('abc','a',NULL) AS value"],
  ['tr empty characters', "SELECT TRANSLATE('abc','','') AS value"],
  ['tr empty input', "SELECT TRANSLATE('','a','b') AS value"],
  ['tr length mismatch longer', "SELECT TRANSLATE('abc','ab','x') AS value"],
  ['tr length mismatch shorter', "SELECT TRANSLATE('abc','a','xy') AS value"],
  ['tr trailing space mismatch', "SELECT TRANSLATE('a b','a ','x') AS value"],
  ['tr trailing space translation', "SELECT TRANSLATE('a b','a ','x_') AS value"],
  ['tr duplicate characters', "SELECT TRANSLATE('aab','aa','xy') AS value"],
  ['tr no chaining', "SELECT TRANSLATE('abc','ab','bc') AS value"],
  ['tr case insensitive default', "SELECT TRANSLATE('ABCabc','abc','xyz') AS value"],
  ['tr accent insensitive probe', "SELECT TRANSLATE(N'eéE',N'e',N'x') AS value"],
  ['tr binary collation', "SELECT TRANSLATE('ABCabc' COLLATE Latin1_General_100_BIN2,'abc','xyz') AS value"],
  ['tr case sensitive collation', "SELECT TRANSLATE('ABCabc' COLLATE Latin1_General_100_CS_AS,'abc','xyz') AS value"],
  ['tr conflicting explicit collations', "SELECT TRANSLATE('abc' COLLATE Latin1_General_100_BIN2,'a' COLLATE Latin1_General_100_CS_AS,'x') AS value"],
  ['tr varchar width', "SELECT TRANSLATE(CAST('abc' AS VARCHAR(10)),CAST('a' AS VARCHAR(30)),CAST('x' AS VARCHAR(40))) AS value"],
  ['tr nvarchar width', "SELECT TRANSLATE(CAST(N'abc' AS NVARCHAR(10)),N'a',N'x') AS value"],
  ['tr varchar input unicode characters', "SELECT TRANSLATE(CAST('abc' AS VARCHAR(10)),N'a',N'x') AS value"],
  ['tr nvarchar input ansi characters', "SELECT TRANSLATE(CAST(N'abc' AS NVARCHAR(10)),'a','x') AS value"],
  ['tr char input', "SELECT TRANSLATE(CAST('ab' AS CHAR(5)),'b ','x_') AS value"],
  ['tr varchar max input', "SELECT TRANSLATE(CAST('abc' AS VARCHAR(MAX)),'a','x') AS value"],
  ['tr nvarchar max input', "SELECT TRANSLATE(CAST(N'abc' AS NVARCHAR(MAX)),N'a',N'x') AS value"],
  ['tr varchar max characters', "SELECT TRANSLATE(CAST('abc' AS VARCHAR(10)),CAST('a' AS VARCHAR(MAX)),'x') AS value"],
  ['tr long varchar max value', "SELECT DATALENGTH(TRANSLATE(REPLICATE(CAST('a' AS VARCHAR(MAX)),9000),'a','b')) AS bytes,LEFT(TRANSLATE(REPLICATE(CAST('a' AS VARCHAR(MAX)),9000),'a','b'),3) AS prefix"],
  ['tr long nvarchar max value', "SELECT DATALENGTH(TRANSLATE(REPLICATE(CAST(N'a' AS NVARCHAR(MAX)),9000),N'a',N'b')) AS bytes"],
  ['tr integer input', "SELECT TRANSLATE(12321,'1','9') AS value"],
  ['tr decimal input', "SELECT TRANSLATE(CAST(1.50 AS DECIMAL(8,2)),'.','') AS value"],
  ['tr date input', "SELECT TRANSLATE(CAST('2024-01-02' AS DATE),'-','/') AS value"],
  ['tr binary input', "SELECT TRANSLATE(0x4142,'A','B') AS value"],
  ['tr integer characters', "SELECT TRANSLATE('a1b','1',2) AS value"],
  ['tr two arguments', "SELECT TRANSLATE('abc','a') AS value"],
  ['tr four arguments', "SELECT TRANSLATE('abc','a','b','c') AS value"],
  ['tr supplementary default collation', "SELECT TRANSLATE(N'a😀b',N'😀',N'xy') AS value"],
  ['tr supplementary default collation mismatch', "SELECT TRANSLATE(N'a😀b',N'😀',N'x') AS value"],
  ['tr supplementary sc collation', "SELECT TRANSLATE(N'a😀b' COLLATE Latin1_General_100_CI_AS_SC,N'😀',N'x') AS value"],
  ['tr supplementary sc collation pair', "SELECT TRANSLATE(N'a😀b' COLLATE Latin1_General_100_CI_AS_SC,N'😀',N'xy') AS value"],
  ['tr supplementary replacement', "SELECT TRANSLATE(N'abc' COLLATE Latin1_General_100_CI_AS_SC,N'b',N'😀') AS value"],
  ['tr lone surrogate', "SELECT TRANSLATE(N'a'+NCHAR(55357)+N'b',NCHAR(55357),N'x') AS value"],
  ['tr utf8 collation', "SELECT TRANSLATE(CAST(N'aéb' AS VARCHAR(10)) COLLATE Latin1_General_100_CI_AS_SC_UTF8,N'é',N'e') AS value"],
  ['tr column rows', 'SELECT id,TRANSLATE(a,\'au\',\'AU\') AS value FROM dbo.cwt_src ORDER BY id'],
  ['tr column mismatch row', "SELECT id,TRANSLATE(N'a-b+c',sep,N'x') AS value FROM dbo.cwt_src ORDER BY id"],
]

const cases = [...concatWsCases, ...translateCases]

const errorFields = e => ({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })

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

// Prepare once, execute each value set on the same handle, then unprepare.
async function prepared(connection, sql, declarations, valueSets) {
  let result
  let complete = () => {}
  const fresh = () => ({ sets: [], done: [], errors: [], info: [], returnStatus: null })
  const request = new Request(sql, (...args) => complete(...args))
  for (const [name, type, options] of declarations) request.addParameter(name, type, undefined, options)
  const onError = e => result?.errors.push(errorFields(e))
  const onInfo = e => result?.info.push(errorFields(e))
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(x => ({ name: x.colName, type: x.type.name, length: x.dataLength ?? null, precision: x.precision ?? null, scale: x.scale ?? null, flags: x.flags, collation: canonical(x.collation ?? null) })), rows: [] }))
  request.on('row', row => result.sets.at(-1).rows.push(row.map(x => x.value)))
  for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => result?.done.push({ kind, rowCount: rowCount ?? null, more }))
  request.on('doneProc', (_count, _more, status) => { if (result) result.returnStatus = status })
  const phase = start => new Promise(resolveDone => {
    result = fresh()
    complete = (error, rowCount) => {
      result.rowCount = rowCount
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      resolveDone(result)
    }
    start()
  })
  // Tedious reports sp_prepare completion through 'prepared'/'error', not the request callback.
  const preparePhase = () => new Promise(resolveDone => {
    result = fresh()
    const finish = error => {
      request.off('prepared', onPrepared)
      request.off('error', onFailed)
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      result.prepared = !error
      resolveDone(result)
    }
    const onPrepared = () => finish()
    const onFailed = error => finish(error)
    request.once('prepared', onPrepared)
    request.once('error', onFailed)
    connection.prepare(request)
  })
  try {
    const prepare = await preparePhase()
    const executions = []
    for (const values of valueSets) {
      request.error = undefined
      executions.push({ values, result: await phase(() => connection.execute(request, values)) })
    }
    // Tedious keeps a failed execution's error on the request; do not report it for sp_unprepare.
    request.error = undefined
    const unprepare = await phase(() => connection.unprepare(request))
    return canonical({ prepare, executions, unprepare })
  } finally {
    result = undefined
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

const rpcPrograms = [
  ['rpc cws unicode', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, ',', { length: 5 }], ['a', TYPES.NVarChar, 'a', { length: 10 }], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc cws ansi', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.VarChar, ',', { length: 5 }], ['a', TYPES.VarChar, 'a', { length: 10 }], ['b', TYPES.VarChar, 'b', { length: 20 }]]],
  ['rpc cws null separator', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, null, { length: 5 }], ['a', TYPES.NVarChar, 'a', { length: 10 }], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc cws null argument', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, ',', { length: 5 }], ['a', TYPES.NVarChar, null, { length: 10 }], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc cws mixed types', "SELECT CONCAT_WS(@sep,@a,@n,@d) AS value", [
    ['sep', TYPES.VarChar, '|', { length: 1 }], ['a', TYPES.NVarChar, 'a', { length: 10 }], ['n', TYPES.Int, 42], ['d', TYPES.Date, new Date('2024-01-02T00:00:00Z')]]],
  ['rpc cws inferred length argument', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, ',', { length: 5 }], ['a', TYPES.NVarChar, 'a'], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc cws max argument', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, ',', { length: 5 }], ['a', TYPES.NVarChar, 'a', { length: 5000 }], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc cws unicode replay', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, ',', { length: 5 }], ['a', TYPES.NVarChar, 'a', { length: 10 }], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc tr unicode', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, '[a]', { length: 20 }], ['from', TYPES.NVarChar, '[]', { length: 5 }], ['to', TYPES.NVarChar, '()', { length: 5 }]]],
  ['rpc tr ansi', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.VarChar, '[a]', { length: 20 }], ['from', TYPES.VarChar, '[]', { length: 5 }], ['to', TYPES.VarChar, '()', { length: 5 }]]],
  ['rpc tr max input', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, '[a]', { length: 5000 }], ['from', TYPES.NVarChar, '[]', { length: 5 }], ['to', TYPES.NVarChar, '()', { length: 5 }]]],
  ['rpc tr null input', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, null, { length: 20 }], ['from', TYPES.NVarChar, '[]', { length: 5 }], ['to', TYPES.NVarChar, '()', { length: 5 }]]],
  ['rpc tr length mismatch', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, '[a]', { length: 20 }], ['from', TYPES.NVarChar, '[]', { length: 5 }], ['to', TYPES.NVarChar, '(', { length: 5 }]]],
  ['rpc tr mismatch then select', "SELECT TRANSLATE(@input,@from,@to) AS value; SELECT 1 AS after_error", [
    ['input', TYPES.NVarChar, 'abc', { length: 20 }], ['from', TYPES.NVarChar, 'ab', { length: 5 }], ['to', TYPES.NVarChar, 'x', { length: 5 }]]],
  ['rpc tr unicode replay', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, '[a]', { length: 20 }], ['from', TYPES.NVarChar, '[]', { length: 5 }], ['to', TYPES.NVarChar, '()', { length: 5 }]]],
]

const preparedPrograms = [
  ['prepared cws', 'SELECT CONCAT_WS(@sep,@a,@b) AS value', [
    ['sep', TYPES.NVarChar, { length: 5 }], ['a', TYPES.NVarChar, { length: 10 }], ['b', TYPES.VarChar, { length: 20 }]], [
    { sep: ',', a: 'a', b: 'b' },
    { sep: null, a: 'a', b: 'b' },
    { sep: ',', a: null, b: 'b' },
    { sep: ',', a: null, b: null },
    { sep: '😀', a: '𝄞', b: 'b' },
  ]],
  ['prepared tr', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, { length: 20 }], ['from', TYPES.NVarChar, { length: 5 }], ['to', TYPES.NVarChar, { length: 5 }]], [
    { input: '[a]', from: '[]', to: '()' },
    { input: null, from: '[]', to: '()' },
    { input: '[a]', from: '[]', to: '(' },
    { input: '[a]', from: '', to: '' },
    { input: 'a😀b', from: '😀', to: 'xy' },
    { input: '[a]', from: '[]', to: '()' },
  ]],
  ['prepared tr ansi', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.VarChar, { length: 20 }], ['from', TYPES.VarChar, { length: 5 }], ['to', TYPES.VarChar, { length: 5 }]], [
    { input: 'abc', from: 'ab', to: 'xy' },
    { input: 'abc', from: 'ab', to: 'x' },
  ]],
]

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    if (process.env.CWT_TRACE) console.error(name)
    const result = canonical(await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql)))
    records.push({
      name, sql,
      ...(parameters ? { parameters: parameters.map(([parameter, type, value, options]) => ({
        name: parameter, type: type.name, value: canonical(value), ...(options ? { options } : {}),
      })) } : {}),
      result,
    })
  }
  await record('server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version,CAST(SERVERPROPERTY('Collation') AS NVARCHAR(128)) AS server_collation,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
  await record('create source', 'CREATE TABLE dbo.cwt_src(id INT,sep NVARCHAR(3),a VARCHAR(10),u NVARCHAR(10),n INT,bin VARCHAR(5) COLLATE Latin1_General_100_BIN2,cs VARCHAR(5) COLLATE Latin1_General_100_CS_AS)')
  await record('insert source', "INSERT dbo.cwt_src VALUES (1,N'-','a',N'u',1,'x','y'),(2,NULL,'a',NULL,NULL,NULL,NULL),(3,N'+',NULL,N'u',3,NULL,NULL),(4,N'',NULL,NULL,NULL,NULL,NULL)")
  for (const [name, sql] of cases) await record(name, sql)
  for (const [name, sql, parameters] of rpcPrograms) await record(name, sql, parameters)
  for (const [name, sql, declarations, valueSets] of preparedPrograms) {
    if (process.env.CWT_TRACE) console.error(name)
    records.push({
      name, sql,
      declarations: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, ...(options ? { options } : {}) })),
      prepared: await prepared(connection, sql, declarations, valueSets),
    })
  }
  await record('connection reusable', 'SELECT @@TRANCOUNT AS transaction_count,1 AS reusable')
  return records
}

function validate(run) {
  assert.equal(run.length, 3 + cases.length + rpcPrograms.length + preparedPrograms.length + 1)
  const get = name => {
    const found = run.find(record => record.name === name)
    assert(found, 'missing ' + name)
    return found.result
  }
  const rows = name => get(name).sets[0]?.rows
  const errorNumber = name => get(name).errors[0]?.number
  assert.deepEqual(rows('cws ansi basic'), [['a,b,c']])
  assert.deepEqual(rows('cws null argument skipped'), [['a,b']])
  assert.deepEqual(rows('cws null literal separator'), [['ab']])
  assert.deepEqual(rows('cws empty string arguments kept'), [['a,,b,']])
  assert.deepEqual(rows('tr ansi basic'), [['2*(3+4)/(7-2)']])
  assert.deepEqual(rows('tr null input'), [[null]])
  assert.deepEqual(rows('rpc tr unicode'), [['(a)']])
  assert.equal(get('cws 254 arguments').errors.length, 0)
  assert.notEqual(errorNumber('cws one argument'), undefined)
  assert.notEqual(errorNumber('cws 255 arguments'), undefined)
  assert.notEqual(errorNumber('tr length mismatch longer'), undefined)
  assert.notEqual(errorNumber('rpc tr length mismatch'), undefined)
  assert.notEqual(errorNumber('tr column mismatch row'), undefined)
  for (const record of run) {
    if (record.prepared) {
      assert.equal(record.prepared.unprepare.errors.length, 0, record.name + ': unprepare error')
      for (const phase of [record.prepared.prepare, ...record.prepared.executions.map(e => e.result), record.prepared.unprepare]) {
        assert(phase.done.length > 0, record.name + ': no prepared completion')
      }
    } else assert(record.result.done.length > 0, record.name + ': no completion')
  }
}

// Never hand whole captures to node:assert: a failing assertion would render them.
function firstDifference(left, right) {
  if (left.length !== right.length) return 'record count ' + left.length + ' vs ' + right.length
  const index = left.findIndex((record, i) => !isDeepStrictEqual(record, right[i]))
  return index < 0 ? null : 'record ' + index + ' (' + left[index].name + ')'
}

function requireSame(left, right, label) {
  const difference = firstDifference(left, right)
  if (difference) throw new Error('CONCAT_WS/TRANSLATE observations differ ' + label + ' at ' + difference)
}

function validateFourCaptures(actual) {
  if (actual.containers.length !== 2) throw new Error('expected two containers')
  for (const container of actual.containers) {
    if (container.image !== referenceImage) throw new Error('unexpected reference image ' + container.image)
    if (container.runs.length !== 2) throw new Error('expected two fresh databases per container')
    for (const run of container.runs) validate(run)
    requireSame(container.runs[0], container.runs[1], 'across fresh databases')
  }
  requireSame(actual.containers[0].runs[0], actual.containers[1].runs[0], 'across containers')
}

async function readRetained() {
  try { return JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
}

if (checkFixture) {
  const retained = await readRetained()
  if (!retained) throw new Error('no retained fixture')
  validateFourCaptures(retained)
  console.log('Retained fixture holds ' + retained.containers[0].runs[0].length + ' matching CONCAT_WS/TRANSLATE observations in four fresh databases across two containers')
} else {
  if (writeFixture && existsSync(fixture)) throw new Error('refusing to overwrite retained fixture')
  await mkdir(resolve(output, '..'), { recursive: true })
  const containers = []
  for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
    await withReferenceContainer(async (config, container) => {
      const runs = []
      for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
        const run = await isolatedReference({
          ...config, options: { ...config.options, requestTimeout: 120000 },
        }, observe)
        runs.push(run)
      }
      containers.push({ image: container.image, runs })
    })
  }
  const actual = { containers }
  // Retain the raw artifact before validation so a failing capture can be inspected.
  await writeFile(output, JSON.stringify(actual) + '\n')
  if (oneDatabase) validate(containers[0].runs[0])
  else validateFourCaptures(actual)
  const retained = writeFixture ? undefined : await readRetained()
  if (retained && !oneDatabase) {
    for (let c = 0; c < 2; c++) for (let d = 0; d < 2; d++) {
      requireSame(actual.containers[c].runs[d], retained.containers[c].runs[d], 'from retained fixture (container ' + c + ', database ' + d + ')')
    }
    if (!isDeepStrictEqual(actual, retained)) throw new Error('CONCAT_WS/TRANSLATE capture envelope differs from retained fixture')
  }
  if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
  console.log('Captured ' + containers[0].runs[0].length + ' CONCAT_WS/TRANSLATE observations' +
    (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
    (retained && !oneDatabase ? ' and matched retained fixture' : '') +
    (writeFixture ? '; wrote new retained fixture' : ''))
}
