#!/usr/bin/env node
// Retain SQL Server PIVOT and UNPIVOT rows, result descriptors, diagnostics and
// completion tokens. Every observation that returns more than one row orders
// the rows explicitly; result sets are never treated as ordered otherwise.
//
// Conventions follow docs/reference-captures.md (PR #312). The request and
// prepared helpers below replicate that PR's scripts/lib/reference.mjs rules
// because they are not on main yet:
// - return status is recorded only when a RETURNSTATUS arrives during that
//   request or prepared step; the value tedious carries on the connection is
//   cleared before each request/step;
// - sp_prepare completes through the Request's 'prepared'/'error' events;
// - only server errors raised during an execution are attributed to it, and
//   tedious' sticky request.error is cleared before each prepared phase;
// - rows and messages are bounded per result, overflow is counted.
// Plan-dependent statements have unique text; constraints are named
// explicitly; no clock reads and no server-side WHILE loops.
process.env.TZ = 'UTC'
if (new Date(0).getTimezoneOffset() !== 0) throw new Error('could not switch the process time zone to UTC')

const { mkdir, writeFile, readFile } = await import('node:fs/promises')
const { resolve } = await import('node:path')
const { Request, TYPES } = await import('tedious')
const { canonical } = await import('./lib/compatibility.mjs')
const { withReferenceContainer } = await import('./lib/reference-container.mjs')
const { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } = await import('./lib/reference.mjs')

const fixture = new URL('../reference/pivot-unpivot.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/pivot-unpivot-reference-v1/capture.json')

const LIMITS = Object.freeze({ rows: 2000, messages: 200 })

// ---------------------------------------------------------------------------
// Source data. Constraint names are explicit so diagnostics do not embed
// per-database generated names.
const setup = [
  ['create sales', `CREATE TABLE dbo.pv_sales(
    id INT NOT NULL CONSTRAINT pk_pv_sales PRIMARY KEY,
    region VARCHAR(10) NULL, yr INT NOT NULL, qtr CHAR(2) NULL, amount INT NULL,
    price DECIMAL(9,2) NULL, note NVARCHAR(20) NULL, big BIGINT NULL, ratio FLOAT NULL,
    flag BIT NULL, day DATE NULL)`],
  ['insert sales', `INSERT dbo.pv_sales(id,region,yr,qtr,amount,price,note,big,ratio,flag,day) VALUES
    (1,'east',2023,'Q1',10,1.50,N'a',100,0.5,1,'2024-01-01'),
    (2,'east',2023,'Q1',20,2.25,N'b',200,1.5,0,'2024-01-01'),
    (3,'east',2023,'Q2',NULL,3.00,N'c',NULL,NULL,1,'2024-02-01'),
    (4,'west',2023,'Q1',5,NULL,NULL,50,2.0,NULL,'2024-01-01'),
    (5,'west',2024,'Q3',7,4.75,N'd',70,3.5,1,'2024-03-01'),
    (6,'west',2024,NULL,9,5.00,N'e',90,4.0,0,NULL),
    (7,NULL,2024,'Q2',11,6.10,N'f',110,5.5,1,'2024-02-01'),
    (8,'east',2024,'q1',13,7.20,N'g',130,6.0,0,'2024-01-01'),
    (9,'east',2024,'Q4',NULL,NULL,NULL,NULL,NULL,NULL,'2024-04-01')`],
  ['create region', `CREATE TABLE dbo.pv_region(code VARCHAR(10) NOT NULL CONSTRAINT pk_pv_region PRIMARY KEY, name NVARCHAR(20) NOT NULL)`],
  ['insert region', `INSERT dbo.pv_region(code,name) VALUES ('east',N'East'),('west',N'West'),('north',N'North')`],
  ['create wide', `CREATE TABLE dbo.pv_wide(
    id INT NOT NULL CONSTRAINT pk_pv_wide PRIMARY KEY, label VARCHAR(10) NULL,
    q1 INT NULL, q2 INT NULL, q3 INT NULL, s1 SMALLINT NULL,
    v5 VARCHAR(5) NULL, v10 VARCHAR(10) NULL, v5b VARCHAR(5) NULL, n5 NVARCHAR(5) NULL, c5 CHAR(5) NULL,
    d1 DECIMAL(5,2) NULL, d2 DECIMAL(5,2) NULL, d3 DECIMAL(6,2) NULL,
    v5cs VARCHAR(5) COLLATE Latin1_General_CS_AS NULL, nn5 INT NOT NULL CONSTRAINT df_pv_wide_nn5 DEFAULT 0,
    [a b] INT NULL, [x]]y] INT NULL)`],
  ['insert wide', `INSERT dbo.pv_wide(id,label,q1,q2,q3,s1,v5,v10,v5b,n5,c5,d1,d2,d3,v5cs,nn5,[a b],[x]]y]) VALUES
    (1,'one',1,2,3,4,'a','aa','b',N'n','c',1.00,2.00,3.00,'x',7,5,6),
    (2,'two',NULL,20,NULL,NULL,NULL,'bb',NULL,NULL,NULL,NULL,2.50,NULL,NULL,8,NULL,NULL),
    (3,'three',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,9,NULL,NULL),
    (4,NULL,40,41,42,43,'dd','dddd','ee',N'nn','cc',4.00,4.10,4.20,'y',10,44,45)`],
]

const src = '(SELECT region,qtr,amount FROM dbo.pv_sales) s'
const quarters = '[Q1],[Q2],[Q3],[Q4],[Q9]'
// Describe through the DMF so the rows can be ordered explicitly.
const describe = (sql, parameters = 'NULL') => `SELECT * FROM sys.dm_exec_describe_first_result_set(N'${sql.replaceAll("'", "''")}',${parameters},0) ORDER BY column_ordinal`
const columnsOf = table => `SELECT c.column_id,c.name,t.name AS type_name,c.max_length,c.precision,c.scale,c.is_nullable,c.collation_name FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('${table}') ORDER BY c.column_id`

const cases = []
const add = (name, sql) => cases.push([name, sql])

// PIVOT with each aggregate; missing value Q9, NULL keys, NULL amounts.
for (const aggregate of ['SUM', 'AVG', 'COUNT', 'COUNT_BIG', 'MIN', 'MAX', 'STDEV', 'STDEVP', 'VAR', 'VARP', 'CHECKSUM_AGG']) {
  add(`pivot ${aggregate.toLowerCase()} int`, `SELECT region,${quarters} FROM ${src} PIVOT (${aggregate}(amount) FOR qtr IN (${quarters})) p ORDER BY region`)
}
add('pivot sum decimal', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,price FROM dbo.pv_sales) s PIVOT (SUM(price) FOR qtr IN ([Q1],[Q2])) p ORDER BY region')
add('pivot avg decimal', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,price FROM dbo.pv_sales) s PIVOT (AVG(price) FOR qtr IN ([Q1],[Q2])) p ORDER BY region')
add('pivot sum bigint', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,big FROM dbo.pv_sales) s PIVOT (SUM(big) FOR qtr IN ([Q1],[Q2])) p ORDER BY region')
add('pivot avg float', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,ratio FROM dbo.pv_sales) s PIVOT (AVG(ratio) FOR qtr IN ([Q1],[Q2])) p ORDER BY region')
add('pivot max nvarchar', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,note FROM dbo.pv_sales) s PIVOT (MAX(note) FOR qtr IN ([Q1],[Q2])) p ORDER BY region')
add('pivot count nvarchar', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,note FROM dbo.pv_sales) s PIVOT (COUNT(note) FOR qtr IN ([Q1],[Q2])) p ORDER BY region')
add('pivot min date', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,day FROM dbo.pv_sales) s PIVOT (MIN(day) FOR qtr IN ([Q1],[Q2])) p ORDER BY region')
add('pivot max bit', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,flag FROM dbo.pv_sales) s PIVOT (MAX(flag) FOR qtr IN ([Q1],[Q2])) p ORDER BY region')
add('pivot sum bit', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,flag FROM dbo.pv_sales) s PIVOT (SUM(flag) FOR qtr IN ([Q1],[Q2])) p ORDER BY region')
add('pivot count star', `SELECT region,[Q1] FROM ${src} PIVOT (COUNT(*) FOR qtr IN ([Q1])) p ORDER BY region`)
add('pivot count distinct', `SELECT region,[Q1] FROM ${src} PIVOT (COUNT(DISTINCT amount) FOR qtr IN ([Q1])) p ORDER BY region`)
add('pivot aggregate expression', `SELECT region,[Q1] FROM ${src} PIVOT (SUM(amount+1) FOR qtr IN ([Q1])) p ORDER BY region`)
add('pivot string_agg', 'SELECT region,[Q1] FROM (SELECT region,qtr,note FROM dbo.pv_sales) s PIVOT (STRING_AGG(note,N\',\') FOR qtr IN ([Q1])) p ORDER BY region')
add('pivot approx_count_distinct', `SELECT region,[Q1] FROM ${src} PIVOT (APPROX_COUNT_DISTINCT(amount) FOR qtr IN ([Q1])) p ORDER BY region`)
add('pivot scalar function', `SELECT region,[Q1] FROM ${src} PIVOT (ABS(amount) FOR qtr IN ([Q1])) p ORDER BY region`)
add('pivot grouping function', `SELECT region,[Q1] FROM ${src} PIVOT (GROUPING(amount) FOR qtr IN ([Q1])) p ORDER BY region`)
add('pivot two aggregates', `SELECT region,[Q1] FROM ${src} PIVOT (SUM(amount),MAX(amount) FOR qtr IN ([Q1])) p ORDER BY region`)
add('pivot null value warning off', `SET ANSI_WARNINGS OFF; SELECT region,[Q1],[Q2] FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p ORDER BY region; SET ANSI_WARNINGS ON`)

// Implicit grouping columns and column order.
add('implicit grouping whole table', 'SELECT * FROM dbo.pv_sales PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p ORDER BY id')
add('implicit grouping two columns', 'SELECT * FROM (SELECT region,yr,qtr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2],[Q3],[Q4])) p ORDER BY region,yr')
add('implicit grouping column order', 'SELECT * FROM (SELECT amount,qtr,yr,region FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ([Q2],[Q1])) p ORDER BY yr,region')
add('implicit grouping none', 'SELECT * FROM (SELECT qtr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2],[Q9])) p')
add('implicit grouping none count', 'SELECT * FROM (SELECT qtr,amount FROM dbo.pv_sales) s PIVOT (COUNT(amount) FOR qtr IN ([Q1],[Q2],[Q9])) p')
add('implicit grouping none empty', 'SELECT * FROM (SELECT qtr,amount FROM dbo.pv_sales WHERE 1=0) s PIVOT (COUNT(amount) FOR qtr IN ([Q1],[Q2])) p')
add('implicit grouping empty', `SELECT * FROM (SELECT region,qtr,amount FROM dbo.pv_sales WHERE 1=0) s PIVOT (COUNT(amount) FOR qtr IN ([Q1],[Q2])) p ORDER BY region`)
add('implicit grouping null only rows', 'SELECT * FROM (SELECT id,qtr,amount FROM dbo.pv_sales WHERE id IN (6,9)) s PIVOT (COUNT(amount) FOR qtr IN ([Q1],[Q4])) p ORDER BY id')
add('pivot column not selectable', `SELECT region,qtr FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) p ORDER BY region`)
add('value column not selectable', `SELECT region,amount FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) p ORDER BY region`)
add('source alias not visible', `SELECT s.region FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) p ORDER BY 1`)
add('pivot alias qualified', `SELECT p.region,p.[Q1] FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) p ORDER BY p.region`)
add('pivot alias omitted', `SELECT region,[Q1] FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) ORDER BY region`)
add('pivot column is value column', 'SELECT * FROM (SELECT region,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR amount IN ([10],[20])) p ORDER BY region')
add('pivot unknown pivot column', `SELECT * FROM ${src} PIVOT (SUM(amount) FOR nope IN ([Q1])) p ORDER BY region`)
add('pivot unknown value column', `SELECT * FROM ${src} PIVOT (SUM(nope) FOR qtr IN ([Q1])) p ORDER BY region`)

// NULL, missing and converted pivot values.
add('null key string identifier', "SELECT * FROM (VALUES('NULL',1),(CAST(NULL AS VARCHAR(4)),2),('x',3)) v(k,a) PIVOT (SUM(a) FOR k IN ([NULL],[x])) p")
add('missing all values sum', `SELECT region,[Z1],[Z2] FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Z1],[Z2])) p ORDER BY region`)
add('missing all values count', `SELECT region,[Z1],[Z2] FROM ${src} PIVOT (COUNT(amount) FOR qtr IN ([Z1],[Z2])) p ORDER BY region`)
add('case-insensitive key match', `SELECT region,[q1],[Q2] FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([q1],[Q2])) p ORDER BY region`)
add('case-sensitive key collation', "SELECT region,[Q1],[q1] FROM (SELECT region,qtr COLLATE Latin1_General_CS_AS AS qtr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[q1])) p ORDER BY region")
add('trailing space key', `SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1 ],[Q2])) p ORDER BY region`)
add('trailing space duplicate key', `SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q1 ])) p ORDER BY region`)
add('key longer than column', `SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1x],[Q2])) p ORDER BY region`)
add('int key', 'SELECT * FROM (SELECT region,yr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR yr IN ([2023],[2024],[2025])) p ORDER BY region')
add('int key leading zero', 'SELECT * FROM (SELECT region,yr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR yr IN ([02023],[2024])) p ORDER BY region')
add('int key duplicate value', 'SELECT * FROM (SELECT region,yr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR yr IN ([2023],[02023])) p ORDER BY region')
add('int key not numeric', 'SELECT * FROM (SELECT region,yr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR yr IN ([2023],[abc])) p ORDER BY region')
add('int key decimal text', 'SELECT * FROM (SELECT region,yr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR yr IN ([2023.0])) p ORDER BY region')
add('date key', 'SELECT * FROM (SELECT region,day,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR day IN ([2024-01-01],[20240201],[2024-12-31])) p ORDER BY region')
add('date key invalid', 'SELECT * FROM (SELECT region,day,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR day IN ([2024-13-01])) p ORDER BY region')
add('bit key', 'SELECT * FROM (SELECT region,flag,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR flag IN ([0],[1])) p ORDER BY region')
add('decimal key', 'SELECT * FROM (SELECT region,price,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR price IN ([1.50],[2.25],[3])) p ORDER BY region')
add('decimal key equal values', 'SELECT * FROM (SELECT region,price,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR price IN ([1.50],[1.5])) p ORDER BY region')
add('nvarchar key', 'SELECT * FROM (SELECT region,note,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR note IN ([a],[b],[é])) p ORDER BY region')
add('computed key', 'SELECT * FROM (SELECT region,yr%100 AS y2,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR y2 IN ([23],[24])) p ORDER BY region')

// Types, widths and nullability of pivoted and grouping columns.
const typed = [
  ['sum int', 'SUM(amount)', 'amount'], ['count int', 'COUNT(amount)', 'amount'], ['count_big int', 'COUNT_BIG(amount)', 'amount'],
  ['avg int', 'AVG(amount)', 'amount'], ['sum decimal', 'SUM(price)', 'price'], ['avg decimal', 'AVG(price)', 'price'],
  ['max nvarchar', 'MAX(note)', 'note'], ['min date', 'MIN(day)', 'day'], ['stdev int', 'STDEV(amount)', 'amount'],
]
for (const [label, aggregate, column] of typed) {
  const query = `SELECT * FROM (SELECT id,region,qtr,${column} FROM dbo.pv_sales) s PIVOT (${aggregate} FOR qtr IN ([Q1],[Q2])) p`
  add(`describe ${label}`, describe(query))
}
add('select into sum', 'SELECT * INTO dbo.pv_into_sum FROM (SELECT id,region,qtr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p; ' + columnsOf('dbo.pv_into_sum'))
add('select into count', 'SELECT * INTO dbo.pv_into_count FROM (SELECT id,region,qtr,amount FROM dbo.pv_sales) s PIVOT (COUNT(amount) FOR qtr IN ([Q1],[Q2])) p; ' + columnsOf('dbo.pv_into_count'))
add('select into max nvarchar', 'SELECT * INTO dbo.pv_into_max FROM (SELECT id,region,qtr,note FROM dbo.pv_sales) s PIVOT (MAX(note) FOR qtr IN ([Q1],[Q2])) p; ' + columnsOf('dbo.pv_into_max'))
add('select into avg decimal', 'SELECT * INTO dbo.pv_into_avg FROM (SELECT region,qtr,price FROM dbo.pv_sales) s PIVOT (AVG(price) FOR qtr IN ([Q1],[Q2])) p; ' + columnsOf('dbo.pv_into_avg'))

// Quoted, bracketed and duplicate identifiers.
add('double-quoted identifiers', `SELECT region,"Q1","Q2" FROM ${src} PIVOT (SUM(amount) FOR qtr IN ("Q1","Q2")) p ORDER BY region`)
add('quoted identifier off', `EXEC(N'SET QUOTED_IDENTIFIER OFF; SELECT region FROM (SELECT region,qtr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ("Q1")) p ORDER BY region')`)
add('unbracketed identifiers', `SELECT region,Q1,Q2 FROM ${src} PIVOT (SUM(amount) FOR qtr IN (Q1,Q2)) p ORDER BY region`)
add('string literal in list', `SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ('Q1')) p ORDER BY region`)
add('numeric literal in list', 'SELECT * FROM (SELECT region,yr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR yr IN (2023)) p ORDER BY region')
add('variable in list', `DECLARE @q CHAR(2)='Q1'; SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN (@q)) p ORDER BY region`)
add('bracket escape identifier', "SELECT * FROM (VALUES('a]b',1),('a b',2),('select',3)) v(k,a) PIVOT (SUM(a) FOR k IN ([a]]b],[a b],[select])) p")
add('empty bracket identifier', `SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([])) p ORDER BY region`)
add('duplicate identifiers', `SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q1])) p ORDER BY region`)
add('duplicate case-different identifiers', `SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[q1])) p ORDER BY region`)
add('identifier equals grouping column', 'SELECT * FROM (SELECT region AS Q1,qtr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p ORDER BY 1')
add('identifier equals pivot column', `SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([qtr],[Q1])) p ORDER BY region`)
add('identifier equals value column', `SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([amount],[Q1])) p ORDER BY region`)
add('identifier 128 characters', `SELECT * FROM (VALUES(CAST(REPLICATE('k',128) AS VARCHAR(200)),1)) v(k,a) PIVOT (SUM(a) FOR k IN ([${'k'.repeat(128)}])) p`)
add('identifier 129 characters', `SELECT * FROM (VALUES(CAST(REPLICATE('k',129) AS VARCHAR(200)),1)) v(k,a) PIVOT (SUM(a) FOR k IN ([${'k'.repeat(129)}])) p`)
add('duplicate source column names', 'SELECT * FROM (SELECT s.region,r.code AS region,s.qtr,s.amount FROM dbo.pv_sales s JOIN dbo.pv_region r ON r.code=s.region) x PIVOT (SUM(amount) FOR qtr IN ([Q1])) p ORDER BY 1')

// UNPIVOT.
add('unpivot basic', 'SELECT id,label,quarter,val FROM dbo.pv_wide UNPIVOT (val FOR quarter IN (q1,q2,q3)) u ORDER BY id,quarter')
add('unpivot star', 'SELECT * FROM (SELECT id,label,q1,q2,q3 FROM dbo.pv_wide) w UNPIVOT (val FOR quarter IN (q1,q2,q3)) u ORDER BY id,quarter')
add('unpivot star whole table', 'SELECT * FROM dbo.pv_wide UNPIVOT (val FOR quarter IN (q1,q2,q3)) u ORDER BY id,quarter')
add('unpivot list order', 'SELECT id,quarter,val FROM (SELECT id,q1,q2,q3 FROM dbo.pv_wide) w UNPIVOT (val FOR quarter IN (q3,q1,q2)) u ORDER BY id,val')
add('unpivot not null column', 'SELECT id,k,val FROM (SELECT id,q1,nn5 FROM dbo.pv_wide) w UNPIVOT (val FOR k IN (q1,nn5)) u ORDER BY id,k')
add('unpivot int smallint conflict', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN (q1,s1)) u ORDER BY id,k')
add('unpivot varchar length conflict', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN (v5,v10)) u ORDER BY id,k')
add('unpivot varchar same length', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN (v5,v5b)) u ORDER BY id,k')
add('unpivot varchar nvarchar conflict', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN (v5,n5)) u ORDER BY id,k')
add('unpivot varchar char conflict', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN (v5,c5)) u ORDER BY id,k')
add('unpivot decimal same', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN (d1,d2)) u ORDER BY id,k')
add('unpivot decimal precision conflict', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN (d1,d3)) u ORDER BY id,k')
add('unpivot collation conflict', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN (v5,v5cs)) u ORDER BY id,k')
add('unpivot cast reconciles', 'SELECT id,k,val FROM (SELECT id,CAST(q1 AS INT) AS q1,CAST(s1 AS INT) AS s1 FROM dbo.pv_wide) w UNPIVOT (val FOR k IN (q1,s1)) u ORDER BY id,k')
add('unpivot varchar max', "SELECT id,k,val FROM (SELECT id,CAST(v5 AS VARCHAR(MAX)) AS a,CAST(v10 AS VARCHAR(MAX)) AS b FROM dbo.pv_wide) w UNPIVOT (val FOR k IN (a,b)) u ORDER BY id,k")
add('unpivot special names', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN ([a b],[x]]y])) u ORDER BY id,k')
add('unpivot name case as written', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN ([Q1],q2)) u ORDER BY id,val')
add('unpivot duplicate column', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN (q1,q1)) u ORDER BY id,k')
add('unpivot name conflicts existing', 'SELECT * FROM dbo.pv_wide UNPIVOT (val FOR label IN (q1,q2)) u ORDER BY id')
add('unpivot value conflicts existing', 'SELECT * FROM dbo.pv_wide UNPIVOT (label FOR k IN (q1,q2)) u ORDER BY id')
add('unpivot unknown column', 'SELECT * FROM dbo.pv_wide UNPIVOT (val FOR k IN (q1,nope)) u ORDER BY id')
add('unpivot source column not selectable', 'SELECT id,q1 FROM dbo.pv_wide UNPIVOT (val FOR k IN (q1,q2)) u ORDER BY id')
add('unpivot string literal in list', "SELECT * FROM dbo.pv_wide UNPIVOT (val FOR k IN ('q1')) u ORDER BY id")
add('unpivot only unpivoted columns', 'SELECT * FROM (SELECT q1,q2 FROM dbo.pv_wide) w UNPIVOT (val FOR k IN (q1,q2)) u ORDER BY k,val')
add('unpivot empty source', 'SELECT * FROM (SELECT id,q1,q2 FROM dbo.pv_wide WHERE 1=0) w UNPIVOT (val FOR k IN (q1,q2)) u ORDER BY id')
add('unpivot single column', 'SELECT id,k,val FROM dbo.pv_wide UNPIVOT (val FOR k IN (q2)) u ORDER BY id')
add('unpivot apply keeps nulls contrast', 'SELECT w.id,x.k,x.val FROM dbo.pv_wide w CROSS APPLY (VALUES(\'q1\',w.q1),(\'q2\',w.q2)) x(k,val) ORDER BY w.id,x.k')
add('unpivot describe', describe('SELECT * FROM (SELECT id,label,q1,q2 FROM dbo.pv_wide) w UNPIVOT (val FOR k IN (q1,q2)) u'))
add('unpivot describe varchar', describe('SELECT * FROM (SELECT id,v5,v5b FROM dbo.pv_wide) w UNPIVOT (val FOR k IN (v5,v5b)) u'))
add('describe parameterized pivot', describe('SELECT * FROM (SELECT region,qtr,amount*@m AS amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ([Q1])) p', "N'@m decimal(5,2)'"))
add('unpivot in cte', 'WITH w AS (SELECT id,q1,q2 FROM dbo.pv_wide) SELECT id,k,val FROM w UNPIVOT (val FOR k IN (q1,q2)) u ORDER BY id,k')
add('unpivot joined derived source', "SELECT id,code,k,val FROM (SELECT w.id,r.code,w.q1,w.q2 FROM dbo.pv_wide w JOIN dbo.pv_region r ON r.code IN ('east','west')) s UNPIVOT (val FOR k IN (q1,q2)) u ORDER BY id,code,k")
add('unpivot directly after join', "SELECT * FROM (SELECT id,q1,q2 FROM dbo.pv_wide) w JOIN dbo.pv_region r ON r.code='east' UNPIVOT (val FOR k IN (q1,q2)) u ORDER BY id,k")
add('join unpivot result', "SELECT u.id,u.k,u.val,r.name FROM (SELECT id,label,q1,q2 FROM dbo.pv_wide) w UNPIVOT (val FOR k IN (q1,q2)) u LEFT JOIN dbo.pv_region r ON r.code=CASE u.id WHEN 1 THEN 'east' WHEN 2 THEN 'west' END ORDER BY u.id,u.k")
add('unpivot select into', 'SELECT * INTO dbo.pv_into_unpivot FROM (SELECT id,label,q1,q2,nn5 FROM dbo.pv_wide) w UNPIVOT (val FOR k IN (q1,q2)) u; ' + columnsOf('dbo.pv_into_unpivot'))
add('unpivot select into special names', 'SELECT * INTO dbo.pv_into_unpivot_names FROM (SELECT id,[a b],[x]]y] FROM dbo.pv_wide) w UNPIVOT (val FOR k IN ([a b],[x]]y])) u; ' + columnsOf('dbo.pv_into_unpivot_names') + '; SELECT id,k,DATALENGTH(k) AS k_bytes FROM dbo.pv_into_unpivot_names ORDER BY id,k')

// Derived tables, CTEs, joins, views and nesting.
add('pivot in cte', 'WITH s AS (SELECT region,qtr,amount FROM dbo.pv_sales) SELECT * FROM s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p ORDER BY region')
add('cte over pivot', `WITH p AS (SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) pv) SELECT region,[Q1]+[Q2] AS total,ISNULL([Q1],0)+ISNULL([Q2],0) AS total0 FROM p ORDER BY region`)
add('pivot joined derived source', 'SELECT * FROM (SELECT r.name,s.qtr,s.amount FROM dbo.pv_sales s JOIN dbo.pv_region r ON r.code=s.region) src PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p ORDER BY name')
add('pivot directly after join', 'SELECT * FROM dbo.pv_region r JOIN (SELECT region,qtr,amount FROM dbo.pv_sales) s ON s.region=r.code PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p ORDER BY code')
add('join pivot result', `SELECT r.code,r.name,p.[Q1],p.[Q2] FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p JOIN dbo.pv_region r ON r.code=p.region ORDER BY r.code`)
add('left join pivot result', `SELECT r.code,p.[Q1],p.[Q2] FROM dbo.pv_region r LEFT JOIN (SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) pv) p ON p.region=r.code ORDER BY r.code`)
add('cross apply correlated pivot', 'SELECT r.code,x.[Q1],x.[Q2] FROM dbo.pv_region r CROSS APPLY (SELECT * FROM (SELECT qtr,amount FROM dbo.pv_sales WHERE region=r.code) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p) x ORDER BY r.code')
add('pivot then unpivot', `SELECT region,q,v FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2],[Q3])) p UNPIVOT (v FOR q IN ([Q1],[Q2],[Q3])) u ORDER BY region,q`)
add('unpivot then pivot', 'SELECT * FROM (SELECT id,q1,q2 FROM dbo.pv_wide) w UNPIVOT (val FOR k IN (q1,q2)) u PIVOT (MAX(val) FOR k IN ([q1],[q2])) p ORDER BY id')
add('double pivot', 'SELECT * FROM (SELECT region,yr,qtr,amount,price FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p1 PIVOT (SUM(price) FOR yr IN ([2023],[2024])) p2 ORDER BY region,[Q1],[Q2],[2023],[2024]')
add('pivot where on pivoted column', `SELECT region,[Q1] FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p WHERE [Q1] IS NOT NULL ORDER BY region`)
add('pivot group by output', `SELECT COUNT(*) AS regions,SUM([Q1]) AS q1_total,COUNT([Q2]) AS q2_count FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p`)
add('pivot order by pivoted column', `SELECT region,[Q1] FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) p ORDER BY [Q1] DESC,region`)
add('pivot top', `SELECT TOP (2) region,[Q1] FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) p ORDER BY region`)
add('pivot scalar subquery', `SELECT (SELECT [Q1] FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) p WHERE region='east') AS east_q1`)
add('pivot exists subquery', `SELECT r.code FROM dbo.pv_region r WHERE EXISTS (SELECT 1 FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) p WHERE p.region=r.code AND p.[Q1]>6) ORDER BY r.code`)
add('pivot window over output', `SELECT region,[Q1],SUM([Q1]) OVER () AS all_q1 FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) p ORDER BY region`)
add('pivot recursive anchor', `WITH r(region,q1,n) AS (SELECT region,[Q1],1 FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) p UNION ALL SELECT region,q1,n+1 FROM r WHERE n<2) SELECT region,q1,n FROM r ORDER BY region,n`)
add('pivot recursive member', 'WITH r(n) AS (SELECT 1 UNION ALL SELECT [Q1] FROM (SELECT qtr,n AS v FROM r CROSS JOIN (VALUES(\'Q1\')) q(qtr)) s PIVOT (MAX(v) FOR qtr IN ([Q1])) p WHERE [Q1]<3) SELECT n FROM r ORDER BY n')
add('pivot view', `CREATE VIEW dbo.pv_view AS SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p`)
add('pivot view select', 'SELECT region,[Q1],[Q2] FROM dbo.pv_view ORDER BY region')
add('pivot view columns', columnsOf('dbo.pv_view'))
add('insert select pivot', `CREATE TABLE dbo.pv_target(region VARCHAR(10) NULL,q1 INT NULL,q2 INT NULL); INSERT dbo.pv_target(region,q1,q2) SELECT region,[Q1],[Q2] FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p; SELECT region,q1,q2 FROM dbo.pv_target ORDER BY region`)
add('update from pivot', `UPDATE t SET q2=p.[Q1] FROM dbo.pv_target t JOIN (SELECT * FROM ${src} PIVOT (SUM(amount) FOR qtr IN ([Q1])) pv) p ON p.region=t.region; SELECT region,q1,q2 FROM dbo.pv_target ORDER BY region`)
add('pivot table hint on source', `SELECT * FROM dbo.pv_sales WITH (NOLOCK) PIVOT (SUM(amount) FOR qtr IN ([Q1])) p ORDER BY id`)
add('dynamic pivot', "DECLARE @cols NVARCHAR(200)=(SELECT STRING_AGG(QUOTENAME(qtr),',') WITHIN GROUP (ORDER BY qtr) FROM (SELECT DISTINCT qtr FROM dbo.pv_sales WHERE qtr IS NOT NULL) d); DECLARE @sql NVARCHAR(MAX)=N'SELECT * FROM (SELECT region,qtr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ('+@cols+N')) p ORDER BY region'; SELECT @cols AS cols; EXEC sp_executesql @sql")

// Parameterized (sp_executesql) observations. Each has unique text.
const boundCases = [
  ['rpc pivot filter', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,amount FROM dbo.pv_sales WHERE yr=@yr) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p ORDER BY region /*rpc filter*/', [['yr', TYPES.Int, 2023]]],
  ['rpc pivot scaled decimal', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,amount*@m AS amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p ORDER BY region /*rpc scaled*/', [['m', TYPES.Decimal, 1.5, { precision: 5, scale: 2 }]]],
  ['rpc pivot nvarchar value', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,@tag+note AS note FROM dbo.pv_sales) s PIVOT (MAX(note) FOR qtr IN ([Q1],[Q2])) p ORDER BY region /*rpc nvarchar*/', [['tag', TYPES.NVarChar, 'x', { length: 10 }]]],
  ['rpc pivot null parameter', 'SELECT region,[Q1],[Q2] FROM (SELECT region,qtr,amount FROM dbo.pv_sales WHERE yr=@yr) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2])) p ORDER BY region /*rpc null*/', [['yr', TYPES.Int, null]]],
  ['rpc pivot parameter in list', 'SELECT * FROM (SELECT region,qtr,amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN (@q)) p ORDER BY region /*rpc list*/', [['q', TYPES.Char, 'Q1', { length: 2 }]]],
  ['rpc pivot divide error', 'SELECT region,[Q1] FROM (SELECT region,qtr,amount/@d AS amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ([Q1])) p ORDER BY region /*rpc divide*/', [['d', TYPES.Int, 0]]],
  ['rpc unpivot filter', 'SELECT id,k,val FROM (SELECT id,q1,q2,q3 FROM dbo.pv_wide WHERE id>=@min) w UNPIVOT (val FOR k IN (q1,q2,q3)) u ORDER BY id,k /*rpc unpivot*/', [['min', TYPES.Int, 2]]],
]

// Prepared (sp_prepare/sp_execute/sp_unprepare) observations, unique text.
const preparedCases = [
  ['prepared pivot filter', 'SELECT region,[Q1],[Q2],[Q3] FROM (SELECT region,qtr,amount FROM dbo.pv_sales WHERE yr=@yr) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q2],[Q3])) p ORDER BY region /*prepared filter*/',
    [['yr', TYPES.Int]], [{ yr: 2023 }, { yr: 2024 }, { yr: 2099 }, { yr: null }]],
  ['prepared pivot divide', 'SELECT region,[Q1] FROM (SELECT region,qtr,amount/@d AS amount FROM dbo.pv_sales) s PIVOT (SUM(amount) FOR qtr IN ([Q1])) p ORDER BY region /*prepared divide*/',
    [['d', TYPES.Int]], [{ d: 1 }, { d: 0 }, { d: 2 }]],
  ['prepared unpivot', 'SELECT id,k,val FROM (SELECT id,q1,q2,q3 FROM dbo.pv_wide WHERE id>=@min) w UNPIVOT (val FOR k IN (q1,q2,q3)) u ORDER BY id,k /*prepared unpivot*/',
    [['min', TYPES.Int]], [{ min: 1 }, { min: 4 }, { min: 5 }]],
  ['prepared pivot invalid', 'SELECT * FROM (SELECT region,qtr,amount FROM dbo.pv_sales WHERE yr=@yr) s PIVOT (SUM(amount) FOR qtr IN ([Q1],[Q1])) p ORDER BY region /*prepared invalid*/',
    [['yr', TYPES.Int]], [{ yr: 2023 }]],
]

// ---------------------------------------------------------------------------
const messageFields = m => ({ number: m.number, state: m.state, class: m.class, lineNumber: m.lineNumber, message: m.message })
const columnFields = c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })

// One batch (execSqlBatch) or sp_executesql RPC (execSql) with per-request
// return status and bounded rows/messages.
function request(connection, sql, parameters) {
  return new Promise(done => {
    const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
    let open = true
    const overflow = kind => { result.truncated ??= { rows: 0, messages: 0 }; result.truncated[kind]++ }
    const message = list => token => {
      if (!open) return
      if (result.errors.length + result.info.length >= LIMITS.messages) overflow('messages')
      else result[list].push(messageFields(token))
    }
    const onError = message('errors')
    const onInfo = message('info')
    const finish = (error, rowCount) => {
      if (!open) return
      open = false
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.rowCount = rowCount
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      done(result)
    }
    const req = new Request(sql, finish)
    req.on('columnMetadata', columns => result.sets.push({ columns: columns.map(columnFields), rows: [] }))
    req.on('row', columns => {
      const set = result.sets.at(-1)
      if (!set || set.rows.length >= LIMITS.rows) overflow('rows')
      else set.rows.push(columns.map(c => c.value))
    })
    for (const kind of ['done', 'doneInProc', 'doneProc']) req.on(kind, (rowCount, more) => {
      if (result.done.length >= LIMITS.messages) overflow('messages')
      else result.done.push({ kind, rowCount: rowCount ?? null, more })
    })
    req.on('doneProc', (_count, _more, status) => { if (status !== undefined) result.returnStatus = status })
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    // Clear a status carried over from an earlier request (tedious clears it
    // only on DONEPROC); only a status arriving during this request is kept.
    connection.procReturnStatusValue = undefined
    try {
      if (parameters) {
        for (const [name, type, value, options] of parameters) req.addParameter(name, type, value, options)
        connection.execSql(req)
      } else connection.execSqlBatch(req)
    } catch (error) { finish(error, undefined) }
  })
}

// Replica of PR #312 capturePrepared (scripts/lib/reference.mjs on
// origin/work/prepared-capture-helper-v1 at 29c8c58).
async function capturePrepared(connection, sql, declarations, valueSets) {
  let result
  let complete = () => {}
  const reported = new Set()
  const fresh = () => ({ sets: [], done: [], errors: [], info: [], returnStatus: null })
  const overflow = kind => { result.truncated ??= { rows: 0, messages: 0 }; result.truncated[kind]++ }
  const message = list => token => {
    if (!result) return
    if (result.errors.length + result.info.length >= LIMITS.messages) overflow('messages')
    else result[list].push(messageFields(token))
  }
  const onError = message('errors')
  const onInfo = message('info')
  const req = new Request(sql, (error, rowCount) => complete(error, rowCount))
  for (const [name, type, options] of declarations) req.addParameter(name, type, undefined, options)
  req.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(columnFields), rows: [] }))
  req.on('row', columns => {
    if (!result) return
    const set = result.sets.at(-1)
    if (!set || set.rows.length >= LIMITS.rows) overflow('rows')
    else set.rows.push(columns.map(c => c.value))
  })
  for (const kind of ['done', 'doneInProc', 'doneProc']) req.on(kind, (rowCount, more) => {
    if (!result) return
    if (result.done.length >= LIMITS.messages) overflow('messages')
    else result.done.push({ kind, rowCount: rowCount ?? null, more })
  })
  req.on('doneProc', (_count, _more, status) => { if (result && status !== undefined) result.returnStatus = status })
  const phase = start => new Promise(resolvePhase => {
    result = fresh()
    complete = (error, rowCount) => {
      complete = () => {}
      const finished = result
      result = undefined
      finished.rowCount = rowCount
      if (error && !reported.has(error) && !finished.errors.length) finished.errors.push({ message: error.message, number: error.number ?? null })
      if (error) reported.add(error)
      resolvePhase(finished)
    }
    req.error = undefined
    connection.procReturnStatusValue = undefined
    start()
  })
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  const onPrepared = () => complete(undefined, undefined)
  const onPrepareError = error => complete(error, undefined)
  try {
    const prepare = await phase(() => {
      req.once('prepared', onPrepared)
      req.once('error', onPrepareError)
      connection.prepare(req)
    })
    req.off('prepared', onPrepared)
    req.off('error', onPrepareError)
    if (req.handle === undefined) return { prepare, prepared: false, executions: [], unprepare: null }
    const executions = []
    for (const values of valueSets) executions.push({ values, result: await phase(() => connection.execute(req, values)) })
    const unprepare = await phase(() => connection.unprepare(req))
    return { prepare, prepared: true, executions, unprepare }
  } finally {
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

const describeParameters = parameters => parameters.map(([name, type, value, options]) => canonical({ name, type: type.name, value, ...(options ? { options } : {}) }))

async function observe(connection) {
  const records = []
  for (const [name, sql] of [...setup, ...cases]) records.push({ name, sql, result: canonical(await request(connection, sql)) })
  for (const [name, sql, parameters] of boundCases) {
    records.push({ name, sql, parameters: describeParameters(parameters), result: canonical(await request(connection, sql, parameters)) })
  }
  for (const [name, sql, declarations, valueSets] of preparedCases) {
    const prepared = await capturePrepared(connection, sql, declarations, valueSets)
    records.push({
      name, sql,
      declarations: declarations.map(([parameter, type, options]) => canonical({ name: parameter, type: type.name, ...(options ? { options } : {}) })),
      prepared: canonical(prepared),
    })
  }
  const reuse = await request(connection, 'SELECT @@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state')
  records.push({ name: 'session reusable', sql: 'SELECT @@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state', result: canonical(reuse) })
  return records
}

// Small targeted checks on individual values; whole captures are compared only
// through assertSameCapture.
function check(condition, message) { if (!condition) throw new Error(message) }
function validate(run) {
  check(run.length === setup.length + cases.length + boundCases.length + preparedCases.length + 1, 'unexpected record count')
  for (const record of run) {
    const phases = record.prepared ? [record.prepared.prepare, ...record.prepared.executions.map(e => e.result), record.prepared.unprepare].filter(Boolean) : [record.result]
    for (const phase of phases) {
      check(!phase.truncated, `${record.name}: bounded capture truncated`)
      check(phase.done.length > 0 || phase.errors.length > 0 || record.prepared, `${record.name}: no completion`)
    }
  }
  for (const [name] of setup) check(run.find(r => r.name === name).result.errors.length === 0, `${name}: setup failed`)
  const reuse = run.at(-1).result
  check(reuse.errors.length === 0 && reuse.sets[0]?.rows[0]?.[0] === 0, 'session not reusable')
  const get = name => {
    const found = run.find(record => record.name === name)
    check(found, `${name}: missing record`)
    return found.result ?? found.prepared
  }
  const rows = name => JSON.stringify(get(name).sets.map(set => set.rows))
  const names = name => get(name).sets[0].columns.map(column => column.name).join(',')
  const errors = name => get(name).errors.map(error => error.number).join(',')
  check(rows('pivot sum int') === '[[[null,null,11,null,null,null],["east",43,null,null,null,null],["west",5,null,7,null,null]]]', 'pivot sum int rows')
  check(rows('pivot count int') === '[[[null,0,1,0,0,0],["east",3,0,0,0,0],["west",1,0,1,0,0]]]', 'pivot count int rows')
  check(rows('implicit grouping none empty') === '[[]]', 'empty ungrouped pivot returns no row')
  check(names('implicit grouping column order') === 'yr,region,Q2,Q1', 'pivot star column order')
  check(names('unpivot star') === 'id,label,val,quarter', 'unpivot star column order')
  check(get('unpivot basic').sets[0].columns[2].type === 'NVarChar' && get('unpivot basic').sets[0].columns[2].length === 256, 'unpivot name column type')
  for (const [name, expected] of [
    ['pivot checksum_agg int', '406'], ['pivot count star', '102'], ['pivot scalar function', '195'],
    ['int key not numeric', '8114,473'], ['duplicate identifiers', '8156'], ['identifier equals pivot column', '265'],
    ['identifier equals grouping column', '265,8156'], ['unpivot int smallint conflict', '8167'],
    ['unpivot varchar length conflict', '8167'], ['unpivot duplicate column', '277'], ['pivot recursive member', '4190'],
  ]) check(errors(name) === expected, `${name}: expected errors ${expected}, got ${errors(name).slice(0, 80)}`)
  const divide = get('prepared pivot divide')
  check(divide.executions[1].result.errors.map(e => e.number).join(',') === '8134', 'prepared divide error attribution')
  check(divide.executions[2].result.errors.length === 0 && divide.executions[2].result.returnStatus === 0, 'prepared divide recovery')
}

// Refuse before any container starts; the fixture is checked by existence only.
if (writeFixture) await refuseExistingFixture(fixture)

await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
      const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, observe)
      if (!oneDatabase) validate(run)
      runs.push(run)
    }
    if (!oneDatabase) assertSameCapture(runs[0], runs[1], 'PIVOT/UNPIVOT observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) assertSameCapture(containers[0].runs[0], containers[1].runs[0], 'PIVOT/UNPIVOT observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
if (!writeFixture && !oneDatabase) {
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'PIVOT/UNPIVOT observations differ from retained fixture')
}
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${containers[0].runs[0].length} PIVOT/UNPIVOT observations` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained ? ' and matched retained fixture' : '') + (writeFixture ? '; wrote retained fixture' : ''))
