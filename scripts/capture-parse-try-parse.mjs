#!/usr/bin/env node
// Retain SQL Server PARSE and TRY_PARSE rows, descriptors, diagnostics and
// completion tokens for ordinary batches, sp_executesql RPC calls and
// sp_prepare/sp_execute/sp_unprepare sequences.
//
// Run with `node --max-old-space-size=2048` under an RSS watchdog (see
// docs/parse-try-parse.md). Whole captures are only compared with the bounded
// helpers from scripts/lib/reference.mjs, never with node:assert.
import assert from 'node:assert/strict'
import { mkdir, readFile, realpath, rename, stat, unlink, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/parse-try-parse.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/parse-try-parse-reference-v1/capture.json')

// Every batch ends in a unique `/*name*/` comment so no two programs share
// plan cache text. Culture values are literals unless a case says otherwise.
const setup = [
  ['server properties', "SELECT CAST(SERVERPROPERTY('ProductMajorVersion') AS INT) AS major, CAST(SERVERPROPERTY('Collation') AS NVARCHAR(128)) AS server_collation, CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation, @@LANGUAGE AS language, @@DATEFIRST AS datefirst"],
  ['create alias type', 'CREATE TYPE dbo.parse_alias FROM INT'],
  ['create source', 'CREATE TABLE dbo.parse_src(id INT NOT NULL CONSTRAINT parse_src_pk PRIMARY KEY, txt NVARCHAR(40) NULL, culture NVARCHAR(20) NULL)'],
  ['insert source', "INSERT dbo.parse_src VALUES (1,N'12',N'en-US'),(2,N'1.234,5',N'de-DE'),(3,N'x',N'en-US'),(4,NULL,N'en-US'),(5,N'7',NULL),(6,N'8',N'xx-XX')"],
]

const cases = []
// PARSE and TRY_PARSE of the same text/target/culture.
// DATETIMEOFFSET results also project style-121 text because tedious
// returns the instant without its offset.
function both(name, text, target, culture) {
  const using = culture === undefined ? '' : ` USING ${culture}`
  for (const fn of ['PARSE', 'TRY_PARSE']) {
    const expression = `${fn}(${text} AS ${target}${using})`
    const offsetText = target.startsWith('DATETIMEOFFSET') ? `, CONVERT(NVARCHAR(40),${expression},121) AS text` : ''
    cases.push([`${fn.toLowerCase()} ${name}`, `SELECT ${expression} AS value${offsetText}`])
  }
}

// Integer targets: whitespace, signs, grouping, fractions and ranges.
both('int basic', "N'123'", 'INT')
both('int varchar input', "'123'", 'INT')
both('int leading trailing spaces', "N'  123  '", 'INT')
both('int tab newline', "NCHAR(9)+N'123'+NCHAR(10)", 'INT')
both('int nbsp', "NCHAR(160)+N'123'", 'INT')
both('int plus', "N'+123'", 'INT')
both('int minus', "N'-123'", 'INT')
both('int trailing minus', "N'123-'", 'INT')
both('int parentheses', "N'(123)'", 'INT')
both('int space after sign', "N'- 123'", 'INT')
both('int grouping en-US', "N'1,234'", 'INT', "'en-US'")
both('int bad grouping en-US', "N'1,2,3,4'", 'INT', "'en-US'")
both('int grouping de-DE', "N'1.234'", 'INT', "'de-DE'")
both('int fraction zero', "N'123.0'", 'INT')
both('int fraction', "N'123.5'", 'INT')
both('int fraction negative', "N'-2.5'", 'INT')
both('int exponent', "N'1e3'", 'INT')
both('int hex', "N'0x10'", 'INT')
both('int currency en-US', "N'$123'", 'INT', "'en-US'")
both('int max', "N'2147483647'", 'INT')
both('int overflow', "N'2147483648'", 'INT')
both('int min', "N'-2147483648'", 'INT')
both('int empty', "N''", 'INT')
both('int spaces only', "N'   '", 'INT')
both('int letters', "N'abc'", 'INT')
both('int typed null', 'CAST(NULL AS NVARCHAR(10))', 'INT')
both('int untyped null', 'NULL', 'INT')
both('int fullwidth digits', "NCHAR(65297)+NCHAR(65298)", 'INT')
both('int arabic-indic digits ar-SA', "NCHAR(1633)+NCHAR(1634)", 'INT', "'ar-SA'")
both('tinyint max', "N'255'", 'TINYINT')
both('tinyint overflow', "N'256'", 'TINYINT')
both('tinyint negative', "N'-1'", 'TINYINT')
both('tinyint negative zero', "N'-0'", 'TINYINT')
both('smallint overflow', "N'32768'", 'SMALLINT')
both('bigint max', "N'9223372036854775807'", 'BIGINT')
both('bigint overflow', "N'9223372036854775808'", 'BIGINT')

// Exact numeric, money and approximate targets.
both('decimal default', "N'123.456'", 'DECIMAL')
both('decimal scale rounding', "N'1.235'", 'DECIMAL(10,2)')
both('decimal scale rounding negative', "N'-1.235'", 'DECIMAL(10,2)')
both('decimal half even check', "N'1.225'", 'DECIMAL(10,2)')
both('decimal precision overflow', "N'12345.6'", 'DECIMAL(5,2)')
both('decimal grouped en-US', "N'1,234.5'", 'DECIMAL(10,2)', "'en-US'")
both('decimal comma de-DE', "N'1.234,5'", 'DECIMAL(10,2)', "'de-DE'")
both('decimal point de-DE', "N'1234.5'", 'DECIMAL(10,2)', "'de-DE'")
both('decimal comma fr-FR', "N'1234,5'", 'DECIMAL(10,2)', "'fr-FR'")
both('decimal space group fr-FR', "N'1 234,5'", 'DECIMAL(10,2)', "'fr-FR'")
both('decimal nbsp group fr-FR', "N'1'+NCHAR(160)+N'234,5'", 'DECIMAL(10,2)', "'fr-FR'")
both('decimal narrow nbsp group fr-FR', "N'1'+NCHAR(8239)+N'234,5'", 'DECIMAL(10,2)', "'fr-FR'")
both('decimal exponent', "N'1.5e2'", 'DECIMAL(10,2)')
both('decimal leading point', "N'.5'", 'DECIMAL(10,2)')
both('decimal trailing point', "N'5.'", 'DECIMAL(10,2)')
both('decimal many digits', "N'0.123456789012345678901234567890123456789'", 'DECIMAL(38,38)')
both('numeric', "N'-0.5'", 'NUMERIC(4,1)')
both('money en-US currency', "N'$1,234.5678'", 'MONEY', "'en-US'")
both('money en-US negative currency', "N'-$1.50'", 'MONEY', "'en-US'")
both('money en-US parentheses currency', "N'($1.50)'", 'MONEY', "'en-US'")
both('money en-US five decimals', "N'1.23456'", 'MONEY', "'en-US'")
both('money en-GB pound', "NCHAR(163)+N'12.50'", 'MONEY', "'en-GB'")
both('money en-US pound', "NCHAR(163)+N'12.50'", 'MONEY', "'en-US'")
both('money de-DE euro suffix', "N'1.234,50 '+NCHAR(8364)", 'MONEY', "'de-DE'")
both('money de-DE euro prefix', "NCHAR(8364)+N'1.234,50'", 'MONEY', "'de-DE'")
both('money ja-JP yen', "NCHAR(165)+N'1,234'", 'MONEY', "'ja-JP'")
both('money overflow', "N'922337203685478'", 'MONEY')
both('smallmoney', "N'214748.3647'", 'SMALLMONEY')
both('smallmoney overflow', "N'214748.3648'", 'SMALLMONEY')
both('float', "N'0.1'", 'FLOAT')
both('float exponent', "N'-1.5E+300'", 'FLOAT')
both('float overflow', "N'1e309'", 'FLOAT')
both('float infinity', "N'Infinity'", 'FLOAT')
both('float nan', "N'NaN'", 'FLOAT')
both('float de-DE comma', "N'0,5'", 'FLOAT', "'de-DE'")
both('float grouped en-US', "N'1,000.5'", 'FLOAT', "'en-US'")
both('real', "N'3.4028235E+38'", 'REAL')
both('real overflow', "N'3.5E+38'", 'REAL')

// Date and time targets.
both('date iso', "N'2024-01-02'", 'DATE')
both('date documented example', "N'Monday, 13 December 2010'", 'DATETIME2', "'en-US'")
both('date en-US month first', "N'12/13/2010'", 'DATE', "'en-US'")
both('date en-US day first invalid', "N'13/12/2010'", 'DATE', "'en-US'")
both('date en-GB day first', "N'13/12/2010'", 'DATE', "'en-GB'")
both('date en-GB ambiguous', "N'01/02/2010'", 'DATE', "'en-GB'")
both('date en-US ambiguous', "N'01/02/2010'", 'DATE', "'en-US'")
both('date de-DE dotted', "N'02.01.2024'", 'DATE', "'de-DE'")
both('date de-DE month name', "N'2. Januar 2024'", 'DATE', "'de-DE'")
both('date de-DE english month name', "N'2 January 2024'", 'DATE', "'de-DE'")
both('date fr-FR month name', "N'2 janvier 2024'", 'DATE', "'fr-FR'")
both('date ja-JP slashes', "N'2024/01/02'", 'DATE', "'ja-JP'")
both('date ar-SA hijri', "N'20/06/1445'", 'DATE', "'ar-SA'")
both('date ar-SA gregorian', "N'2024-01-02'", 'DATE', "'ar-SA'")
both('date two digit year', "N'1/2/49'", 'DATE', "'en-US'")
both('date two digit year 30', "N'1/2/30'", 'DATE', "'en-US'")
both('date february 30', "N'2024-02-30'", 'DATE')
both('date leap day', "N'2024-02-29'", 'DATE')
both('date year 1', "N'0001-01-01'", 'DATE')
both('date with time', "N'2024-01-02 03:04:05'", 'DATE')
both('date whitespace', "N'  2024-01-02  '", 'DATE')
both('date empty', "N''", 'DATE')
both('date numeric string', "N'20240102'", 'DATE')
both('datetime', "N'2024-01-02 03:04:05.1234567'", 'DATETIME')
both('datetime rounding', "N'2024-01-02 03:04:05.998'", 'DATETIME')
both('datetime round to next day', "N'2024-01-02 23:59:59.9999999'", 'DATETIME')
both('datetime before 1753', "N'1752-12-31'", 'DATETIME')
both('datetime 1753', "N'1753-01-01'", 'DATETIME')
both('datetime max round overflow', "N'9999-12-31 23:59:59.999'", 'DATETIME')
both('smalldatetime seconds', "N'2024-01-02 03:04:29.998'", 'SMALLDATETIME')
both('smalldatetime round up', "N'2024-01-02 03:04:30'", 'SMALLDATETIME')
both('smalldatetime out of range', "N'2079-06-07'", 'SMALLDATETIME')
both('datetime2 default', "N'2024-01-02T03:04:05.1234567'", 'DATETIME2')
both('datetime2 scale 3 rounding', "N'2024-01-02T03:04:05.1235'", 'DATETIME2(3)')
both('datetime2 scale 0 round up', "N'2024-01-02T03:04:05.5'", 'DATETIME2(0)')
both('datetime2 eight fraction digits', "N'2024-01-02T03:04:05.12345678'", 'DATETIME2')
both('datetime2 max round overflow', "N'9999-12-31T23:59:59.99999999'", 'DATETIME2(0)')
both('datetime2 pm en-US', "N'1/2/2024 3:04:05 PM'", 'DATETIME2', "'en-US'")
both('datetime2 offset converted', "N'2024-01-02T03:04:05+05:30'", 'DATETIME2')
both('datetime2 zulu', "N'2024-01-02T03:04:05Z'", 'DATETIME2')
// A time-only string takes the server's current date. The parsed value is
// evaluated once, between two reads of the UTC date (container TZ=UTC), and
// only whether its date equals one of those reads is retained, so a run that
// crosses UTC midnight still records 1. The time part is retained exactly.
for (const fn of ['PARSE', 'TRY_PARSE']) cases.push([`${fn.toLowerCase()} datetime2 time only`, `DECLARE @before DATE=CAST(SYSUTCDATETIME() AS DATE); DECLARE @parsed DATETIME2=${fn}(N'03:04:05' AS DATETIME2); DECLARE @after DATE=CAST(SYSUTCDATETIME() AS DATE); SELECT CASE WHEN CAST(@parsed AS DATE) IN (@before,@after) THEN 1 ELSE 0 END AS is_current_date, CAST(@parsed AS TIME) AS time_part`])
both('time', "N'03:04:05.1234567'", 'TIME')
both('time pm', "N'3:04 PM'", 'TIME', "'en-US'")
both('time scale 0 rounding', "N'23:59:59.6'", 'TIME(0)')
both('time with date', "N'2024-01-02 03:04:05'", 'TIME')
both('time over 24 hours', "N'25:00:00'", 'TIME')
both('datetimeoffset', "N'2024-01-02 03:04:05 +05:30'", 'DATETIMEOFFSET')
both('datetimeoffset zulu', "N'2024-01-02T03:04:05.123Z'", 'DATETIMEOFFSET(3)')
both('datetimeoffset no offset', "N'2024-01-02 03:04:05'", 'DATETIMEOFFSET')
both('datetimeoffset offset over 14', "N'2024-01-02 03:04:05 +15:00'", 'DATETIMEOFFSET')

// Invalid target types and culture arguments.
for (const [name, target] of [
  ['target varchar', 'VARCHAR(10)'], ['target nvarchar', 'NVARCHAR(10)'], ['target bit', 'BIT'],
  ['target uniqueidentifier', 'UNIQUEIDENTIFIER'], ['target varbinary', 'VARBINARY(10)'], ['target xml', 'XML'],
  ['target sql_variant', 'SQL_VARIANT'], ['target user alias', 'dbo.parse_alias'],
]) both(name, "N'1'", target)
both('culture invalid', "N'1'", 'INT', "'xx-XX'")
both('culture empty', "N'1,5'", 'DECIMAL(5,2)', "''")
both('culture neutral de', "N'1,5'", 'DECIMAL(5,2)', "'de'")
both('culture lowercase', "N'1,5'", 'DECIMAL(5,2)', "'de-de'")
both('culture language name', "N'1,5'", 'DECIMAL(5,2)', "'German'")
both('culture typed null', "N'1'", 'INT', 'CAST(NULL AS NVARCHAR(10))')
both('culture untyped null', "N'1'", 'INT', 'NULL')
both('culture varchar', "N'1,5'", 'DECIMAL(5,2)', "CAST('de-DE' AS VARCHAR(10))")
both('culture integer', "N'1'", 'INT', '1033')
both('culture expression', "N'1,5'", 'DECIMAL(5,2)', "N'de-'+N'DE'")
both('culture invariant name', "N'1,234.5'", 'DECIMAL(10,2)', "'Invariant Language (Invariant Country)'")

// Non-string inputs.
both('input int', '123', 'INT')
both('input decimal', '1.5', 'DECIMAL(5,2)')
both('input datetime', "CAST('2024-01-02' AS DATETIME)", 'DATE')
both('input varbinary', '0x31', 'INT')
both('input char padded', "CAST('12' AS CHAR(10))", 'INT')
both('input nvarchar max', "CAST(N'12' AS NVARCHAR(MAX))", 'INT')
both('input over 4000 characters', "REPLICATE(CAST(N' ' AS NVARCHAR(MAX)),5000)+N'1'", 'INT')

// Session language as the default culture (restored in the same batch).
for (const [name, language, text, target] of [
  ['language german decimal', 'German', "N'1,5'", 'DECIMAL(5,2)'],
  ['language german date', 'German', "N'02.01.2024'", 'DATE'],
  ['language british date', 'British', "N'13/12/2010'", 'DATE'],
  ['language french money', 'French', "N'1 234,50'", 'MONEY'],
]) cases.push([name, `SET LANGUAGE ${language}; SELECT PARSE(${text} AS ${target}) AS parse_value, TRY_PARSE(${text} AS ${target}) AS try_value; SET LANGUAGE us_english`])
cases.push(['dateformat does not apply', "SET DATEFORMAT dmy; SELECT PARSE(N'01/02/2010' AS DATE) AS parse_value, TRY_PARSE(N'01/02/2010' AS DATE) AS try_value, CONVERT(DATE,N'01/02/2010') AS convert_value; SET DATEFORMAT mdy"])

// Syntax and argument forms.
cases.push(['parse missing as', "SELECT PARSE(N'1') AS value"])
cases.push(['parse style argument', "SELECT PARSE(N'1' AS INT, 1) AS value"])
cases.push(['try_parse missing as', "SELECT TRY_PARSE(N'1') AS value"])
cases.push(['parse null as int alias', 'SELECT PARSE(NULL AS INT) AS value, TRY_PARSE(NULL AS INT) AS try_value'])

// Descriptors and metadata.
cases.push(['describe numeric targets', "EXEC sp_describe_first_result_set N'SELECT PARSE(N''1'' AS INT) AS p_int, TRY_PARSE(N''1'' AS INT) AS t_int, PARSE(N''1'' AS DECIMAL) AS p_dec, TRY_PARSE(N''1'' AS DECIMAL(10,2)) AS t_dec, PARSE(N''1'' AS MONEY) AS p_money, TRY_PARSE(N''1'' AS FLOAT) AS t_float, PARSE(N''1'' AS REAL) AS p_real'"])
cases.push(['describe temporal targets', "EXEC sp_describe_first_result_set N'SELECT PARSE(N''1'' AS DATE) AS p_date, TRY_PARSE(N''1'' AS TIME(3)) AS t_time, PARSE(N''1'' AS DATETIME) AS p_dt, TRY_PARSE(N''1'' AS DATETIME2(4)) AS t_dt2, PARSE(N''1'' AS DATETIMEOFFSET) AS p_dto, TRY_PARSE(N''1'' AS SMALLDATETIME) AS t_sdt'"])
cases.push(['describe column source', "EXEC sp_describe_first_result_set N'SELECT PARSE(txt AS INT) AS p, TRY_PARSE(txt AS INT USING culture) AS t FROM dbo.parse_src'"])
cases.push(['select into', "SELECT PARSE(N'1' AS INT) AS p_int, TRY_PARSE(N'1' AS INT) AS t_int, PARSE(N'2024-01-02' AS DATETIME2(3)) AS p_dt2, TRY_PARSE(N'1' AS DECIMAL(10,2)) AS t_dec INTO dbo.parse_into; SELECT c.name,t.name AS type_name,c.max_length,c.precision,c.scale,c.is_nullable FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('dbo.parse_into') ORDER BY c.column_id"])

// Column sources, per-row cultures and failure part way through a result.
cases.push(['try_parse column culture', 'SELECT id, TRY_PARSE(txt AS DECIMAL(10,2) USING culture) AS value FROM dbo.parse_src ORDER BY id'])
cases.push(['parse column fixed culture', "SELECT id, PARSE(txt AS DECIMAL(10,2) USING 'en-US') AS value FROM dbo.parse_src WHERE id IN (1,4) ORDER BY id"])
cases.push(['parse column error midway', "SELECT id, PARSE(txt AS INT USING 'en-US') AS value FROM dbo.parse_src ORDER BY id"])
cases.push(['parse column invalid culture row', 'SELECT id, PARSE(txt AS INT USING culture) AS value FROM dbo.parse_src WHERE id IN (1,6) ORDER BY id'])
cases.push(['try_parse column invalid culture row', 'SELECT id, TRY_PARSE(txt AS INT USING culture) AS value FROM dbo.parse_src WHERE id IN (1,6) ORDER BY id'])
cases.push(['parse column null culture row', 'SELECT id, PARSE(txt AS INT USING culture) AS value FROM dbo.parse_src WHERE id=5'])
cases.push(['parse where predicate', "SELECT id FROM dbo.parse_src WHERE TRY_PARSE(txt AS INT USING 'en-US') > 10 ORDER BY id"])

// Error handling interaction.
cases.push(['parse in try catch', "BEGIN TRY SELECT PARSE(N'x' AS INT) AS value; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number, ERROR_SEVERITY() AS severity, ERROR_STATE() AS state, ERROR_MESSAGE() AS message; END CATCH"])
cases.push(['parse error continues batch', "SELECT PARSE(N'x' AS INT) AS value; SELECT 2 AS after_error"])
cases.push(['parse error xact_abort', "SET XACT_ABORT ON; BEGIN TRAN; SELECT PARSE(N'x' AS INT) AS value; SELECT 2 AS after_error; COMMIT"])
cases.push(['xact_abort cleanup', 'IF @@TRANCOUNT > 0 ROLLBACK; SET XACT_ABORT OFF; SELECT @@TRANCOUNT AS transaction_count'])

for (const entry of cases) entry[1] += ` /*${entry[0]}*/`

// sp_executesql calls; each statement text is unique by its trailing comment.
const rpcCases = [
  ['rpc parse nvarchar', 'SELECT PARSE(@s AS INT) AS value', [['s', TYPES.NVarChar, ' 42 ', { length: 20 }]]],
  ['rpc parse varchar', 'SELECT PARSE(@s AS DECIMAL(10,2) USING @c) AS value', [['s', TYPES.VarChar, '1.234,5', { length: 20 }], ['c', TYPES.VarChar, 'de-DE', { length: 10 }]]],
  ['rpc parse culture parameter', 'SELECT PARSE(@s AS DECIMAL(10,2) USING @c) AS value', [['s', TYPES.NVarChar, '1,234.5', { length: 20 }], ['c', TYPES.NVarChar, 'en-US', { length: 10 }]]],
  ['rpc parse invalid', 'SELECT PARSE(@s AS INT) AS value', [['s', TYPES.NVarChar, 'abc', { length: 20 }]]],
  ['rpc try_parse invalid', 'SELECT TRY_PARSE(@s AS INT) AS value', [['s', TYPES.NVarChar, 'abc', { length: 20 }]]],
  ['rpc parse null', 'SELECT PARSE(@s AS INT) AS value', [['s', TYPES.NVarChar, null, { length: 20 }]]],
  ['rpc parse invalid culture', 'SELECT PARSE(@s AS INT USING @c) AS value', [['s', TYPES.NVarChar, '1', { length: 20 }], ['c', TYPES.NVarChar, 'xx-XX', { length: 10 }]]],
  ['rpc try_parse invalid culture', 'SELECT TRY_PARSE(@s AS INT USING @c) AS value', [['s', TYPES.NVarChar, '1', { length: 20 }], ['c', TYPES.NVarChar, 'xx-XX', { length: 10 }]]],
  ['rpc try_parse null culture', 'SELECT TRY_PARSE(@s AS INT USING @c) AS value', [['s', TYPES.NVarChar, '1', { length: 20 }], ['c', TYPES.NVarChar, null, { length: 10 }]]],
  ['rpc parse date culture', 'SELECT PARSE(@s AS DATETIME2(3) USING @c) AS value', [['s', TYPES.NVarChar, '13/12/2010 14:15:16.1235', { length: 40 }], ['c', TYPES.NVarChar, 'en-GB', { length: 10 }]]],
  ['rpc parse max input', 'SELECT PARSE(@s AS MONEY USING @c) AS value', [['s', TYPES.NVarChar, '$12.34', { length: Infinity }], ['c', TYPES.NVarChar, 'en-US', { length: 10 }]]],
  ['rpc parse int parameter', 'SELECT PARSE(@s AS INT) AS value', [['s', TYPES.Int, 12]]],
]
for (const entry of rpcCases) entry[1] += ` /*${entry[0]}*/`

// sp_prepare/sp_execute sequences. Each statement text is unique and each
// sequence mixes failing and succeeding executions to expose stale errors.
// An invalid target type is not prepared here: sp_prepare still returns a
// handle and the later messages embed that nondeterministic handle value.
const preparedCases = [
  ['prepared parse int', 'SELECT PARSE(@s AS INT USING @c) AS value',
    [['s', TYPES.NVarChar, { length: 40 }], ['c', TYPES.NVarChar, { length: 10 }]],
    [{ s: '12', c: 'en-US' }, { s: 'abc', c: 'en-US' }, { s: '1,234', c: 'en-US' }, { s: '1.234', c: 'de-DE' }, { s: '1', c: 'xx-XX' }, { s: null, c: 'en-US' }, { s: '3', c: null }, { s: '2147483648', c: 'en-US' }, { s: '5', c: 'en-US' }]],
  ['prepared try_parse decimal', 'SELECT TRY_PARSE(@s AS DECIMAL(10,2) USING @c) AS value',
    [['s', TYPES.NVarChar, { length: 40 }], ['c', TYPES.NVarChar, { length: 10 }]],
    [{ s: '1,5', c: 'de-DE' }, { s: 'x', c: 'de-DE' }, { s: '1,5', c: 'en-US' }, { s: '1', c: 'xx-XX' }, { s: '99999999999', c: 'en-US' }, { s: '2.5', c: 'en-US' }]],
  ['prepared parse date', 'SELECT PARSE(@s AS DATE USING @c) AS value',
    [['s', TYPES.NVarChar, { length: 40 }], ['c', TYPES.NVarChar, { length: 10 }]],
    [{ s: '13/12/2010', c: 'en-GB' }, { s: '13/12/2010', c: 'en-US' }, { s: '12/13/2010', c: 'en-US' }]],
]
for (const entry of preparedCases) entry[1] += ` /*${entry[0]}*/`

// Return status per request. tedious keeps the RETURNSTATUS token value on
// the connection (procReturnStatusValue) and reports it with the next
// doneProc event, so a value can outlive the request that received it. The
// tracker clears that value when each request starts and records only
// RETURNSTATUS tokens that arrive while the request is outstanding; a
// request that receives none records null. The doneProc argument is never
// used.
function trackReturnStatus(connection) {
  let value
  let seen = null
  Object.defineProperty(connection, 'procReturnStatusValue', {
    configurable: true,
    get: () => value,
    set: next => { value = next; if (next !== undefined && seen) seen.push(next) },
  })
  return {
    begin() { value = undefined; seen = [] },
    end() {
      const statuses = seen ?? []
      seen = null
      value = undefined
      return statuses.length ? statuses.at(-1) : null
    },
  }
}

async function tracked(tracker, run) {
  tracker.begin()
  let status = null
  let result
  try { result = await run() } finally { status = tracker.end() }
  result.returnStatus = status
  return result
}

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

// Prepared capture replicating capturePrepared from the owner-authored
// prepared helper (PR #312, not yet on main): completion of sp_prepare is
// awaited through the `prepared`/`error` events; request.error is cleared
// before every phase; only error tokens raised while a phase is outstanding
// are recorded; an error object is never attributed to a later phase; rows
// and messages are bounded per phase.
const PREPARED_LIMITS = Object.freeze({ rows: 10000, messages: 200 })
const messageFields = m => ({ number: m.number, state: m.state, class: m.class, lineNumber: m.lineNumber, message: m.message })
const columnFields = c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })

async function capturePrepared(connection, tracker, sql, declarations, valueSets) {
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
  const phase = start => new Promise(resolve => {
    result = fresh()
    complete = (error, rowCount) => {
      complete = () => {}
      const finished = result
      result = undefined
      finished.returnStatus = tracker.end()
      finished.rowCount = rowCount
      if (error && !reported.has(error) && !finished.errors.length) finished.errors.push({ message: error.message, number: error.number ?? null })
      if (error) reported.add(error)
      resolve(finished)
    }
    request.error = undefined
    tracker.begin()
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
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

const describeOptions = options => options ? { options: { ...options, ...(options.length === Infinity ? { length: 'max' } : {}) } } : {}

async function observe(connection) {
  const records = []
  const tracker = trackReturnStatus(connection)
  for (const [name, sql] of [...setup, ...cases]) records.push({ name, sql, result: canonical(await tracked(tracker, () => capture(connection, sql))) })
  for (const [name, sql, parameters] of rpcCases) records.push({
    name, sql, protocol: 'sp_executesql',
    parameters: parameters.map(([parameter, type, value, options]) => ({ name: parameter, type: type.name, value, ...describeOptions(options) })),
    result: canonical(await tracked(tracker, () => rpc(connection, sql, parameters))),
  })
  for (const [name, sql, declarations, valueSets] of preparedCases) records.push({
    name, sql, protocol: 'sp_prepare/sp_execute/sp_unprepare',
    parameters: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, ...describeOptions(options) })),
    ...canonical(await capturePrepared(connection, tracker, sql, declarations, valueSets)),
  })
  const reuse = 'SELECT @@TRANCOUNT AS transaction_count, @@LANGUAGE AS language, 1 AS reusable /*reuse*/'
  records.push({ name: 'reuse', sql: reuse, result: canonical(await tracked(tracker, () => capture(connection, reuse))) })
  return records
}

// Small targeted assertions on individual values only.
function validate(run) {
  assert.equal(run.length, setup.length + cases.length + rpcCases.length + preparedCases.length + 1)
  const get = name => {
    const record = run.find(record => record.name === name)
    assert(record, 'missing ' + name)
    return record
  }
  for (const [name] of setup) assert.equal(get(name).result.errors.length, 0, name)
  for (const record of run) {
    if (record.result) assert(record.result.done.length > 0, record.name + ': no completion')
    else for (const execution of record.executions) assert(execution.result.done.length > 0, record.name + ': no completion')
  }
  const value = name => get(name).result.sets[0]?.rows[0]?.[0]
  const error = name => get(name).result.errors[0]?.number
  for (const [name, expected] of [
    ['parse int basic', 123], ['parse int leading trailing spaces', 123], ['parse int grouping en-US', 1234],
    ['try_parse int letters', null], ['try_parse int overflow', null], ['parse int typed null', null],
    ['parse decimal comma de-DE', 1234.5], ['try_parse date en-US day first invalid', null],
  ]) assert.equal(value(name), expected, name)
  for (const [name, expected] of [
    ['parse int letters', 9819], ['parse target bit', 10761], ['try_parse target bit', 10761],
  ]) assert.equal(error(name), expected, name)
  const prepared = get('prepared parse int')
  assert.equal(prepared.prepared, true)
  assert.equal(prepared.executions[0].result.errors.length, 0)
  assert.equal(prepared.executions[1].result.errors[0]?.number, 9819)
  assert.equal(prepared.executions[2].result.errors.length, 0, 'no stale prepared error')
  assert.equal(prepared.executions.at(-1).result.errors.length, 0, 'no stale prepared error')
  assert.equal(get('reuse').result.sets[0].rows[0][0], 0)
  // Return statuses come only from RETURNSTATUS tokens of the same request.
  assert.equal(get('rpc parse nvarchar').result.returnStatus, 0)
  assert.equal(get('rpc parse invalid').result.returnStatus, 9819)
  assert.equal(get('parse int basic').result.returnStatus, null)
  assert.equal(prepared.executions[1].result.returnStatus, -6)
}

// Local copy of refuseFixtureOutput from the owner-authored PR #312 (not yet
// on main): the scratch output must never alias the retained fixture, even
// through symlinked directories or before either file exists.
async function canonicalPath(path) {
  const absolute = resolve(path instanceof URL ? fileURLToPath(path) : path)
  try { return await realpath(absolute) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return resolve(await canonicalPath(dirname(absolute)), basename(absolute))
  }
}
// A hard link has its own canonical path, so existing files are also compared
// by device and inode.
async function fileIdentity(path) {
  try { const info = await stat(path, { bigint: true }); return `${info.dev}:${info.ino}` } catch (error) {
    if (error.code === 'ENOENT') return null
    throw error
  }
}
async function refuseFixtureOutput(outputPath, fixturePath) {
  const refuse = () => { throw new Error('refusing to write capture output over retained fixture ' + fileURLToPath(fixturePath)) }
  if (await canonicalPath(outputPath) === await canonicalPath(fixturePath)) refuse()
  const [outputId, fixtureId] = await Promise.all([fileIdentity(outputPath), fileIdentity(fixturePath)])
  if (outputId !== null && outputId === fixtureId) refuse()
}

// The output is written to a fresh sibling file and renamed into place, so
// the rename replaces the directory entry and never writes into an inode
// that another name (such as a hard link to the fixture made during the
// run) still shares.
async function replaceOutput(outputPath, text) {
  const temporary = `${outputPath}.${process.pid}.tmp`
  await writeFile(temporary, text, { flag: 'wx' })
  try { await rename(temporary, outputPath) } catch (error) {
    await unlink(temporary).catch(() => {})
    throw error
  }
}

// Both checks run before any container starts.
await refuseFixtureOutput(output, fixture)
if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
const runsPerContainer = oneDatabase ? 1 : 2
for (let containerIndex = 0; containerIndex < runsPerContainer; containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    console.log('started container ' + container.name)
    const runs = []
    for (let databaseIndex = 0; databaseIndex < runsPerContainer; databaseIndex++) {
      const run = await isolatedReference({
        ...config, options: { ...config.options, requestTimeout: 120000 },
      }, observe)
      if (!oneDatabase) validate(run)
      runs.push(run)
    }
    if (!oneDatabase) assertSameCapture(runs[0], runs[1], 'PARSE/TRY_PARSE observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) assertSameCapture(containers[0].runs[0], containers[1].runs[0], 'PARSE/TRY_PARSE observations differ across containers')
const actual = { containers }
await refuseFixtureOutput(output, fixture)
await replaceOutput(output, JSON.stringify(actual) + '\n')
let retained
if (!oneDatabase) {
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
}
if (retained) assertSameCapture(actual, retained, 'PARSE/TRY_PARSE observations differ from retained fixture')
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${containers[0].runs[0].length} PARSE/TRY_PARSE programs` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained ? ' and matched retained fixture' : ''))
