#!/usr/bin/env node
// SQL Server evidence for user-defined scalar, inline and multi-statement
// table-valued functions (issue #720, docs/gaps-functions.md).
//
// Every case runs its batches in order in a fresh database. A result keeps
// column descriptors, rows, errors and completion tokens as tedious reports
// them. tests/compat/functions.test.mjs replays the same cases against
// msduck and compares them with the retained fixture.
//
// usage: capture-gaps-functions.mjs --write-fixture
//   Starts the pinned reference container (or MSSQL_REFERENCE_IMAGE), or
//   uses an existing server when MSSQL_REFERENCE_HOST, _USER and _PASSWORD
//   are set, and writes reference/gaps-functions.json (never overwriting).
//   --output file.json writes a capture elsewhere for inspection.
import { writeFile } from 'node:fs/promises'
import { canonical, capture } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, isolatedReference, referenceConfig, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

export const fixture = new URL('../reference/gaps-functions.json', import.meta.url)

const rows = `CREATE TABLE dbo.t(id int, v varchar(10));
INSERT dbo.t VALUES (1, 'a'), (2, 'b'), (3, NULL);`

// Each case: a name and its batches, run in order in one fresh database.
export const cases = [
  { name: 'scalar repro', batches: [
    'CREATE FUNCTION dbo.foo(@x int) RETURNS int AS BEGIN RETURN @x+1; END;',
    "SELECT dbo.foo(1) AS a, dbo.foo(NULL) AS b, dbo.foo('5') AS c",
    rows,
    'SELECT id, dbo.foo(id) AS n FROM dbo.t WHERE dbo.foo(id) > 2 ORDER BY dbo.foo(id) DESC',
    "SELECT name, type, type_desc FROM sys.objects WHERE name = 'foo'",
  ] },
  { name: 'parameterless scalar', batches: [
    "CREATE FUNCTION dbo.p0() RETURNS varchar(10) AS BEGIN RETURN 'hi' END",
    'SELECT dbo.p0() AS v',
  ] },
  { name: 'options', batches: [
    'CREATE FUNCTION dbo.caller(@x int) RETURNS int WITH EXECUTE AS CALLER AS BEGIN DECLARE @y int = @x * 2; RETURN @y END',
    'CREATE FUNCTION dbo.nn(@x int) RETURNS int WITH RETURNS NULL ON NULL INPUT AS BEGIN RETURN ISNULL(@x, 42) END',
    'CREATE FUNCTION dbo.cn(@x int) RETURNS int WITH SCHEMABINDING, CALLED ON NULL INPUT AS BEGIN RETURN ISNULL(@x, 42) END',
    'SELECT dbo.caller(4) AS a, dbo.nn(NULL) AS b, dbo.nn(1) AS c, dbo.cn(NULL) AS d',
  ] },
  { name: 'control flow and assignments', batches: [
    rows,
    `CREATE FUNCTION dbo.classify(@x int) RETURNS varchar(10) AS
BEGIN
  DECLARE @r varchar(10) = 'small'
  IF @x IS NULL RETURN 'none'
  IF @x > 1
  BEGIN
    SET @r = 'mid'
    IF @x > 2 SET @r = 'big'
  END
  ELSE SET @r += '!'
  RETURN @r
END`,
    'SELECT id, dbo.classify(id) AS c FROM dbo.t ORDER BY id',
    'SELECT dbo.classify(NULL) AS c',
    'CREATE FUNCTION dbo.above(@x int) RETURNS int AS BEGIN DECLARE @r int; SELECT @r = COUNT(*) FROM dbo.t WHERE id > @x; RETURN @r END',
    'CREATE FUNCTION dbo.last_id(@x int) RETURNS int AS BEGIN DECLARE @r int = -1; SELECT @r = id FROM dbo.t WHERE id > @x ORDER BY id; RETURN @r END',
    'SELECT id, dbo.above(id) AS a, dbo.last_id(id - 1) AS l, dbo.last_id(5) AS none FROM dbo.t ORDER BY id',
    'DECLARE @r int = dbo.above(0); SET @r = @r + dbo.above(1); IF dbo.above(@r) = 0 SELECT @r AS r',
  ] },
  { name: 'nested calls', batches: [
    rows,
    'CREATE FUNCTION dbo.inc(@x int) RETURNS int AS BEGIN RETURN @x + 1 END',
    'CREATE FUNCTION dbo.twice(@x int) RETURNS int AS BEGIN RETURN dbo.inc(dbo.inc(@x)) END',
    'CREATE FUNCTION dbo.cnt(@x int) RETURNS int AS BEGIN RETURN (SELECT COUNT(*) FROM dbo.t WHERE id <= @x) END',
    'CREATE TABLE dbo.u(id int, w int); INSERT dbo.u VALUES (1, 10), (2, 20)',
    'SELECT u.id, dbo.twice(u.id) AS tw, dbo.cnt(w) AS by_w, dbo.cnt(id) AS by_id FROM dbo.u ORDER BY u.id',
    'SELECT dbo.inc(SUM(id)) AS s FROM dbo.t',
  ] },
  { name: 'inline table-valued', batches: [
    rows,
    'CREATE FUNCTION dbo.rows_for(@id int) RETURNS TABLE AS RETURN SELECT id, v FROM dbo.t WHERE id <= @id',
    'CREATE FUNCTION dbo.it(@n int) RETURNS TABLE AS RETURN (SELECT @n AS n, @n * 2 AS d)',
    'SELECT * FROM dbo.it(3)',
    'SELECT it.n, it.d FROM it(4)',
    'SELECT t.id, r.id AS rid FROM dbo.t CROSS APPLY dbo.rows_for(t.id) r ORDER BY 1, 2',
    'SELECT t.id, r.id AS rid FROM dbo.t OUTER APPLY dbo.rows_for(t.id - 1) r ORDER BY 1, 2',
    'SELECT t.id, r.id AS rid FROM dbo.t CROSS APPLY dbo.rows_for(id) r ORDER BY 1, 2',
    'SELECT t.id, t.v, r.v AS rv FROM dbo.t JOIN dbo.rows_for(2) r ON r.id = t.id ORDER BY t.id',
    'SELECT a.n, b.d FROM dbo.it(1) a CROSS JOIN dbo.it(2) b',
    'SELECT id FROM dbo.t WHERE id IN (SELECT n FROM dbo.it(2))',
    'DECLARE @v int; SET @v = (SELECT d FROM dbo.it(21)); SELECT @v AS v',
    'CREATE FUNCTION dbo.top2(@m int) RETURNS TABLE AS RETURN WITH q AS (SELECT id FROM dbo.t WHERE id <= @m) SELECT TOP 2 id FROM q ORDER BY id DESC',
    'CREATE FUNCTION dbo.pairs(@m int) RETURNS TABLE AS RETURN SELECT a.id, b.n FROM dbo.top2(@m) a CROSS APPLY dbo.it(a.id) b',
    'SELECT * FROM dbo.pairs(3) ORDER BY id',
    "SELECT name, type, type_desc FROM sys.objects WHERE name IN ('rows_for', 'it') ORDER BY name",
  ] },
  { name: 'multi-statement table-valued', batches: [
    "CREATE FUNCTION dbo.mt(@n int) RETURNS @t TABLE(i int NOT NULL, s varchar(5) DEFAULT 'd') AS BEGIN INSERT @t VALUES (@n, 'a'); IF @n > 5 INSERT INTO @t(i) SELECT @n + 1; RETURN; END",
    'SELECT * FROM dbo.mt(10)',
    'SELECT * FROM dbo.mt(1)',
    "SELECT name, type, type_desc FROM sys.objects WHERE name = 'mt'",
    `CREATE FUNCTION dbo.chars(@s varchar(100)) RETURNS @r TABLE (pos int, ch char(1)) AS
BEGIN
  DECLARE @p int = 1
  WHILE @p <= LEN(@s)
  BEGIN
    IF SUBSTRING(@s, @p, 1) = ',' BEGIN SET @p += 1; CONTINUE END
    INSERT @r VALUES (@p, SUBSTRING(@s, @p, 1))
    SET @p += 1
  END
  RETURN
END`,
    "SELECT * FROM dbo.chars('a,b,,c')",
    "DECLARE @x varchar(20) = 'x;y'; SELECT pos, ch FROM dbo.chars(@x) ORDER BY pos DESC",
  ] },
  { name: 'loops and recursion', batches: [
    'CREATE FUNCTION dbo.total(@x int) RETURNS int AS BEGIN DECLARE @i int = 0, @s int = 0; WHILE @i < @x BEGIN SET @i += 1; IF @i > 4 BREAK; SET @s = @s + @i END; RETURN @s END',
    'SELECT dbo.total(3) AS a, dbo.total(9) AS b',
    'CREATE FUNCTION dbo.fact(@n int) RETURNS bigint AS BEGIN IF @n <= 1 RETURN 1; RETURN @n * dbo.fact(@n - 1) END',
    'SELECT dbo.fact(10) AS f',
    'SELECT dbo.fact(33) AS f',
  ] },
  { name: 'per-row loops and recursion', batches: [
    rows,
    'CREATE FUNCTION dbo.total(@x int) RETURNS int AS BEGIN DECLARE @i int = 0, @s int = 0; WHILE @i < @x BEGIN SET @i += 1; SET @s = @s + @i END; RETURN @s END',
    'CREATE FUNCTION dbo.fact(@n int) RETURNS bigint AS BEGIN IF @n <= 1 RETURN 1; RETURN @n * dbo.fact(@n - 1) END',
    'SELECT id, dbo.total(id) AS s FROM dbo.t ORDER BY id',
    'SELECT id, dbo.fact(id) AS f FROM dbo.t ORDER BY id',
  ] },
  { name: 'views', batches: [
    rows,
    'CREATE FUNCTION dbo.inc(@x int) RETURNS int AS BEGIN RETURN @x + 1 END',
    'CREATE VIEW dbo.vw AS SELECT id, dbo.inc(id) AS n FROM dbo.t',
    'SELECT id, n FROM dbo.vw ORDER BY id',
    'ALTER FUNCTION dbo.inc(@x int) RETURNS int AS BEGIN RETURN @x + 100 END',
    'SELECT id, n FROM dbo.vw ORDER BY id',
  ] },
  { name: 'definition statements', batches: [
    'CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN @x END',
    'CREATE FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN @x END',
    'ALTER FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN @x + 100; END',
    'SELECT dbo.f(1) AS a',
    'CREATE OR ALTER FUNCTION dbo.f(@x int) RETURNS int AS BEGIN RETURN @x + 2; END',
    'CREATE OR ALTER FUNCTION dbo.g(@x int) RETURNS int AS BEGIN RETURN @x + 3; END',
    'SELECT dbo.f(1) AS a, dbo.g(1) AS b',
    'ALTER FUNCTION dbo.nope(@x int) RETURNS int AS BEGIN RETURN @x END',
    'ALTER FUNCTION dbo.f(@x int) RETURNS TABLE AS RETURN SELECT 1 AS a',
    'CREATE FUNCTION dbo.itf() RETURNS TABLE AS RETURN SELECT 1 AS a',
    'ALTER FUNCTION dbo.itf() RETURNS @t TABLE(a int) AS BEGIN RETURN END',
    'CREATE TABLE dbo.tab(x int)',
    'CREATE FUNCTION dbo.tab() RETURNS int AS BEGIN RETURN 1 END',
    'CREATE OR ALTER FUNCTION dbo.tab() RETURNS int AS BEGIN RETURN 1 END',
    'DROP FUNCTION dbo.tab',
    'DROP FUNCTION dbo.nope',
    'DROP FUNCTION IF EXISTS dbo.nope, dbo.g',
    'SELECT dbo.g(1)',
    'DROP FUNCTION dbo.f, dbo.nope',
    "SELECT CASE WHEN OBJECT_ID('dbo.f') IS NULL THEN 0 ELSE 1 END AS f, CASE WHEN OBJECT_ID('dbo.itf') IS NULL THEN 0 ELSE 1 END AS itf",
    'SELECT 1 AS one\nCREATE FUNCTION dbo.late() RETURNS int AS BEGIN RETURN 1 END',
    'CREATE FUNCTION nosuch.f() RETURNS int AS BEGIN RETURN 1 END',
  ] },
  { name: 'compile errors', batches: [
    'CREATE FUNCTION dbo.e1(@x int) RETURNS int AS BEGIN RETURN @y END',
    'CREATE FUNCTION dbo.e2(@x int) RETURNS int AS BEGIN SET @x = 1 END',
    'CREATE FUNCTION dbo.e3(@x int) RETURNS int AS BEGIN SELECT 1; RETURN 1 END',
    "CREATE FUNCTION dbo.e4(@x int) RETURNS int AS BEGIN PRINT 'x'; RETURN 1 END",
    'CREATE FUNCTION dbo.e5(@x int) RETURNS int AS BEGIN IF @x > 0 RETURN 1 ELSE RETURN 2 END',
    'CREATE FUNCTION dbo.e6(@x int) RETURNS int AS BEGIN RETURN NEWID() END',
    'CREATE FUNCTION dbo.e7(@x int) RETURNS @t TABLE(a int) AS BEGIN RETURN 1 END',
    'CREATE FUNCTION dbo.e8(@x int) RETURNS TABLE AS BEGIN RETURN SELECT 1 a END',
    'CREATE FUNCTION dbo.e9(@x int) RETURNS int AS RETURN @x',
    "CREATE FUNCTION dbo.e10(@x int) RETURNS int AS BEGIN RAISERROR('x', 16, 1); RETURN 1 END",
    'CREATE FUNCTION dbo.e11(@x int) RETURNS int AS BEGIN BEGIN TRAN; RETURN 1 END',
    'CREATE FUNCTION dbo.e13(@x int) RETURNS int AS BEGIN RETURN END',
    'CREATE TABLE dbo.t(id int)',
    'CREATE FUNCTION dbo.e12(@x int) RETURNS int AS BEGIN INSERT INTO dbo.t VALUES (1); RETURN 1 END',
    "SELECT COUNT(*) AS n FROM sys.objects WHERE name LIKE 'e%' AND type = 'FN'",
  ] },
  { name: 'call errors', batches: [
    'CREATE FUNCTION dbo.f(@a int, @b int = 7) RETURNS int AS BEGIN RETURN @a + @b END',
    'CREATE FUNCTION dbo.it(@n int) RETURNS TABLE AS RETURN SELECT @n AS n',
    'SELECT dbo.f(1, DEFAULT) AS a, dbo.f(1, 2) AS b',
    'SELECT dbo.f(1)',
    'SELECT dbo.f(1, 2, 3)',
    'SELECT dbo.nofn(1)',
    'SELECT * FROM dbo.nofn(1)',
    'SELECT * FROM dbo.it(DEFAULT)',
    'CREATE FUNCTION dbo.dd(@a int, @b int) RETURNS int AS BEGIN RETURN ISNULL(@b, -1) END',
    'SELECT dbo.dd(1, DEFAULT) AS d',
    'CREATE FUNCTION dbo.div(@x int) RETURNS int AS BEGIN RETURN @x / 0 END',
    'SELECT dbo.div(1)',
    'CREATE FUNCTION dbo.inc(@x int) RETURNS int AS BEGIN RETURN @x + 1 END',
    'SELECT dbo.inc(2147483647)',
  ] },
  { name: 'dependencies', batches: [
    'CREATE FUNCTION dbo.inc(@x int) RETURNS int AS BEGIN RETURN @x + 1 END',
    'CREATE TABLE dbo.c(a int, b AS dbo.inc(a))',
    'INSERT dbo.c(a) VALUES (5); SELECT a, b FROM dbo.c',
    'ALTER FUNCTION dbo.inc(@x int) RETURNS int AS BEGIN RETURN @x + 2 END',
    'DROP FUNCTION dbo.inc',
    'CREATE FUNCTION dbo.inc3(@x int) RETURNS int AS BEGIN RETURN @x + 1 END',
    'CREATE TABLE dbo.e(a int CONSTRAINT DF_e_a DEFAULT dbo.inc3(1))',
    'INSERT dbo.e DEFAULT VALUES; SELECT a FROM dbo.e',
    'DROP FUNCTION dbo.inc3',
    'CREATE TABLE dbo.g(x int)',
    'CREATE FUNCTION dbo.sb(@x int) RETURNS int WITH SCHEMABINDING AS BEGIN RETURN (SELECT COUNT(*) FROM dbo.g WHERE x = @x) END',
    'CREATE FUNCTION dbo.sb2(@x int) RETURNS int WITH SCHEMABINDING AS BEGIN RETURN (SELECT COUNT(*) FROM g WHERE x = @x) END',
    'DROP TABLE dbo.g',
    'CREATE FUNCTION dbo.sb3(@x int) RETURNS int WITH SCHEMABINDING AS BEGIN RETURN dbo.sb(@x) END',
    'DROP FUNCTION dbo.sb',
    'ALTER FUNCTION dbo.sb(@x int) RETURNS int WITH SCHEMABINDING AS BEGIN RETURN 0 END',
    'CREATE FUNCTION dbo.sb4(@x int) RETURNS int WITH SCHEMABINDING AS BEGIN RETURN dbo.inc(@x) END',
    'DROP FUNCTION dbo.sb3',
    'DROP FUNCTION dbo.sb',
    'DROP TABLE dbo.g',
    'DROP TABLE dbo.c',
    'DROP FUNCTION dbo.inc',
  ] },
  { name: 'values through statement execution', batches: [
    'CREATE FUNCTION dbo.idn(@a nvarchar(10)) RETURNS nvarchar(10) AS BEGIN DECLARE @k int = 0; WHILE @k < 1 SET @k += 1; RETURN @a END',
    'CREATE FUNCTION dbo.idd(@a decimal(5,2)) RETURNS decimal(5,2) AS BEGIN DECLARE @k int = 0; WHILE @k < 1 SET @k += 1; RETURN @a END',
    'CREATE FUNCTION dbo.iddt(@a datetime) RETURNS datetime AS BEGIN DECLARE @k int = 0; WHILE @k < 1 SET @k += 1; RETURN @a END',
    'CREATE FUNCTION dbo.idf(@a float) RETURNS float AS BEGIN DECLARE @k int = 0; WHILE @k < 1 SET @k += 1; RETURN @a END',
    'CREATE FUNCTION dbo.iddate(@a date) RETURNS date AS BEGIN DECLARE @k int = 0; WHILE @k < 1 SET @k += 1; RETURN @a END',
    'CREATE FUNCTION dbo.iddto(@a datetimeoffset(7)) RETURNS datetimeoffset(7) AS BEGIN DECLARE @k int = 0; WHILE @k < 1 SET @k += 1; RETURN @a END',
    'CREATE FUNCTION dbo.idg(@a uniqueidentifier) RETURNS uniqueidentifier AS BEGIN DECLARE @k int = 0; WHILE @k < 1 SET @k += 1; RETURN @a END',
    'CREATE FUNCTION dbo.idt(@a time(2)) RETURNS time(2) AS BEGIN DECLARE @k int = 0; WHILE @k < 1 SET @k += 1; RETURN @a END',
    'CREATE FUNCTION dbo.idb(@a varbinary(4)) RETURNS varbinary(4) AS BEGIN DECLARE @k int = 0; WHILE @k < 1 SET @k += 1; RETURN @a END',
    'CREATE FUNCTION dbo.idm(@a money) RETURNS money AS BEGIN DECLARE @k int = 0; WHILE @k < 1 SET @k += 1; RETURN @a END',
    'CREATE FUNCTION dbo.idc(@a char(3)) RETURNS char(5) AS BEGIN DECLARE @k int = 0; WHILE @k < 1 SET @k += 1; RETURN @a END',
    "SELECT dbo.idn(N'ü€''') AS a, dbo.idd(1.5) AS b, dbo.iddt('2020-01-02 03:04:05.007') AS c, dbo.idf(0.1) AS d, dbo.iddate('2021-03-04') AS e",
    "SELECT dbo.iddto('2021-03-04 05:06:07.1234567 -05:30') AS f, dbo.idg('6F9619FF-8B86-D011-B42D-00C04FC964FF') AS g, dbo.idt('01:02:03.456') AS h, dbo.idb(0x0A0B) AS i, dbo.idm(12.3456) AS j, dbo.idc('ab') AS k, dbo.idn(NULL) AS l",
  ] },
  { name: 'defaults and computed columns', batches: [
    'CREATE FUNCTION dbo.inc(@x int) RETURNS int AS BEGIN RETURN @x + 1 END',
    'CREATE TABLE dbo.d(a int, b int DEFAULT dbo.inc(41), c AS dbo.inc(a))',
    'INSERT dbo.d(a) VALUES (1); UPDATE dbo.d SET a = dbo.inc(a); SELECT a, b, c FROM dbo.d',
    'CREATE FUNCTION dbo.it(@n int) RETURNS TABLE AS RETURN SELECT @n AS n UNION ALL SELECT @n + 1',
    'INSERT dbo.d(a) SELECT n FROM dbo.it(10); DELETE dbo.d WHERE a IN (SELECT n FROM dbo.it(11)); SELECT a, b, c FROM dbo.d ORDER BY a',
  ] },
]

/// One case's results, each batch in a fresh database.
export async function runCase(connection, entry) {
  const results = []
  for (const sql of entry.batches) results.push(canonical(await capture(connection, sql)))
  return results
}

async function main() {
  const args = process.argv.slice(2)
  const output = args[args.indexOf('--output') + 1]
  if (!args.includes('--write-fixture') && !(args.includes('--output') && output)) {
    throw new Error('usage: capture-gaps-functions.mjs --write-fixture | --output file.json')
  }
  if (args.includes('--write-fixture')) await refuseExistingFixture(fixture)
  const work = async (config, container) => {
    const admin = await connect(config)
    const version = (await capture(admin, 'SELECT @@VERSION AS version')).sets[0].rows[0][0]
    admin.close()
    const results = []
    for (const entry of cases) {
      results.push({ name: entry.name, results: await isolatedReference(config, connection => runCase(connection, entry)) })
    }
    return { image: container?.image ?? null, version, cases: results }
  }
  const capture_ = process.env.MSSQL_REFERENCE_HOST
    ? await work(referenceConfig())
    : await withReferenceContainer(work)
  if (args.includes('--write-fixture')) await writeNewFixture(fixture, capture_)
  else await writeFile(output, JSON.stringify(capture_, null, 1) + '\n')
}

if (import.meta.url === `file://${process.argv[1]}`) await main()
