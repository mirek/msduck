#!/usr/bin/env node
// Retain SQL Server HASHBYTES, CHECKSUM and BINARY_CHECKSUM rows, descriptors,
// diagnostics and completions for ordinary batches, sp_executesql RPC and
// prepared handles.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { resolve } from 'node:path'
import { isDeepStrictEqual } from 'node:util'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'

const fixture = new URL('../reference/hashbytes-checksum.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/compatibility/hashbytes-checksum/capture.json')

const algorithms = ['MD2', 'MD4', 'MD5', 'SHA', 'SHA1', 'SHA2_256', 'SHA2_512']

const cases = [
  ...algorithms.map(a => [`hashbytes ${a} varchar`, `SELECT HASHBYTES('${a}','abc') AS h`]),
  ...algorithms.map(a => [`hashbytes ${a} nvarchar`, `SELECT HASHBYTES('${a}',N'abc') AS h`]),
  ...algorithms.map(a => [`hashbytes ${a} varbinary`, `SELECT HASHBYTES('${a}',0x616263) AS h`]),
  ['hashbytes lowercase algorithm', "SELECT HASHBYTES('sha2_256','abc') AS a,HASHBYTES('Md5','abc') AS b"],
  ['hashbytes unicode algorithm', "SELECT HASHBYTES(N'SHA2_256','abc') AS h"],
  ['hashbytes algorithm trailing space', "SELECT HASHBYTES('MD5 ','abc') AS h"],
  ['hashbytes algorithm leading space', "SELECT HASHBYTES(' MD5','abc') AS h"],
  ['hashbytes invalid algorithm', "SELECT HASHBYTES('SHA3_256','abc') AS h"],
  ['hashbytes sha2_384 algorithm', "SELECT HASHBYTES('SHA2_384','abc') AS h"],
  ['hashbytes empty algorithm', "SELECT HASHBYTES('','abc') AS h"],
  ['hashbytes null algorithm', "SELECT HASHBYTES(CAST(NULL AS VARCHAR(10)),'abc') AS h"],
  ['hashbytes untyped null algorithm', "SELECT HASHBYTES(NULL,'abc') AS h"],
  ['hashbytes invalid algorithm null input', "SELECT HASHBYTES('SHA3_256',CAST(NULL AS VARCHAR(10))) AS h"],
  ['hashbytes invalid algorithm per row', "SELECT HASHBYTES(a,'abc') AS h FROM (VALUES('MD5'),('BOGUS')) d(a)"],
  ['hashbytes invalid algorithm empty source', "SELECT HASHBYTES(a,'abc') AS h FROM (VALUES('BOGUS')) d(a) WHERE 1=0"],
  ['hashbytes algorithm variable', "DECLARE @a VARCHAR(20)='SHA1'; SELECT HASHBYTES(@a,'abc') AS h"],
  ['hashbytes algorithm column', "SELECT alg,HASHBYTES(alg,txt) AS h FROM dbo.hash_src WHERE alg IS NOT NULL ORDER BY id"],
  ['hashbytes integer algorithm', "SELECT HASHBYTES(5,'abc') AS h"],
  ['hashbytes null varchar input', "SELECT HASHBYTES('SHA2_256',CAST(NULL AS VARCHAR(10))) AS h"],
  ['hashbytes null nvarchar input', "SELECT HASHBYTES('SHA2_256',CAST(NULL AS NVARCHAR(10))) AS h"],
  ['hashbytes untyped null input', "SELECT HASHBYTES('SHA2_256',NULL) AS h"],
  ['hashbytes empty varchar', "SELECT HASHBYTES('SHA2_256','') AS a,HASHBYTES('SHA2_256',N'') AS b,HASHBYTES('SHA2_256',0x) AS c"],
  ['hashbytes case sensitivity', "SELECT HASHBYTES('MD5','ABC') AS upper_value,HASHBYTES('MD5','abc') AS lower_value"],
  ['hashbytes trailing spaces', "SELECT HASHBYTES('MD5','abc ') AS a,HASHBYTES('MD5',N'abc ') AS b"],
  ['hashbytes char padding', "SELECT HASHBYTES('MD5',CAST('abc' AS CHAR(5))) AS a,HASHBYTES('MD5',CAST(N'abc' AS NCHAR(5))) AS b"],
  ['hashbytes cp1252 character', "SELECT HASHBYTES('MD5','é') AS a,HASHBYTES('MD5',N'é') AS b"],
  ['hashbytes utf8 collation', "SELECT HASHBYTES('MD5',CAST(N'é' AS VARCHAR(10)) COLLATE Latin1_General_100_CI_AS_SC_UTF8) AS h"],
  ['hashbytes utf8 column', "SELECT HASHBYTES('MD5',utf8) AS h FROM dbo.hash_src WHERE id=1"],
  ['hashbytes supplementary character', "SELECT HASHBYTES('MD5',N'\u{1f600}') AS h"],
  ['hashbytes collation clause', "SELECT HASHBYTES('MD5','abc' COLLATE Latin1_General_BIN2) AS h"],
  ['hashbytes varchar max', "SELECT HASHBYTES('SHA2_256',CAST('abc' AS VARCHAR(MAX))) AS h"],
  ['hashbytes nvarchar max', "SELECT HASHBYTES('SHA2_256',CAST(N'abc' AS NVARCHAR(MAX))) AS h"],
  ['hashbytes varbinary max', "SELECT HASHBYTES('SHA2_256',CAST(0x616263 AS VARBINARY(MAX))) AS h"],
  ['hashbytes varchar 8000 bytes', "SELECT HASHBYTES('SHA2_256',REPLICATE('a',8000)) AS h"],
  ['hashbytes varchar 10000 bytes', "SELECT HASHBYTES('SHA2_256',REPLICATE(CAST('a' AS VARCHAR(MAX)),10000)) AS h"],
  ['hashbytes nvarchar 10000 bytes', "SELECT HASHBYTES('SHA2_256',REPLICATE(CAST(N'a' AS NVARCHAR(MAX)),5000)) AS h"],
  ['hashbytes varbinary 10000 bytes', "SELECT HASHBYTES('SHA2_512',CAST(REPLICATE(CAST('a' AS VARCHAR(MAX)),10000) AS VARBINARY(MAX))) AS h"],
  ['hashbytes md5 10000 bytes', "SELECT HASHBYTES('MD5',REPLICATE(CAST('a' AS VARCHAR(MAX)),10000)) AS h"],
  ['hashbytes integer input', "SELECT HASHBYTES('MD5',1) AS h"],
  ['hashbytes decimal input', "SELECT HASHBYTES('MD5',1.5) AS h"],
  ['hashbytes datetime input', "SELECT HASHBYTES('MD5',CAST('2024-01-02' AS DATETIME)) AS h"],
  ['hashbytes uniqueidentifier input', "SELECT HASHBYTES('MD5',CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER)) AS h"],
  ['hashbytes xml input', "SELECT HASHBYTES('MD5',CAST('<a/>' AS XML)) AS h"],
  ['hashbytes text input', "SELECT HASHBYTES('MD5',CAST('abc' AS TEXT)) AS h"],
  ['hashbytes one argument', "SELECT HASHBYTES('MD5') AS h"],
  ['hashbytes three arguments', "SELECT HASHBYTES('MD5','a','b') AS h"],
  ['hashbytes unnamed column', "SELECT HASHBYTES('MD5','abc')"],
  ['hashbytes datalength', "SELECT DATALENGTH(HASHBYTES('MD5','abc')) AS md5,DATALENGTH(HASHBYTES('SHA1','abc')) AS sha1,DATALENGTH(HASHBYTES('SHA2_512','abc')) AS sha512"],
  ['hashbytes select into descriptor', "SELECT HASHBYTES('MD5','abc') AS h INTO #hash_into; SELECT c.name,t.name AS type_name,c.max_length,c.is_nullable FROM tempdb.sys.columns c JOIN tempdb.sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('tempdb..#hash_into'); DROP TABLE #hash_into"],

  ['checksum int', "SELECT CHECKSUM(1) AS a,CHECKSUM(0) AS b,CHECKSUM(-1) AS c,CHECKSUM(2147483647) AS d"],
  ['checksum integer family', "SELECT CHECKSUM(CAST(1 AS TINYINT)) AS t,CHECKSUM(CAST(1 AS SMALLINT)) AS s,CHECKSUM(CAST(1 AS BIGINT)) AS b,CHECKSUM(CAST(1 AS BIT)) AS bit_value,CHECKSUM(CAST(4294967296 AS BIGINT)) AS big"],
  ['checksum decimal family', "SELECT CHECKSUM(CAST(1.5 AS DECIMAL(10,1))) AS a,CHECKSUM(CAST(1.50 AS DECIMAL(10,2))) AS b,CHECKSUM(CAST(1.5 AS NUMERIC(38,10))) AS c,CHECKSUM(1.5) AS literal_value"],
  ['checksum approximate family', "SELECT CHECKSUM(CAST(1.5 AS FLOAT)) AS f,CHECKSUM(CAST(1.5 AS REAL)) AS r,CHECKSUM(CAST(0 AS FLOAT)) AS zero,CHECKSUM(CAST(-0.0 AS FLOAT)) AS negative_zero"],
  ['checksum money family', "SELECT CHECKSUM(CAST(1.5 AS MONEY)) AS m,CHECKSUM(CAST(1.5 AS SMALLMONEY)) AS sm"],
  ['checksum date family', "SELECT CHECKSUM(CAST('2024-01-02' AS DATE)) AS d,CHECKSUM(CAST('2024-01-02T03:04:05' AS DATETIME)) AS dt,CHECKSUM(CAST('2024-01-02T03:04:05' AS SMALLDATETIME)) AS sdt,CHECKSUM(CAST('2024-01-02T03:04:05.1234567' AS DATETIME2(7))) AS dt2,CHECKSUM(CAST('03:04:05.1234567' AS TIME(7))) AS tm,CHECKSUM(CAST('2024-01-02T03:04:05+02:00' AS DATETIMEOFFSET(7))) AS dto"],
  ['checksum datetime2 precision', "SELECT CHECKSUM(CAST('2024-01-02T03:04:05' AS DATETIME2(0))) AS p0,CHECKSUM(CAST('2024-01-02T03:04:05' AS DATETIME2(7))) AS p7"],
  ['checksum datetimeoffset offsets', "SELECT CHECKSUM(CAST('2024-01-02T03:04:05+00:00' AS DATETIMEOFFSET)) AS utc,CHECKSUM(CAST('2024-01-02T05:04:05+02:00' AS DATETIMEOFFSET)) AS plus_two"],
  ['checksum uniqueidentifier', "SELECT CHECKSUM(CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER)) AS h"],
  ['checksum binary', "SELECT CHECKSUM(0x616263) AS a,CHECKSUM(CAST(0x616263 AS BINARY(5))) AS b,CHECKSUM(0x) AS c"],
  ['checksum varchar', "SELECT CHECKSUM('abc') AS a,CHECKSUM('') AS empty_value,CHECKSUM('a') AS single"],
  ['checksum nvarchar', "SELECT CHECKSUM(N'abc') AS a,CHECKSUM(N'') AS empty_value,CHECKSUM(N'a') AS single"],
  ['checksum case default collation', "SELECT CHECKSUM('abc') AS lower_value,CHECKSUM('ABC') AS upper_value,CHECKSUM(N'abc') AS n_lower,CHECKSUM(N'ABC') AS n_upper"],
  ['checksum case sensitive collation', "SELECT CHECKSUM('abc' COLLATE Latin1_General_CS_AS) AS lower_value,CHECKSUM('ABC' COLLATE Latin1_General_CS_AS) AS upper_value"],
  ['checksum nvarchar case sensitive collation', "SELECT CHECKSUM(N'abc' COLLATE Latin1_General_CS_AS) AS lower_value,CHECKSUM(N'ABC' COLLATE Latin1_General_CS_AS) AS upper_value,CHECKSUM(N'abc' COLLATE Latin1_General_BIN2) AS bin_lower,CHECKSUM(N'ABC' COLLATE Latin1_General_BIN2) AS bin_upper"],
  ['checksum sql collation case sensitive', "SELECT CHECKSUM('abc' COLLATE SQL_Latin1_General_CP1_CS_AS) AS lower_value,CHECKSUM('ABC' COLLATE SQL_Latin1_General_CP1_CS_AS) AS upper_value,CHECKSUM('abc' COLLATE SQL_Latin1_General_CP1_CI_AS) AS ci_lower"],
  ['checksum binary collation', "SELECT CHECKSUM('abc' COLLATE Latin1_General_BIN2) AS lower_value,CHECKSUM('ABC' COLLATE Latin1_General_BIN2) AS upper_value"],
  ['checksum accent insensitive collation', "SELECT CHECKSUM(N'é' COLLATE Latin1_General_CI_AI) AS accented,CHECKSUM(N'e' COLLATE Latin1_General_CI_AI) AS plain,CHECKSUM(N'é' COLLATE Latin1_General_CI_AS) AS accented_as,CHECKSUM(N'e' COLLATE Latin1_General_CI_AS) AS plain_as"],
  ['checksum collation columns', "SELECT id,CHECKSUM(ci) AS ci,CHECKSUM(cs) AS cs,CHECKSUM(bin) AS bin FROM dbo.checksum_collation ORDER BY id"],
  ['checksum trailing spaces', "SELECT CHECKSUM('abc') AS a,CHECKSUM('abc ') AS b,CHECKSUM('abc   ') AS c,CHECKSUM(N'abc  ') AS d,CHECKSUM(' abc') AS leading_space"],
  ['checksum char padding', "SELECT CHECKSUM(CAST('abc' AS CHAR(10))) AS a,CHECKSUM(CAST(N'abc' AS NCHAR(10))) AS b"],
  ['checksum varchar versus nvarchar', "SELECT CHECKSUM('abc') AS v,CHECKSUM(N'abc') AS n,CHECKSUM(CAST('abc' AS VARCHAR(MAX))) AS vmax,CHECKSUM(CAST(N'abc' AS NVARCHAR(MAX))) AS nmax"],
  ['checksum long input', "SELECT CHECKSUM(REPLICATE(CAST('a' AS VARCHAR(MAX)),10000)) AS a,CHECKSUM(REPLICATE(CAST(N'a' AS NVARCHAR(MAX)),5000)) AS b"],
  ['checksum untyped null', "SELECT CHECKSUM(NULL) AS a"],
  ['checksum untyped null second argument', "SELECT CHECKSUM(1,NULL) AS a"],
  ['checksum typed nulls', "SELECT CHECKSUM(CAST(NULL AS INT)) AS a,CHECKSUM(CAST(NULL AS VARCHAR(10))) AS b,CHECKSUM(CAST(NULL AS NVARCHAR(10))) AS c,CHECKSUM(CAST(NULL AS DATE)) AS d,CHECKSUM(CAST(NULL AS VARBINARY(10))) AS e"],
  ['checksum typed null among arguments', "SELECT CHECKSUM(CAST(NULL AS INT),1) AS a,CHECKSUM(1,CAST(NULL AS INT)) AS b,CHECKSUM(CAST(NULL AS INT),CAST(NULL AS INT)) AS c,CHECKSUM(1) AS d"],
  ['checksum null column', "SELECT id,CHECKSUM(i) AS i,CHECKSUM(v) AS v,CHECKSUM(i,v) AS both_values FROM dbo.checksum_src WHERE id=2"],
  ['checksum multiple arguments', "SELECT CHECKSUM(1,2) AS a,CHECKSUM(2,1) AS b,CHECKSUM('a','b') AS c,CHECKSUM('b','a') AS d,CHECKSUM('ab') AS e,CHECKSUM(1,'a',CAST('2024-01-02' AS DATE)) AS mixed"],
  ['checksum many arguments', "SELECT CHECKSUM(1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20) AS h"],
  ['checksum star', "SELECT id,CHECKSUM(*) AS h FROM dbo.checksum_src ORDER BY id"],
  ['checksum star equals columns', "SELECT id,CHECKSUM(*) AS star,CHECKSUM(id,i,d,v,n,dt) AS listed FROM dbo.checksum_src ORDER BY id"],
  ['checksum qualified star', "SELECT s.id,CHECKSUM(s.*) AS h FROM dbo.checksum_src AS s ORDER BY s.id"],
  ['checksum star noncomparable column', "SELECT CHECKSUM(*) AS h FROM dbo.noncomparable_src"],
  ['checksum sql_variant', "SELECT CHECKSUM(CAST(1 AS SQL_VARIANT)) AS a,CHECKSUM(CAST('abc' AS SQL_VARIANT)) AS b"],
  ['checksum text', "SELECT CHECKSUM(CAST('abc' AS TEXT)) AS h"],
  ['checksum ntext', "SELECT CHECKSUM(CAST(N'abc' AS NTEXT)) AS h"],
  ['checksum image', "SELECT CHECKSUM(CAST(0x61 AS IMAGE)) AS h"],
  ['checksum xml', "SELECT CHECKSUM(CAST('<a/>' AS XML)) AS h"],
  ['checksum text column', "SELECT CHECKSUM(t) AS h FROM dbo.noncomparable_src"],
  ['checksum xml column', "SELECT CHECKSUM(x) AS h FROM dbo.noncomparable_src"],
  ['checksum no arguments', "SELECT CHECKSUM() AS h"],
  ['checksum unnamed column', "SELECT CHECKSUM('abc')"],
  ['checksum select into descriptor', "SELECT CHECKSUM('abc') AS h,CHECKSUM(CAST(NULL AS INT)) AS n,BINARY_CHECKSUM('abc') AS b INTO #checksum_into; SELECT c.name,t.name AS type_name,c.max_length,c.is_nullable FROM tempdb.sys.columns c JOIN tempdb.sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('tempdb..#checksum_into') ORDER BY c.column_id; DROP TABLE #checksum_into"],

  ['binary_checksum int', "SELECT BINARY_CHECKSUM(1) AS a,BINARY_CHECKSUM(0) AS b,BINARY_CHECKSUM(-1) AS c,BINARY_CHECKSUM(2147483647) AS d"],
  ['binary_checksum numeric family', "SELECT BINARY_CHECKSUM(CAST(1 AS TINYINT)) AS t,BINARY_CHECKSUM(CAST(1 AS SMALLINT)) AS s,BINARY_CHECKSUM(CAST(1 AS BIGINT)) AS b,BINARY_CHECKSUM(CAST(1 AS BIT)) AS bit_value,BINARY_CHECKSUM(CAST(1.5 AS DECIMAL(10,1))) AS d1,BINARY_CHECKSUM(CAST(1.50 AS DECIMAL(10,2))) AS d2,BINARY_CHECKSUM(CAST(1.5 AS FLOAT)) AS f,BINARY_CHECKSUM(CAST(1.5 AS REAL)) AS r,BINARY_CHECKSUM(CAST(1.5 AS MONEY)) AS m"],
  ['binary_checksum date family', "SELECT BINARY_CHECKSUM(CAST('2024-01-02' AS DATE)) AS d,BINARY_CHECKSUM(CAST('2024-01-02T03:04:05' AS DATETIME)) AS dt,BINARY_CHECKSUM(CAST('2024-01-02T03:04:05.1234567' AS DATETIME2(7))) AS dt2,BINARY_CHECKSUM(CAST('03:04:05.1234567' AS TIME(7))) AS tm,BINARY_CHECKSUM(CAST('2024-01-02T03:04:05+02:00' AS DATETIMEOFFSET(7))) AS dto"],
  ['binary_checksum uniqueidentifier and binary', "SELECT BINARY_CHECKSUM(CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER)) AS g,BINARY_CHECKSUM(0x616263) AS b,BINARY_CHECKSUM(0x) AS empty_value"],
  ['binary_checksum character', "SELECT BINARY_CHECKSUM('abc') AS v,BINARY_CHECKSUM(N'abc') AS n,BINARY_CHECKSUM('') AS empty_value,BINARY_CHECKSUM(N'') AS n_empty"],
  ['binary_checksum case', "SELECT BINARY_CHECKSUM('abc') AS lower_value,BINARY_CHECKSUM('ABC') AS upper_value,BINARY_CHECKSUM('abc' COLLATE Latin1_General_BIN2) AS bin_lower,BINARY_CHECKSUM('ABC' COLLATE Latin1_General_CS_AS) AS cs_upper"],
  ['binary_checksum collation columns', "SELECT id,BINARY_CHECKSUM(ci) AS ci,BINARY_CHECKSUM(cs) AS cs,BINARY_CHECKSUM(bin) AS bin FROM dbo.checksum_collation ORDER BY id"],
  ['binary_checksum trailing spaces', "SELECT BINARY_CHECKSUM('abc') AS a,BINARY_CHECKSUM('abc ') AS b,BINARY_CHECKSUM(N'abc ') AS c,BINARY_CHECKSUM(CAST('abc' AS CHAR(10))) AS d"],
  ['binary_checksum varchar versus nvarchar', "SELECT BINARY_CHECKSUM(CAST('abc' AS VARCHAR(MAX))) AS vmax,BINARY_CHECKSUM(CAST(N'abc' AS NVARCHAR(MAX))) AS nmax"],
  ['binary_checksum long input', "SELECT BINARY_CHECKSUM(REPLICATE(CAST('a' AS VARCHAR(MAX)),10000)) AS a,BINARY_CHECKSUM(REPLICATE(CAST('a' AS VARCHAR(MAX)),255)) AS b,BINARY_CHECKSUM(REPLICATE(CAST('a' AS VARCHAR(MAX)),256)) AS c"],
  ['binary_checksum untyped null', "SELECT BINARY_CHECKSUM(NULL) AS a"],
  ['binary_checksum untyped null among arguments', "SELECT BINARY_CHECKSUM(NULL,1) AS a,BINARY_CHECKSUM(1,NULL) AS b,BINARY_CHECKSUM(1) AS c"],
  ['binary_checksum typed nulls', "SELECT BINARY_CHECKSUM(CAST(NULL AS INT)) AS a,BINARY_CHECKSUM(CAST(NULL AS NVARCHAR(10))) AS b,BINARY_CHECKSUM(CAST(NULL AS INT),1) AS c,BINARY_CHECKSUM(1,CAST(NULL AS INT)) AS d"],
  ['binary_checksum text with comparable argument', "SELECT BINARY_CHECKSUM(CAST('abc' AS TEXT),1) AS a,BINARY_CHECKSUM(1) AS b"],
  ['binary_checksum multiple arguments', "SELECT BINARY_CHECKSUM(1,2) AS a,BINARY_CHECKSUM(2,1) AS b,BINARY_CHECKSUM('a','b') AS c,BINARY_CHECKSUM('b','a') AS d,BINARY_CHECKSUM('ab') AS e"],
  ['binary_checksum star', "SELECT id,BINARY_CHECKSUM(*) AS star,BINARY_CHECKSUM(id,i,d,v,n,dt) AS listed FROM dbo.checksum_src ORDER BY id"],
  ['binary_checksum star noncomparable column', "SELECT id,BINARY_CHECKSUM(*) AS star,BINARY_CHECKSUM(id) AS id_only FROM dbo.noncomparable_src"],
  ['binary_checksum sql_variant', "SELECT BINARY_CHECKSUM(CAST(1 AS SQL_VARIANT)) AS a"],
  ['binary_checksum text', "SELECT BINARY_CHECKSUM(CAST('abc' AS TEXT)) AS h"],
  ['binary_checksum xml', "SELECT BINARY_CHECKSUM(CAST('<a/>' AS XML)) AS h"],
  ['binary_checksum no arguments', "SELECT BINARY_CHECKSUM() AS h"],
  ['binary_checksum unnamed column', "SELECT BINARY_CHECKSUM('abc')"],
]

const setup = [
  ['create hash source', 'CREATE TABLE dbo.hash_src(id INT,alg VARCHAR(20),txt NVARCHAR(20),utf8 VARCHAR(20) COLLATE Latin1_General_100_CI_AS_SC_UTF8)'],
  ['insert hash source', "INSERT dbo.hash_src VALUES (1,'MD5',N'abc',N'é'),(2,'sha1',N'abc',NULL),(3,NULL,N'abc',NULL),(4,'SHA2_256',NULL,NULL)"],
  ['create checksum source', 'CREATE TABLE dbo.checksum_src(id INT,i INT,d DECIMAL(10,2),v VARCHAR(20),n NVARCHAR(20),dt DATETIME2(3))'],
  ['insert checksum source', "INSERT dbo.checksum_src VALUES (1,1,1.50,'abc',N'abc','2024-01-02T03:04:05.123'),(2,NULL,NULL,NULL,NULL,NULL),(3,-7,-0.01,'ABC ',N'ABC ','1900-01-01')"],
  ['create collation source', 'CREATE TABLE dbo.checksum_collation(id INT,ci VARCHAR(10) COLLATE Latin1_General_CI_AS,cs VARCHAR(10) COLLATE Latin1_General_CS_AS,bin VARCHAR(10) COLLATE Latin1_General_BIN2)'],
  ['insert collation source', "INSERT dbo.checksum_collation VALUES (1,'abc','abc','abc'),(2,'ABC','ABC','ABC'),(3,'abc ','abc ','abc ')"],
  ['create noncomparable source', 'CREATE TABLE dbo.noncomparable_src(id INT,t TEXT,x XML)'],
  ['insert noncomparable source', "INSERT dbo.noncomparable_src VALUES (1,'abc','<a/>')"],
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

// One prepared handle is reused for every value set, then released.
async function prepared(connection, sql, declarations, valueSets) {
  let result
  let complete = () => {}
  const fresh = () => ({ sets: [], done: [], errors: [], info: [], returnStatus: null })
  const fields = e => ({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
  const onError = e => result?.errors.push(fields(e))
  const onInfo = e => result?.info.push(fields(e))
  const request = new Request(sql, (error, rowCount) => complete(error, rowCount))
  for (const [name, type, options] of declarations) request.addParameter(name, type, undefined, options)
  request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })), rows: [] }))
  request.on('row', row => result?.sets.at(-1).rows.push(row.map(c => c.value)))
  for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => result?.done.push({ kind, rowCount: rowCount ?? null, more }))
  request.on('doneProc', (_count, _more, status) => { if (result) result.returnStatus = status })
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  const phase = start => new Promise(done => {
    result = fresh()
    complete = (error, rowCount) => {
      result.rowCount = rowCount
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      const finished = result
      result = undefined
      done(canonical(finished))
    }
    start()
  })
  // tedious reports sp_prepare completion through 'prepared'/'error' events
  // instead of the request callback.
  const onPrepared = () => complete(undefined, undefined)
  const onPrepareError = error => complete(error, undefined)
  try {
    const prepare = await phase(() => {
      request.once('prepared', onPrepared)
      request.once('error', onPrepareError)
      connection.prepare(request)
    })
    request.off('prepared', onPrepared)
    request.off('error', onPrepareError)
    if (request.handle === undefined) return { prepare, executions: [], unprepare: null }
    const executions = []
    for (const values of valueSets) executions.push({ values, result: await phase(() => connection.execute(request, values)) })
    const unprepare = await phase(() => connection.unprepare(request))
    return { prepare, executions, unprepare }
  } finally {
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

const long = 'a'.repeat(9000)
const rpcCases = [
  ['rpc hashbytes nvarchar input', TYPES.NVarChar, 'abc', 20],
  ['rpc hashbytes varchar input', TYPES.VarChar, 'abc', 20],
  ['rpc hashbytes null nvarchar input', TYPES.NVarChar, null, 20],
  ['rpc hashbytes varbinary input', TYPES.VarBinary, Buffer.from('abc'), 20],
  ['rpc hashbytes nvarchar max input', TYPES.NVarChar, long, 9000],
  ['rpc hashbytes varchar max input', TYPES.VarChar, long, 9000],
  ['rpc hashbytes varbinary max input', TYPES.VarBinary, Buffer.from(long), 9000],
  ['rpc hashbytes int input', TYPES.Int, 1, undefined],
]

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    const result = canonical(await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql)))
    records.push({
      name, sql,
      ...(parameters ? { parameters: parameters.map(([parameter, type, value, options]) => ({
        name: parameter, type: type.name, value: canonical(value), ...(options ? { options } : {}),
      })) } : {}),
      result,
    })
  }
  for (const [name, sql] of setup) await record(name, sql)
  for (const [name, sql] of cases) await record(name, sql)
  const hashSql = 'SELECT HASHBYTES(@algorithm,@input) AS h'
  for (const [name, type, value, length] of rpcCases) await record(name, hashSql, [
    ['algorithm', TYPES.VarChar, 'SHA2_256', { length: 20 }],
    ['input', type, value, length === undefined ? undefined : { length }],
  ])
  for (const [name, type, algorithm] of [
    ['rpc hashbytes nvarchar algorithm', TYPES.NVarChar, 'md5'],
    ['rpc hashbytes invalid algorithm', TYPES.VarChar, 'SHA3_256'],
    ['rpc hashbytes null algorithm', TYPES.VarChar, null],
  ]) await record(name, hashSql, [
    ['algorithm', type, algorithm, { length: 20 }],
    ['input', TYPES.VarChar, 'abc', { length: 20 }],
  ])
  const checksumSql = 'SELECT CHECKSUM(@value) AS c,BINARY_CHECKSUM(@value) AS b'
  for (const [name, type, value, options] of [
    ['rpc checksum nvarchar', TYPES.NVarChar, 'abc', { length: 20 }],
    ['rpc checksum nvarchar upper', TYPES.NVarChar, 'ABC', { length: 20 }],
    ['rpc checksum varchar', TYPES.VarChar, 'abc', { length: 20 }],
    ['rpc checksum nvarchar trailing space', TYPES.NVarChar, 'abc ', { length: 20 }],
    ['rpc checksum int', TYPES.Int, 1, undefined],
    ['rpc checksum null int', TYPES.Int, null, undefined],
    ['rpc checksum varbinary', TYPES.VarBinary, Buffer.from('abc'), { length: 20 }],
    ['rpc checksum nvarchar max', TYPES.NVarChar, long, { length: 9000 }],
  ]) await record(name, checksumSql, [['value', type, value, options]])
  await record('rpc checksum multiple parameters', 'SELECT CHECKSUM(@a,@b) AS c,BINARY_CHECKSUM(@a,@b) AS b', [
    ['a', TYPES.Int, 1], ['b', TYPES.NVarChar, 'abc', { length: 20 }],
  ])
  const preparedSql = 'SELECT HASHBYTES(@algorithm,@input) AS h,CHECKSUM(@input) AS c,BINARY_CHECKSUM(@input) AS b'
  const handle = await prepared(connection, preparedSql, [
    ['algorithm', TYPES.VarChar, { length: 20 }],
    ['input', TYPES.NVarChar, { length: 20 }],
  ], [
    { algorithm: 'SHA2_256', input: 'abc' },
    { algorithm: 'MD5', input: 'ABC' },
    { algorithm: 'SHA2_256', input: null },
    { algorithm: 'SHA3_256', input: 'abc' },
    { algorithm: 'SHA2_256', input: 'abc' },
  ])
  records.push({ name: 'prepared hashbytes and checksums', sql: preparedSql, prepared: handle })
  return records
}

const hex = {
  MD5: '900150983cd24fb0d6963f7d28e17f72',
  SHA1: 'a9993e364706816aba3e25717850c26c9cd0d89d',
  SHA2_256: 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
}

function validate(run) {
  assert.equal(run.length, setup.length + cases.length + rpcCases.length + 3 + 8 + 1 + 1)
  const get = name => run.find(record => record.name === name)?.result
  for (const [algorithm, value] of Object.entries(hex)) {
    const result = get(`hashbytes ${algorithm} varchar`)
    assert.deepEqual(result.sets[0].rows, [[{ kind: 'binary', value }]], algorithm)
    assert.equal(result.sets[0].columns[0].type, 'VarBinary', algorithm)
    assert.equal(result.sets[0].columns[0].length, 8000, algorithm)
  }
  assert.deepEqual(get('hashbytes SHA2_256 varbinary').sets[0].rows, [[{ kind: 'binary', value: hex.SHA2_256 }]])
  assert.deepEqual(get('rpc hashbytes varchar input').sets[0].rows, [[{ kind: 'binary', value: hex.SHA2_256 }]])
  assert.deepEqual(get('hashbytes null varchar input').sets[0].rows, [[null]])
  assert.equal(get('checksum int').sets[0].columns[0].type, 'IntN')
  assert.deepEqual(get('hashbytes MD2 varchar').sets[0].rows, [[null]])
  assert.deepEqual(get('hashbytes invalid algorithm').sets[0].rows, [[null]])
  assert.deepEqual(get('checksum typed nulls').sets[0].rows, [[2147483647, 2147483647, 2147483647, 2147483647, 2147483647]])
  for (const [name, number] of [
    ['hashbytes untyped null algorithm', 8116],
    ['hashbytes integer input', 8116],
    ['hashbytes one argument', 174],
    ['checksum untyped null', 8116],
    ['checksum text', 8116],
    ['checksum qualified star', 102],
    ['checksum no arguments', 1076],
    ['binary_checksum untyped null', 8184],
  ]) assert.equal(get(name).errors[0]?.number, number, name)
  for (const record of run) {
    const results = record.prepared ? [record.prepared.prepare, ...record.prepared.executions.map(x => x.result), record.prepared.unprepare].filter(Boolean) : [record.result]
    for (const result of results) assert(result.done.length > 0, `${record.name}: no completion`)
  }
}

function firstDifference(a, b) {
  if (a.length !== b.length) return `record count ${a.length} versus ${b.length}`
  const index = a.findIndex((record, i) => !isDeepStrictEqual(record, b[i]))
  return `record ${index} (${a[index]?.name})`
}
function requireSame(a, b, label) {
  if (!isDeepStrictEqual(a, b)) throw new Error(`${label}: first difference at ${firstDifference(a, b)}`)
}

// Refuse silent overwrite before starting any container.
if (writeFixture && existsSync(fixture)) throw new Error('refusing to overwrite retained fixture reference/hashbytes-checksum.json')
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
    if (!oneDatabase) requireSame(runs[0], runs[1], 'HASHBYTES/CHECKSUM observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) {
  requireSame(containers[0].runs[0], containers[1].runs[0], 'HASHBYTES/CHECKSUM observations differ across containers')
  requireSame(containers[0].runs[1], containers[1].runs[1], 'HASHBYTES/CHECKSUM observations differ across containers')
}
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
if (!writeFixture && !oneDatabase && existsSync(fixture)) {
  retained = JSON.parse(await readFile(fixture, 'utf8'))
  if (!Array.isArray(retained?.containers) || retained.containers.length !== containers.length) throw new Error('retained fixture has a different container count')
  for (let i = 0; i < containers.length; i++) {
    if (retained.containers[i].image !== containers[i].image) throw new Error(`retained fixture container ${i} image differs`)
    if (retained.containers[i].runs?.length !== containers[i].runs.length) throw new Error(`retained fixture container ${i} run count differs`)
    for (let j = 0; j < containers[i].runs.length; j++) requireSame(containers[i].runs[j], retained.containers[i].runs[j], `HASHBYTES/CHECKSUM observations differ from retained fixture (container ${i}, database ${j})`)
  }
}
if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
console.log(`Captured ${containers[0].runs[0].length} HASHBYTES/CHECKSUM/BINARY_CHECKSUM observations` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained ? ' and matched retained fixture' : '') + (writeFixture ? '; wrote new retained fixture' : ''))
