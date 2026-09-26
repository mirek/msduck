#!/usr/bin/env node
// Retain SQL Server FORMAT rows, descriptors, diagnostics and completions.
//
// Modes:
//   (default)        capture two fresh databases in each of two containers,
//                    require all four to match, write the artifact and compare
//                    it with the retained fixture when one exists.
//   --write-fixture  as above, then retain the fixture; refuses before any
//                    container starts when the fixture already exists.
//   --one-database   one diagnostic database in one container; never retained
//                    or compared.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

// tedious derives a DATETIMEOFFSET parameter's offset from the host time zone.
// Node applies a runtime TZ change to later conversions, so pinning it here keeps
// the captured parameters and results independent of the host.
process.env.TZ = 'UTC'

const fixture = new URL('../reference/format.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/format-reference-v1/capture.json')

// Culture matrix: one column per culture; the first omits the argument.
const cultures = [
  ['c_default', null], ['c_iv', "'iv'"], ['c_en_us', "'en-US'"], ['c_de_de', "'de-DE'"],
  ['c_fr_fr', "'fr-FR'"], ['c_ja_jp', "'ja-JP'"], ['c_ar_sa', "'ar-SA'"],
]
const matrix = (value, format) => 'SELECT ' + cultures.map(([name, culture]) =>
  `FORMAT(${value},N'${format.replaceAll("'", "''")}'${culture === null ? '' : ',' + culture}) AS ${name}`).join(',')

const dec = "CAST(-1234567.8951 AS DECIMAL(19,4))"
const int = "CAST(-1234567 AS INT)"
const money = "CAST(1234567.8951 AS MONEY)"
const flt = "CAST(1234567.891 AS FLOAT)"
const dt2 = "CAST('2024-03-05 14:07:09.1234567' AS DATETIME2(7))"
const date = "CAST('2024-03-05' AS DATE)"
const dto = "CAST('2024-03-05 14:07:09.1234567 -05:30' AS DATETIMEOFFSET(7))"
const time = "CAST('14:07:09.1234567' AS TIME(7))"

const matrixCases = [
  ...['N', 'N0', 'C', 'P', 'E', 'G', 'F2', '#,##0.00', '0.###'].map(f => ['decimal', dec, f]),
  ...['N', 'C', 'D8'].map(f => ['int', int, f]),
  ...['C', 'N'].map(f => ['money', money, f]),
  ...['N', 'G', 'E3', 'R'].map(f => ['float', flt, f]),
  ...['d', 'D', 'f', 'F', 'g', 'G', 'M', 'Y', 't', 'T', 'dddd, MMMM d', 'MMM tt', 'yyyy-MM-dd'].map(f => ['datetime2', dt2, f]),
  ...['d', 'D'].map(f => ['date', date, f]),
  ...['O', 'R', 'u', 'U', 'D', 'zzz K'].map(f => ['datetimeoffset', dto, f]),
  ...['c', 'g', 'G', 'hh\\:mm'].map(f => ['time', time, f]),
].map(([type, value, format]) => [`matrix ${type} ${format}`, matrix(value, format)])

// Standard and custom specifier catalogs on en-US; an invalid specifier yields
// NULL in its own column without affecting the others.
const columns = (value, formats, culture = 'en-US') => 'SELECT ' + formats.map(([name, format]) =>
  `FORMAT(${value},N'${format.replaceAll("'", "''")}','${culture}') AS ${name}`).join(',')
const numericStandard = [
  ['c', 'C'], ['c0', 'C0'], ['d', 'D'], ['d10', 'D10'], ['e', 'E'], ['e2', 'e2'], ['f', 'F'], ['f3', 'F3'],
  ['g', 'G'], ['g5', 'G5'], ['n', 'N'], ['n2', 'N2'], ['p', 'P'], ['p1', 'P1'], ['r', 'R'], ['x', 'X'], ['x8', 'x8'],
  ['b', 'B'], ['n99', 'N99'], ['n100', 'N100'], ['q', 'Q'], ['empty', ''], ['space', ' '],
]
const numericCustom = [
  ['grouped', '#,##0.00'], ['padded', '0000000000'], ['optional', '#.##'], ['scaled', '#,##0,'], ['scaled2', '0,,.0'],
  ['sections', '0.00;(0.00);zero'], ['two_sections', '0.0;neg'], ['sci', '0.0e+00'], ['sci_upper', '0.00E0'],
  ['pct', '0.0%'], ['permille', '0.0‰'], ['literal', "'#'0'!'"], ['escaped', '\\#0'], ['quoted_dq', '"n="0'],
  ['no_digits', 'abc'], ['hash_only', '#'],
]
const dateStandard = ['d', 'D', 'f', 'F', 'g', 'G', 'M', 'O', 'R', 's', 't', 'T', 'u', 'U', 'Y']
  .map(f => [`s_${f === f.toLowerCase() ? f : f.toLowerCase() + '_upper'}`, f])
const dateCustom = [
  ['full_pattern', 'yyyy-MM-dd HH:mm:ss.fffffff'], ['f_upper', 'ss.FFFFFFF'], ['words', 'dddd, MMMM d, yyyy'],
  ['short_words', 'ddd MMM'], ['twelve', 'h:mm:ss tt'], ['two_digit_year', 'yy'], ['one_char_d', '%d'],
  ['era', 'gg yyyy'], ['offset', 'zzz'], ['offset_z', 'z'], ['kind', 'K'], ['literal', "'at' HH'h'"],
  ['escaped', 'HH\\:mm'], ['separators', 'yyyy/MM/dd HH:mm'], ['single_h', 'H'], ['only_d', 'd'], ['m_minute', 'mm'],
]
const timeFormats = [
  ['c', 'c'], ['g', 'g'], ['g_upper', 'G'], ['hh_escaped', 'hh\\:mm\\:ss'], ['hh_colon', 'hh:mm'], ['upper_hh', 'HH\\:mm'],
  ['fraction', 'fffffff'], ['quoted', "hh':'mm"], ['t', 't'], ['d_upper', 'D'],
]

const catalogCases = [
  ['numeric standard int', columns('CAST(1234 AS INT)', numericStandard)],
  ['numeric standard negative int', columns('CAST(-1234 AS INT)', numericStandard)],
  ['numeric standard decimal', columns('CAST(-1234.5678 AS DECIMAL(10,4))', numericStandard)],
  ['numeric standard float', columns('CAST(0.1 AS FLOAT)', numericStandard)],
  ['numeric custom positive', columns('CAST(1234567.891 AS DECIMAL(12,3))', numericCustom)],
  ['numeric custom negative', columns('CAST(-1234567.891 AS DECIMAL(12,3))', numericCustom)],
  ['numeric custom zero', columns('CAST(0 AS DECIMAL(12,3))', numericCustom)],
  ['numeric custom float', columns('CAST(0.000123 AS FLOAT)', numericCustom)],
  ['date standard datetime2', columns(dt2, dateStandard)],
  ['date standard datetime', columns("CAST('2024-03-05 14:07:09.997' AS DATETIME)", dateStandard)],
  ['date standard date', columns(date, dateStandard)],
  ['date standard smalldatetime', columns("CAST('2024-03-05 14:07:29' AS SMALLDATETIME)", dateStandard)],
  ['date standard datetimeoffset', columns(dto, dateStandard)],
  ['date custom datetime2', columns(dt2, dateCustom)],
  ['date custom datetimeoffset', columns(dto, dateCustom)],
  ['date custom datetime', columns("CAST('2024-03-05 14:07:09.997' AS DATETIME)", dateCustom)],
  ['time formats', columns(time, timeFormats)],
  ['time formats zero', columns("CAST('00:00:00' AS TIME(0))", timeFormats)],
]

// One program per input type with a few general formats on en-US.
const typeFormats = [['g', 'G'], ['n2', 'N2'], ['custom', '0.00'], ['x', 'X'], ['d', 'd']]
const typeCases = [
  ['tinyint', 'CAST(255 AS TINYINT)'], ['smallint', 'CAST(-32768 AS SMALLINT)'], ['int', 'CAST(-2147483648 AS INT)'],
  ['bigint max', 'CAST(9223372036854775807 AS BIGINT)'], ['bigint min', 'CAST(-9223372036854775808 AS BIGINT)'],
  ['int literal', '42'], ['decimal literal', '42.50'], ['float literal', '4.25E1'],
  ['decimal 28 digits', 'CAST(1234567890123456789012345678 AS DECIMAL(28,0))'],
  ['decimal 29 digits', 'CAST(79228162514264337593543950335 AS DECIMAL(29,0))'],
  ['decimal beyond clr', 'CAST(79228162514264337593543950336 AS DECIMAL(29,0))'],
  ['decimal 38 scale', 'CAST(0.12345678901234567890123456789012345678 AS DECIMAL(38,38))'],
  ['decimal trailing zeros', 'CAST(1.50000 AS DECIMAL(10,5))'],
  ['numeric', 'CAST(-0.5 AS NUMERIC(5,2))'],
  ['money', 'CAST(-922337203685477.5808 AS MONEY)'], ['smallmoney', 'CAST(214748.3647 AS SMALLMONEY)'],
  ['float large', 'CAST(1.2345678901234567E+20 AS FLOAT)'], ['float small', 'CAST(1.5E-7 AS FLOAT)'],
  ['float negative zero', 'CAST(-0.0E0 AS FLOAT)'], ['real', 'CAST(0.1 AS REAL)'], ['float of real', 'CAST(CAST(0.1 AS REAL) AS FLOAT)'],
  ['bit', 'CAST(1 AS BIT)'],
  ['date min', "CAST('0001-01-01' AS DATE)"], ['datetime2 max', "CAST('9999-12-31 23:59:59.9999999' AS DATETIME2(7))"],
  ['datetime2 scale 0', "CAST('2024-03-05 14:07:09' AS DATETIME2(0))"],
  ['datetimeoffset max offset', "CAST('2024-03-05 14:07:09 +14:00' AS DATETIMEOFFSET(0))"],
  ['time max', "CAST('23:59:59.9999999' AS TIME(7))"],
  ['varchar', "'1234'"], ['nvarchar', "N'1234'"], ['uniqueidentifier', "CAST('00000000-0000-0000-0000-000000000001' AS UNIQUEIDENTIFIER)"],
  ['varbinary', '0x01'], ['sql_variant int', 'CAST(CAST(5 AS INT) AS SQL_VARIANT)'], ['xml', "CAST('<a/>' AS XML)"],
].map(([name, value]) => [`type ${name}`, columns(value, typeFormats)])

const caseNull = [
  // NULL and argument handling
  ['null untyped value', "SELECT FORMAT(NULL,'N') AS value"],
  ['null typed value', "SELECT FORMAT(CAST(NULL AS INT),'N') AS value"],
  ['null typed date value', "SELECT FORMAT(CAST(NULL AS DATETIME2),'d','en-US') AS value"],
  ['null format', "SELECT FORMAT(1,NULL) AS value"],
  ['null typed format', "SELECT FORMAT(1,CAST(NULL AS NVARCHAR(10))) AS value"],
  ['null culture', "SELECT FORMAT(1,'N',NULL) AS value"],
  ['null typed culture', "SELECT FORMAT(1,'N',CAST(NULL AS NVARCHAR(10))) AS value"],
  ['null value unknown culture', "SELECT FORMAT(CAST(NULL AS INT),'N','xx-XX') AS value"],
  ['null value rejected culture', "SELECT FORMAT(CAST(NULL AS INT),'N','Klingon') AS value"],
  ['null value null culture', "SELECT FORMAT(CAST(NULL AS INT),'N',CAST(NULL AS NVARCHAR(10))) AS value"],
  ['null typed format rejected culture', "SELECT FORMAT(1,CAST(NULL AS NVARCHAR(10)),'Klingon') AS value"],
  ['null format unknown culture', "SELECT FORMAT(1,NULL,'xx-XX') AS value"],
  ['empty format', "SELECT FORMAT(1234.5,'') AS value"],
  ['empty culture', "SELECT FORMAT(1234.5,'N','') AS value"],
  ['varchar format', "SELECT FORMAT(1234.5,CAST('N' AS VARCHAR(5))) AS value"],
  ['nvarchar max format', "SELECT FORMAT(1234.5,CAST(N'N' AS NVARCHAR(MAX))) AS value"],
  ['int format', "SELECT FORMAT(1234.5,1) AS value"],
  ['int culture', "SELECT FORMAT(1234.5,'N',1033) AS value"],
  ['too few arguments', "SELECT FORMAT(1) AS value"],
  ['too many arguments', "SELECT FORMAT(1,'N','en-US',1) AS value"],
  // cultures
  ['unknown culture', "SELECT FORMAT(1,'N','xx-XX') AS value"],
  ['unknown culture underscore', "SELECT FORMAT(1,'N','en_US') AS value"],
  ['invalid culture word', "SELECT FORMAT(1,'N','Klingon') AS value"],
  ['unknown culture with date', "SELECT FORMAT(CAST('2024-03-05 14:07:09' AS DATETIME2(0)),'D','xx-XX') AS value"],
  ['invalid culture after row', "SELECT 1 AS before_value; SELECT FORMAT(1,'N','Klingon') AS value; SELECT 2 AS after_value"],
  ['null typed format de-DE', "SELECT FORMAT(CAST(1234.5 AS DECIMAL(10,2)),CAST(NULL AS NVARCHAR(10)),'de-DE') AS value"],
  // culture name acceptance: one program per name because a rejection aborts the statement
  ...['iv', 'Invariant Language (Invariant Country)', 'tlh', 'zz', 'x', 'abc', 'abcdefgh', 'abcdefghi', 'Klingo', 'klingon',
    'en-USA', 'de_DE', 'en-US-x-test', 'qps-ploc', 'C', 'POSIX', 'en-US ', 'EN-us', 'en-US-POSIX', 'sr-Latn-RS', 'zh-TW']
    .map(culture => [`culture name ${culture}`, `SELECT FORMAT(-1234.5,'N','${culture}') AS n,FORMAT(CAST('2024-03-05 14:07:09' AS DATETIME2(0)),'D','${culture}') AS d`]),
  ['culture lowercase', "SELECT FORMAT(1234.5,'N','de-de') AS lower_value,FORMAT(1234.5,'N','DE-DE') AS upper_value"],
  ['culture neutral', "SELECT FORMAT(1234.5,'N','de') AS de,FORMAT(1234.5,'N','fr') AS fr,FORMAT(1234.5,'N','ja') AS ja,FORMAT(1234.5,'N','ar') AS ar"],
  ['culture other', "SELECT FORMAT(1234.5,'N','de-CH') AS de_ch,FORMAT(1234.5,'N','en-IN') AS en_in,FORMAT(1234567.5,'N','hi-IN') AS hi_in,FORMAT(1234.5,'C','zh-Hans') AS zh_hans,FORMAT(1234.5,'N','sv-SE') AS sv_se"],
  ['culture lcid string', "SELECT FORMAT(1234.5,'N','1031') AS value"],
  ['culture padded', "SELECT FORMAT(1234.5,'N',' de-DE') AS value"],
  // language defaults
  ['language default us_english', "SELECT @@LANGUAGE AS language, FORMAT(1234.5,'N') AS n, FORMAT(CAST('2024-03-05' AS DATE),'D') AS d"],
  ['language german default', "SET LANGUAGE German; SELECT @@LANGUAGE AS language, FORMAT(1234.5,'N') AS n, FORMAT(CAST('2024-03-05' AS DATE),'D') AS d, FORMAT(1234.5,'N','en-US') AS explicit; SET LANGUAGE us_english"],
  ['language japanese default', "SET LANGUAGE Japanese; SELECT @@LANGUAGE AS language, FORMAT(1234.5,'C') AS c, FORMAT(CAST('2024-03-05' AS DATE),'D') AS d; SET LANGUAGE us_english"],
  ['language british default', "SET LANGUAGE British; SELECT @@LANGUAGE AS language, FORMAT(CAST('2024-03-05' AS DATE),'d') AS d, FORMAT(1234.5,'C') AS c; SET LANGUAGE us_english"],
  // result type and length
  ['result variant properties', "SELECT CAST(SQL_VARIANT_PROPERTY(FORMAT(1,'N'),'BaseType') AS NVARCHAR(128)) AS base_type,CAST(SQL_VARIANT_PROPERTY(FORMAT(1,'N'),'MaxLength') AS INT) AS max_length,CAST(SQL_VARIANT_PROPERTY(FORMAT(1,'N'),'Collation') AS NVARCHAR(128)) AS collation_name"],
  ['result describe', "SELECT name,system_type_name,max_length,is_nullable,collation_name FROM sys.dm_exec_describe_first_result_set(N'SELECT FORMAT(1,''N'') AS a,FORMAT(SYSDATETIME(),''d'',''de-DE'') AS b,FORMAT(v,f,c) AS c FROM dbo.fmt_nn',NULL,0) ORDER BY column_ordinal"],
  ['result long output', "SELECT LEN(FORMAT(1,REPLICATE(N'0',4000))) AS len_value"],
  ['result format over 4000', "SELECT LEN(FORMAT(1,REPLICATE(CAST(N'0' AS NVARCHAR(MAX)),4001))) AS len_value"],
  ['result truncated format concatenation', "SELECT LEN(FORMAT(1,REPLICATE(N'0',3000)+N''''+REPLICATE(N'x',1000)+N'''')) AS len_value"],
  ['result expands beyond 4000', "SELECT LEN(v) AS len_value,DATALENGTH(v) AS data_length,RIGHT(v,9) AS tail FROM (SELECT FORMAT(CAST('2024-03-05' AS DATE),REPLICATE(N'dddd ',800),'en-US') AS v) AS x"],
  ['result expands beyond 4000 max', "SELECT LEN(v) AS len_value,DATALENGTH(v) AS data_length FROM (SELECT FORMAT(CAST('2024-03-05' AS DATE),CAST(REPLICATE(CAST(N'dddd ' AS NVARCHAR(MAX)),800) AS NVARCHAR(MAX)),'en-US') AS v) AS x"],
  ['result collate', "SELECT FORMAT(1,'N') COLLATE Latin1_General_BIN2 AS value"],
  ['result into table', "SELECT FORMAT(1,'N') AS v INTO #fmt_into; SELECT name,TYPE_NAME(system_type_id) AS type_name,max_length,is_nullable,collation_name FROM tempdb.sys.columns WHERE object_id=OBJECT_ID('tempdb..#fmt_into'); DROP TABLE #fmt_into"],
  ['persisted computed rejected', "CREATE TABLE dbo.fmt_computed(v INT,f AS FORMAT(v,'N') PERSISTED)"],
  ['computed not persisted', "CREATE TABLE dbo.fmt_computed2(v INT,f AS FORMAT(v,'N')); INSERT dbo.fmt_computed2(v) VALUES (1234); SELECT f FROM dbo.fmt_computed2; DROP TABLE dbo.fmt_computed2"],
  ['deterministic property', "SELECT OBJECTPROPERTY(OBJECT_ID('dbo.fmt_view'),'IsDeterministic') AS is_deterministic,COLUMNPROPERTY(OBJECT_ID('dbo.fmt_view'),'f','IsDeterministic') AS column_deterministic"],
  // rows
  ['columns', "SELECT id,FORMAT(v,f,c) AS value,FORMAT(d,f,c) AS date_value FROM dbo.fmt_src ORDER BY id"],
  ['columns invalid culture row', "SELECT id,FORMAT(v,'N',c) AS value FROM dbo.fmt_bad ORDER BY id"],
  ['where filter', "SELECT id FROM dbo.fmt_src WHERE FORMAT(v,'N0','en-US')=N'1,235' ORDER BY id"],
]

const setup = [
  ['environment', "SELECT CAST(SERVERPROPERTY('ProductMajorVersion') AS INT) AS major, CAST(SERVERPROPERTY('Collation') AS NVARCHAR(128)) AS server_collation, DATABASEPROPERTYEX(DB_NAME(),'Collation') AS database_collation, @@LANGUAGE AS language"],
  ['create source', 'CREATE TABLE dbo.fmt_src(id INT CONSTRAINT fmt_src_pk PRIMARY KEY,v DECIMAL(10,2),d DATETIME2(3),f NVARCHAR(20),c NVARCHAR(10))'],
  ['insert source', "INSERT dbo.fmt_src VALUES (1,1234.5,'2024-03-05 14:07:09.123',N'N0',N'en-US'),(2,1234.5,'2024-03-05 14:07:09.123',N'N2',N'de-DE'),(3,NULL,NULL,N'N',N'fr-FR'),(4,1234.5,'2024-03-05 14:07:09.123',NULL,N'ja-JP'),(5,1234.5,'2024-03-05 14:07:09.123',N'Q',NULL),(6,-0.5,'2024-03-05 14:07:09.123',N'P',N'ar-SA')"],
  ['create bad culture source', 'CREATE TABLE dbo.fmt_bad(id INT CONSTRAINT fmt_bad_pk PRIMARY KEY,v INT,c NVARCHAR(10))'],
  ['insert bad culture source', "INSERT dbo.fmt_bad VALUES (1,1,N'en-US'),(2,2,N'xx-XX'),(3,3,N'de-DE')"],
  ['create not null source', 'CREATE TABLE dbo.fmt_nn(v INT NOT NULL,f NVARCHAR(10) NOT NULL,c NVARCHAR(10) NOT NULL)'],
  ['create view', "CREATE VIEW dbo.fmt_view WITH SCHEMABINDING AS SELECT FORMAT(v,'N') AS f FROM dbo.fmt_nn"],
]

const cases = [...setup, ...matrixCases, ...catalogCases, ...typeCases, ...caseNull]

// sp_executesql RPC: [name, sql, parameters]. Each statement text is unique.
const nv = (name, value, length = 20) => [name, TYPES.NVarChar, value, { length }]
const moment = new Date('2024-03-05T14:07:09.123Z')
const rpcCases = [
  ['rpc decimal', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.Decimal, -1234567.8951, { precision: 19, scale: 4 }], nv('f', 'N'), nv('c', 'de-DE')]],
  ['rpc int', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.Int, 1234], nv('f', 'D8'), nv('c', 'en-US')]],
  ['rpc bigint', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.BigInt, '9223372036854775807'], nv('f', 'N0'), nv('c', 'fr-FR')]],
  ['rpc money', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.Money, 1234567.8951], nv('f', 'C'), nv('c', 'ja-JP')]],
  ['rpc float', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.Float, 0.1], nv('f', 'R'), nv('c', 'ar-SA')]],
  ['rpc datetime2', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.DateTime2, moment, { scale: 7 }], nv('f', 'D'), nv('c', 'ar-SA')]],
  ['rpc datetimeoffset', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.DateTimeOffset, moment, { scale: 7 }], nv('f', 'O'), nv('c', 'en-US')]],
  ['rpc date', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.Date, moment], nv('f', 'D'), nv('c', 'ja-JP')]],
  ['rpc time', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.Time, moment, { scale: 7 }], nv('f', 'hh\\:mm\\:ss\\.fff'), nv('c', 'en-US')]],
  ['rpc null culture', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.Int, 1234], nv('f', 'N'), nv('c', null)]],
  ['rpc null format', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.Int, 1234], nv('f', null), nv('c', 'en-US')]],
  ['rpc unknown culture', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.Int, 1234], nv('f', 'N'), nv('c', 'xx-XX')]],
  ['rpc invalid culture', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.Int, 1234], nv('f', 'N'), nv('c', 'Klingon')]],
  ['rpc invalid format', 'SELECT FORMAT(@v,@f,@c) AS value', [['v', TYPES.Int, 1234], nv('f', 'Q'), nv('c', 'en-US')]],
  ['rpc varchar format', 'SELECT FORMAT(@v,@f) AS value', [['v', TYPES.Int, 1234], ['f', TYPES.VarChar, 'N', { length: 20 }]]],
  ['rpc nvarchar max format', 'SELECT FORMAT(@v,@f) AS value', [['v', TYPES.Int, 1234], nv('f', 'N', Infinity)]],
  ['rpc nvarchar value', 'SELECT FORMAT(@v,@f) AS value', [nv('v', '1234'), nv('f', 'N')]],
].map(([name, sql, parameters]) => [name, `${sql} /*${name}*/`, parameters])

// sp_prepare/sp_execute: one handle per statement, executed with each value set.
// Rejected cultures are followed by valid executions to prove no stale error.
const preparedCases = [
  ['prepared decimal', 'SELECT FORMAT(@v,@f,@c) AS value',
    [['v', TYPES.Decimal, { precision: 19, scale: 4 }], ['f', TYPES.NVarChar, { length: 20 }], ['c', TYPES.NVarChar, { length: 10 }]],
    [{ v: 1234.5, f: 'N', c: 'en-US' }, { v: 1234.5, f: 'N', c: 'de-DE' }, { v: 1234.5, f: 'N', c: 'xx-XX' },
      { v: 1234.5, f: 'C', c: 'fr-FR' }, { v: 1234.5, f: 'Q', c: 'en-US' }, { v: null, f: 'N', c: 'en-US' },
      { v: 1234.5, f: null, c: 'en-US' }, { v: 1234.5, f: 'N', c: null }, { v: -0.5, f: 'P', c: 'ar-SA' }]],
  ['prepared datetime2', 'SELECT FORMAT(@v,@f,@c) AS value',
    [['v', TYPES.DateTime2, { scale: 7 }], ['f', TYPES.NVarChar, { length: 40 }], ['c', TYPES.NVarChar, { length: 10 }]],
    [{ v: moment, f: 'D', c: 'en-US' }, { v: moment, f: 'D', c: 'ja-JP' }, { v: moment, f: 'd', c: 'Klingon' },
      { v: moment, f: 'yyyy-MM-dd', c: 'ar-SA' }, { v: moment, f: 'G', c: 'fr-FR' }]],
].map(([name, sql, declarations, values]) => [name, `${sql} /*${name}*/`, declarations, values])

// Return statuses are recorded only from RETURNSTATUS tokens that arrive while
// the request is outstanding. tedious keeps the token value on the connection
// (procReturnStatusValue) and hands it to the next 'doneProc' listener, so the
// doneProc argument is never used as evidence. This accessor keeps tedious'
// own behavior and reports each token to the request that is currently being
// observed. It never reads a previous value.
function returnStatusTokens(connection) {
  if (connection.msduckReturnStatus) return connection.msduckReturnStatus
  let stored = connection.procReturnStatusValue
  const tokens = { sink: null }
  Object.defineProperty(connection, 'procReturnStatusValue', {
    configurable: true,
    get: () => stored,
    set: value => {
      stored = value
      if (value !== undefined && tokens.sink) tokens.sink(value)
    },
  })
  connection.msduckReturnStatus = tokens
  return tokens
}

// Runs one request with a fresh per-request status. Returns null when no
// RETURNSTATUS token arrived during it, or the last token value otherwise.
async function withReturnStatus(connection, run) {
  const tokens = returnStatusTokens(connection)
  let status = null
  tokens.sink = value => { status = value }
  try {
    const result = await run()
    return { result, status }
  } finally { tokens.sink = null }
}

async function rpc(connection, sql, parameters) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      for (const [name, type, value, options] of parameters) request.addParameter(name, type, value, options)
      connection.execSql(request)
    },
  }
  const { result, status } = await withReturnStatus(connection, () => capture(transport, sql))
  result.returnStatus = status
  return result
}

// Replicates capturePrepared from the owner's prepared-capture helper (PR #312,
// docs/reference-captures.md) until it reaches main: sp_prepare completion comes
// from the 'prepared'/'error' events, request.error is cleared before each phase,
// an error object already attributed to an earlier phase is never reported again,
// and only tokens raised while a phase is outstanding are recorded. Each phase's
// returnStatus starts as null and is set only by a RETURNSTATUS token received
// during that phase.
const PREPARED_LIMITS = Object.freeze({ rows: 10000, messages: 200 })
const messageFields = m => ({ number: m.number, state: m.state, class: m.class, lineNumber: m.lineNumber, message: m.message })
const columnFields = c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })

async function capturePrepared(connection, sql, declarations, valueSets) {
  const limits = PREPARED_LIMITS
  let result
  let complete = () => {}
  const reported = new Set()
  const fresh = () => ({ sets: [], done: [], errors: [], info: [], returnStatus: null })
  const overflow = kind => { result.truncated ??= { rows: 0, messages: 0 }; result.truncated[kind]++ }
  const message = list => token => {
    if (!result) return
    if (result.errors.length + result.info.length >= limits.messages) overflow('messages')
    else result[list].push(messageFields(token))
  }
  const onError = message('errors')
  const onInfo = message('info')
  const request = new Request(sql, (error, rowCount) => complete(error, rowCount))
  for (const [name, type, parameterOptions] of declarations) request.addParameter(name, type, undefined, parameterOptions)
  request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(columnFields), rows: [] }))
  request.on('row', columns => {
    if (!result) return
    const set = result.sets.at(-1)
    if (!set || set.rows.length >= limits.rows) overflow('rows')
    else set.rows.push(columns.map(c => c.value))
  })
  for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => {
    if (!result) return
    if (result.done.length >= limits.messages) overflow('messages')
    else result.done.push({ kind, rowCount: rowCount ?? null, more })
  })
  const tokens = returnStatusTokens(connection)
  const phase = start => new Promise(resolve => {
    result = fresh()
    const current = result
    tokens.sink = value => { current.returnStatus = value }
    complete = (error, rowCount) => {
      complete = () => {}
      tokens.sink = null
      const finished = result
      result = undefined
      finished.rowCount = rowCount
      if (error && !reported.has(error) && !finished.errors.length) finished.errors.push({ message: error.message, number: error.number ?? null })
      if (error) reported.add(error)
      resolve(finished)
    }
    request.error = undefined
    start()
  })
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
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
    if (request.handle === undefined) return { prepare, prepared: false, executions: [], unprepare: null }
    const executions = []
    for (const values of valueSets) executions.push({ values, result: await phase(() => connection.execute(request, values)) })
    const unprepare = await phase(() => connection.unprepare(request))
    return { prepare, prepared: true, executions, unprepare }
  } finally {
    tokens.sink = null
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

const describeOptions = options => options ? { options: { ...options, ...(options.length === Infinity ? { length: 'max' } : {}) } } : {}

async function batch(connection, sql) {
  const { result, status } = await withReturnStatus(connection, () => capture(connection, sql))
  result.returnStatus = status
  return result
}

async function observe(connection) {
  const records = []
  for (const [name, sql] of cases) records.push({ name, sql, result: canonical(await batch(connection, sql)) })
  for (const [name, sql, parameters] of rpcCases) records.push({
    name, sql, protocol: 'sp_executesql',
    parameters: parameters.map(([parameter, type, value, options]) => ({ name: parameter, type: type.name, value: canonical(value), ...describeOptions(options) })),
    result: canonical(await rpc(connection, sql, parameters)),
  })
  for (const [name, sql, declarations, valueSets] of preparedCases) {
    const captured = await capturePrepared(connection, sql, declarations, valueSets)
    records.push({
      name, sql, protocol: 'sp_prepare/sp_execute/sp_unprepare',
      parameters: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, ...describeOptions(options) })),
      prepare: canonical(captured.prepare),
      prepared: captured.prepared,
      executions: captured.executions.map(({ values, result }) => ({ values: canonical(values), result: canonical(result) })),
      unprepare: canonical(captured.unprepare),
    })
  }
  const reuse = 'SELECT @@TRANCOUNT AS transaction_count, @@LANGUAGE AS language, 1 AS reusable'
  records.push({ name: 'reuse', sql: reuse, result: canonical(await batch(connection, reuse)) })
  return records
}

// Targeted assertions on individual values only; never whole captures.
function validate(run) {
  assert.equal(run.length, cases.length + rpcCases.length + preparedCases.length + 1)
  const get = name => {
    const record = run.find(record => record.name === name)
    if (!record) throw new Error('missing ' + name)
    return record
  }
  const result = name => get(name).result
  const cell = (name, column = 0, set = 0) => {
    const found = result(name).sets[set]
    const index = typeof column === 'number' ? column : found?.columns.findIndex(c => c.name === column)
    return found?.rows[0]?.[index]
  }
  for (const [name] of setup) assert.equal(result(name).errors.length, 0, name)
  for (const record of run) {
    if (record.result) assert(record.result.done.length > 0, record.name + ': no completion')
    else for (const execution of record.executions) assert(execution.result.done.length > 0, record.name + ': no completion')
  }
  for (const [name, column, expected] of [
    ['matrix decimal N', 'c_en_us', '-1,234,567.90'],
    ['matrix decimal N', 'c_default', '-1,234,567.90'],
    ['matrix decimal N', 'c_iv', '-1,234,567.90'],
    ['matrix decimal N', 'c_de_de', '-1.234.567,90'],
    ['matrix decimal C', 'c_en_us', '($1,234,567.90)'],
    ['matrix int D8', 'c_en_us', '-01234567'],
    ['matrix datetime2 d', 'c_iv', '03/05/2024'],
    ['matrix datetime2 yyyy-MM-dd', 'c_ar_sa', '1445-08-24'],
    ['matrix datetimeoffset U', 'c_en_us', null],
    ['numeric standard int', 'x', '4D2'],
    ['numeric standard int', 'r', null],
    ['numeric standard int', 'n100', 'N11234'],
    ['numeric standard decimal', 'x', null],
    ['numeric custom negative', 'sections', '(1234567.89)'],
    ['time formats', 'hh_colon', null],
    ['type int', 'g', '-2147483648'],
    ['type date min', 'n2', 'N2'],
    ['null typed value', 0, null],
    ['null typed format', 0, '1'],
    ['unknown culture', 0, '1.00'],
    ['culture name iv', 'n', '-1,234.50'],
    ['rpc datetimeoffset', 0, '2024-03-05T14:07:09.1230000+00:00'],
  ]) assert.equal(cell(name, column), expected, `${name} ${column}`)
  for (const name of ['matrix decimal N', 'type int', 'null typed value', 'rpc decimal']) {
    const column = result(name).sets[0].columns[0]
    assert.equal(column.type, 'NVarChar', name)
    assert.equal(column.length, 8000, name)
  }
  for (const [name, number] of [
    ['empty culture', 9818], ['null culture', 9818], ['invalid culture word', 9818], ['culture name x', 9818],
    ['rpc invalid culture', 9818], ['rpc null culture', 9818], ['null untyped value', 8116], ['null format', 8116],
    ['type bit', 8116], ['type nvarchar', 8116], ['too few arguments', 189], ['result format over 4000', 8152],
    ['persisted computed rejected', 4936],
  ]) assert.equal(result(name).errors[0]?.number, number, name)
  const preparedDecimal = get('prepared decimal')
  assert.equal(preparedDecimal.prepared, true)
  assert.equal(preparedDecimal.executions[7].result.errors[0]?.number, 9818)
  for (const index of [0, 1, 2, 3, 4, 5, 6, 8]) assert.equal(preparedDecimal.executions[index].result.errors.length, 0, 'prepared decimal execution ' + index)
  // Statuses come only from RETURNSTATUS tokens received during each request.
  assert.equal(preparedDecimal.prepare.returnStatus, 8116, 'sp_prepare after a failed RPC')
  assert.equal(preparedDecimal.executions[7].result.returnStatus, -6)
  assert.equal(preparedDecimal.executions[8].result.returnStatus, 0)
  assert.equal(result('rpc null culture').returnStatus, 9818)
  assert.equal(result('rpc decimal').returnStatus, 0)
  assert.equal(result('matrix decimal N').returnStatus, null)
  const preparedDate = get('prepared datetime2')
  assert.equal(preparedDate.prepare.returnStatus, 0, 'sp_prepare after a successful sp_unprepare')
  assert.equal(preparedDate.executions[2].result.errors[0]?.number, 9818)
  for (const index of [0, 1, 3, 4]) assert.equal(preparedDate.executions[index].result.errors.length, 0, 'prepared datetime2 execution ' + index)
  assert.equal(result('reuse').errors.length, 0)
}

// Refuse before any container starts; existence check only.
if (writeFixture) await refuseExistingFixture(fixture)

await mkdir(resolve(output, '..'), { recursive: true })
const count = oneDatabase ? 1 : 2
const containers = []
for (let containerIndex = 0; containerIndex < count; containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    console.error(`container ${container.name} started`)
    const runs = []
    for (let databaseIndex = 0; databaseIndex < count; databaseIndex++) {
      const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, observe)
      if (!oneDatabase) validate(run)
      runs.push(run)
    }
    if (!oneDatabase) assertSameCapture(runs[1], runs[0], 'FORMAT observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) assertSameCapture(containers[1].runs[0], containers[0].runs[0], 'FORMAT observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
if (!writeFixture && !oneDatabase) {
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
}
if (retained) assertSameCapture(actual, retained, 'FORMAT observations differ from retained fixture')
if (writeFixture) await writeNewFixture(fixture, actual)
const executions = preparedCases.reduce((total, entry) => total + entry[3].length, 0)
console.log(`Captured ${containers[0].runs[0].length} FORMAT programs (${cases.length} batches, ${rpcCases.length} sp_executesql, ` +
  `${preparedCases.length} prepared sequences with ${executions} executions, 1 reuse check)` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained ? ' and matched retained fixture' : '') + (writeFixture ? '; retained fixture written' : ''))
