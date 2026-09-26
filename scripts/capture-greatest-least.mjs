#!/usr/bin/env node
// Retain SQL Server GREATEST/LEAST rows, descriptors, diagnostics and completions.
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { mkdir, readFile, realpath, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { isDeepStrictEqual } from 'node:util'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'

const fixture = new URL('../reference/greatest-least.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/greatest-least/capture.json')

// Ordered token evidence. The shared capture() buckets metadata, messages and
// DONE events separately and keeps only DONE kind/count/more, so this script
// also records every token the request handler sees, in wire order, with the
// raw DONE status word, current command and count field. tedious drops the
// INXACT bit and invalid counts, so the DONE parsers are wrapped to retain the
// 12 raw bytes. Both hooks are local to this process.
const require = createRequire(import.meta.url)
const doneParsers = require('tedious/lib/token/done-token-parser.js')
const { RequestTokenHandler } = require('tedious/lib/token/handler.js')
for (const name of ['doneParser', 'doneInProcParser', 'doneProcParser']) {
  const original = doneParsers[name]
  assert.equal(typeof original, 'function', name)
  doneParsers[name] = (buffer, offset, options) => {
    const result = original(buffer, offset, options)
    assert(options.tdsVersion >= '7_2', 'expected an 8-byte DONE count')
    result.value.raw = {
      status: buffer.readUInt16LE(offset),
      command: buffer.readUInt16LE(offset + 2),
      count: buffer.readBigUInt64LE(offset + 4).toString(),
    }
    return result
  }
}
const taps = new WeakMap()
const message = token => ({ number: token.number, state: token.state, class: token.class, lineNumber: token.lineNumber, procName: token.procName, message: token.message })
const done = token => ({ status: token.raw.status, command: token.raw.command, count: token.raw.count })
for (const [method, describe] of Object.entries({
  onColMetadata: token => ({ columns: token.columns.map(column => column.colName) }),
  onRow: () => ({}),
  onOrder: token => ({ columns: token.orderColumns }),
  onErrorMessage: message,
  onInfoMessage: message,
  onReturnStatus: token => ({ value: token.value }),
  onReturnValue: token => ({ paramName: token.paramName, value: token.value }),
  onDone: done,
  onDoneInProc: done,
  onDoneProc: done,
  onDatabaseChange: token => ({ newValue: token.newValue }),
  onLanguageChange: token => ({ newValue: token.newValue }),
  onCharsetChange: token => ({ newValue: token.newValue }),
  onSqlCollationChange: token => ({ newValue: token.newValue }),
  onPacketSizeChange: token => ({ newValue: token.newValue }),
  onBeginTransaction: token => ({ newValue: token.newValue }),
  onCommitTransaction: token => ({ oldValue: token.oldValue }),
  onRollbackTransaction: token => ({ oldValue: token.oldValue }),
  onResetConnection: () => ({}),
})) {
  const original = RequestTokenHandler.prototype[method]
  assert.equal(typeof original, 'function', method)
  RequestTokenHandler.prototype[method] = function (token) {
    taps.get(this.request)?.push(canonical({ token: token.name, ...describe(token) }))
    return original.call(this, token)
  }
}

// Reject an output destination that resolves to the retained fixture,
// including symlinked directories and a not-yet-existing file.
async function canonicalPath(path) {
  const absolute = resolve(path instanceof URL ? fileURLToPath(path) : path)
  try { return await realpath(absolute) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    const parent = dirname(absolute)
    if (parent === absolute) return absolute
    return resolve(await canonicalPath(parent), basename(absolute))
  }
}
if (await canonicalPath(output) === await canonicalPath(fixture)) {
  throw new Error('refusing to write capture output over retained fixture ' + fileURLToPath(fixture))
}

const numbered = count => Array.from({ length: count }, (_, index) => String(index + 1)).join(',')

// Each pair projects GREATEST and LEAST over the same argument list and is
// also described through sys.dm_exec_describe_first_result_set.
const pairs = [
  // NULL handling and arity.
  ['int with null', '1,NULL,3'],
  ['typed null first', 'CAST(NULL AS INT),2'],
  ['all typed nulls', 'CAST(NULL AS INT),CAST(NULL AS INT)'],
  ['untyped nulls', 'NULL,NULL'],
  ['single untyped null', 'NULL'],
  ['single int', '5'],
  ['negative ints', '-5,-3,-10'],
  ['254 arguments', numbered(254)],
  ['255 arguments', numbered(255)],
  // Integer family.
  ['tinyint smallint', 'CAST(1 AS TINYINT),CAST(2 AS SMALLINT)'],
  ['tinyint only', 'CAST(1 AS TINYINT),CAST(200 AS TINYINT)'],
  ['int bigint', 'CAST(1 AS INT),CAST(2 AS BIGINT)'],
  ['bit int', 'CAST(1 AS BIT),0'],
  ['bit only', 'CAST(1 AS BIT),CAST(0 AS BIT)'],
  // Exact numerics.
  ['decimal literals', '1.5,2.25'],
  ['int decimal', '7,CAST(3.25 AS DECIMAL(5,2))'],
  ['bigint decimal', 'CAST(7 AS BIGINT),CAST(3.25 AS DECIMAL(5,2))'],
  ['decimal widths', 'CAST(1.5 AS DECIMAL(5,2)),CAST(2.25 AS DECIMAL(10,4))'],
  ['decimal scale beats integer digits', 'CAST(123456 AS DECIMAL(6,0)),CAST(0.12345 AS DECIMAL(5,5))'],
  ['decimal precision cap', 'CAST(1 AS DECIMAL(38,0)),CAST(0.5 AS DECIMAL(38,10))'],
  ['decimal cap overflow', 'CAST(99999999999999999999999999999999999999 AS DECIMAL(38,0)),CAST(0.5 AS DECIMAL(38,10))'],
  ['decimal 38 38 int', 'CAST(0.5 AS DECIMAL(38,38)),1'],
  ['numeric decimal', 'CAST(1.5 AS NUMERIC(4,1)),CAST(2.5 AS DECIMAL(4,1))'],
  // Approximate numerics.
  ['int float', '1,CAST(2.5 AS FLOAT)'],
  ['real float', 'CAST(1.5 AS REAL),CAST(0.1 AS FLOAT)'],
  ['real int', 'CAST(1.5 AS REAL),2'],
  ['decimal float', 'CAST(1.25 AS DECIMAL(5,2)),CAST(1 AS FLOAT)'],
  ['float literal', '1e0,2'],
  // Money.
  ['money int', 'CAST(1.2345 AS MONEY),2'],
  ['smallmoney money', 'CAST(1.5 AS SMALLMONEY),CAST(2.25 AS MONEY)'],
  ['smallmoney int', 'CAST(1.5 AS SMALLMONEY),1'],
  ['money decimal', 'CAST(1.2345 AS MONEY),CAST(1.5 AS DECIMAL(10,2))'],
  ['money float', 'CAST(1.2345 AS MONEY),CAST(1 AS FLOAT)'],
  // Character types.
  ['varchar literals', "'abc','b'"],
  ['varchar widths', "CAST('abc' AS VARCHAR(5)),CAST('b' AS VARCHAR(10))"],
  ['char varchar', "CAST('ab' AS CHAR(3)),CAST('ab' AS VARCHAR(5))"],
  ['char widths', "CAST('a' AS CHAR(3)),CAST('b' AS CHAR(5))"],
  ['varchar nvarchar', "CAST('a' AS VARCHAR(10)),CAST(N'b' AS NVARCHAR(3))"],
  ['nchar char', "CAST(N'a' AS NCHAR(2)),CAST('b' AS CHAR(6))"],
  ['varchar max', "CAST('a' AS VARCHAR(MAX)),CAST('b' AS VARCHAR(10))"],
  ['nvarchar max varchar', "CAST(N'a' AS NVARCHAR(MAX)),CAST('b' AS VARCHAR(10))"],
  ['case-insensitive tie lower first', "'a','A'"],
  ['case-insensitive tie upper first', "'A','a'"],
  ['trailing space tie longer first', "'a ','a'"],
  ['trailing space tie shorter first', "'a','a '"],
  ['empty string null', "'',NULL"],
  ['int numeric string', "5,'10'"],
  ['int nonnumeric string', "5,'abc'"],
  ['string decimal', "'1.5',CAST(2 AS DECIMAL(3,1))"],
  ['string float', "'1.5',CAST(2 AS FLOAT)"],
  // Date and time types.
  ['date datetime', "CAST('2024-01-02' AS DATE),CAST('2024-01-01T12:00:00' AS DATETIME)"],
  ['date datetime2', "CAST('2024-01-02' AS DATE),CAST('2024-01-01T12:00:00.123' AS DATETIME2(3))"],
  ['datetime2 scales', "CAST('2024-01-01T00:00:00.123' AS DATETIME2(3)),CAST('2024-01-01T00:00:00.1234567' AS DATETIME2(7))"],
  ['smalldatetime datetime', "CAST('2024-01-01T10:00:00' AS SMALLDATETIME),CAST('2024-01-01T09:59:59.997' AS DATETIME)"],
  ['datetime datetime2', "CAST('2024-01-01T00:00:00.003' AS DATETIME),CAST('2024-01-01T00:00:00.0020000' AS DATETIME2(7))"],
  ['datetimeoffset mixed offsets', "CAST('2024-01-01T10:00:00+02:00' AS DATETIMEOFFSET(3)),CAST('2024-01-01T09:00:00+00:00' AS DATETIMEOFFSET(0))"],
  ['datetimeoffset datetime2', "CAST('2024-01-01T10:00:00+02:00' AS DATETIMEOFFSET(3)),CAST('2024-01-01T09:00:00' AS DATETIME2(0))"],
  ['time scales', "CAST('10:00:00.5' AS TIME(1)),CAST('10:00:00.25' AS TIME(7))"],
  ['date string', "CAST('2024-01-02' AS DATE),'2024-05-01'"],
  ['string first date', "'2024-05-01',CAST('2024-01-02' AS DATE)"],
  ['date int', "CAST('2024-01-02' AS DATE),1"],
  ['datetime int', "CAST('2024-01-02' AS DATETIME),1"],
  ['time date', "CAST('10:00' AS TIME),CAST('2024-01-02' AS DATE)"],
  ['time datetime', "CAST('10:00' AS TIME),CAST('2024-01-02' AS DATETIME)"],
  // Other scalar families.
  ['uniqueidentifier', "CAST('00000000-0000-0000-0000-000000000002' AS UNIQUEIDENTIFIER),CAST('01000000-0000-0000-0000-000000000000' AS UNIQUEIDENTIFIER)"],
  ['uniqueidentifier string', "CAST('00000000-0000-0000-0000-000000000002' AS UNIQUEIDENTIFIER),'01000000-0000-0000-0000-000000000000'"],
  ['varbinary', 'CAST(0x01 AS VARBINARY(4)),CAST(0x02 AS VARBINARY(4))'],
  ['binary', 'CAST(0x01 AS BINARY(2)),CAST(0x02 AS BINARY(2))'],
  ['varbinary int', 'CAST(0x01 AS VARBINARY(4)),2'],
  ['xml', "CAST('<a/>' AS XML),CAST('<b/>' AS XML)"],
  ['sql_variant', 'CAST(1 AS SQL_VARIANT),CAST(2 AS SQL_VARIANT)'],
  ['sql_variant int', 'CAST(1 AS SQL_VARIANT),2'],
  ['sql_variant mixed families', "CAST(1 AS SQL_VARIANT),CAST('a' AS SQL_VARIANT),CAST(2.5 AS SQL_VARIANT)"],
  ['xml second argument', "1,CAST('<a/>' AS XML)"],
  ['varbinary max', 'CAST(0x01 AS VARBINARY(MAX)),CAST(0x0102 AS VARBINARY(3))'],
  // Collations.
  ['explicit collation bin', "'a' COLLATE Latin1_General_BIN,'B'"],
  ['explicit collation cs', "'a' COLLATE Latin1_General_CS_AS,'B'"],
  ['explicit collation conflict', "'a' COLLATE Latin1_General_CS_AS,'B' COLLATE Latin1_General_BIN"],
  // Column-sourced declarations.
  ['not null columns', 'nn1,nn2', 'FROM dbo.gl_src WHERE id=1'],
  ['not null and nullable columns', 'nn1,n1', 'FROM dbo.gl_src ORDER BY id'],
  ['not null column and literal', 'nn1,0', 'FROM dbo.gl_src WHERE id=1'],
  ['nullable columns per row', 'n1,n2', 'FROM dbo.gl_src ORDER BY id'],
  ['implicit collation columns conflict', 'cs,bin', 'FROM dbo.gl_src WHERE id=1'],
  ['implicit column explicit literal', "cs,'B' COLLATE Latin1_General_BIN", 'FROM dbo.gl_src WHERE id=1'],
  ['implicit column and literal', "bin,'B'", 'FROM dbo.gl_src WHERE id=1'],
  ['text column', 'txt,txt', 'FROM dbo.gl_src WHERE id=1'],
  ['ntext column', 'ntxt,ntxt', 'FROM dbo.gl_src WHERE id=1'],
  ['image column', 'img,img', 'FROM dbo.gl_src WHERE id=1'],
  // Evaluation errors.
  ['divide by zero argument', '1,1/0'],
  ['null and divide by zero', 'NULL,1/0'],
]

// Forms that do not share the pair shape.
const statements = [
  ['greatest without arguments', 'SELECT GREATEST() AS g'],
  ['least without arguments', 'SELECT LEAST() AS l'],
  ['where predicate', 'SELECT id FROM dbo.gl_src WHERE GREATEST(n1,n2)>2 ORDER BY id'],
  ['order by', 'SELECT id FROM dbo.gl_src ORDER BY LEAST(n1,n2) DESC,id'],
  ['over aggregates', 'SELECT GREATEST(MAX(n1),MIN(n2)) AS g,LEAST(MAX(n1),MIN(n2)) AS l FROM dbo.gl_src'],
  ['least xml', "SELECT LEAST(CAST('<a/>' AS XML),CAST('<b/>' AS XML)) AS l"],
  ['least text column', 'SELECT LEAST(txt,txt) AS l FROM dbo.gl_src'],
  ['decimal cap overflow text', "SELECT CONVERT(VARCHAR(60),GREATEST(CAST(99999999999999999999999999999999999999 AS DECIMAL(38,0)),CAST(0.5 AS DECIMAL(38,10)))) AS g,CONVERT(VARCHAR(60),LEAST(CAST(99999999999999999999999999999999999999 AS DECIMAL(38,0)),CAST(0.5 AS DECIMAL(38,10)))) AS l"],
  ['decimal scale rounding text', "SELECT CONVERT(VARCHAR(60),LEAST(CAST(1 AS DECIMAL(38,0)),CAST(0.5 AS DECIMAL(38,10)),CAST(1.4 AS DECIMAL(38,10)))) AS l,CONVERT(VARCHAR(60),LEAST(CAST(2 AS DECIMAL(38,0)),CAST(-0.5 AS DECIMAL(38,10)))) AS l_negative"],
  ['datetimeoffset text', "SELECT CONVERT(VARCHAR(40),GREATEST(CAST('2024-01-01T10:00:00+02:00' AS DATETIMEOFFSET(3)),CAST('2024-01-01T09:00:00' AS DATETIME2(0)))) AS g,CONVERT(VARCHAR(40),LEAST(CAST('2024-01-01T10:00:00+02:00' AS DATETIMEOFFSET(3)),CAST('2024-01-01T09:00:00' AS DATETIME2(0)))) AS l"],
  ['datetimeoffset equal instants text', "SELECT CONVERT(VARCHAR(40),GREATEST(CAST('2024-01-01T10:00:00+02:00' AS DATETIMEOFFSET(0)),CAST('2024-01-01T08:00:00+00:00' AS DATETIMEOFFSET(0)))) AS g,CONVERT(VARCHAR(40),LEAST(CAST('2024-01-01T08:00:00+00:00' AS DATETIMEOFFSET(0)),CAST('2024-01-01T10:00:00+02:00' AS DATETIMEOFFSET(0)))) AS l"],
  ['datetime datetime2 text', "SELECT CONVERT(VARCHAR(40),GREATEST(CAST('2024-01-01T00:00:00.003' AS DATETIME),CAST('2024-01-01T00:00:00.0020000' AS DATETIME2(7)))) AS g"],
  ['varchar max long value', "SELECT GREATEST(REPLICATE(CAST('x' AS VARCHAR(MAX)),9000),'a') AS g"],
  ['varchar max long length', "SELECT DATALENGTH(GREATEST(REPLICATE(CAST('x' AS VARCHAR(MAX)),9000),'a')) AS g_bytes,DATALENGTH(LEAST(REPLICATE(CAST('x' AS VARCHAR(MAX)),9000),'y')) AS l_bytes"],
  ['nvarchar max long length', "SELECT DATALENGTH(GREATEST(REPLICATE(CAST(N'x' AS NVARCHAR(MAX)),5000),N'a')) AS g_bytes"],
  ['nested', 'SELECT GREATEST(LEAST(3,7),LEAST(5,9)) AS g,LEAST(GREATEST(3,7),GREATEST(5,9)) AS l'],
  ['select into', "SELECT GREATEST(CAST(1 AS INT),CAST(2.5 AS DECIMAL(5,2))) AS g,LEAST('a',CAST(N'b' AS NVARCHAR(3))) AS l INTO dbo.gl_into; SELECT c.name,t.name AS type_name,c.max_length,c.precision,c.scale,c.is_nullable,c.collation_name FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('dbo.gl_into') ORDER BY c.column_id"],
]

const pairSql = (list, tail) => `SELECT GREATEST(${list}) AS g,LEAST(${list}) AS l${tail ? ' ' + tail : ''}`
const describeSql = (sql, parameters = 'NULL') =>
  `SELECT column_ordinal,name,system_type_name,max_length,precision,scale,is_nullable,collation_name,error_number,error_state,error_message,error_type_desc FROM sys.dm_exec_describe_first_result_set(N'${sql.replaceAll("'", "''")}',${parameters},0) ORDER BY column_ordinal`

const bound = 'SELECT GREATEST(@a,@b) AS g,LEAST(@a,@b) AS l'
const rpcCases = [
  ['rpc int int', [['a', TYPES.Int, 3], ['b', TYPES.Int, 7]], "N'@a int,@b int'"],
  ['rpc int null', [['a', TYPES.Int, null], ['b', TYPES.Int, 7]], "N'@a int,@b int'"],
  ['rpc null null', [['a', TYPES.Int, null], ['b', TYPES.Int, null]], "N'@a int,@b int'"],
  ['rpc int decimal', [['a', TYPES.Int, 3], ['b', TYPES.Decimal, 3.25, { precision: 10, scale: 2 }]], "N'@a int,@b decimal(10,2)'"],
  ['rpc nvarchar int', [['a', TYPES.NVarChar, '10', { length: 10 }], ['b', TYPES.Int, 9]], "N'@a nvarchar(10),@b int'"],
  ['rpc varchar nvarchar', [['a', TYPES.VarChar, 'a', { length: 10 }], ['b', TYPES.NVarChar, 'B', { length: 20 }]], "N'@a varchar(10),@b nvarchar(20)'"],
  ['rpc money float', [['a', TYPES.Money, 1.5], ['b', TYPES.Float, 1.25]], "N'@a money,@b float'"],
  ['rpc date datetime2', [['a', TYPES.Date, new Date('2024-01-02T00:00:00Z')], ['b', TYPES.DateTime2, new Date('2024-01-01T12:00:00.123Z'), { scale: 3 }]], "N'@a date,@b datetime2(3)'"],
  ['rpc varbinary', [['a', TYPES.VarBinary, Buffer.from([1]), { length: 4 }], ['b', TYPES.VarBinary, Buffer.from([2]), { length: 4 }]], "N'@a varbinary(4),@b varbinary(4)'"],
  ['rpc int int replay', [['a', TYPES.Int, 3], ['b', TYPES.Int, 7]], "N'@a int,@b int'"],
]

const preparedSql = 'SELECT GREATEST(@a,@b,@c) AS g,LEAST(@a,@b,@c) AS l'
const preparedExecutions = [
  { a: 1, b: 2.5, c: '3' },
  { a: null, b: null, c: null },
  { a: 5, b: null, c: '4.75' },
  { a: 1, b: 2.5, c: 'x' },
  { a: 1, b: 2.5, c: '3' },
]

async function observed(connection, sql, parameters) {
  const tokens = []
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      taps.set(request, tokens)
      if (!parameters) return connection.execSqlBatch(request)
      for (const [name, type, value, options] of parameters) {
        request.addParameter(name, type, value, options)
      }
      connection.execSql(request)
    },
  }
  const result = await capture(transport, sql)
  return { ...canonical(result), tokens }
}

async function prepared(connection) {
  let complete = () => {}
  const request = new Request(preparedSql, (...args) => complete(...args))
  request.addParameter('a', TYPES.Int)
  request.addParameter('b', TYPES.Decimal, undefined, { precision: 6, scale: 2 })
  request.addParameter('c', TYPES.VarChar, undefined, { length: 8 })
  const preparation = { errors: [], tokens: [] }
  taps.set(request, preparation.tokens)
  await new Promise(resolve => {
    complete = error => { if (error) preparation.errors.push({ message: error.message, number: error.number ?? null }); resolve() }
    request.once('prepared', () => resolve())
    connection.prepare(request)
  })
  const executions = []
  const unpreparation = { tokens: [] }
  try {
    for (const values of preparedExecutions) {
      const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
      const tokens = []
      taps.set(request, tokens)
      const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
      const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
      const onMetadata = metadata => result.sets.push({ columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })), rows: [] })
      const onRow = row => result.sets.at(-1).rows.push(row.map(c => c.value))
      const done = Object.fromEntries(['done', 'doneInProc', 'doneProc'].map(kind => [kind, (rowCount, more, status) => {
        result.done.push({ kind, rowCount: rowCount ?? null, more })
        if (kind === 'doneProc') result.returnStatus = status
      }]))
      connection.on('errorMessage', onError); connection.on('infoMessage', onInfo)
      request.on('columnMetadata', onMetadata); request.on('row', onRow)
      for (const [kind, listener] of Object.entries(done)) request.on(kind, listener)
      try {
        await new Promise(resolve => {
          complete = (error, rowCount) => {
            result.rowCount = rowCount
            if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          }
          request.error = undefined
          connection.execute(request, values)
        })
      } finally {
        connection.off('errorMessage', onError); connection.off('infoMessage', onInfo)
        request.off('columnMetadata', onMetadata); request.off('row', onRow)
        for (const [kind, listener] of Object.entries(done)) request.off(kind, listener)
      }
      executions.push({ values, result: { ...canonical(result), tokens } })
    }
  } finally {
    taps.set(request, unpreparation.tokens)
    await new Promise((resolve, reject) => {
      complete = error => error ? reject(error) : resolve()
      request.error = undefined
      connection.unprepare(request)
    })
  }
  return { preparation, executions, unpreparation }
}

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    const result = await observed(connection, sql, parameters)
    records.push({
      name, sql,
      ...(parameters ? { parameters: parameters.map(([parameter, type, value, options]) => canonical({
        name: parameter, type: type.name, value, ...(options ? { options } : {}),
      })) } : {}),
      result,
    })
  }
  await record('create source', 'CREATE TABLE dbo.gl_src(id INT NOT NULL,nn1 INT NOT NULL,nn2 INT NOT NULL,n1 INT NULL,n2 INT NULL,cs VARCHAR(10) COLLATE Latin1_General_CS_AS,bin VARCHAR(10) COLLATE Latin1_General_BIN,txt TEXT,ntxt NTEXT,img IMAGE)')
  await record('insert source', "INSERT dbo.gl_src VALUES (1,1,2,NULL,5,'a','a','t',N't',0x01),(2,3,4,4,NULL,'b','B',NULL,NULL,NULL),(3,5,6,1,3,'c','c','u',N'u',0x02)")
  await record('server version', "SELECT CAST(SERVERPROPERTY('ProductMajorVersion') AS INT) AS major,CAST(SERVERPROPERTY('Collation') AS NVARCHAR(128)) AS server_collation,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation,@@DATEFIRST AS datefirst,SESSIONPROPERTY('ANSI_WARNINGS') AS ansi_warnings,SESSIONPROPERTY('ARITHABORT') AS arithabort")
  for (const [name, list, tail] of pairs) {
    const sql = pairSql(list, tail)
    await record(name, sql)
    await record('describe ' + name, describeSql(sql))
  }
  for (const [name, sql] of statements) await record(name, sql)
  for (const [name, parameters, declaration] of rpcCases) {
    await record(name, bound, parameters)
    if (!name.endsWith('replay')) await record('describe ' + name, describeSql(bound, declaration))
  }
  records.push({ name: 'prepared int decimal varchar', sql: preparedSql, parameters: [
    { name: 'a', type: 'Int' }, { name: 'b', type: 'Decimal', options: { precision: 6, scale: 2 } }, { name: 'c', type: 'VarChar', options: { length: 8 } },
  ], result: await prepared(connection) })
  await record('describe prepared int decimal varchar', describeSql(preparedSql, "N'@a int,@b decimal(6,2),@c varchar(8)'"))
  return records
}

const expectedRecords = 3 + pairs.length * 2 + statements.length + rpcCases.length * 2 - 1 + 2

function validate(run) {
  assert.equal(run.length, expectedRecords)
  const get = name => {
    const record = run.find(record => record.name === name)
    assert(record, 'missing ' + name)
    return record.result
  }
  assert.equal(get('server version').sets[0].rows[0][0], 17)
  for (const [name, rows] of [
    ['int with null', [[3, 1]]],
    ['untyped nulls', [[null, null]]],
    ['254 arguments', [[254, 1]]],
    ['case-insensitive tie lower first', [['a', 'a']]],
    ['case-insensitive tie upper first', [['A', 'A']]],
    ['trailing space tie longer first', [['a ', 'a ']]],
    ['explicit collation bin', [['a', 'B']]],
    ['explicit collation cs', [['B', 'a']]],
    ['nullable columns per row', [[5, 5], [4, 4], [3, 1]]],
    ['decimal cap overflow text', [['99999999999999999999999999999999999999', '1']]],
    ['decimal scale rounding text', [['1', '-1']]],
    ['datetimeoffset equal instants text', [['2024-01-01 10:00:00 +02:00', '2024-01-01 08:00:00 +00:00']]],
    ['rpc int null', [[7, 7]]],
  ]) assert.deepEqual(get(name).sets[0].rows, rows, name)
  for (const [name, type, nullable] of [
    ['untyped nulls', 'int', true],
    ['single int', 'int', false],
    ['tinyint smallint', 'smallint', true],
    ['int decimal', 'decimal(5,2)', true],
    ['bigint decimal', 'decimal(21,2)', true],
    ['decimal scale beats integer digits', 'decimal(11,5)', true],
    ['decimal precision cap', 'decimal(38,0)', true],
    ['decimal 38 38 int', 'decimal(38,37)', true],
    ['decimal literals', 'numeric(3,2)', false],
    ['real int', 'real', true],
    ['money decimal', 'decimal(19,4)', true],
    ['smallmoney int', 'smallmoney', true],
    ['varchar nvarchar', 'nvarchar(10)', true],
    ['nchar char', 'nchar(6)', true],
    ['varchar max', 'varchar(8000)', true],
    ['nvarchar max varchar', 'nvarchar(4000)', true],
    ['varbinary max', 'varbinary(8000)', true],
    ['datetime datetime2', 'datetime2(7)', true],
    ['datetimeoffset datetime2', 'datetimeoffset(3)', true],
    ['sql_variant', 'sql_variant', true],
    ['not null columns', 'int', false],
    ['not null and nullable columns', 'int', true],
    ['rpc int decimal', 'decimal(12,2)', true],
    ['rpc varchar nvarchar', 'nvarchar(20)', true],
  ]) {
    const rows = get('describe ' + name).sets[0].rows
    assert.deepEqual(rows.map(row => [row[2], row[6]]), [[type, nullable], [type, nullable]], name)
  }
  for (const [name, number] of [
    ['255 arguments', 189],
    ['greatest without arguments', 189],
    ['least without arguments', 189],
    ['xml', 8116],
    ['least xml', 8116],
    ['text column', 8116],
    ['ntext column', 8116],
    ['image column', 8116],
    ['explicit collation conflict', 468],
    ['implicit collation columns conflict', 468],
    ['date int', 206],
    ['time date', 206],
    ['int nonnumeric string', 245],
    ['divide by zero argument', 8134],
    ['null and divide by zero', 8134],
    ['varchar max long value', 8152],
  ]) assert.equal(get(name).errors[0]?.number, number, name)
  const order = result => result.tokens.map(token => token.token + (token.number ? ':' + token.number : '') + (token.status !== undefined ? ':' + token.status : ''))
  for (const [name, tokens] of [
    ['int with null', ['COLMETADATA', 'ROW', 'DONE:16']],
    ['divide by zero argument', ['COLMETADATA', 'ERROR:8134', 'DONE:2']],
    ['null and divide by zero', ['COLMETADATA', 'ERROR:8134', 'DONE:2']],
    ['int nonnumeric string', ['COLMETADATA', 'ERROR:245', 'DONE:2']],
    ['varchar max long value', ['COLMETADATA', 'ERROR:8152', 'DONE:2']],
    ['255 arguments', ['ERROR:189', 'DONE:2']],
    ['explicit collation conflict', ['ERROR:468', 'DONE:2']],
    ['xml', ['ERROR:8116', 'DONE:2']],
    ['rpc int int', ['COLMETADATA', 'ROW', 'DONEINPROC:17', 'RETURNSTATUS', 'DONEPROC:0']],
  ]) assert.deepEqual(order(get(name)), tokens, name)
  const preparedRun = run.find(record => record.name === 'prepared int decimal varchar').result
  assert.deepEqual(preparedRun.preparation.errors, [])
  assert.deepEqual(preparedRun.executions.map(execution => execution.result.errors[0]?.number ?? null), [null, null, null, 8114, null])
  assert.deepEqual(order(preparedRun.executions[3].result), ['COLMETADATA', 'ERROR:8114', 'DONEPROC:2'])
  for (const record of run) {
    const results = record.name.startsWith('prepared') ? record.result.executions.map(execution => execution.result) : [record.result]
    for (const result of results) {
      assert(result.done.length > 0, record.name + ': no completion')
      assert(/^DONE(PROC)?$/.test(result.tokens.at(-1)?.token), record.name + ': no final DONE token')
    }
  }
}

// Never hand whole captures to node:assert: on Node 24 a failed
// deepStrictEqual/strictEqual inspects its operands to build the
// AssertionError, even with a custom message, and a multi-MB capture can grow
// the process without a practical bound. Compare with isDeepStrictEqual and
// raise plain errors that name only the first differing record.
function same(left, right, message) {
  if (isDeepStrictEqual(left, right)) return
  const a = left?.containers ? left.containers.flatMap(c => c.runs.flat()) : left
  const b = right?.containers ? right.containers.flatMap(c => c.runs.flat()) : right
  const index = Array.isArray(a) && Array.isArray(b)
    ? a.findIndex((record, i) => !isDeepStrictEqual(record, b[i])) : -1
  throw new Error(message + (index >= 0 ? ` (first difference at record ${index}: ${JSON.stringify(a[index]?.name ?? null)})` : ''))
}

// Refuse before starting any container, without parsing the fixture.
const fixtureExists = existsSync(fixture)
if (writeFixture && fixtureExists) throw new Error('refusing to overwrite retained fixture ' + fixture.pathname)

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
    same(runs[0], runs[1], 'GREATEST/LEAST observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
same(containers[0].runs[0], containers[1].runs[0], 'GREATEST/LEAST observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
const retained = fixtureExists ? JSON.parse(await readFile(fixture, 'utf8')) : undefined
if (retained !== undefined) same(actual, retained, 'GREATEST/LEAST observations differ from retained fixture')
if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
console.log('Captured ' + containers[0].runs[0].length + ' GREATEST/LEAST observations in four fresh databases across two containers' + (retained ? ' and matched retained fixture' : ''))
