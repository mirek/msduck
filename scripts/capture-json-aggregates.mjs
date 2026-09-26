#!/usr/bin/env node
// Retain SQL Server JSON_OBJECTAGG and JSON_ARRAYAGG rows, descriptors,
// diagnostics and completions for batches, sp_executesql and sp_prepare.
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { isDeepStrictEqual } from 'node:util'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/json-aggregates.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/compatibility/json-aggregates/capture.json')

const setup = [
  ['create source', 'CREATE TABLE dbo.json_agg_src(id INT NOT NULL PRIMARY KEY,g INT,seq INT,rank_key INT,k NVARCHAR(20),v NVARCHAR(20),n INT,ansi VARCHAR(20))'],
  ['insert source', "INSERT dbo.json_agg_src VALUES (1,1,2,2,N'b',N'y',2,'b'),(2,1,1,1,N'a',N'x',1,'a'),(3,1,3,NULL,N'c',NULL,NULL,NULL),(4,2,1,1,N'd',N'x',4,'x'),(5,2,2,1,N'd',N'x',5,'x'),(6,4,1,1,NULL,N'z',6,'z')"],
  ['create empty', 'CREATE TABLE dbo.json_agg_empty(g INT,k NVARCHAR(20),v INT)'],
]

// One scalar per family; each is aggregated from a single row.
const scalars = [
  ['tinyint', 'CAST(255 AS TINYINT)'],
  ['smallint', 'CAST(-32768 AS SMALLINT)'],
  ['int', 'CAST(-2147483648 AS INT)'],
  ['bigint', 'CAST(9223372036854775807 AS BIGINT)'],
  ['bit true', 'CAST(1 AS BIT)'],
  ['bit false', 'CAST(0 AS BIT)'],
  ['decimal', 'CAST(-12.340 AS DECIMAL(10,3))'],
  ['numeric', 'CAST(1.5 AS NUMERIC(5,2))'],
  ['money', 'CAST(12.3456 AS MONEY)'],
  ['smallmoney', 'CAST(-1.5 AS SMALLMONEY)'],
  ['float', 'CAST(0.1 AS FLOAT)'],
  ['float negative zero', 'CAST(-0e0 AS FLOAT)'],
  ['real', 'CAST(0.1 AS REAL)'],
  ['date', "CAST('2024-01-02' AS DATE)"],
  ['time', "CAST('03:04:05.1234567' AS TIME(7))"],
  ['datetime', "CAST('2024-01-02T03:04:05.123' AS DATETIME)"],
  ['smalldatetime', "CAST('2024-01-02T03:04:00' AS SMALLDATETIME)"],
  ['datetime2', "CAST('2024-01-02T03:04:05.1234567' AS DATETIME2(7))"],
  ['datetimeoffset', "CAST('2024-01-02T03:04:05.1+05:30' AS DATETIMEOFFSET(1))"],
  ['uniqueidentifier', "CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER)"],
  ['char padded', "CAST('ab' AS CHAR(5))"],
  ['varchar', "CAST('ab' AS VARCHAR(5))"],
  ['nchar padded', "CAST(N'ab' AS NCHAR(5))"],
  ['nvarchar max', "CAST(N'ab' AS NVARCHAR(MAX))"],
  ['varbinary', 'CAST(0x414243 AS VARBINARY(3))'],
  ['xml', "CAST('<a>1</a>' AS XML)"],
  ['sql_variant', 'CAST(CAST(1 AS INT) AS SQL_VARIANT)'],
  ['hierarchyid', "CAST('/1/' AS HIERARCHYID)"],
  ['json type', "CAST(N'{\"x\":1}' AS JSON)"],
  ['typed null int', 'CAST(NULL AS INT)'],
]

// Characters needing JSON escaping, built without relying on client literals.
const escapeSource = "N'q\"b\\s/t'+NCHAR(9)+N'n'+NCHAR(10)+N'c'+NCHAR(1)+N'd'+NCHAR(127)+N'e'+NCHAR(233)+N's'+NCHAR(0xD83D)+NCHAR(0xDE00)"
const src = 'dbo.json_agg_src'

const cases = [
  // JSON_OBJECTAGG shape and NULL handling.
  ['objectagg constant no from', "SELECT JSON_OBJECTAGG('a':1) AS value"],
  ['objectagg single row', "SELECT JSON_OBJECTAGG(k:v) AS value FROM " + src + " WHERE id=2"],
  ['objectagg group', "SELECT JSON_OBJECTAGG(k:v) AS value FROM " + src + " WHERE g=1"],
  ['objectagg int values', "SELECT JSON_OBJECTAGG(k:n) AS value FROM " + src + " WHERE g=1"],
  ['objectagg null default', "SELECT JSON_OBJECTAGG(k:v) AS value FROM " + src + " WHERE id=3"],
  ['objectagg null on null', "SELECT JSON_OBJECTAGG(k:v NULL ON NULL) AS value FROM " + src + " WHERE g=1"],
  ['objectagg absent on null', "SELECT JSON_OBJECTAGG(k:v ABSENT ON NULL) AS value FROM " + src + " WHERE g=1"],
  ['objectagg absent all null', "SELECT JSON_OBJECTAGG(k:v ABSENT ON NULL) AS value FROM " + src + " WHERE id=3"],
  ['objectagg both null clauses', "SELECT JSON_OBJECTAGG(k:v NULL ON NULL ABSENT ON NULL) AS value FROM " + src],
  ['objectagg empty input', "SELECT JSON_OBJECTAGG(k:v) AS value FROM " + src + " WHERE 1=0"],
  ['objectagg empty table', "SELECT JSON_OBJECTAGG(k:v) AS value FROM dbo.json_agg_empty"],
  ['objectagg empty grouped', "SELECT g,JSON_OBJECTAGG(k:v) AS value FROM dbo.json_agg_empty GROUP BY g"],
  ['objectagg grouped', "SELECT g,JSON_OBJECTAGG(k:n) AS value FROM " + src + " WHERE k IS NOT NULL GROUP BY g ORDER BY g"],
  ['objectagg outer empty group', "SELECT groups.g,JSON_OBJECTAGG(COALESCE(s.k,N'none'):s.n) AS value FROM (VALUES(1),(3)) groups(g) LEFT JOIN " + src + " s ON s.g=groups.g AND s.id=2 GROUP BY groups.g ORDER BY groups.g"],
  ['objectagg duplicate keys', "SELECT JSON_OBJECTAGG(k:n) AS value FROM " + src + " WHERE g=2"],
  ['objectagg null key', "SELECT JSON_OBJECTAGG(k:v) AS value FROM " + src + " WHERE g=4"],
  ['objectagg null key grouped', "SELECT g,JSON_OBJECTAGG(k:v) AS value FROM " + src + " GROUP BY g ORDER BY g"],
  ['objectagg null key absent on null', "SELECT JSON_OBJECTAGG(k:v ABSENT ON NULL) AS value FROM " + src + " WHERE g=4"],
  ['objectagg literal null key', "SELECT JSON_OBJECTAGG(NULL:1) AS value"],
  ['objectagg integer key', "SELECT JSON_OBJECTAGG(n:v) AS value FROM " + src + " WHERE id=2"],
  ['objectagg binary key', "SELECT JSON_OBJECTAGG(0x41:1) AS value"],
  ['objectagg padded char key', "SELECT JSON_OBJECTAGG(CAST('k' AS CHAR(3)):1) AS value"],
  ['objectagg key escaping', `SELECT JSON_OBJECTAGG(${escapeSource}:1) AS value`],
  ['objectagg value escaping', `SELECT JSON_OBJECTAGG('v':${escapeSource}) AS value`],
  ['objectagg ansi column', "SELECT JSON_OBJECTAGG(k:ansi) AS value FROM " + src + " WHERE id=2"],
  ['objectagg nested object', "SELECT JSON_OBJECTAGG(k:JSON_OBJECT('n':n,'v':v)) AS value FROM " + src + " WHERE id=2"],
  ['objectagg nested array', "SELECT JSON_OBJECTAGG(k:JSON_ARRAY(n,v)) AS value FROM " + src + " WHERE id=2"],
  ['objectagg json_query value', "SELECT JSON_OBJECTAGG('q':JSON_QUERY(N'{\"x\":[1,2]}')) AS value"],
  ['objectagg json text as string', "SELECT JSON_OBJECTAGG('q':N'{\"x\":1}') AS value"],
  ['objectagg nested arrayagg', "SELECT JSON_OBJECTAGG(g:JSON_ARRAYAGG(n)) AS value FROM " + src + " GROUP BY g"],
  ['objectagg over grouped subquery', "SELECT JSON_OBJECTAGG(CAST(g AS NVARCHAR(10)):items) AS value FROM (SELECT g,JSON_ARRAYAGG(n ORDER BY seq) AS items FROM " + src + " WHERE g=1 GROUP BY g) d"],
  ['objectagg returning json', "SELECT JSON_OBJECTAGG(k:v RETURNING JSON) AS value FROM " + src + " WHERE id=2"],
  ['objectagg absent returning json', "SELECT JSON_OBJECTAGG(k:v ABSENT ON NULL RETURNING JSON) AS value FROM " + src + " WHERE g=1 AND id<3"],
  ['objectagg returning nvarchar', "SELECT JSON_OBJECTAGG(k:v RETURNING NVARCHAR(MAX)) AS value FROM " + src],
  ['objectagg comma syntax', "SELECT JSON_OBJECTAGG(k,v) AS value FROM " + src],
  ['objectagg missing value', "SELECT JSON_OBJECTAGG(k:) AS value FROM " + src],
  ['objectagg no arguments', "SELECT JSON_OBJECTAGG() AS value FROM " + src],
  ['objectagg two pairs', "SELECT JSON_OBJECTAGG(k:v,'x':1) AS value FROM " + src],
  ['objectagg order by', "SELECT JSON_OBJECTAGG(k:v ORDER BY k) AS value FROM " + src],
  ['objectagg within group', "SELECT JSON_OBJECTAGG(k:v) WITHIN GROUP (ORDER BY k) AS value FROM " + src],
  ['objectagg distinct', "SELECT JSON_OBJECTAGG(DISTINCT k:v) AS value FROM " + src],
  ['objectagg over partition', "SELECT id,JSON_OBJECTAGG(k:n) OVER(PARTITION BY g) AS value FROM " + src + " WHERE k IS NOT NULL AND g=1 ORDER BY id"],
  ['objectagg over empty', "SELECT id,JSON_OBJECTAGG(k:n) OVER() AS value FROM " + src + " WHERE id IN (1,2) ORDER BY id"],
  ['objectagg over order', "SELECT id,JSON_OBJECTAGG(k:n) OVER(ORDER BY id) AS value FROM " + src + " WHERE id IN (1,2) ORDER BY id"],
  ['objectagg over frame', "SELECT id,JSON_OBJECTAGG(k:n) OVER(ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS value FROM " + src + " WHERE id IN (1,2) ORDER BY id"],
  ['objectagg ungrouped column', "SELECT g,JSON_OBJECTAGG(k:v) AS value FROM " + src],
  ['objectagg in where', "SELECT id FROM " + src + " WHERE JSON_OBJECTAGG(k:v) IS NOT NULL"],
  ['objectagg nested aggregate', "SELECT JSON_OBJECTAGG(k:COUNT(*)) AS value FROM " + src],
  ['objectagg having', "SELECT g FROM " + src + " WHERE k IS NOT NULL GROUP BY g HAVING LEN(JSON_OBJECTAGG(k:n))>10 ORDER BY g"],
  ['objectagg isjson', "SELECT ISJSON(JSON_OBJECTAGG(k:v)) AS value FROM " + src + " WHERE g=1"],
  ['objectagg long value', "SELECT DATALENGTH(JSON_OBJECTAGG(CAST(id AS NVARCHAR(10)):REPLICATE(N'x',4000))) AS bytes FROM " + src],
  ['objectagg describe', "EXEC sp_describe_first_result_set N'SELECT JSON_OBJECTAGG(k:v) AS o, JSON_OBJECTAGG(k:ansi) AS a, JSON_OBJECTAGG(k:n RETURNING JSON) AS j FROM dbo.json_agg_src'"],
  ['objectagg select into', "SELECT JSON_OBJECTAGG(k:v) AS o,JSON_ARRAYAGG(v) AS a,JSON_ARRAYAGG(v RETURNING JSON) AS j INTO dbo.json_agg_into FROM " + src + " WHERE id=2; SELECT c.name,t.name AS type_name,c.max_length,c.is_nullable,c.collation_name FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('dbo.json_agg_into') ORDER BY c.column_id"],

  // JSON_ARRAYAGG shape, ordering and NULL handling.
  ['arrayagg constant no from', 'SELECT JSON_ARRAYAGG(1) AS value'],
  ['arrayagg single row', "SELECT JSON_ARRAYAGG(v) AS value FROM " + src + " WHERE id=2"],
  ['arrayagg order asc', "SELECT JSON_ARRAYAGG(k ORDER BY seq) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg order desc', "SELECT JSON_ARRAYAGG(k ORDER BY seq DESC) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg order null keys', "SELECT JSON_ARRAYAGG(k ORDER BY rank_key,seq) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg order null keys desc', "SELECT JSON_ARRAYAGG(k ORDER BY rank_key DESC,seq) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg order multiple keys', "SELECT JSON_ARRAYAGG(id ORDER BY g DESC,seq ASC) AS value FROM " + src],
  ['arrayagg order by expression', "SELECT JSON_ARRAYAGG(id ORDER BY -id) AS value FROM " + src],
  ['arrayagg order by value itself', "SELECT JSON_ARRAYAGG(k ORDER BY k DESC) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg order by ordinal', "SELECT JSON_ARRAYAGG(id ORDER BY 1) AS value FROM " + src],
  ['arrayagg order by collate', "SELECT JSON_ARRAYAGG(v ORDER BY v COLLATE Latin1_General_BIN2,id) AS value FROM (VALUES(1,N'b'),(2,N'B'),(3,N'a')) d(id,v)"],
  ['arrayagg order ties identical', "SELECT JSON_ARRAYAGG(v ORDER BY rank_key) AS value FROM " + src + " WHERE g=2"],
  ['arrayagg null default', "SELECT JSON_ARRAYAGG(v ORDER BY seq) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg null on null', "SELECT JSON_ARRAYAGG(v ORDER BY seq NULL ON NULL) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg absent on null', "SELECT JSON_ARRAYAGG(v ORDER BY seq ABSENT ON NULL) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg null on null no order', "SELECT JSON_ARRAYAGG(v NULL ON NULL) AS value FROM " + src + " WHERE id=3"],
  ['arrayagg all null default', "SELECT JSON_ARRAYAGG(v) AS value FROM " + src + " WHERE id=3"],
  ['arrayagg clause before order', "SELECT JSON_ARRAYAGG(v NULL ON NULL ORDER BY seq) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg both null clauses', "SELECT JSON_ARRAYAGG(v NULL ON NULL ABSENT ON NULL) AS value FROM " + src],
  ['arrayagg empty input', "SELECT JSON_ARRAYAGG(v) AS value FROM " + src + " WHERE 1=0"],
  ['arrayagg empty input null on null', "SELECT JSON_ARRAYAGG(v NULL ON NULL) AS value FROM " + src + " WHERE 1=0"],
  ['arrayagg empty table', "SELECT JSON_ARRAYAGG(v) AS value FROM dbo.json_agg_empty"],
  ['arrayagg empty grouped', "SELECT g,JSON_ARRAYAGG(v) AS value FROM dbo.json_agg_empty GROUP BY g"],
  ['arrayagg grouped', "SELECT g,JSON_ARRAYAGG(n ORDER BY seq) AS value FROM " + src + " GROUP BY g ORDER BY g"],
  ['arrayagg outer empty group', "SELECT groups.g,JSON_ARRAYAGG(s.n ORDER BY s.seq) AS value,JSON_ARRAYAGG(s.n ORDER BY s.seq NULL ON NULL) AS nulls FROM (VALUES(1),(3)) groups(g) LEFT JOIN " + src + " s ON s.g=groups.g GROUP BY groups.g ORDER BY groups.g"],
  ['arrayagg value escaping', `SELECT JSON_ARRAYAGG(${escapeSource}) AS value`],
  ['arrayagg ansi column', "SELECT JSON_ARRAYAGG(ansi ORDER BY seq) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg nested object', "SELECT JSON_ARRAYAGG(JSON_OBJECT('id':id,'k':k) ORDER BY id) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg nested array', "SELECT JSON_ARRAYAGG(JSON_ARRAY(id,v) ORDER BY id) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg json_query value', "SELECT JSON_ARRAYAGG(JSON_QUERY(N'[1,{\"a\":2}]')) AS value"],
  ['arrayagg json text as string', "SELECT JSON_ARRAYAGG(N'[1,2]') AS value"],
  ['arrayagg nested objectagg', "SELECT JSON_ARRAYAGG(o ORDER BY g) AS value FROM (SELECT g,JSON_OBJECTAGG(k:n) AS o FROM " + src + " WHERE k IS NOT NULL GROUP BY g) d"],
  ['arrayagg of aggregate', "SELECT JSON_ARRAYAGG(COUNT(*)) AS value FROM " + src],
  ['arrayagg returning json', "SELECT JSON_ARRAYAGG(v ORDER BY seq RETURNING JSON) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg null on null returning json', "SELECT JSON_ARRAYAGG(v ORDER BY seq NULL ON NULL RETURNING JSON) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg returning before order', "SELECT JSON_ARRAYAGG(v RETURNING JSON ORDER BY seq) AS value FROM " + src],
  ['arrayagg returning varchar', "SELECT JSON_ARRAYAGG(v RETURNING VARCHAR(100)) AS value FROM " + src],
  ['arrayagg within group', "SELECT JSON_ARRAYAGG(v) WITHIN GROUP (ORDER BY seq) AS value FROM " + src],
  ['arrayagg within group desc', "SELECT JSON_ARRAYAGG(k) WITHIN GROUP (ORDER BY seq DESC) AS value FROM " + src + " WHERE g=1"],
  ['arrayagg unordered scan', "SELECT JSON_ARRAYAGG(id) AS value FROM " + src],
  ['arrayagg correlated subquery', "SELECT o.g,(SELECT JSON_ARRAYAGG(i.n ORDER BY i.seq) FROM " + src + " i WHERE i.g=o.g) AS value FROM (VALUES(1),(3)) o(g) ORDER BY o.g"],
  ['arrayagg distinct', "SELECT JSON_ARRAYAGG(DISTINCT v) AS value FROM " + src],
  ['arrayagg two arguments', "SELECT JSON_ARRAYAGG(v,n) AS value FROM " + src],
  ['arrayagg no arguments', "SELECT JSON_ARRAYAGG() AS value FROM " + src],
  ['arrayagg key syntax', "SELECT JSON_ARRAYAGG(k:v) AS value FROM " + src],
  ['arrayagg over partition', "SELECT id,JSON_ARRAYAGG(n) OVER(PARTITION BY g) AS value FROM " + src + " WHERE id IN (1,4) ORDER BY id"],
  ['arrayagg over empty', "SELECT id,JSON_ARRAYAGG(n) OVER() AS value FROM " + src + " WHERE id=2 ORDER BY id"],
  ['arrayagg over order', "SELECT id,JSON_ARRAYAGG(n) OVER(ORDER BY id) AS value FROM " + src + " WHERE id IN (1,2) ORDER BY id"],
  ['arrayagg order and over', "SELECT id,JSON_ARRAYAGG(n ORDER BY seq) OVER(PARTITION BY g) AS value FROM " + src + " WHERE g=1 ORDER BY id"],
  ['arrayagg incompatible orders', "SELECT JSON_ARRAYAGG(n ORDER BY seq) AS a,JSON_ARRAYAGG(n ORDER BY seq DESC) AS b FROM " + src + " WHERE g=1"],
  ['arrayagg with string_agg orders', "SELECT JSON_ARRAYAGG(n ORDER BY seq) AS a,STRING_AGG(k,N',') WITHIN GROUP (ORDER BY seq DESC) AS b FROM " + src + " WHERE g=1"],
  ['arrayagg same orders', "SELECT JSON_ARRAYAGG(n ORDER BY seq) AS a,JSON_ARRAYAGG(k ORDER BY seq) AS b FROM " + src + " WHERE g=1"],
  ['arrayagg ordered and unordered', "SELECT JSON_ARRAYAGG(n ORDER BY seq) AS a,JSON_ARRAYAGG(k) AS b FROM " + src + " WHERE id=2"],
  ['arrayagg ungrouped column', "SELECT g,JSON_ARRAYAGG(v) AS value FROM " + src],
  ['arrayagg in where', "SELECT id FROM " + src + " WHERE JSON_ARRAYAGG(v) IS NOT NULL"],
  ['arrayagg isjson', "SELECT ISJSON(JSON_ARRAYAGG(v ORDER BY seq NULL ON NULL)) AS value FROM " + src],
  ['arrayagg long value', "SELECT DATALENGTH(JSON_ARRAYAGG(REPLICATE(N'x',4000))) AS bytes, LEN(JSON_ARRAYAGG(REPLICATE(CAST(N'y' AS NVARCHAR(MAX)),10000))) AS chars FROM " + src],
  ['arrayagg describe', "EXEC sp_describe_first_result_set N'SELECT JSON_ARRAYAGG(v) AS u, JSON_ARRAYAGG(ansi ORDER BY seq) AS a, JSON_ARRAYAGG(n RETURNING JSON) AS j FROM dbo.json_agg_src'"],
  ['arrayagg in json_object', "SELECT JSON_OBJECT('g':g,'items':JSON_ARRAYAGG(n ORDER BY seq)) AS value FROM " + src + " WHERE g=1 GROUP BY g"],
  ['arrayagg in json_array', "SELECT JSON_ARRAY(JSON_ARRAYAGG(n ORDER BY seq),JSON_OBJECTAGG(k:n)) AS value FROM " + src + " WHERE g=1 AND n IS NOT NULL"],
  ['arrayagg json_query over result', "SELECT JSON_QUERY(JSON_ARRAYAGG(n ORDER BY seq)) AS value, JSON_VALUE(JSON_ARRAYAGG(n ORDER BY seq),'$[1]') AS second FROM " + src + " WHERE g=1"],
  ['aggregates in view', "EXEC('CREATE VIEW dbo.json_agg_view AS SELECT g,JSON_ARRAYAGG(n ORDER BY seq) AS items,JSON_OBJECTAGG(COALESCE(k,N''-''):n) AS obj FROM dbo.json_agg_src GROUP BY g'); SELECT g,items,obj FROM dbo.json_agg_view ORDER BY g"],
  ['aggregates view columns', "SELECT c.name,t.name AS type_name,c.max_length,c.is_nullable,c.collation_name FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('dbo.json_agg_view') ORDER BY c.column_id"],
]

for (const [family, expression] of scalars) {
  cases.push([`objectagg scalar ${family}`, `SELECT JSON_OBJECTAGG('v':${expression}) AS value`])
  cases.push([`arrayagg scalar ${family}`, `SELECT JSON_ARRAYAGG(${expression}) AS value`])
}

const describeOptions = options => options ? { options: { ...options, ...(options.length === Infinity ? { length: 'max' } : {}) } } : {}

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

const rpcCases = [
  ['rpc objectagg bound key and value', "SELECT JSON_OBJECTAGG(@k:@v) AS value FROM (VALUES(1),(2)) d(i) WHERE i=1", [
    ['k', TYPES.NVarChar, 'key"1', { length: 20 }], ['v', TYPES.Int, 7],
  ]],
  ['rpc objectagg null value', "SELECT JSON_OBJECTAGG(@k:@v) AS value,JSON_OBJECTAGG(@k:@v ABSENT ON NULL) AS absent", [
    ['k', TYPES.NVarChar, 'a', { length: 20 }], ['v', TYPES.NVarChar, null, { length: 20 }],
  ]],
  ['rpc objectagg null key', "SELECT JSON_OBJECTAGG(@k:1) AS value", [['k', TYPES.NVarChar, null, { length: 20 }]]],
  ['rpc objectagg group filter', "SELECT JSON_OBJECTAGG(k:n) AS value FROM dbo.json_agg_src WHERE g=@g AND k IS NOT NULL", [['g', TYPES.Int, 2]]],
  ['rpc arrayagg group filter', "SELECT JSON_ARRAYAGG(v ORDER BY seq NULL ON NULL) AS value FROM dbo.json_agg_src WHERE g=@g", [['g', TYPES.Int, 1]]],
  ['rpc arrayagg empty filter', "SELECT JSON_ARRAYAGG(v ORDER BY seq) AS value FROM dbo.json_agg_src WHERE g=@g", [['g', TYPES.Int, 99]]],
  ['rpc arrayagg bound values', "SELECT JSON_ARRAYAGG(x) AS value FROM (VALUES(@s),(@t)) d(x)", [
    ['s', TYPES.VarChar, 'x', { length: 20 }], ['t', TYPES.NVarChar, 'yé', { length: 20 }],
  ]],
  ['rpc arrayagg bound families', "SELECT JSON_ARRAYAGG(@n) AS n,JSON_ARRAYAGG(@f) AS f,JSON_ARRAYAGG(@d) AS d,JSON_ARRAYAGG(@dt) AS dt,JSON_ARRAYAGG(@b) AS b", [
    ['n', TYPES.BigInt, '9007199254740993'], ['f', TYPES.Float, 0.5], ['d', TYPES.Decimal, 1.5, { precision: 10, scale: 3 }],
    ['dt', TYPES.DateTime2, new Date('2024-01-02T03:04:05.123Z'), { scale: 3 }], ['b', TYPES.Bit, true],
  ]],
  ['rpc arrayagg max value', "SELECT JSON_ARRAYAGG(@v) AS value", [['v', TYPES.NVarChar, 'max', { length: Infinity }]]],
  ['rpc arrayagg replay', "SELECT JSON_ARRAYAGG(v ORDER BY seq NULL ON NULL) AS value FROM dbo.json_agg_src WHERE g=@g", [['g', TYPES.Int, 1]]],
]

// sp_prepare/sp_execute/sp_unprepare: one handle per statement, executed with each value set.
const preparedCases = [
  ['prepared arrayagg by group', "SELECT JSON_ARRAYAGG(v ORDER BY seq NULL ON NULL) AS value,JSON_ARRAYAGG(n ORDER BY seq) AS numbers FROM dbo.json_agg_src WHERE g=@g",
    [['g', TYPES.Int]],
    [{ g: 1 }, { g: 2 }, { g: 99 }, { g: null }, { g: 1 }]],
  ['prepared arrayagg incompatible orders', "SELECT JSON_ARRAYAGG(v ORDER BY seq) AS value,JSON_ARRAYAGG(n ORDER BY seq DESC) AS numbers FROM dbo.json_agg_src WHERE g=@g",
    [['g', TYPES.Int]],
    [{ g: 1 }]],
  ['prepared objectagg by group', "SELECT JSON_OBJECTAGG(k:n) AS value FROM dbo.json_agg_src WHERE g=@g",
    [['g', TYPES.Int]],
    [{ g: 1 }, { g: 2 }, { g: 4 }, { g: 99 }, { g: 1 }]],
  ['prepared objectagg bound pair', "SELECT JSON_OBJECTAGG(@k:@v ABSENT ON NULL) AS value,JSON_OBJECTAGG(@k:@v) AS value_null",
    [['k', TYPES.NVarChar, { length: 20 }], ['v', TYPES.NVarChar, { length: 20 }]],
    [{ k: 'a', v: 'x' }, { k: 'a', v: null }, { k: null, v: 'x' }, { k: 'b"', v: '\t' }]],
]

const errorFields = e => ({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
const newResult = () => ({ sets: [], done: [], errors: [], info: [], returnStatus: null })

// Prepare completion is signalled by the request's 'prepared'/'error' events,
// not by its callback; execute and unprepare complete through the callback.
async function prepared(connection, sql, declarations, executions) {
  let result
  let complete = () => {}
  const request = new Request(sql, (...values) => complete(...values))
  for (const [name, type, options] of declarations) request.addParameter(name, type, undefined, options)
  const onError = e => result?.errors.push(errorFields(e))
  const onInfo = e => result?.info.push(errorFields(e))
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(x => ({ name: x.colName, type: x.type.name, length: x.dataLength ?? null, precision: x.precision ?? null, scale: x.scale ?? null, flags: x.flags, collation: canonical(x.collation ?? null) })), rows: [] }))
  request.on('row', row => result?.sets.at(-1).rows.push(row.map(x => x.value)))
  for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => result?.done.push({ kind, rowCount: rowCount ?? null, more }))
  request.on('doneProc', (_count, _more, status) => { if (result) result.returnStatus = status })
  const outcomes = []
  try {
    result = newResult()
    const prepareError = await new Promise(done => {
      request.once('prepared', () => done(null))
      request.once('error', error => done(error))
      connection.prepare(request)
    })
    const preparation = result
    if (prepareError) {
      if (!preparation.errors.length) preparation.errors.push({ message: prepareError.message, number: prepareError.number ?? null })
      result = undefined
      return { preparation: canonical(preparation), prepared: false, executions: [] }
    }
    for (const values of executions) {
      result = newResult()
      await new Promise(done => {
        complete = (error, rowCount) => {
          result.rowCount = rowCount
          if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
          done()
        }
        request.error = undefined
        connection.execute(request, values)
      })
      outcomes.push({ values, result: canonical(result) })
    }
    result = newResult()
    await new Promise(done => {
      complete = error => {
        if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
        done()
      }
      request.error = undefined
      connection.unprepare(request)
    })
    const unpreparation = result
    result = undefined
    return { preparation: canonical(preparation), prepared: true, executions: outcomes, unpreparation: canonical(unpreparation) }
  } finally {
    complete = () => {}
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

async function observe(connection) {
  const records = []
  const batch = async (name, sql) => records.push({ name, sql, result: canonical(await capture(connection, sql)) })
  for (const [name, sql] of [...setup, ...cases]) await batch(name, sql)
  for (const [name, sql, parameters] of rpcCases) {
    records.push({
      name, sql, protocol: 'sp_executesql',
      parameters: parameters.map(([parameter, type, value, options]) => canonical({
        name: parameter, type: type.name, value: value instanceof Date ? value.toISOString() : value, ...describeOptions(options),
      })),
      result: canonical(await rpc(connection, sql, parameters)),
    })
  }
  for (const [name, sql, declarations, executions] of preparedCases) {
    records.push({
      name, sql, protocol: 'sp_prepare/sp_execute/sp_unprepare',
      parameters: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, ...describeOptions(options) })),
      ...(await prepared(connection, sql, declarations, executions)),
    })
  }
  await batch('reuse', 'SELECT @@TRANCOUNT AS transaction_count, 1 AS reusable')
  return records
}

// Bounded checks only: never hand captures or fixture values to node:assert.
function check(condition, message) { if (!condition) throw new Error(String(message).slice(0, 300)) }
function expectValue(actual, expected, label) {
  if (isDeepStrictEqual(actual, expected)) return
  const show = value => { const text = JSON.stringify(value) ?? String(value); return text.length > 120 ? text.slice(0, 120) + '...' : text }
  throw new Error(`${label}: got ${show(actual)}, expected ${show(expected)}`)
}

function validate(run) {
  expectValue(run.length, setup.length + cases.length + rpcCases.length + preparedCases.length + 1, 'record count')
  const byName = new Map(run.map(record => [record.name, record]))
  const get = name => {
    const record = byName.get(name)
    check(record, 'missing record ' + name)
    return record
  }
  const result = name => get(name).result
  const rows = name => result(name).sets[0]?.rows
  for (const [name] of setup) expectValue(result(name).errors.length, 0, name + ' errors')
  for (const record of run) {
    if (record.result) check(record.result.done.length > 0, record.name + ': no completion')
    else if (record.prepared) for (const execution of record.executions) check(execution.result.done.length > 0, record.name + ': no completion')
  }
  for (const [name, expected] of EXPECTED_ROWS) expectValue(rows(name), expected, name + ' rows')
  for (const [name, type, length] of EXPECTED_COLUMNS) {
    const column = result(name).sets[0]?.columns[0]
    expectValue([column?.type, column?.length], [type, length], name + ' descriptor')
  }
  for (const [name, number] of EXPECTED_ERRORS) expectValue(result(name).errors[0]?.number, number, name + ' error')
  for (const [name, expected] of EXPECTED_PREPARED) {
    const record = get(name)
    expectValue([record.prepared, record.executions.length], expected, name + ' prepared executions')
  }
  expectValue(get('prepared arrayagg incompatible orders').preparation.errors[0]?.number, 8711, 'prepared incompatible orders error')
  expectValue(result('reuse').errors.length, 0, 'reuse errors')
}

// Filled from the first diagnostic capture; the retained fixture is authoritative.
const EXPECTED_ROWS = [
  ['objectagg group', [['{"b":"y","a":"x","c":null}']]],
  ['objectagg null on null', [['{"b":"y","a":"x","c":null}']]],
  ['objectagg absent on null', [['{"b":"y","a":"x"}']]],
  ['objectagg absent all null', [['{}']]],
  ['objectagg empty input', [[null]]],
  ['objectagg empty grouped', []],
  ['objectagg duplicate keys', [['{"d":4,"d":5}']]],
  ['objectagg binary key', [['{"QQ==":1}']]],
  ['objectagg over order', [[1, '{"b":2}'], [2, '{"b":2,"a":1}']]],
  ['arrayagg order asc', [['["a","b","c"]']]],
  ['arrayagg order desc', [['["c","b","a"]']]],
  ['arrayagg order null keys', [['["c","a","b"]']]],
  ['arrayagg order by collate', [['["B","a","b"]']]],
  ['arrayagg null default', [['["x","y"]']]],
  ['arrayagg null on null', [['["x","y",null]']]],
  ['arrayagg all null default', [['[]']]],
  ['arrayagg empty input', [[null]]],
  ['arrayagg empty input null on null', [[null]]],
  ['arrayagg outer empty group', [[1, '[1,2]', '[1,2,null]'], [3, '[]', '[null]']]],
  ['arrayagg within group desc', [['["b","a","c"]']]],
  ['arrayagg scalar float', [['[1.000000000000000e-001]']]],
  ['objectagg scalar typed null int', [['{"v":null}']]],
  ['arrayagg scalar typed null int', [['[]']]],
  ['rpc objectagg null value', [['{"a":null}', '{}']]],
]
const EXPECTED_COLUMNS = [
  ['objectagg group', 'NVarChar', 65535],
  ['arrayagg order asc', 'NVarChar', 65535],
  ['arrayagg ansi column', 'NVarChar', 65535],
  ['objectagg returning json', 'VarChar', 65535],
  ['arrayagg returning json', 'VarChar', 65535],
  ['arrayagg scalar json type', 'VarChar', 65535],
]
const EXPECTED_ERRORS = [
  ['objectagg both null clauses', 102],
  ['objectagg null key', 13638],
  ['objectagg null key absent on null', 13638],
  ['objectagg returning nvarchar', 102],
  ['objectagg comma syntax', 174],
  ['objectagg order by', 156],
  ['objectagg within group', 102],
  ['objectagg nested arrayagg', 130],
  ['objectagg ungrouped column', 8120],
  ['objectagg in where', 147],
  ['arrayagg order by ordinal', 5308],
  ['arrayagg clause before order', 156],
  ['arrayagg distinct', 313],
  ['arrayagg two arguments', 174],
  ['arrayagg order and over', 156],
  ['arrayagg incompatible orders', 8711],
  ['arrayagg with string_agg orders', 8711],
  ['objectagg scalar hierarchyid', 13666],
  ['arrayagg scalar hierarchyid', 13666],
  ['rpc objectagg null key', 13638],
]
const EXPECTED_PREPARED = preparedCases.map(([name, , , executions]) => [name, name.includes('incompatible') ? [false, 0] : [true, executions.length]])

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
const count = oneDatabase ? 1 : 2
const containers = []
for (let containerIndex = 0; containerIndex < count; containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    console.error(`started container ${container.name}`)
    const runs = []
    for (let databaseIndex = 0; databaseIndex < count; databaseIndex++) {
      const run = await isolatedReference({
        ...config, options: { ...config.options, requestTimeout: 120000 },
      }, observe)
      if (!oneDatabase) validate(run)
      runs.push(run)
    }
    if (!oneDatabase) assertSameCapture(runs[0], runs[1], 'JSON aggregate observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) assertSameCapture(containers[0].runs[0], containers[1].runs[0], 'JSON aggregate observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
if (!writeFixture && !oneDatabase) {
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
}
if (retained) assertSameCapture(actual, retained, 'JSON aggregate observations differ from retained fixture')
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${containers[0].runs[0].length} JSON aggregate observations` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained ? ' and matched retained fixture' : ''))
