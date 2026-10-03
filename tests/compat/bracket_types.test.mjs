// Delimited system type names such as CONVERT([nvarchar](200), x), [int]
// columns and DECLARE @x [decimal](5,2), sys-qualified names, sysname, and
// CONVERT to nvarchar(max) without a style (issue #865, docs/bracket-types.md).
// Each case runs its statements in order in a fresh database and is compared
// with `reference`, captured from the pinned SQL Server 2025 image (see
// docs/bracket-types.md). The differences listed in `known` are asserted as
// msduck's current behavior; each one also holds for the plain spelling.
import { isDeepStrictEqual } from 'node:util'
import { test } from 'node:test'
import { start } from '../support/client.mjs'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'

// Bounded comparisons: never hand whole captures to node:assert.
function same(actual, expected, label) {
  if (isDeepStrictEqual(actual, expected)) return
  const text = value => JSON.stringify(value).slice(0, 600)
  throw new Error(`${label}\n  actual:   ${text(actual)}\n  expected: ${text(expected)}`)
}

const view = result => ({
  sets: result.sets.map(set => ({ columns: set.columns.map(c => [c.name, c.type, c.length, c.precision, c.scale]), rows: set.rows })),
  errors: result.errors.map(e => [e.number, e.state, e.class, e.message]),
})
const firstError = local => local.errors[0]?.slice(0, 3) ?? null
const rowsOnly = (local, expected, label) => same(local.sets.map(s => s.rows), expected.sets.map(s => s.rows), label)

// Differences from SQL Server that the plain spelling shares, by case and step.
const known = {
  // numeric(5,1) is described as decimal.
  'system-types#1': (local, expected, label) => {
    rowsOnly(local, expected, label)
    same(local.sets[0].columns[3], ['e', 'DecimalN', 17, 5, 1], label)
  },
  // A binary literal shorter than binary(n) is not padded.
  'system-types#3': local => same(firstError(local), [50000, 1, 16], 'binary width'),
  // decimal without a precision keeps the operand's scale (SQL Server: 18, 0).
  'system-types#4': local => same(local.sets[0].columns[0], ['a', 'DecimalN', 17, 18, 3], 'default decimal'),
  // Names that are not system types keep msduck's existing errors (243 or
  // 2715 in SQL Server).
  'other-delimiters#2': local => same(firstError(local), [208, 1, 16], 'dbo.int'),
  'non-system#0': local => same(firstError(local), [208, 1, 16], 'notatype'),
  'non-system#1': local => same(firstError(local), [208, 1, 16], 'notatype(5)'),
  'non-system#3': local => same(firstError(local), [208, 1, 16], 'TRY_CONVERT notatype'),
  'declare#2': local => same(local.errors, [[40515, 1, 16, 'unsupported scalar declaration [notatype]']], 'DECLARE notatype'),
  // int(5) reaches DuckDB (SQL Server: 291).
  'non-system#4': local => same(firstError(local), [50000, 1, 16], '[int](5)'),
  'non-system#5': local => same(firstError(local), [50000, 1, 16], 'int(5)'),
  // sys.columns descriptors differ; the rows match.
  'computed-persisted#3': rowsOnly,
  'session-default#5': rowsOnly,
  'bracketed-columns#5': rowsOnly,
  // The overflow error has state 1 (SQL Server: 2).
  'nested#1': (local, expected, label) => {
    same(local.sets, expected.sets, label)
    same(local.errors.map(e => [e[0], e[2], e[3]]), expected.errors.map(e => [e[0], e[2], e[3]]), label)
  },
  // sysname columns are not supported, written plainly or delimited.
  'sysname#3': local => same(firstError(local), [208, 1, 16], 'sysname column'),
  // msduck's generic syntax error (SQL Server: 156).
  'batch-boundaries#2': local => same(firstError(local), [102, 1, 15], 'syntax error'),
}

const columns = table => `SELECT c.name, TYPE_NAME(c.user_type_id) AS type, c.max_length, c.precision, c.scale, c.is_nullable, c.is_computed FROM sys.columns c WHERE c.object_id = OBJECT_ID('${table}') ORDER BY c.column_id`
const cases = [
  ['report-convert', [
    `SELECT CONVERT([nvarchar](200), JSON_VALUE(N'{"a":"foo"}',N'$.a')) AS v`,
    `SELECT CONVERT(nvarchar(200), JSON_VALUE(N'{"a":"foo"}',N'$.a')) AS v`,
  ]],
  ['system-types', [
    `SELECT CAST(1 AS [int]) AS a, TRY_CONVERT([int], '12') AS b, CAST(1.5 AS [decimal](10,2)) AS c, CONVERT([datetime2](3), '2020-01-02 03:04:05.1234567') AS d, CONVERT([varbinary](max), 0x0102) AS e, CAST(N'x' AS [sysname]) AS f, TRY_CAST('x' AS [int]) AS g, CAST(1 AS [bigint]) AS h, CAST(1 AS [smallint]) AS i, CAST(1 AS [tinyint]) AS j, CAST(1 AS [bit]) AS k`,
    `SELECT CAST('ab' AS [char](3)) AS a, CAST('ab' AS [varchar](max)) AS b, CAST(N'ab' AS [nchar](2)) AS c, CAST(1.25 AS [numeric](5,1)) AS e, CAST('2020-01-02' AS [date]) AS f, CAST('03:04:05.123' AS [time](3)) AS g, CAST('2020-01-02 03:04:05 +01:00' AS [datetimeoffset](2)) AS h, CAST('2020-01-02 03:04' AS [smalldatetime]) AS i, CAST('2020-01-02 03:04:05.003' AS [datetime]) AS j`,
    `SELECT CAST(1.5 AS [money]) AS a, CAST(1.5 AS [smallmoney]) AS b, CAST(1.5 AS [float]) AS c, CAST(1.5 AS [real]) AS d, CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS [uniqueidentifier]) AS e, CAST(N'ab' AS [nvarchar]) AS f, CAST('ab' AS [varchar]) AS g, CAST(1.5 AS [float](24)) AS h`,
    `SELECT CAST(0x01 AS [binary](2)) AS a`,
    `SELECT CAST(12.345 AS [decimal]) AS a`,
    `SELECT CAST(1 AS [INT]) AS a, CONVERT([NVarChar](5), 12) AS b, CONVERT([nvarchar](max), N'x') AS c, CONVERT([varchar](10), CAST('2020-01-02' AS date), 112) AS d`,
  ]],
  ['other-delimiters', [
    `SELECT CAST(1 AS "int") AS a`,
    `SELECT CONVERT("nvarchar"(5), 12) AS a`,
    `SELECT CAST(1 AS [dbo].[int]) AS a`,
    `SELECT CAST(1 AS [sys].[int]) AS a`,
    `SELECT CAST(1 AS sys.int) AS a`,
  ]],
  ['non-system', [
    `SELECT CAST(1 AS [notatype]) AS a`,
    `SELECT CONVERT([notatype](5), 1) AS a`,
    `SELECT CAST(1 AS [int ]) AS a`,
    `SELECT TRY_CONVERT([notatype], 1) AS a`,
    `SELECT CAST(1 AS [int](5)) AS a`,
    `SELECT CAST(1 AS int(5)) AS a`,
  ]],
  ['names-not-types', [
    `SELECT [int] FROM (SELECT 1 AS [int]) t`,
    `SELECT 1 [int], 2 AS [nvarchar]`,
    `DECLARE @t TABLE ([int] int); INSERT @t VALUES (1); SELECT [int] FROM @t`,
  ]],
  ['computed-persisted', [
    `CREATE TABLE items (id int NOT NULL, body nvarchar(max) NOT NULL, value AS (CONVERT([nvarchar](200),JSON_VALUE(body,N'$.a'))) PERSISTED)`,
    `INSERT items (id, body) VALUES (1, N'{"a":"foo"}'), (2, N'{}')`,
    `SELECT id, value FROM items ORDER BY id`,
    columns('items'),
    `SELECT definition FROM sys.computed_columns WHERE object_id = OBJECT_ID('items')`,
  ]],
  ['session-default', [
    `CREATE TABLE items (id int NOT NULL, value nvarchar(450) NULL DEFAULT CONVERT([nvarchar](450),SESSION_CONTEXT(N'foo')), n [int] NULL DEFAULT (CAST(SESSION_CONTEXT(N'n') AS [int])))`,
    `EXEC sp_set_session_context N'foo', N'bar'`,
    `EXEC sp_set_session_context N'n', 42`,
    `INSERT items (id) VALUES (1)`,
    `SELECT id, value, n FROM items`,
    columns('items'),
    `SELECT definition FROM sys.default_constraints WHERE parent_object_id = OBJECT_ID('items') ORDER BY parent_column_id`,
  ]],
  ['bracketed-columns', [
    `CREATE TABLE [dbo].[items] ([id] [int] NOT NULL, [name] [nvarchar](100) NULL, [amount] [decimal](18, 2) NULL, [stamp] [datetime2](7) NULL, [blob] [varbinary](max) NULL, [flag] [bit] NOT NULL, [uid] [uniqueidentifier] NULL)`,
    `ALTER TABLE [dbo].[items] ADD [extra] [varchar](10) NULL, [more] [nchar](3) NULL`,
    `ALTER TABLE [dbo].[items] ALTER COLUMN [extra] [nvarchar](20) NULL`,
    `INSERT items (id, name, amount, stamp, blob, flag, uid, extra, more) VALUES (1, N'n', 1.5, '2020-01-02', 0x01, 1, '6F9619FF-8B86-D011-B42D-00C04FC964FF', N'e', N'm')`,
    `SELECT * FROM items`,
    columns('items'),
  ]],
  ['declare', [
    `DECLARE @x [nvarchar](10) = N'abc', @y [int] = 5, @z [decimal](5,2) = 1.239; SELECT @x AS x, @y AS y, @z AS z`,
    `DECLARE @t TABLE ([a] [int] NOT NULL, [b] [nvarchar](5)); INSERT @t VALUES (1, N'x'); SELECT * FROM @t`,
    `DECLARE @x [notatype]`,
  ]],
  ['nested', [
    `IF 1 = 1 BEGIN SELECT CONVERT([nvarchar](5), 123) AS a END`,
    `SELECT (SELECT CAST(x AS [nvarchar](3)) FROM (VALUES (12345)) v(x)) AS a, CASE WHEN 1=1 THEN CONVERT([varchar](2), 'abc') END AS b`,
    `EXEC sp_executesql N'SELECT CONVERT([nvarchar](4), @p) AS a', N'@p [int]', @p = 42`,
  ]],
  ['max-convert', [
    `SELECT CONVERT(nvarchar(max), N'x') AS a, TRY_CONVERT(nvarchar(max), 1) AS b, CONVERT(varbinary(max), 0x0102) AS c, CONVERT([nvarchar](max), JSON_VALUE(N'{"a":"\u00fc"}', N'$.a')) AS d, TRY_CONVERT([varchar](max), 12.5) AS e`,
  ]],
  ['sysname', [
    `DECLARE @x sysname = N'a', @y [sysname] = N'b'; SELECT @x AS a, @y AS b`,
    `SELECT CAST(N'x' AS sysname) AS a, CONVERT([sysname], N'y') AS b, TRY_CONVERT(sysname, 1) AS c`,
    `EXEC sp_executesql N'SELECT @p AS a', N'@p sysname', @p = N'z'`,
    `CREATE TABLE names (id int NOT NULL, a sysname NULL, b [sysname] NULL)`,
  ]],
  ['modules', [
    `CREATE PROCEDURE dbo.p @a [int], @b [nvarchar](5) = N'x', @c sysname = N's' AS SELECT CONVERT([nvarchar](10), @a) + @b + @c AS r`,
    `EXEC dbo.p 7`,
    `CREATE FUNCTION dbo.f (@a [int]) RETURNS [nvarchar](10) AS BEGIN RETURN CONVERT([nvarchar](10), @a * 2) END`,
    `SELECT dbo.f(21) AS r`,
    `CREATE FUNCTION dbo.g (@a [int]) RETURNS TABLE AS RETURN SELECT CAST(@a AS [bigint]) AS v`,
    `SELECT v FROM dbo.g(3)`,
    `CREATE VIEW dbo.v AS SELECT CAST(1 AS [bigint]) AS c, CONVERT([nvarchar](5), N'abc') AS d`,
    `SELECT c, d FROM dbo.v`,
  ]],
  ['batch-boundaries', [
    `SELECT 1 AS a SELECT CAST(2 AS [int]) AS b SELECT 3 AS c`,
    `SELECT CAST(1 AS [int]) AS a; SELECT [int] FROM (SELECT 4 AS [int]) t`,
    `SELECT CAST(1 AS [int] AS a`,
  ]],
]

// Captured from Microsoft SQL Server 2025 (RTM-CU7) (KB5096981) - 17.0.4065.4 (X64).
const reference = {"image":"mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a","version":"Microsoft SQL Server 2025 (RTM-CU7) (KB5096981) - 17.0.4065.4 (X64) ","cases":{"report-convert":[{"sets":[{"columns":[["v","NVarChar",400,null,null]],"rows":[["foo"]]}],"errors":[]},{"sets":[{"columns":[["v","NVarChar",400,null,null]],"rows":[["foo"]]}],"errors":[]}],"system-types":[{"sets":[{"columns":[["a","IntN",4,null,null],["b","IntN",4,null,null],["c","DecimalN",17,10,2],["d","DateTime2",null,null,3],["e","VarBinary",65535,null,null],["f","NVarChar",256,null,null],["g","IntN",4,null,null],["h","IntN",8,null,null],["i","IntN",2,null,null],["j","IntN",1,null,null],["k","BitN",1,null,null]],"rows":[[1,12,1.5,{"kind":"date","value":"2020-01-02T03:04:05.123Z","nanosecondsDelta":0},{"kind":"binary","value":"0102"},"x",null,"1",1,1,true]]}],"errors":[]},{"sets":[{"columns":[["a","Char",3,null,null],["b","VarChar",65535,null,null],["c","NChar",4,null,null],["e","NumericN",17,5,1],["f","Date",null,null,null],["g","Time",null,null,3],["h","DateTimeOffset",null,null,2],["i","DateTimeN",4,null,null],["j","DateTimeN",8,null,null]],"rows":[["ab ","ab","ab",1.3,{"kind":"date","value":"2020-01-02T00:00:00.000Z"},{"kind":"date","value":"1970-01-01T03:04:05.123Z","nanosecondsDelta":0},{"kind":"date","value":"2020-01-02T02:04:05.000Z","nanosecondsDelta":0},{"kind":"date","value":"2020-01-02T03:04:00.000Z"},{"kind":"date","value":"2020-01-02T03:04:05.003Z"}]]}],"errors":[]},{"sets":[{"columns":[["a","MoneyN",8,null,null],["b","MoneyN",4,null,null],["c","FloatN",8,null,null],["d","FloatN",4,null,null],["e","UniqueIdentifier",16,null,null],["f","NVarChar",60,null,null],["g","VarChar",30,null,null],["h","FloatN",4,null,null]],"rows":[[1.5,1.5,1.5,1.5,"6F9619FF-8B86-D011-B42D-00C04FC964FF","ab","ab",1.5]]}],"errors":[]},{"sets":[{"columns":[["a","Binary",2,null,null]],"rows":[[{"kind":"binary","value":"0100"}]]}],"errors":[]},{"sets":[{"columns":[["a","DecimalN",17,18,0]],"rows":[[12]]}],"errors":[]},{"sets":[{"columns":[["a","IntN",4,null,null],["b","NVarChar",10,null,null],["c","NVarChar",65535,null,null],["d","VarChar",10,null,null]],"rows":[[1,"12","x","20200102"]]}],"errors":[]}],"other-delimiters":[{"sets":[{"columns":[["a","IntN",4,null,null]],"rows":[[1]]}],"errors":[]},{"sets":[{"columns":[["a","NVarChar",10,null,null]],"rows":[["12"]]}],"errors":[]},{"sets":[],"errors":[[243,1,16,"Type dbo.int is not a defined system type."]]},{"sets":[{"columns":[["a","IntN",4,null,null]],"rows":[[1]]}],"errors":[]},{"sets":[{"columns":[["a","IntN",4,null,null]],"rows":[[1]]}],"errors":[]}],"non-system":[{"sets":[],"errors":[[243,1,16,"Type notatype is not a defined system type."]]},{"sets":[],"errors":[[243,1,16,"Type notatype is not a defined system type."]]},{"sets":[{"columns":[["a","IntN",4,null,null]],"rows":[[1]]}],"errors":[]},{"sets":[],"errors":[[243,1,16,"Type notatype is not a defined system type."]]},{"sets":[],"errors":[[291,1,16,"CAST or CONVERT: invalid attributes specified for type 'int'"]]},{"sets":[],"errors":[[291,1,16,"CAST or CONVERT: invalid attributes specified for type 'int'"]]}],"names-not-types":[{"sets":[{"columns":[["int","Int",null,null,null]],"rows":[[1]]}],"errors":[]},{"sets":[{"columns":[["int","Int",null,null,null],["nvarchar","Int",null,null,null]],"rows":[[1,2]]}],"errors":[]},{"sets":[{"columns":[["int","IntN",4,null,null]],"rows":[[1]]}],"errors":[]}],"computed-persisted":[{"sets":[],"errors":[]},{"sets":[],"errors":[]},{"sets":[{"columns":[["id","Int",null,null,null],["value","NVarChar",400,null,null]],"rows":[[1,"foo"],[2,null]]}],"errors":[]},{"sets":[{"columns":[["name","NVarChar",256,null,null],["type","NVarChar",256,null,null],["max_length","SmallInt",null,null,null],["precision","TinyInt",null,null,null],["scale","TinyInt",null,null,null],["is_nullable","BitN",1,null,null],["is_computed","BitN",1,null,null]],"rows":[["id","int",4,10,0,false,false],["body","nvarchar",-1,0,0,false,false],["value","nvarchar",400,0,0,true,true]]}],"errors":[]},{"sets":[{"columns":[["definition","NVarChar",65535,null,null]],"rows":[["(CONVERT([nvarchar](200),json_value([body],N'$.a')))"]]}],"errors":[]}],"session-default":[{"sets":[],"errors":[]},{"sets":[],"errors":[]},{"sets":[],"errors":[]},{"sets":[],"errors":[]},{"sets":[{"columns":[["id","Int",null,null,null],["value","NVarChar",900,null,null],["n","IntN",4,null,null]],"rows":[[1,"bar",42]]}],"errors":[]},{"sets":[{"columns":[["name","NVarChar",256,null,null],["type","NVarChar",256,null,null],["max_length","SmallInt",null,null,null],["precision","TinyInt",null,null,null],["scale","TinyInt",null,null,null],["is_nullable","BitN",1,null,null],["is_computed","BitN",1,null,null]],"rows":[["id","int",4,10,0,false,false],["value","nvarchar",900,0,0,true,false],["n","int",4,10,0,true,false]]}],"errors":[]},{"sets":[{"columns":[["definition","NVarChar",65535,null,null]],"rows":[["(CONVERT([nvarchar](450),session_context(N'foo')))"],["(CONVERT([int],session_context(N'n')))"]]}],"errors":[]}],"bracketed-columns":[{"sets":[],"errors":[]},{"sets":[],"errors":[]},{"sets":[],"errors":[]},{"sets":[],"errors":[]},{"sets":[{"columns":[["id","Int",null,null,null],["name","NVarChar",200,null,null],["amount","DecimalN",17,18,2],["stamp","DateTime2",null,null,7],["blob","VarBinary",65535,null,null],["flag","Bit",null,null,null],["uid","UniqueIdentifier",16,null,null],["extra","NVarChar",40,null,null],["more","NChar",6,null,null]],"rows":[[1,"n",1.5,{"kind":"date","value":"2020-01-02T00:00:00.000Z","nanosecondsDelta":0},{"kind":"binary","value":"01"},true,"6F9619FF-8B86-D011-B42D-00C04FC964FF","e","m  "]]}],"errors":[]},{"sets":[{"columns":[["name","NVarChar",256,null,null],["type","NVarChar",256,null,null],["max_length","SmallInt",null,null,null],["precision","TinyInt",null,null,null],["scale","TinyInt",null,null,null],["is_nullable","BitN",1,null,null],["is_computed","BitN",1,null,null]],"rows":[["id","int",4,10,0,false,false],["name","nvarchar",200,0,0,true,false],["amount","decimal",9,18,2,true,false],["stamp","datetime2",8,27,7,true,false],["blob","varbinary",-1,0,0,true,false],["flag","bit",1,1,0,false,false],["uid","uniqueidentifier",16,0,0,true,false],["extra","nvarchar",40,0,0,true,false],["more","nchar",6,0,0,true,false]]}],"errors":[]}],"declare":[{"sets":[{"columns":[["x","NVarChar",20,null,null],["y","IntN",4,null,null],["z","DecimalN",17,5,2]],"rows":[["abc",5,1.24]]}],"errors":[]},{"sets":[{"columns":[["a","Int",null,null,null],["b","NVarChar",10,null,null]],"rows":[[1,"x"]]}],"errors":[]},{"sets":[],"errors":[[2715,3,16,"Column, parameter, or variable #1: Cannot find data type notatype."]]}],"nested":[{"sets":[{"columns":[["a","NVarChar",10,null,null]],"rows":[["123"]]}],"errors":[]},{"sets":[{"columns":[["a","NVarChar",6,null,null],["b","VarChar",2,null,null]],"rows":[]}],"errors":[[8115,2,16,"Arithmetic overflow error converting expression to data type nvarchar."]]},{"sets":[{"columns":[["a","NVarChar",8,null,null]],"rows":[["42"]]}],"errors":[]}],"max-convert":[{"sets":[{"columns":[["a","NVarChar",65535,null,null],["b","NVarChar",65535,null,null],["c","VarBinary",65535,null,null],["d","NVarChar",65535,null,null],["e","VarChar",65535,null,null]],"rows":[["x","1",{"kind":"binary","value":"0102"},"ü","12.5"]]}],"errors":[]}],"sysname":[{"sets":[{"columns":[["a","NVarChar",256,null,null],["b","NVarChar",256,null,null]],"rows":[["a","b"]]}],"errors":[]},{"sets":[{"columns":[["a","NVarChar",256,null,null],["b","NVarChar",256,null,null],["c","NVarChar",256,null,null]],"rows":[["x","y","1"]]}],"errors":[]},{"sets":[{"columns":[["a","NVarChar",256,null,null]],"rows":[["z"]]}],"errors":[]},{"sets":[],"errors":[]}],"modules":[{"sets":[],"errors":[]},{"sets":[{"columns":[["r","NVarChar",286,null,null]],"rows":[["7xs"]]}],"errors":[]},{"sets":[],"errors":[]},{"sets":[{"columns":[["r","NVarChar",20,null,null]],"rows":[["42"]]}],"errors":[]},{"sets":[],"errors":[]},{"sets":[{"columns":[["v","IntN",8,null,null]],"rows":[["3"]]}],"errors":[]},{"sets":[],"errors":[]},{"sets":[{"columns":[["c","IntN",8,null,null],["d","NVarChar",10,null,null]],"rows":[["1","abc"]]}],"errors":[]}],"batch-boundaries":[{"sets":[{"columns":[["a","Int",null,null,null]],"rows":[[1]]},{"columns":[["b","IntN",4,null,null]],"rows":[[2]]},{"columns":[["c","Int",null,null,null]],"rows":[[3]]}],"errors":[]},{"sets":[{"columns":[["a","IntN",4,null,null]],"rows":[[1]]},{"columns":[["int","Int",null,null,null]],"rows":[[4]]}],"errors":[]},{"sets":[],"errors":[[156,1,15,"Incorrect syntax near the keyword 'AS'."]]}]}}

let databases = 0
for (const [id, statements] of cases) {
  test(`${id} matches SQL Server`, async t => {
    const connection = await start(t)
    const name = `bracket_types_${process.pid}_${databases++}`
    for (const sql of [`CREATE DATABASE ${name}`, `USE ${name}`]) same((await capture(connection, sql)).errors, [], sql)
    for (const [index, sql] of statements.entries()) {
      const label = `${id}#${index}`
      const local = view(canonical(await capture(connection, sql)))
      const expected = reference.cases[id][index]
      if (known[label]) known[label](local, expected, `${label}: ${sql}`)
      else same(local, expected, `${label}: ${sql}`)
    }
  })
}

