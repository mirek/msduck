#!/usr/bin/env node
// Retain SQL Server ISNUMERIC and ISDATE rows, descriptors, diagnostics and
// completions for ordinary batches, sp_executesql RPC and prepared handles.
import assert from 'node:assert/strict'
import { mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/isnumeric-isdate.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/compatibility/isnumeric-isdate/capture.json')

// Each probe row reports ISNUMERIC next to TRY_CONVERT results so the fixture
// shows which target types actually accept an ISNUMERIC=1 string.
const numericProbe = (label, values, prefix = '') => [label,
  `${prefix}SELECT id,v,ISNUMERIC(v) AS isnumeric_value,TRY_CONVERT(INT,v) AS as_int,TRY_CONVERT(BIGINT,v) AS as_bigint,TRY_CONVERT(DECIMAL(38,10),v) AS as_decimal,TRY_CONVERT(FLOAT,v) AS as_float,TRY_CONVERT(MONEY,v) AS as_money FROM (VALUES ${values.map((v, i) => `(${i + 1},${v})`).join(',')}) d(id,v) ORDER BY id`]

const dateProbe = (label, values, prefix = '', suffix = '') => [label,
  `${prefix}SELECT id,v,ISDATE(v) AS isdate_value,TRY_CONVERT(DATETIME,v) AS as_datetime,TRY_CONVERT(SMALLDATETIME,v) AS as_smalldatetime,TRY_CONVERT(DATE,v) AS as_date,TRY_CONVERT(DATETIME2(7),v) AS as_datetime2 FROM (VALUES ${values.map((v, i) => `(${i + 1},${v})`).join(',')}) d(id,v) ORDER BY id${suffix}`]

const q = s => `'${s.replaceAll("'", "''")}'`
const nq = s => `N'${s.replaceAll("'", "''")}'`

const numericStrings = [
  '0', '123', '-123', '+123', '1.5', '.5', '5.', '.', '+', '-', '+.', '-.', '1.2.3', '--1', '+-1', '-+1', '1-', '1+',
  '1e5', '1E5', '1e+5', '1e-5', '1e', 'e5', 'e', '1e1.5', '.e1', '1.e1', '1d5', '1D5', '1d', '1e5e5', '1ee5',
  '1,000', '1,000.50', ',', ',1', '1,', '1,,2', '1,2,3', '12,34', '1.000,5', ',.', '.,',
  ' 1', '1 ', ' 1 ', '1 000', '- 1', '+ 1', '', ' ', '   ',
  '0x1A', '0x', '0x0', '1A', 'A', 'abc', '1a', '&H1A', '%1', '1%',
  '$', '$1', '$-1', '-$1', '$ 1', '1$', '$1.5', '$$1', '$,', '$.', '$1,000', '$1e5', '+$1', '$+1',
  '\\1', '\\', '1\\', 'Inf', 'NaN', '-Inf', 'Infinity', '1/2', '1:2',
  '2147483647', '2147483648', '-2147483649', '9223372036854775808', '922337203685477.5807', '922337203685477.5808',
  '1e308', '1e309', '-1e309', '1e-400', '99999999999999999999999999999999999999', '999999999999999999999999999999999999999',
]

const unicodeNumericStrings = [
  ['currency euro', '€1'], ['currency pound', '£1'], ['currency yen', '¥1'], ['currency cent', '¢1'],
  ['currency generic', '¤1'], ['currency rupee', '₹1'], ['currency won', '₩1'], ['currency baht', '฿1'],
  ['currency fullwidth dollar', '＄1'], ['currency new shekel', '₪1'], ['currency euro alone', '€'],
  ['fullwidth digits', '１２３'], ['arabic indic digits', '١٢٣'], ['devanagari digits', '१२३'],
  ['superscript two', '²'], ['one half', '½'], ['roman numeral', 'Ⅻ'], ['fullwidth plus', '＋1'],
  ['fullwidth minus', '－1'], ['minus sign', '−1'], ['nbsp leading', ' 1'], ['ideographic space', '　1'],
  ['fullwidth period', '1．5'], ['arabic decimal separator', '1٫5'],
]

const whitespace = [
  ['tab leading', 'CHAR(9)+\'1\''], ['lf leading', 'CHAR(10)+\'1\''], ['cr leading', 'CHAR(13)+\'1\''],
  ['vt leading', 'CHAR(11)+\'1\''], ['ff leading', 'CHAR(12)+\'1\''], ['nbsp varchar leading', 'CHAR(160)+\'1\''],
  ['tab trailing', '\'1\'+CHAR(9)'], ['lf trailing', '\'1\'+CHAR(10)'], ['cr trailing', '\'1\'+CHAR(13)'],
  ['nul trailing', '\'1\'+CHAR(0)'], ['nul leading', 'CHAR(0)+\'1\''], ['tab inside', '\'1\'+CHAR(9)+\'2\''],
  ['tab after sign', '\'-\'+CHAR(9)+\'1\''], ['crlf trailing', '\'1\'+CHAR(13)+CHAR(10)'],
]

const dateStrings = [
  '2024-01-02', '20240102', '2024-01-02T03:04:05', '2024-01-02 03:04:05', '2024-01-02T03:04:05.123', '2024-01-02 03:04:05.1234',
  '2024-01-02T03:04:05.1234567', '2024-01-02T03:04:05Z', '2024-01-02T03:04:05.123Z', '2024-01-02T03:04:05+02:00',
  '2024-01-02 03:04:05 +02:00', '2024-02-29', '2023-02-29', '2024-02-30', '2024-13-01', '2024-00-01', '2024-01-00', '2024-01-32',
  '1753-01-01', '1752-12-31', '9999-12-31', '9999-12-31 23:59:59.997', '9999-12-31 23:59:59.998', '9999-12-31 23:59:59.999',
  '0001-01-01', '10000-01-01', '1900-01-01', '1899-12-31', '2079-06-06', '2079-06-07',
  '01/02/2024', '13/01/2024', '01/13/2024', '1/2/24', '01-02-2024', '01.02.2024', '2024/01/02', '2024.01.02', '2024-1-2',
  '240102', '2024', '24', '1', '0', '202401', '2024-01', '2024-01-02T', 'T03:04:05', '2024W01', '2024-W01-1',
  'Jan 2 2024', 'January 2, 2024', '2 January 2024', '2 Jan 2024', 'Jan 2024', 'January', 'Jan', '2024 Jan 2', 'Janu 2 2024',
  'Monday, January 1, 2024', '12:00', '12:00 PM', '12:00:00.000', '23:59:59.999', '24:00', '25:00', '12:60', '12 PM', '12:00 AM', '3PM',
  '2024-01-02 12:00 PM', '2024-01-02 13:00 PM', '2024-01-02 03:04', '2024-01-02 03:04:05:123', '2024-01-02 03.04.05',
  ' 2024-01-02', '2024-01-02 ', ' 2024-01-02 ', '', ' ', 'abc', '2024-01-02abc', '{ts \'2024-01-02 03:04:05\'}', '{d \'2024-01-02\'}',
]

const cases = [
  numericProbe('isnumeric ascii strings', numericStrings.map(q)),
  numericProbe('isnumeric nvarchar strings', numericStrings.slice(0, 40).map(nq)),
  numericProbe('isnumeric unicode strings', unicodeNumericStrings.map(([, v]) => nq(v))),
  numericProbe('isnumeric unicode strings as varchar', unicodeNumericStrings.map(([, v]) => `CAST(${nq(v)} AS VARCHAR(20))`)),
  numericProbe('isnumeric whitespace controls', whitespace.map(([, v]) => v)),
  // Four rows precede error 8152 raised by the 9000-character fifth row.
  numericProbe('isnumeric long strings', ["REPLICATE('9',309)", "REPLICATE('9',400)", "'1'+REPLICATE('0',4000)", "'.'+REPLICATE('0',4000)+'1'", "REPLICATE(CAST('9' AS VARCHAR(MAX)),9000)"]),
  ...[310, 4000, 4001, 8000, 8001].map(n => [`isnumeric varchar max length ${n}`, `SELECT ISNUMERIC(REPLICATE(CAST('1' AS VARCHAR(MAX)),${n})) AS h`]),
  ...[310, 4000, 4001, 8000, 8001].map(n => [`isnumeric nvarchar max length ${n}`, `SELECT ISNUMERIC(REPLICATE(CAST(N'1' AS NVARCHAR(MAX)),${n})) AS h`]),
  ...[4000, 4001, 8000, 8001].map(n => [`isnumeric padded varchar max length ${n}`, `SELECT ISNUMERIC(REPLICATE(CAST(' ' AS VARCHAR(MAX)),${n - 1})+'1') AS h`]),
  ['isnumeric float range boundaries', "SELECT ISNUMERIC('1'+REPLICATE('0',308)) AS digits_309,ISNUMERIC('1'+REPLICATE('0',309)) AS digits_310,ISNUMERIC('1.7976931348623157e308') AS float_max,ISNUMERIC('1.7976931348623159e308') AS above_float_max,ISNUMERIC('-1.7976931348623159e308') AS below_float_min,ISNUMERIC('4.9e-324') AS denormal_min,ISNUMERIC('0.'+REPLICATE('0',400)+'1') AS tiny_fraction,ISNUMERIC('$'+REPLICATE('9',309)) AS currency_overflow,ISNUMERIC(REPLICATE('9',308)+'.9') AS fraction_near_limit"],
  ['isnumeric unicode single characters', "SELECT STRING_AGG(CAST(value AS VARCHAR(MAX)),',') WITHIN GROUP (ORDER BY value) AS code_points,COUNT(*) AS count_value FROM GENERATE_SERIES(0,65535) WHERE ISNUMERIC(NCHAR(value))=1"],
  ['isnumeric unicode prefix characters', "SELECT STRING_AGG(CAST(value AS VARCHAR(MAX)),',') WITHIN GROUP (ORDER BY value) AS code_points,COUNT(*) AS count_value FROM GENERATE_SERIES(0,65535) WHERE ISNUMERIC(NCHAR(value)+N'1')=1"],
  ['isnumeric unicode suffix characters', "SELECT STRING_AGG(CAST(value AS VARCHAR(MAX)),',') WITHIN GROUP (ORDER BY value) AS code_points,COUNT(*) AS count_value FROM GENERATE_SERIES(0,65535) WHERE ISNUMERIC(N'1'+NCHAR(value))=1"],
  ['isnumeric unicode infix characters', "SELECT STRING_AGG(CAST(value AS VARCHAR(MAX)),',') WITHIN GROUP (ORDER BY value) AS code_points,COUNT(*) AS count_value FROM GENERATE_SERIES(0,65535) WHERE ISNUMERIC(N'1'+NCHAR(value)+N'2')=1"],
  ['isnumeric varchar single bytes', "SELECT STRING_AGG(CAST(value AS VARCHAR(MAX)),',') WITHIN GROUP (ORDER BY value) AS code_points,COUNT(*) AS count_value FROM GENERATE_SERIES(0,255) WHERE ISNUMERIC(CHAR(value))=1"],
  ['isnumeric varchar prefix bytes', "SELECT STRING_AGG(CAST(value AS VARCHAR(MAX)),',') WITHIN GROUP (ORDER BY value) AS code_points,COUNT(*) AS count_value FROM GENERATE_SERIES(0,255) WHERE ISNUMERIC(CHAR(value)+'1')=1"],
  ['isnumeric varchar suffix bytes', "SELECT STRING_AGG(CAST(value AS VARCHAR(MAX)),',') WITHIN GROUP (ORDER BY value) AS code_points,COUNT(*) AS count_value FROM GENERATE_SERIES(0,255) WHERE ISNUMERIC('1'+CHAR(value))=1"],
  ['isnumeric typed numerics', "SELECT ISNUMERIC(1) AS int_value,ISNUMERIC(CAST(1 AS TINYINT)) AS tinyint_value,ISNUMERIC(CAST(1 AS BIGINT)) AS bigint_value,ISNUMERIC(1.5) AS decimal_value,ISNUMERIC(CAST(1.5 AS FLOAT)) AS float_value,ISNUMERIC(CAST(1.5 AS REAL)) AS real_value,ISNUMERIC(CAST(1.5 AS MONEY)) AS money_value,ISNUMERIC(CAST(1.5 AS SMALLMONEY)) AS smallmoney_value,ISNUMERIC(CAST(1 AS BIT)) AS bit_value,ISNUMERIC(1e5) AS float_literal"],
  ['isnumeric typed nulls', "SELECT ISNUMERIC(NULL) AS untyped,ISNUMERIC(CAST(NULL AS INT)) AS int_null,ISNUMERIC(CAST(NULL AS VARCHAR(10))) AS varchar_null,ISNUMERIC(CAST(NULL AS NVARCHAR(10))) AS nvarchar_null"],
  ['isnumeric datetime', "SELECT ISNUMERIC(CAST('2024-01-02' AS DATETIME)) AS a,ISNUMERIC(CAST('2024-01-02' AS SMALLDATETIME)) AS b"],
  ['isnumeric date', "SELECT ISNUMERIC(CAST('2024-01-02' AS DATE)) AS h"],
  ['isnumeric date null', 'SELECT ISNUMERIC(CAST(NULL AS DATE)) AS h'],
  ['isnumeric datetime2', "SELECT ISNUMERIC(CAST('2024-01-02' AS DATETIME2)) AS h"],
  ['isnumeric time', "SELECT ISNUMERIC(CAST('03:04' AS TIME)) AS h"],
  ['isnumeric datetimeoffset', "SELECT ISNUMERIC(CAST('2024-01-02T00:00:00+02:00' AS DATETIMEOFFSET)) AS h"],
  ['isnumeric uniqueidentifier', "SELECT ISNUMERIC(CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER)) AS h"],
  ['isnumeric binary', "SELECT ISNUMERIC(0x31) AS a,ISNUMERIC(CAST('1' AS VARBINARY(10))) AS b"],
  ['isnumeric char padding', "SELECT ISNUMERIC(CAST('1' AS CHAR(10))) AS a,ISNUMERIC(CAST(N'1' AS NCHAR(10))) AS b,ISNUMERIC(CAST('' AS CHAR(3))) AS c"],
  ['isnumeric varchar max', "SELECT ISNUMERIC(CAST('12' AS VARCHAR(MAX))) AS a,ISNUMERIC(CAST(N'12' AS NVARCHAR(MAX))) AS b"],
  ['isnumeric sql_variant', "SELECT ISNUMERIC(CAST(1 AS SQL_VARIANT)) AS a,ISNUMERIC(CAST('1' AS SQL_VARIANT)) AS b,ISNUMERIC(CAST('x' AS SQL_VARIANT)) AS c"],
  ['isnumeric text', "SELECT ISNUMERIC(CAST('1' AS TEXT)) AS h"],
  ['isnumeric ntext', "SELECT ISNUMERIC(CAST(N'1' AS NTEXT)) AS h"],
  ['isnumeric xml', "SELECT ISNUMERIC(CAST('<a>1</a>' AS XML)) AS h"],
  ['isnumeric collation clause', "SELECT ISNUMERIC('1' COLLATE Latin1_General_BIN2) AS a,ISNUMERIC('$1' COLLATE Latin1_General_100_CI_AS_SC_UTF8) AS b"],
  ['isnumeric utf8 column', "SELECT id,ISNUMERIC(utf8) AS h FROM dbo.isnumeric_src ORDER BY id"],
  ['isnumeric columns', "SELECT id,ISNUMERIC(v) AS v,ISNUMERIC(n) AS n,ISNUMERIC(i) AS i FROM dbo.isnumeric_src ORDER BY id"],
  ['isnumeric where filter', "SELECT id FROM dbo.isnumeric_src WHERE ISNUMERIC(v)=1 ORDER BY id"],
  ['isnumeric no arguments', 'SELECT ISNUMERIC() AS h'],
  ['isnumeric two arguments', "SELECT ISNUMERIC('1','2') AS h"],
  ['isnumeric unnamed column', "SELECT ISNUMERIC('1')"],
  ['isnumeric expression arithmetic', "SELECT ISNUMERIC('1')+ISNUMERIC('x') AS s,ISNUMERIC('1')*1.5 AS m"],
  ['isnumeric select into descriptor', "SELECT ISNUMERIC('1') AS a,ISNUMERIC(CAST(NULL AS VARCHAR(10))) AS b,ISDATE('2024-01-02') AS c,ISDATE(CAST(NULL AS VARCHAR(10))) AS d INTO #isfn_into; SELECT c.name,t.name AS type_name,c.max_length,c.is_nullable FROM tempdb.sys.columns c JOIN tempdb.sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('tempdb..#isfn_into') ORDER BY c.column_id; DROP TABLE #isfn_into"],
  ['isnumeric describe first result set', "SELECT name,system_type_name,is_nullable FROM sys.dm_exec_describe_first_result_set(N'SELECT ISNUMERIC(''1'') AS a,ISDATE(''x'') AS b,ISNUMERIC(NULL) AS c,ISDATE(NULL) AS d',NULL,0) ORDER BY column_ordinal"],

  dateProbe('isdate strings default', dateStrings.map(q)),
  dateProbe('isdate nvarchar strings default', dateStrings.slice(0, 40).map(nq)),
  dateProbe('isdate dateformat dmy', ['01/02/2024', '13/01/2024', '01/13/2024', '2024-01-02', '2024-13-01', '2024-01-13', '20240113', '2024-01-02T03:04:05', '01.02.03', '31/12/99', '1/2/49', '1/2/50'].map(q), 'SET DATEFORMAT dmy; ', '; SET DATEFORMAT mdy'),
  dateProbe('isdate dateformat mdy', ['01/02/2024', '13/01/2024', '01/13/2024', '2024-01-02', '2024-13-01', '2024-01-13', '20240113', '2024-01-02T03:04:05', '01.02.03', '31/12/99', '1/2/49', '1/2/50'].map(q), 'SET DATEFORMAT mdy; ', '; SET DATEFORMAT mdy'),
  dateProbe('isdate dateformat ymd', ['01/02/2024', '24/01/13', '2024/13/01', '2024-01-02', '2024-13-01', '2024-01-13', '20240113', '2024-01-02T03:04:05', '01.02.03'].map(q), 'SET DATEFORMAT ymd; ', '; SET DATEFORMAT mdy'),
  dateProbe('isdate dateformat ydm', ['01/02/2024', '24/13/01', '2024/13/01', '2024-01-02', '2024-13-01', '2024-01-13', '20240113', '2024-01-02T03:04:05', '2024-13-01T03:04:05', '2024-13-01 03:04:05', '01.02.03'].map(q), 'SET DATEFORMAT ydm; ', '; SET DATEFORMAT mdy'),
  dateProbe('isdate dateformat myd', ['01/2024/02', '13/2024/01', '01/2024/13', '2024-01-02', '01/02/2024'].map(q), 'SET DATEFORMAT myd; ', '; SET DATEFORMAT mdy'),
  dateProbe('isdate dateformat dym', ['02/2024/01', '13/2024/01', '01/2024/13', '2024-01-02', '01/02/2024'].map(q), 'SET DATEFORMAT dym; ', '; SET DATEFORMAT mdy'),
  dateProbe('isdate language british', ['01/02/2024', '13/01/2024', '01/13/2024', '2024-01-13', '2024-13-01', 'January 2 2024'].map(q), 'SET LANGUAGE British; ', '; SET LANGUAGE us_english'),
  dateProbe('isdate language deutsch', ['13.01.2024', '01.13.2024', '2024-01-13', '2024-13-01', '2. Januar 2024', 'Januar 2 2024', 'Mai 1 2024', 'May 1 2024', 'Dezember 1 2024', 'December 1 2024', 'Mär 1 2024'].map(nq), 'SET LANGUAGE Deutsch; ', '; SET LANGUAGE us_english'),
  dateProbe('isdate language francais', ['13/01/2024', '01/13/2024', '2024-01-13', '2024-13-01', '2 janvier 2024', 'janvier 2 2024', 'février 1 2024', 'fevrier 1 2024', 'January 2 2024'].map(nq), 'SET LANGUAGE Français; ', '; SET LANGUAGE us_english'),
  dateProbe('isdate language japanese', ['2024/01/13', '2024/13/01', '13/01/2024', '01/13/2024', '2024-01-13', '2024-13-01', 'January 2 2024'].map(nq), 'SET LANGUAGE Japanese; ', '; SET LANGUAGE us_english'),
  dateProbe('isdate language then dateformat', ['13/01/2024', '01/13/2024'].map(q), 'SET LANGUAGE British; SET DATEFORMAT mdy; ', '; SET LANGUAGE us_english; SET DATEFORMAT mdy'),
  ['session set dateformat dmy', 'SET DATEFORMAT dmy'],
  dateProbe('isdate after session dateformat dmy', ['01/02/2024', '13/01/2024', '01/13/2024', '2024-01-13', '2024-13-01', '20240113'].map(q)),
  ['session set dateformat ydm', 'SET DATEFORMAT ydm'],
  dateProbe('isdate after session dateformat ydm', ['2024-01-13', '2024-13-01', '2024-13-01T03:04:05', '20240113'].map(q)),
  ['session restore dateformat mdy', 'SET DATEFORMAT mdy'],
  ['session set language deutsch', 'SET LANGUAGE Deutsch'],
  dateProbe('isdate after session language deutsch', ['13.01.2024', '01.13.2024', '2024-01-13', '2024-13-01', 'Januar 2 2024', 'January 2 2024'].map(nq)),
  ['session restore language us_english', 'SET LANGUAGE us_english'],
  ['isdate session options visible', "SET LANGUAGE Deutsch; SELECT @@LANGUAGE AS language_name,(SELECT date_format FROM sys.dm_exec_sessions WHERE session_id=@@SPID) AS date_format; SET LANGUAGE us_english; SELECT @@LANGUAGE AS language_name,(SELECT date_format FROM sys.dm_exec_sessions WHERE session_id=@@SPID) AS date_format"],
  dateProbe('isdate whitespace controls', ["CHAR(9)+'2024-01-02'", "'2024-01-02'+CHAR(9)", "CHAR(10)+'2024-01-02'", "'2024-01-02'+CHAR(13)+CHAR(10)", "CHAR(160)+'2024-01-02'", "'2024-01-02'+CHAR(0)", "N'　2024-01-02'"]),
  dateProbe('isdate unicode digits', ['２０２４-０１-０２', '٢٠٢٤-٠١-٠٢', '2024－01－02'].map(nq)),
  ...[4000, 4001, 8000, 8001].map(n => [`isdate padded varchar max length ${n}`, `SELECT ISDATE(REPLICATE(CAST(' ' AS VARCHAR(MAX)),${n - 10})+'2024-01-02') AS h`]),
  ...[4000, 4001].map(n => [`isdate padded nvarchar max length ${n}`, `SELECT ISDATE(REPLICATE(CAST(N' ' AS NVARCHAR(MAX)),${n - 10})+N'2024-01-02') AS h`]),
  ['isdate datetime input', "SELECT ISDATE(CAST('2024-01-02' AS DATETIME)) AS h"],
  ['isdate smalldatetime input', "SELECT ISDATE(CAST('2024-01-02' AS SMALLDATETIME)) AS h"],
  ['isdate typed character inputs', "SELECT ISDATE(CAST('2024-01-02' AS CHAR(20))) AS char_value,ISDATE(CAST(N'2024-01-02' AS NCHAR(20))) AS nchar_value,ISDATE(CAST('2024-01-02' AS VARCHAR(MAX))) AS varchar_max_value"],
  ['isdate date input', "SELECT ISDATE(CAST('2024-01-02' AS DATE)) AS h"],
  ['isdate datetime2 input', "SELECT ISDATE(CAST('2024-01-02' AS DATETIME2)) AS h"],
  ['isdate datetime2 early input', "SELECT ISDATE(CAST('0001-01-01' AS DATETIME2)) AS h"],
  ['isdate datetimeoffset input', "SELECT ISDATE(CAST('2024-01-02T00:00:00+02:00' AS DATETIMEOFFSET)) AS h"],
  ['isdate time input', "SELECT ISDATE(CAST('03:04:05' AS TIME)) AS h"],
  ['isdate int input', 'SELECT ISDATE(20240102) AS h'],
  ['isdate decimal input', 'SELECT ISDATE(1.5) AS h'],
  ['isdate float input', 'SELECT ISDATE(CAST(1 AS FLOAT)) AS h'],
  ['isdate money input', 'SELECT ISDATE(CAST(1 AS MONEY)) AS h'],
  ['isdate bit input', 'SELECT ISDATE(CAST(1 AS BIT)) AS h'],
  ['isdate binary input', 'SELECT ISDATE(0x31) AS h'],
  ['isdate uniqueidentifier input', "SELECT ISDATE(CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER)) AS h"],
  ['isdate typed nulls', "SELECT ISDATE(NULL) AS untyped,ISDATE(CAST(NULL AS VARCHAR(10))) AS varchar_null,ISDATE(CAST(NULL AS NVARCHAR(10))) AS nvarchar_null,ISDATE(CAST(NULL AS DATETIME)) AS datetime_null"],
  ['isdate int null', 'SELECT ISDATE(CAST(NULL AS INT)) AS h'],
  ['isdate sql_variant', "SELECT ISDATE(CAST('2024-01-02' AS SQL_VARIANT)) AS h"],
  ['isdate text', "SELECT ISDATE(CAST('2024-01-02' AS TEXT)) AS h"],
  ['isdate ntext', "SELECT ISDATE(CAST(N'2024-01-02' AS NTEXT)) AS h"],
  ['isdate xml', "SELECT ISDATE(CAST('<a/>' AS XML)) AS h"],
  ['isdate columns', "SELECT id,ISDATE(v) AS v,ISDATE(n) AS n,ISDATE(dt) AS dt FROM dbo.isdate_src ORDER BY id"],
  ['isdate date column', 'SELECT ISDATE(d) AS h FROM dbo.isdate_src'],
  ['isdate datetime2 column', 'SELECT ISDATE(d2) AS h FROM dbo.isdate_src'],
  ['isdate no arguments', 'SELECT ISDATE() AS h'],
  ['isdate two arguments', "SELECT ISDATE('2024-01-02','x') AS h"],
  ['isdate unnamed column', "SELECT ISDATE('2024-01-02')"],
  ['isdate in check constraint', "CREATE TABLE #isdate_check(v VARCHAR(30) CONSTRAINT ck_isdate_check CHECK(ISDATE(v)=1)); INSERT #isdate_check VALUES('2024-01-02'); INSERT #isdate_check VALUES('2024-13-01'); SELECT v FROM #isdate_check; DROP TABLE #isdate_check"],
  ['isdate computed column', "CREATE TABLE #isdate_computed(v VARCHAR(30),p AS ISDATE(v) PERSISTED); DROP TABLE IF EXISTS #isdate_computed"],
  ['isnumeric computed column', "CREATE TABLE #isnumeric_computed(v VARCHAR(30),p AS ISNUMERIC(v) PERSISTED); INSERT #isnumeric_computed(v) VALUES('1'),('$'),('x'); SELECT v,p FROM #isnumeric_computed ORDER BY v; DROP TABLE #isnumeric_computed"],
]

const setup = [
  ['create isnumeric source', 'CREATE TABLE dbo.isnumeric_src(id INT,v VARCHAR(30),n NVARCHAR(30),i INT,utf8 VARCHAR(30) COLLATE Latin1_General_100_CI_AS_SC_UTF8)'],
  ['insert isnumeric source', "INSERT dbo.isnumeric_src VALUES (1,'12',N'12',12,'12'),(2,'$',N'€',NULL,N'€1'),(3,NULL,NULL,NULL,NULL),(4,'1e5',N'１２',-1,N'１'),(5,'abc',N'abc',0,'abc')"],
  ['create isdate source', 'CREATE TABLE dbo.isdate_src(id INT,v VARCHAR(40),n NVARCHAR(40),dt DATETIME,d DATE,d2 DATETIME2)'],
  ['insert isdate source', "INSERT dbo.isdate_src VALUES (1,'2024-01-02',N'2024-01-02','2024-01-02','2024-01-02','2024-01-02'),(2,'0001-01-01',N'2024-13-01',NULL,NULL,NULL)"],
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

const rpcValues = [
  ['rpc nvarchar integer', TYPES.NVarChar, '123', { length: 40 }],
  ['rpc nvarchar currency', TYPES.NVarChar, '$1', { length: 40 }],
  ['rpc nvarchar euro', TYPES.NVarChar, '€1', { length: 40 }],
  ['rpc nvarchar fullwidth digits', TYPES.NVarChar, '１２３', { length: 40 }],
  ['rpc nvarchar exponent', TYPES.NVarChar, '1d5', { length: 40 }],
  ['rpc nvarchar comma', TYPES.NVarChar, '1,2', { length: 40 }],
  ['rpc nvarchar tab', TYPES.NVarChar, '\t1', { length: 40 }],
  ['rpc nvarchar empty', TYPES.NVarChar, '', { length: 40 }],
  ['rpc nvarchar date', TYPES.NVarChar, '2024-01-02', { length: 40 }],
  ['rpc nvarchar datetime2 only', TYPES.NVarChar, '2024-01-02T03:04:05.1234567', { length: 40 }],
  ['rpc nvarchar day first', TYPES.NVarChar, '13/01/2024', { length: 40 }],
  ['rpc nvarchar null', TYPES.NVarChar, null, { length: 40 }],
  ['rpc varchar integer', TYPES.VarChar, '123', { length: 40 }],
  ['rpc varchar euro', TYPES.VarChar, '€1', { length: 40 }],
  ['rpc varchar date', TYPES.VarChar, '2024-01-02', { length: 40 }],
  ['rpc varchar null', TYPES.VarChar, null, { length: 40 }],
  ['rpc int', TYPES.Int, 1, undefined],
  ['rpc int null', TYPES.Int, null, undefined],
  ['rpc float', TYPES.Float, 1.5, undefined],
  ['rpc money', TYPES.Money, 1.5, undefined],
  ['rpc bit', TYPES.Bit, true, undefined],
]

const rpcDateValues = [
  ['rpc isdate datetime', TYPES.DateTime, new Date(Date.UTC(2024, 0, 2, 3, 4, 5)), undefined],
  ['rpc isdate datetime2', TYPES.DateTime2, new Date(Date.UTC(2024, 0, 2, 3, 4, 5)), { scale: 7 }],
  ['rpc isdate date', TYPES.Date, new Date(Date.UTC(2024, 0, 2)), undefined],
  ['rpc isdate int', TYPES.Int, 20240102, undefined],
  ['rpc isdate varbinary', TYPES.VarBinary, Buffer.from('2024-01-02'), { length: 40 }],
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
  const bothSql = 'SELECT ISNUMERIC(@v) AS isnumeric_value,ISDATE(@v) AS isdate_value'
  for (const [name, type, value, options] of rpcValues) await record(name, bothSql, [['v', type, value, options]])
  const isdateSql = 'SELECT ISDATE(@v) AS isdate_value'
  for (const [name, type, value, options] of rpcDateValues) await record(name, isdateSql, [['v', type, value, options]])
  for (const n of [4000, 4001]) {
    await record(`rpc isnumeric nvarchar length ${n}`, 'SELECT ISNUMERIC(@v) AS isnumeric_value', [['v', TYPES.NVarChar, '1'.repeat(n), { length: n }]])
    await record(`rpc isdate nvarchar length ${n}`, isdateSql, [['v', TYPES.NVarChar, ' '.repeat(n - 10) + '2024-01-02', { length: n }]])
  }
  await record('rpc isnumeric varbinary', 'SELECT ISNUMERIC(@v) AS isnumeric_value', [['v', TYPES.VarBinary, Buffer.from('1'), { length: 40 }]])
  await record('rpc isdate dateformat dmy in batch', 'SET DATEFORMAT dmy; SELECT ISDATE(@v) AS isdate_value', [['v', TYPES.NVarChar, '13/01/2024', { length: 40 }]])
  await record('rpc isdate dateformat after dynamic scope', isdateSql, [['v', TYPES.NVarChar, '13/01/2024', { length: 40 }]])
  await record('rpc isdate language deutsch in batch', 'SET LANGUAGE Deutsch; SELECT ISDATE(@v) AS isdate_value', [['v', TYPES.NVarChar, 'Januar 2 2024', { length: 40 }]])
  await record('rpc isdate language after dynamic scope', 'SELECT ISDATE(@v) AS isdate_value,@@LANGUAGE AS language_name', [['v', TYPES.NVarChar, 'Januar 2 2024', { length: 40 }]])
  await record('session dateformat dmy', 'SET DATEFORMAT dmy')
  await record('rpc isdate under session dateformat dmy', isdateSql, [['v', TYPES.NVarChar, '13/01/2024', { length: 40 }]])
  await record('session dateformat mdy', 'SET DATEFORMAT mdy')
  const preparedSql = 'SELECT ISNUMERIC(@v) AS isnumeric_value,ISDATE(@v) AS isdate_value'
  const handle = await prepared(connection, preparedSql, [['v', TYPES.NVarChar, { length: 40 }]], [
    { v: '123' }, { v: '$' }, { v: '1e5' }, { v: '2024-01-02' }, { v: '13/01/2024' }, { v: '0001-01-01' }, { v: null }, { v: '123' },
  ])
  records.push({ name: 'prepared isnumeric and isdate', sql: preparedSql, prepared: handle })
  await record('session dateformat dmy before prepared', 'SET DATEFORMAT dmy')
  const dmyHandle = await prepared(connection, 'SELECT ISDATE(@v) AS isdate_value', [['v', TYPES.VarChar, { length: 40 }]], [
    { v: '13/01/2024' }, { v: '01/13/2024' },
  ])
  records.push({ name: 'prepared isdate under session dateformat dmy', sql: 'SELECT ISDATE(@v) AS isdate_value', prepared: dmyHandle })
  await record('session dateformat restore', 'SET DATEFORMAT mdy')
  return records
}

function validate(run) {
  assert.equal(run.length, setup.length + cases.length + rpcValues.length + rpcDateValues.length + 4 + 1 + 7 + 1 + 1 + 1 + 1)
  const get = name => run.find(record => record.name === name)?.result
  const column = (name, index) => get(name).sets[0].rows.map(row => row[index])
  assert.equal(get('isnumeric typed numerics').sets[0].columns[0].type, 'IntN')
  assert.deepEqual(column('isnumeric ascii strings', 2).slice(0, 3), [1, 1, 1])
  assert.deepEqual(get('isnumeric typed nulls').sets[0].rows, [[0, 0, 0, 0]])
  assert.deepEqual(column('isdate strings default', 2).slice(0, 2), [1, 1])
  for (const [name, number] of [
    ['isnumeric no arguments', 174],
    ['isdate no arguments', 174],
  ]) assert.equal(get(name).errors[0]?.number, number, name)
  for (const record of run) {
    const results = record.prepared ? [record.prepared.prepare, ...record.prepared.executions.map(x => x.result), record.prepared.unprepare].filter(Boolean) : [record.result]
    for (const result of results) assert(result.done.length > 0, `${record.name}: no completion`)
  }
}

// Resolves symlinks in the existing part of a path, so an alias of the
// fixture (or of its directory) compares equal even before the file exists.
// Same logic as the shared refuseFixtureOutput proposed in PR #312.
async function canonicalPath(path) {
  const absolute = resolve(path instanceof URL ? fileURLToPath(path) : path)
  try { return await realpath(absolute) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return resolve(await canonicalPath(dirname(absolute)), basename(absolute))
  }
}

// Filesystem identity of an existing file, or null when it does not exist.
async function identity(path) {
  try { const { dev, ino } = await stat(path, { bigint: true }); return `${dev}:${ino}` }
  catch (error) { if (error.code === 'ENOENT') return null; throw error }
}

// Scratch output must never alias the retained fixture, whether by path,
// symlink or hard link (same device and inode); checked before any container
// starts.
const outputIdentity = await identity(output)
if (await canonicalPath(output) === await canonicalPath(fixture) ||
  (outputIdentity !== null && outputIdentity === await identity(fixture))) throw new Error('refusing to write capture output over retained fixture ' + fileURLToPath(fixture))
if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
const count = oneDatabase ? 1 : 2
for (let containerIndex = 0; containerIndex < count; containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    console.error('started owned container ' + container.name)
    const runs = []
    for (let databaseIndex = 0; databaseIndex < count; databaseIndex++) {
      const run = await isolatedReference({
        ...config, options: { ...config.options, requestTimeout: 120000 },
      }, observe)
      // A diagnostic run writes its artifact before validation for inspection.
      if (!oneDatabase) validate(run)
      runs.push(run)
    }
    if (!oneDatabase) assertSameCapture(runs[0], runs[1], 'ISNUMERIC/ISDATE observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) assertSameCapture(containers[0].runs, containers[1].runs, 'ISNUMERIC/ISDATE observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
if (oneDatabase) validate(containers[0].runs[0])
let retained
if (!writeFixture && !oneDatabase) {
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'ISNUMERIC/ISDATE observations differ from retained fixture')
}
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${containers[0].runs[0].length} ISNUMERIC/ISDATE programs` + (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained ? ' and matched retained fixture' : '') + (writeFixture ? '; wrote new retained fixture' : ''))
