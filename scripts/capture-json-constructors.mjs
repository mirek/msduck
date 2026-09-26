#!/usr/bin/env node
// Retain SQL Server JSON_OBJECT, JSON_ARRAY and JSON_MODIFY rows, descriptors,
// diagnostics and completions.
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { isDeepStrictEqual } from 'node:util'
import { TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'

const fixture = new URL('../reference/json-constructors.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/compatibility/json-constructors/capture.json')

// One scalar per family; each is fed to JSON_OBJECT and JSON_ARRAY.
const scalars = [
  ['tinyint', 'CAST(255 AS TINYINT)'],
  ['smallint', 'CAST(-32768 AS SMALLINT)'],
  ['int', 'CAST(-2147483648 AS INT)'],
  ['bigint', 'CAST(9223372036854775807 AS BIGINT)'],
  ['bit true', 'CAST(1 AS BIT)'],
  ['bit false', 'CAST(0 AS BIT)'],
  ['decimal', 'CAST(-12.340 AS DECIMAL(10,3))'],
  ['decimal zero scale', 'CAST(12345678901234567890 AS DECIMAL(38,0))'],
  ['decimal small', 'CAST(0.00001 AS DECIMAL(10,5))'],
  ['numeric', 'CAST(1.5 AS NUMERIC(5,2))'],
  ['money', 'CAST(12.3456 AS MONEY)'],
  ['smallmoney', 'CAST(-1.5 AS SMALLMONEY)'],
  ['float', 'CAST(0.1 AS FLOAT)'],
  ['float large', 'CAST(1e300 AS FLOAT)'],
  ['float integral', 'CAST(3 AS FLOAT)'],
  ['float negative zero', 'CAST(-0e0 AS FLOAT)'],
  ['real', 'CAST(0.1 AS REAL)'],
  ['date', "CAST('2024-01-02' AS DATE)"],
  ['time', "CAST('03:04:05.1234567' AS TIME(7))"],
  ['datetime', "CAST('2024-01-02T03:04:05.123' AS DATETIME)"],
  ['smalldatetime', "CAST('2024-01-02T03:04:00' AS SMALLDATETIME)"],
  ['datetime2', "CAST('2024-01-02T03:04:05.1234567' AS DATETIME2(7))"],
  ['datetime2 zero scale', "CAST('2024-01-02T03:04:05' AS DATETIME2(0))"],
  ['datetimeoffset', "CAST('2024-01-02T03:04:05.1+05:30' AS DATETIMEOFFSET(1))"],
  ['uniqueidentifier', "CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER)"],
  ['char padded', "CAST('ab' AS CHAR(5))"],
  ['varchar', "CAST('ab' AS VARCHAR(5))"],
  ['nchar padded', "CAST(N'ab' AS NCHAR(5))"],
  ['nvarchar', "CAST(N'ab' AS NVARCHAR(5))"],
  ['varchar max', "CAST('ab' AS VARCHAR(MAX))"],
  ['nvarchar max', "CAST(N'ab' AS NVARCHAR(MAX))"],
  ['binary', 'CAST(0x0102 AS BINARY(2))'],
  ['varbinary', 'CAST(0x414243 AS VARBINARY(3))'],
  ['varbinary max', 'CAST(0x414243 AS VARBINARY(MAX))'],
  ['xml', "CAST('<a>1</a>' AS XML)"],
  ['sql_variant', 'CAST(CAST(1 AS INT) AS SQL_VARIANT)'],
  ['hierarchyid', "CAST('/1/' AS HIERARCHYID)"],
  ['geometry', "geometry::Point(1,2,0)"],
  ['rowversion-like binary(8)', 'CAST(0x00000000000007D1 AS ROWVERSION)'],
  ['typed null int', 'CAST(NULL AS INT)'],
  ['untyped null', 'NULL'],
]

// Characters needing JSON escaping, built without relying on client literals.
const escapeSource = "N'q\"b\\s/t'+NCHAR(9)+N'n'+NCHAR(10)+N'r'+NCHAR(13)+N'f'+NCHAR(12)+N'b'+NCHAR(8)+N'c'+NCHAR(1)+N'z'+NCHAR(31)+N'd'+NCHAR(127)+N'e'+NCHAR(233)+N'u'+NCHAR(8232)+N's'+NCHAR(0xD83D)+NCHAR(0xDE00)"

const cases = [
  // JSON_OBJECT shape, NULL handling and keys.
  ['object empty', 'SELECT JSON_OBJECT() AS value'],
  ['object basic', "SELECT JSON_OBJECT('a':1,'b':'x') AS value"],
  ['object unicode key literal', "SELECT JSON_OBJECT(N'a':N'x') AS value"],
  ['object comma pair syntax', "SELECT JSON_OBJECT('a',1) AS value"],
  ['object null default', "SELECT JSON_OBJECT('a':NULL,'b':1) AS value"],
  ['object null on null', "SELECT JSON_OBJECT('a':NULL,'b':1 NULL ON NULL) AS value"],
  ['object absent on null', "SELECT JSON_OBJECT('a':NULL,'b':1 ABSENT ON NULL) AS value"],
  ['object absent all null', "SELECT JSON_OBJECT('a':NULL ABSENT ON NULL) AS value"],
  ['object absent typed null', "SELECT JSON_OBJECT('a':CAST(NULL AS NVARCHAR(5)) ABSENT ON NULL) AS value"],
  ['object null key', "SELECT JSON_OBJECT(NULL:1) AS value"],
  ['object typed null key', "SELECT JSON_OBJECT(CAST(NULL AS NVARCHAR(5)):1) AS value"],
  ['object integer key', "SELECT JSON_OBJECT(1:2) AS value"],
  ['object decimal key', "SELECT JSON_OBJECT(1.5:2) AS value"],
  ['object date key', "SELECT JSON_OBJECT(CAST('2024-01-02' AS DATE):2) AS value"],
  ['object binary key', "SELECT JSON_OBJECT(0x41:2) AS value"],
  ['object empty key', "SELECT JSON_OBJECT('':1) AS value"],
  ['object padded char key', "SELECT JSON_OBJECT(CAST('k' AS CHAR(3)):1) AS value"],
  ['object key escaping', `SELECT JSON_OBJECT(${escapeSource}:1) AS value`],
  ['object value escaping', `SELECT JSON_OBJECT('v':${escapeSource}) AS value`],
  ['object ansi value escaping', "SELECT JSON_OBJECT('v':CAST('a\"b\\c'+CHAR(9)+CHAR(233) AS VARCHAR(20))) AS value"],
  ['object duplicate keys', "SELECT JSON_OBJECT('a':1,'a':2) AS value"],
  ['object duplicate keys absent', "SELECT JSON_OBJECT('a':NULL,'a':2 ABSENT ON NULL) AS value"],
  ['object nested', "SELECT JSON_OBJECT('o':JSON_OBJECT('x':1),'a':JSON_ARRAY(1,2)) AS value"],
  ['object json_query input', "SELECT JSON_OBJECT('q':JSON_QUERY(N'{\"x\":1}')) AS value"],
  ['object json_query array input', "SELECT JSON_OBJECT('q':JSON_QUERY(N'{\"x\":[1,2]}','$.x')) AS value"],
  ['object json_query null', "SELECT JSON_OBJECT('q':JSON_QUERY(N'{\"x\":1}','$.y')) AS value"],
  ['object json_query null absent', "SELECT JSON_OBJECT('q':JSON_QUERY(N'{\"x\":1}','$.y') ABSENT ON NULL) AS value"],
  ['object json text as string', "SELECT JSON_OBJECT('q':N'{\"x\":1}') AS value"],
  ['object json_value input', "SELECT JSON_OBJECT('q':JSON_VALUE(N'{\"x\":1}','$.x')) AS value"],
  ['object json_modify input', "SELECT JSON_OBJECT('q':JSON_MODIFY(N'{\"x\":1}','$.x',2)) AS value"],
  ['object json_query invalid', "SELECT JSON_OBJECT('q':JSON_QUERY(N'not json')) AS value"],
  ['object many pairs', "SELECT JSON_OBJECT('a':1,'b':2,'c':3,'d':4,'e':5,'f':6,'g':7,'h':8,'i':9,'j':10) AS value"],
  ['object long value', "SELECT DATALENGTH(JSON_OBJECT('a':REPLICATE(CAST(N'x' AS NVARCHAR(MAX)),10000))) AS bytes,LEN(JSON_OBJECT('a':REPLICATE(CAST(N'x' AS NVARCHAR(MAX)),10000))) AS chars"],
  ['object long bounded value', "SELECT DATALENGTH(JSON_OBJECT('a':REPLICATE(N'x',4000),'b':REPLICATE(N'y',4000))) AS bytes"],
  ['object isjson', "SELECT ISJSON(JSON_OBJECT('a':1)) AS value"],
  ['object missing value', "SELECT JSON_OBJECT('a':) AS value"],
  ['object trailing comma', "SELECT JSON_OBJECT('a':1,) AS value"],
  ['object absent and null both', "SELECT JSON_OBJECT('a':1 NULL ON NULL ABSENT ON NULL) AS value"],
  ['object returning json', "SELECT JSON_OBJECT('a':1 RETURNING JSON) AS value"],
  ['returning json describe', "EXEC sp_describe_first_result_set N'SELECT JSON_OBJECT(''a'':1 RETURNING JSON) AS o, JSON_ARRAY(1 RETURNING JSON) AS a'"],
  ['object json type value', "SELECT JSON_OBJECT('j':CAST(N'{\"x\":1}' AS JSON)) AS value"],
  ['array json type value', "SELECT JSON_ARRAY(CAST(N'[1]' AS JSON)) AS value"],
  ['modify json type input', "SELECT JSON_MODIFY(CAST(N'{\"a\":1}' AS JSON),'$.a',2) AS value"],
  ['object describe',"EXEC sp_describe_first_result_set N'SELECT JSON_OBJECT(''a'':1) AS v, JSON_OBJECT() AS e, JSON_OBJECT(''a'':CAST(''x'' AS VARCHAR(5))) AS s'"],
  ['object column source', "SELECT id,JSON_OBJECT('id':id,'txt':txt,'ansi':ansi) AS value FROM dbo.json_ctor_src ORDER BY id"],
  ['object column source absent', "SELECT id,JSON_OBJECT('txt':txt ABSENT ON NULL) AS value FROM dbo.json_ctor_src ORDER BY id"],
  ['object column keys', "SELECT id,JSON_OBJECT(txt:id) AS value FROM dbo.json_ctor_src WHERE txt IS NOT NULL ORDER BY id"],
  ['object column null key', "SELECT id,JSON_OBJECT(txt:id) AS value FROM dbo.json_ctor_src ORDER BY id"],
  ['object select into', "SELECT JSON_OBJECT('a':1) AS o,JSON_ARRAY(1) AS a,JSON_MODIFY(CAST('{}' AS VARCHAR(20)),'$.a',1) AS m INTO dbo.json_ctor_into; SELECT c.name,t.name AS type_name,c.max_length,c.is_nullable,c.collation_name FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('dbo.json_ctor_into') ORDER BY c.column_id"],

  // JSON_ARRAY shape and NULL handling.
  ['array empty', 'SELECT JSON_ARRAY() AS value'],
  ['array basic', "SELECT JSON_ARRAY(1,'x',N'y') AS value"],
  ['array null default', "SELECT JSON_ARRAY(1,NULL,2) AS value"],
  ['array null on null', "SELECT JSON_ARRAY(1,NULL,2 NULL ON NULL) AS value"],
  ['array absent on null', "SELECT JSON_ARRAY(1,NULL,2 ABSENT ON NULL) AS value"],
  ['array all null default', "SELECT JSON_ARRAY(NULL,CAST(NULL AS INT)) AS value"],
  ['array value escaping', `SELECT JSON_ARRAY(${escapeSource}) AS value`],
  ['array nested', "SELECT JSON_ARRAY(JSON_ARRAY(1,2),JSON_OBJECT('a':1),JSON_ARRAY()) AS value"],
  ['array json_query input', "SELECT JSON_ARRAY(JSON_QUERY(N'[1,{\"a\":2}]')) AS value"],
  ['array json text as string', "SELECT JSON_ARRAY(N'[1,2]') AS value"],
  ['array isjson', "SELECT ISJSON(JSON_ARRAY(1,NULL)) AS value"],
  ['array returning json', "SELECT JSON_ARRAY(1 RETURNING JSON) AS value"],
  ['array describe', "EXEC sp_describe_first_result_set N'SELECT JSON_ARRAY(1) AS v, JSON_ARRAY() AS e'"],
  ['array column source', "SELECT id,JSON_ARRAY(id,txt,ansi) AS value FROM dbo.json_ctor_src ORDER BY id"],
  ['array column source null on null', "SELECT id,JSON_ARRAY(id,txt NULL ON NULL) AS value FROM dbo.json_ctor_src ORDER BY id"],
  ['array long value', "SELECT DATALENGTH(JSON_ARRAY(REPLICATE(CAST(N'x' AS NVARCHAR(MAX)),10000))) AS bytes"],
  ['array trailing comma', "SELECT JSON_ARRAY(1,) AS value"],

  // JSON_MODIFY.
  ['modify replace number', "SELECT JSON_MODIFY(N'{\"a\":1}','$.a',2) AS value"],
  ['modify replace string', "SELECT JSON_MODIFY(N'{\"a\":1}','$.a',N'x') AS value"],
  ['modify insert lax', "SELECT JSON_MODIFY(N'{\"a\":1}','$.b',2) AS value"],
  ['modify insert explicit lax', "SELECT JSON_MODIFY(N'{\"a\":1}','lax $.b',2) AS value"],
  ['modify insert strict missing', "SELECT JSON_MODIFY(N'{\"a\":1}','strict $.b',2) AS value"],
  ['modify replace strict', "SELECT JSON_MODIFY(N'{\"a\":1}','strict $.a',2) AS value"],
  ['modify delete lax null', "SELECT JSON_MODIFY(N'{\"a\":1,\"b\":2}','$.a',NULL) AS value"],
  ['modify delete missing lax', "SELECT JSON_MODIFY(N'{\"a\":1}','$.z',NULL) AS value"],
  ['modify strict null sets null', "SELECT JSON_MODIFY(N'{\"a\":1}','strict $.a',NULL) AS value"],
  ['modify strict null missing', "SELECT JSON_MODIFY(N'{\"a\":1}','strict $.z',NULL) AS value"],
  ['modify append', "SELECT JSON_MODIFY(N'{\"arr\":[1,2]}','append $.arr',3) AS value"],
  ['modify append string', "SELECT JSON_MODIFY(N'{\"arr\":[]}','append $.arr',N'x') AS value"],
  ['modify append missing lax', "SELECT JSON_MODIFY(N'{\"a\":1}','append $.arr',3) AS value"],
  ['modify append missing strict', "SELECT JSON_MODIFY(N'{\"a\":1}','append strict $.arr',3) AS value"],
  ['modify append lax keyword order', "SELECT JSON_MODIFY(N'{\"a\":1}','append lax $.arr',3) AS value"],
  ['modify append non-array lax', "SELECT JSON_MODIFY(N'{\"a\":1}','append $.a',3) AS value"],
  ['modify append non-array strict', "SELECT JSON_MODIFY(N'{\"a\":1}','append strict $.a',3) AS value"],
  ['modify append null', "SELECT JSON_MODIFY(N'{\"arr\":[1]}','append $.arr',NULL) AS value"],
  ['modify append json_query', "SELECT JSON_MODIFY(N'{\"arr\":[1]}','append $.arr',JSON_QUERY(N'{\"x\":1}')) AS value"],
  ['modify array index', "SELECT JSON_MODIFY(N'{\"arr\":[1,2,3]}','$.arr[1]',9) AS value"],
  ['modify array index out of range lax', "SELECT JSON_MODIFY(N'{\"arr\":[1]}','$.arr[5]',9) AS value"],
  ['modify array index out of range strict', "SELECT JSON_MODIFY(N'{\"arr\":[1]}','strict $.arr[5]',9) AS value"],
  ['modify array index null', "SELECT JSON_MODIFY(N'{\"arr\":[1,2,3]}','$.arr[1]',NULL) AS value"],
  ['modify root array element', "SELECT JSON_MODIFY(N'[1,2]','$[0]',N'x') AS value"],
  ['modify root', "SELECT JSON_MODIFY(N'{\"a\":1}','$',N'x') AS value"],
  ['modify nested existing', "SELECT JSON_MODIFY(N'{\"o\":{\"x\":1}}','$.o.x',2) AS value"],
  ['modify nested missing parent lax', "SELECT JSON_MODIFY(N'{\"a\":1}','$.o.x',2) AS value"],
  ['modify nested missing parent strict', "SELECT JSON_MODIFY(N'{\"a\":1}','strict $.o.x',2) AS value"],
  ['modify quoted key', "SELECT JSON_MODIFY(N'{\"a b\":1}','$.\"a b\"',2) AS value"],
  ['modify insert quoted key', "SELECT JSON_MODIFY(N'{}','$.\"a\\\"b\"',2) AS value"],
  ['modify duplicate keys', "SELECT JSON_MODIFY(N'{\"a\":1,\"a\":2}','$.a',3) AS value"],
  ['modify duplicate keys delete', "SELECT JSON_MODIFY(N'{\"a\":1,\"a\":2}','$.a',NULL) AS value"],
  ['modify json_query value', "SELECT JSON_MODIFY(N'{\"a\":1}','$.a',JSON_QUERY(N'{\"x\":[1]}')) AS value"],
  ['modify json text as string', "SELECT JSON_MODIFY(N'{\"a\":1}','$.a',N'{\"x\":1}') AS value"],
  ['modify json_object value', "SELECT JSON_MODIFY(N'{\"a\":1}','$.a',JSON_OBJECT('x':1)) AS value"],
  ['modify json_array value', "SELECT JSON_MODIFY(N'{\"a\":1}','$.a',JSON_ARRAY(1,2)) AS value"],
  ['modify value escaping', `SELECT JSON_MODIFY(N'{}','$.v',${escapeSource}) AS value`],
  ['modify preserves whitespace', "SELECT JSON_MODIFY(N'{ \"a\" : 1 ,  \"b\" : [ 1 ] }','$.a',2) AS value"],
  ['modify int', "SELECT JSON_MODIFY(N'{}','$.v',CAST(7 AS INT)) AS value"],
  ['modify bigint', "SELECT JSON_MODIFY(N'{}','$.v',CAST(9223372036854775807 AS BIGINT)) AS value"],
  ['modify bit', "SELECT JSON_MODIFY(N'{}','$.v',CAST(1 AS BIT)) AS value"],
  ['modify decimal', "SELECT JSON_MODIFY(N'{}','$.v',CAST(-12.340 AS DECIMAL(10,3))) AS value"],
  ['modify float', "SELECT JSON_MODIFY(N'{}','$.v',CAST(0.1 AS FLOAT)) AS value"],
  ['modify money', "SELECT JSON_MODIFY(N'{}','$.v',CAST(12.3456 AS MONEY)) AS value"],
  ['modify date', "SELECT JSON_MODIFY(N'{}','$.v',CAST('2024-01-02' AS DATE)) AS value"],
  ['modify datetime2', "SELECT JSON_MODIFY(N'{}','$.v',CAST('2024-01-02T03:04:05.1234567' AS DATETIME2(7))) AS value"],
  ['modify uniqueidentifier', "SELECT JSON_MODIFY(N'{}','$.v',CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER)) AS value"],
  ['modify varbinary', "SELECT JSON_MODIFY(N'{}','$.v',CAST(0x414243 AS VARBINARY(3))) AS value"],
  ['modify xml', "SELECT JSON_MODIFY(N'{}','$.v',CAST('<a/>' AS XML)) AS value"],
  ['modify null input', "SELECT JSON_MODIFY(CAST(NULL AS NVARCHAR(20)),'$.a',1) AS value"],
  ['modify untyped null input', "SELECT JSON_MODIFY(NULL,'$.a',1) AS value"],
  ['modify null path', "SELECT JSON_MODIFY(N'{\"a\":1}',NULL,1) AS value"],
  ['modify typed null path', "SELECT JSON_MODIFY(N'{\"a\":1}',CAST(NULL AS NVARCHAR(10)),1) AS value"],
  ['modify invalid json', "SELECT JSON_MODIFY(N'not json','$.a',1) AS value"],
  ['modify empty json', "SELECT JSON_MODIFY(N'','$.a',1) AS value"],
  ['modify scalar json', "SELECT JSON_MODIFY(N'1','$.a',1) AS value"],
  ['modify truncated json', "SELECT JSON_MODIFY(N'{\"a\":1','$.a',2) AS value"],
  ['modify invalid path no dollar', "SELECT JSON_MODIFY(N'{\"a\":1}','a',2) AS value"],
  ['modify invalid path empty', "SELECT JSON_MODIFY(N'{\"a\":1}','',2) AS value"],
  ['modify invalid path trailing dot', "SELECT JSON_MODIFY(N'{\"a\":1}','$.',2) AS value"],
  ['modify invalid path bad mode', "SELECT JSON_MODIFY(N'{\"a\":1}','loose $.a',2) AS value"],
  ['modify invalid path wildcard', "SELECT JSON_MODIFY(N'{\"a\":1}','$.*',2) AS value"],
  ['modify invalid path bad index', "SELECT JSON_MODIFY(N'{\"a\":[1]}','$.a[x]',2) AS value"],
  ['modify invalid path negative index', "SELECT JSON_MODIFY(N'{\"a\":[1]}','$.a[-1]',2) AS value"],
  ['modify path through scalar lax', "SELECT JSON_MODIFY(N'{\"a\":1}','$.a.b',2) AS value"],
  ['modify path through scalar strict', "SELECT JSON_MODIFY(N'{\"a\":1}','strict $.a.b',2) AS value"],
  ['modify ansi input', "SELECT JSON_MODIFY(CAST('{\"a\":1}' AS VARCHAR(20)),'$.a',N'x'+NCHAR(233)) AS value"],
  ['modify ansi growth', "SELECT JSON_MODIFY(CAST('{\"a\":1}' AS VARCHAR(10)),'$.a',REPLICATE('x',50)) AS value"],
  ['modify nvarchar max input', "SELECT JSON_MODIFY(CAST(N'{\"a\":1}' AS NVARCHAR(MAX)),'$.a',2) AS value"],
  ['modify integer input', "SELECT JSON_MODIFY(1,'$.a',2) AS value"],
  ['modify integer path', "SELECT JSON_MODIFY(N'{\"a\":1}',1,2) AS value"],
  ['modify variable path', "DECLARE @p NVARCHAR(20)=N'$.b'; SELECT JSON_MODIFY(N'{\"a\":1}',@p,2) AS value"],
  ['modify expression path', "SELECT JSON_MODIFY(N'{\"a\":1}',N'$.'+N'b',2) AS value"],
  ['modify rename key', "SELECT JSON_MODIFY(JSON_MODIFY(N'{\"a\":1}','$.b',JSON_VALUE(N'{\"a\":1}','$.a')),'$.a',NULL) AS value"],
  ['modify too few arguments', "SELECT JSON_MODIFY(N'{}','$.a') AS value"],
  ['modify describe', "EXEC sp_describe_first_result_set N'SELECT JSON_MODIFY(N''{}'',''$.a'',1) AS n, JSON_MODIFY(CAST(''{}'' AS VARCHAR(20)),''$.a'',1) AS v, JSON_MODIFY(CAST(N''{}'' AS NVARCHAR(MAX)),''$.a'',1) AS m'"],
  ['modify column source', "SELECT id,JSON_MODIFY(doc,'$.v',txt) AS value FROM dbo.json_ctor_src ORDER BY id"],
  ['modify update statement', "UPDATE dbo.json_ctor_src SET doc=JSON_MODIFY(doc,'append $.list',id) WHERE doc IS NOT NULL; SELECT id,doc FROM dbo.json_ctor_src ORDER BY id"],
]

for (const [family, expression] of scalars) {
  cases.push([`object scalar ${family}`, `SELECT JSON_OBJECT('v':${expression}) AS value`])
  cases.push([`array scalar ${family}`, `SELECT JSON_ARRAY(${expression}) AS value`])
}

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

const boundCases = [
  ['bound object values', "SELECT JSON_OBJECT('s':@s,'n':@n,'b':@b,'d':@d) AS value", [
    ['s', TYPES.NVarChar, 'a"b', { length: 20 }], ['n', TYPES.Int, 7], ['b', TYPES.Bit, true],
    ['d', TYPES.Decimal, 1.5, { precision: 10, scale: 3 }],
  ]],
  ['bound object nulls', "SELECT JSON_OBJECT('s':@s,'n':@n) AS value,JSON_OBJECT('s':@s,'n':@n ABSENT ON NULL) AS absent", [
    ['s', TYPES.NVarChar, null, { length: 20 }], ['n', TYPES.Int, null],
  ]],
  ['bound object key', 'SELECT JSON_OBJECT(@k:1) AS value', [['k', TYPES.NVarChar, 'key', { length: 20 }]]],
  ['bound object ansi key', 'SELECT JSON_OBJECT(@k:1) AS value', [['k', TYPES.VarChar, 'key', { length: 20 }]]],
  ['bound object null key', 'SELECT JSON_OBJECT(@k:1) AS value', [['k', TYPES.NVarChar, null, { length: 20 }]]],
  ['bound object max value', "SELECT JSON_OBJECT('v':@v) AS value", [['v', TYPES.NVarChar, 'max', { length: Infinity }]]],
  ['bound object json text', "SELECT JSON_OBJECT('v':@v) AS value,JSON_OBJECT('v':JSON_QUERY(@v)) AS nested", [
    ['v', TYPES.NVarChar, '{"x":1}', { length: 20 }],
  ]],
  ['bound array values', 'SELECT JSON_ARRAY(@s,@n,@f,@dt) AS value', [
    ['s', TYPES.VarChar, 'x', { length: 20 }], ['n', TYPES.BigInt, '9007199254740993'],
    ['f', TYPES.Float, 0.5], ['dt', TYPES.DateTime2, new Date('2024-01-02T03:04:05.123Z'), { scale: 3 }],
  ]],
  ['bound array nulls', 'SELECT JSON_ARRAY(@s,@n) AS value,JSON_ARRAY(@s,@n NULL ON NULL) AS nulls', [
    ['s', TYPES.NVarChar, null, { length: 20 }], ['n', TYPES.Int, null],
  ]],
  ['bound modify', 'SELECT JSON_MODIFY(@doc,@path,@value) AS value', [
    ['doc', TYPES.NVarChar, '{"a":1}', { length: 100 }], ['path', TYPES.NVarChar, '$.b', { length: 20 }],
    ['value', TYPES.NVarChar, 'x', { length: 20 }],
  ]],
  ['bound modify int value', 'SELECT JSON_MODIFY(@doc,@path,@value) AS value', [
    ['doc', TYPES.NVarChar, '{"a":1}', { length: 100 }], ['path', TYPES.NVarChar, '$.a', { length: 20 }],
    ['value', TYPES.Int, 5],
  ]],
  ['bound modify delete', 'SELECT JSON_MODIFY(@doc,@path,@value) AS value', [
    ['doc', TYPES.NVarChar, '{"a":1,"b":2}', { length: 100 }], ['path', TYPES.NVarChar, '$.a', { length: 20 }],
    ['value', TYPES.NVarChar, null, { length: 20 }],
  ]],
  ['bound modify strict missing', 'SELECT JSON_MODIFY(@doc,@path,@value) AS value', [
    ['doc', TYPES.NVarChar, '{"a":1}', { length: 100 }], ['path', TYPES.NVarChar, 'strict $.b', { length: 20 }],
    ['value', TYPES.Int, 5],
  ]],
  ['bound modify append', 'SELECT JSON_MODIFY(@doc,@path,@value) AS value', [
    ['doc', TYPES.NVarChar, '{"arr":[1]}', { length: 100 }], ['path', TYPES.NVarChar, 'append $.arr', { length: 20 }],
    ['value', TYPES.Int, 2],
  ]],
  ['bound modify invalid json', 'SELECT JSON_MODIFY(@doc,@path,@value) AS value', [
    ['doc', TYPES.NVarChar, 'not json', { length: 100 }], ['path', TYPES.NVarChar, '$.a', { length: 20 }],
    ['value', TYPES.Int, 5],
  ]],
  ['bound modify invalid path', 'SELECT JSON_MODIFY(@doc,@path,@value) AS value', [
    ['doc', TYPES.NVarChar, '{"a":1}', { length: 100 }], ['path', TYPES.NVarChar, 'a', { length: 20 }],
    ['value', TYPES.Int, 5],
  ]],
  ['bound modify null path', 'SELECT JSON_MODIFY(@doc,@path,@value) AS value', [
    ['doc', TYPES.NVarChar, '{"a":1}', { length: 100 }], ['path', TYPES.NVarChar, null, { length: 20 }],
    ['value', TYPES.Int, 5],
  ]],
  ['bound modify ansi doc', 'SELECT JSON_MODIFY(@doc,@path,@value) AS value', [
    ['doc', TYPES.VarChar, '{"a":1}', { length: 100 }], ['path', TYPES.VarChar, '$.a', { length: 20 }],
    ['value', TYPES.VarChar, 'y', { length: 20 }],
  ]],
  ['bound modify replay', 'SELECT JSON_MODIFY(@doc,@path,@value) AS value', [
    ['doc', TYPES.NVarChar, '{"a":1}', { length: 100 }], ['path', TYPES.NVarChar, '$.b', { length: 20 }],
    ['value', TYPES.NVarChar, 'x', { length: 20 }],
  ]],
]

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    const result = canonical(await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql)))
    records.push({
      name, sql,
      ...(parameters ? { parameters: parameters.map(([parameter, type, value, options]) => canonical({
        name: parameter, type: type.name, value: value instanceof Date ? value.toISOString() : value,
        ...(options ? { options: { ...options, ...(options.length === Infinity ? { length: 'max' } : {}) } } : {}),
      })) } : {}),
      result,
    })
  }
  await record('create source', 'CREATE TABLE dbo.json_ctor_src(id INT,txt NVARCHAR(20),ansi VARCHAR(20),doc NVARCHAR(200))')
  await record('insert source', "INSERT dbo.json_ctor_src VALUES (1,N'a\"1',  'x',N'{\"list\":[]}'),(2,NULL,NULL,N'{\"v\":0}'),(3,N'k3','y',NULL)")
  for (const [name, sql] of cases) await record(name, sql)
  for (const [name, sql, parameters] of boundCases) await record(name, sql, parameters)
  return records
}

function validate(run) {
  assert.equal(run.length, cases.length + 2 + boundCases.length)
  const get = name => {
    const found = run.find(record => record.name === name)
    assert(found, name + ': missing record')
    return found.result
  }
  const row = name => get(name).sets[0]?.rows
  assert.deepEqual(row('object empty'), [['{}']])
  assert.deepEqual(row('object basic'), [['{"a":1,"b":"x"}']])
  assert.deepEqual(row('object null default'), [['{"a":null,"b":1}']])
  assert.deepEqual(row('object absent on null'), [['{"b":1}']])
  assert.deepEqual(row('array empty'), [['[]']])
  assert.deepEqual(row('array null default'), [['[1,2]']])
  assert.deepEqual(row('array null on null'), [['[1,null,2]']])
  assert.deepEqual(row('object nested'), [['{"o":{"x":1},"a":[1,2]}']])
  assert.deepEqual(row('modify replace number'), [['{"a":2}']])
  assert.deepEqual(row('modify append'), [['{"arr":[1,2,3]}']])
  assert.deepEqual(row('modify strict null sets null'), [['{"a":null}']])
  assert.deepEqual(row('bound modify'), [['{"a":1,"b":"x"}']])
  for (const name of ['object basic', 'array basic', 'modify replace number', 'modify ansi input', 'bound modify ansi doc']) {
    assert.equal(get(name).sets[0].columns[0].type, 'NVarChar', name)
    assert.equal(get(name).sets[0].columns[0].length, 65535, name)
  }
  for (const name of ['object returning json', 'array returning json', 'object json type value', 'modify json type input']) {
    assert.equal(get(name).sets[0].columns[0].type, 'VarChar', name)
    assert.equal(get(name).sets[0].columns[0].collation.codepage, 'utf-8', name)
  }
  for (const [name, number] of [
    ['object comma pair syntax', 102],
    ['object null key', 13638],
    ['bound object null key', 13638],
    ['object scalar hierarchyid', 13666],
    ['array scalar geometry', 13666],
    ['object json_query invalid', 13609],
    ['modify insert strict missing', 13608],
    ['modify append non-array strict', 13621],
    ['modify root', 13619],
    ['modify invalid json', 13609],
    ['modify invalid path no dollar', 13607],
    ['modify invalid path wildcard', 13660],
    ['modify money', 8116],
    ['modify null path', 8116],
    ['modify too few arguments', 174],
  ]) assert.equal(get(name).errors[0]?.number, number, name)
  for (const record of run) assert(record.result.done.length > 0, record.name + ': no completion')
}

// Node 24 builds a failed assert.equal/deepEqual AssertionError by inspecting
// both operands even with a custom message; for multi-MB captures that grew a
// process to ~123 GB. Compare whole captures with isDeepStrictEqual and throw
// plain errors naming only the first differing record.
function same(left, right, message) {
  if (isDeepStrictEqual(left, right)) return
  const flat = value => value?.containers ? value.containers.flatMap(c => c.runs.flat()) : value
  const a = flat(left)
  const b = flat(right)
  const index = Array.isArray(a) && Array.isArray(b)
    ? Math.max(a.findIndex((record, i) => !isDeepStrictEqual(record, b[i])), a.length === b.length ? -1 : Math.min(a.length, b.length))
    : -1
  throw new Error(message + (index >= 0 ? ` (first difference at record ${index}: ${JSON.stringify(a[index]?.name ?? b[index]?.name ?? null)})` : ''))
}

// Refuse to overwrite before any container starts; never parse the fixture for this.
if (writeFixture && existsSync(fixture)) throw new Error('refusing to overwrite retained fixture ' + fileURLToPath(fixture))

await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
      const run = await isolatedReference({
        ...config, options: { ...config.options, requestTimeout: 120000 },
      }, observe)
      if (!oneDatabase) validate(run)
      runs.push(run)
    }
    if (!oneDatabase) same(runs[0], runs[1], 'JSON constructor observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) same(containers[0].runs[0], containers[1].runs[0], 'JSON constructor observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
if (!writeFixture) {
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
}
if (retained && !oneDatabase) same(actual, retained, 'JSON constructor observations differ from retained fixture')
if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
console.log(`Captured ${containers[0].runs[0].length} JSON constructor observations` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained && !oneDatabase ? ' and matched retained fixture' : ''))
