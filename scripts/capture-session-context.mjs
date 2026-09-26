#!/usr/bin/env node
// Retain SQL Server SESSION_CONTEXT and sp_set_session_context rows,
// descriptors, diagnostics and completion tokens across batches, RPC and
// prepared calls, separate connections and connection reset.
//
// Session context is per connection. Every scenario opens its own labelled
// connections to the fresh database and closes them before the next scenario,
// so no scenario observes state left by another one.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { assertSameCapture, connect, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/session-context.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/session-context-reference-v1/capture.json')

// Exact runtime type of a stored value; tedious decodes sql_variant values to
// JavaScript values, so the base type and its facets are read explicitly.
const probe = (key, tag) => `SELECT SESSION_CONTEXT(${key}) AS value, ` +
  `CONVERT(NVARCHAR(128),SQL_VARIANT_PROPERTY(SESSION_CONTEXT(${key}),'BaseType')) AS base_type, ` +
  `CONVERT(INT,SQL_VARIANT_PROPERTY(SESSION_CONTEXT(${key}),'Precision')) AS precision_value, ` +
  `CONVERT(INT,SQL_VARIANT_PROPERTY(SESSION_CONTEXT(${key}),'Scale')) AS scale_value, ` +
  `CONVERT(INT,SQL_VARIANT_PROPERTY(SESSION_CONTEXT(${key}),'TotalBytes')) AS total_bytes, ` +
  `CONVERT(NVARCHAR(128),SQL_VARIANT_PROPERTY(SESSION_CONTEXT(${key}),'Collation')) AS collation_name, ` +
  `CONVERT(INT,SQL_VARIANT_PROPERTY(SESSION_CONTEXT(${key}),'MaxLength')) AS max_length` +
  (tag ? ` /*${tag}*/` : '')

// Deterministic 0..255 numbers without catalog access.
const numbers = "(SELECT a.n*16+b.n AS n FROM (VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9),(10),(11),(12),(13),(14),(15)) a(n) CROSS JOIN (VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9),(10),(11),(12),(13),(14),(15)) b(n))"
const countFill = (prefix, tag) => `SELECT COUNT(*) AS stored_keys, MIN(n) AS first_stored, MAX(n) AS last_stored FROM ${numbers} x WHERE SESSION_CONTEXT(CONCAT(N'${prefix}',x.n)) IS NOT NULL /*${tag}*/`

// Value types assigned through a typed local variable, then read back.
const variableTypes = [
  ['int', 'INT', '42'],
  ['bigint', 'BIGINT', '9223372036854775807'],
  ['smallint', 'SMALLINT', '-32768'],
  ['tinyint', 'TINYINT', '255'],
  ['bit', 'BIT', '1'],
  ['decimal', 'DECIMAL(10,3)', '-12.340'],
  ['numeric38', 'NUMERIC(38,0)', '12345678901234567890123456789012345678'],
  ['money', 'MONEY', '12.3456'],
  ['smallmoney', 'SMALLMONEY', '-1.5'],
  ['float', 'FLOAT', '0.1'],
  ['real', 'REAL', '0.1'],
  ['nvarchar', 'NVARCHAR(20)', "N'héllo ключ'"],
  ['varchar', 'VARCHAR(20)', "'abc'"],
  ['nchar', 'NCHAR(5)', "N'ab'"],
  ['char', 'CHAR(5)', "'ab'"],
  ['varchar collated', 'VARCHAR(20)', "'abc' COLLATE Latin1_General_BIN2"],
  ['varbinary', 'VARBINARY(10)', '0x0102'],
  ['binary', 'BINARY(4)', '0x0102'],
  ['date', 'DATE', "'2024-02-29'"],
  ['time', 'TIME(3)', "'12:34:56.789'"],
  ['datetime', 'DATETIME', "'2024-02-29T12:34:56.787'"],
  ['smalldatetime', 'SMALLDATETIME', "'2024-02-29T12:34:00'"],
  ['datetime2', 'DATETIME2(7)', "'2024-02-29T12:34:56.1234567'"],
  ['datetimeoffset', 'DATETIMEOFFSET(2)', "'2024-02-29T12:34:56.12+05:30'"],
  ['uniqueidentifier', 'UNIQUEIDENTIFIER', "'6F9619FF-8B86-D011-B42D-00C04FC964FF'"],
  ['sql_variant int', 'SQL_VARIANT', 'CAST(7 AS INT)'],
  ['nvarchar 4000', 'NVARCHAR(4000)', "REPLICATE(N'x',4000)"],
  ['varchar 8000', 'VARCHAR(8000)', "REPLICATE('x',8000)"],
  ['varbinary 8000', 'VARBINARY(8000)', "CAST(REPLICATE('x',8000) AS VARBINARY(8000))"],
]
// Types that cannot be stored in sql_variant, passed through a variable.
const rejectedTypes = [
  ['nvarchar max', 'NVARCHAR(MAX)', "N'x'"],
  ['varchar max', 'VARCHAR(MAX)', "'x'"],
  ['varbinary max', 'VARBINARY(MAX)', '0x01'],
  ['xml', 'XML', "N'<a/>'"],
  ['json', 'JSON', "N'{}'"],
  ['hierarchyid', 'HIERARCHYID', "hierarchyid::GetRoot()"],
  ['vector', 'VECTOR(2)', "'[1,2]'"],
]
// Constants passed directly as the procedure argument.
const literals = [
  ['int literal', '42'],
  ['large integer literal', '2147483648'],
  ['decimal literal', '1.50'],
  ['float literal', '1e1'],
  ['money literal', '$1.5'],
  ['varchar literal', "'abc'"],
  ['nvarchar literal', "N'abc'"],
  ['binary literal', '0x0A0B'],
  ['null literal', 'NULL'],
  ['negative literal', '-5'],
  ['expression argument', '1+1'],
  ['function argument', 'GETDATE()'],
]

const scenarios = [
  {
    name: 'function surface', sessions: ['a'], steps: [
      ['batch', 'a', 'missing key', "SELECT SESSION_CONTEXT(N'missing') AS value"],
      ['batch', 'a', 'missing key probe', probe("N'missing'")],
      ['batch', 'a', 'null key', 'SELECT SESSION_CONTEXT(NULL) AS value'],
      ['batch', 'a', 'empty key', "SELECT SESSION_CONTEXT(N'') AS value"],
      ['batch', 'a', 'varchar key literal', "SELECT SESSION_CONTEXT('missing') AS value"],
      ['batch', 'a', 'integer key', 'SELECT SESSION_CONTEXT(1) AS value'],
      ['batch', 'a', 'no arguments', 'SELECT SESSION_CONTEXT() AS value'],
      ['batch', 'a', 'two arguments', "SELECT SESSION_CONTEXT(N'a',N'b') AS value"],
      ['batch', 'a', 'sysname variable key', "DECLARE @k SYSNAME=N'missing'; SELECT SESSION_CONTEXT(@k) AS value"],
      ['batch', 'a', 'nchar variable key', "DECLARE @k NCHAR(10)=N'missing'; SELECT SESSION_CONTEXT(@k) AS value"],
      ['batch', 'a', 'varchar variable key', "DECLARE @k VARCHAR(10)='missing'; SELECT SESSION_CONTEXT(@k) AS value"],
      ['batch', 'a', 'nvarchar max variable key', "DECLARE @k NVARCHAR(MAX)=N'missing'; SELECT SESSION_CONTEXT(@k) AS value"],
      ['batch', 'a', 'unnamed column', "SELECT SESSION_CONTEXT(N'missing')"],
      ['batch', 'a', 'describe first result set', "EXEC sp_describe_first_result_set N'SELECT SESSION_CONTEXT(N''k'') AS value'"],
      ['batch', 'a', 'select into column', "SELECT SESSION_CONTEXT(N'k') AS value INTO dbo.ctx_into; SELECT c.name, t.name AS type_name, c.max_length, c.precision, c.scale, c.is_nullable FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID(N'dbo.ctx_into')"],
      ['batch', 'a', 'server and database collation', "SELECT CONVERT(NVARCHAR(128),SERVERPROPERTY('Collation')) AS server_collation, CONVERT(NVARCHAR(128),DATABASEPROPERTYEX(DB_NAME(),'Collation')) AS database_collation"],
    ],
  },
  {
    name: 'value types', sessions: ['a'], steps: [
      ...variableTypes.map(([label, type, value]) => ['batch', 'a', `variable ${label}`, `DECLARE @v ${type}=${value}; EXEC sp_set_session_context N'v:${label}', @v; ${probe(`N'v:${label}'`)}`]),
      ...rejectedTypes.map(([label, type, value]) => ['batch', 'a', `rejected ${label}`, `DECLARE @v ${type}=${value}; EXEC sp_set_session_context N'r:${label}', @v; ${probe(`N'r:${label}'`)}`]),
      ...literals.map(([label, value]) => ['batch', 'a', `literal ${label}`, `EXEC sp_set_session_context N'l:${label}', ${value}; ${probe(`N'l:${label}'`)}`]),
      ['batch', 'a', 'values survive the batch', `${probe("N'v:int'")}; ${probe("N'v:datetimeoffset'")}; ${probe("N'v:varchar collated'")}`],
      ['batch', 'a', 'exact text forms', "SELECT CONVERT(NVARCHAR(60),SESSION_CONTEXT(N'v:numeric38')) AS numeric38, CONVERT(NVARCHAR(60),SESSION_CONTEXT(N'v:bigint')) AS bigint_value, CONVERT(NVARCHAR(60),SESSION_CONTEXT(N'v:money')) AS money_value, CONVERT(NVARCHAR(60),SESSION_CONTEXT(N'v:real')) AS real_value, CONVERT(NVARCHAR(60),SESSION_CONTEXT(N'v:datetime2'),121) AS datetime2_value, CONVERT(NVARCHAR(60),SESSION_CONTEXT(N'v:datetimeoffset'),121) AS datetimeoffset_value, CONVERT(NVARCHAR(60),SESSION_CONTEXT(N'v:time'),121) AS time_value, CONVERT(NVARCHAR(60),SESSION_CONTEXT(N'l:large integer literal')) AS large_literal"],
      ['batch', 'a', 'variant arithmetic', "SELECT SESSION_CONTEXT(N'v:int') + 1 AS value"],
      ['batch', 'a', 'variant comparison', "SELECT CASE WHEN SESSION_CONTEXT(N'v:int') = 42 THEN 1 ELSE 0 END AS equal_int, CASE WHEN SESSION_CONTEXT(N'v:int') = N'42' THEN 1 ELSE 0 END AS equal_text, CASE WHEN SESSION_CONTEXT(N'v:bigint') > SESSION_CONTEXT(N'v:int') THEN 1 ELSE 0 END AS bigint_greater"],
      ['batch', 'a', 'variant explicit conversion', "SELECT CAST(SESSION_CONTEXT(N'v:int') AS INT) + 1 AS int_value, CONVERT(NVARCHAR(40),SESSION_CONTEXT(N'v:decimal')) AS decimal_text, CAST(SESSION_CONTEXT(N'v:nvarchar') AS INT) AS bad_int"],
      ['batch', 'a', 'variant implicit assignment', "DECLARE @i INT = SESSION_CONTEXT(N'v:int'); DECLARE @s NVARCHAR(20) = SESSION_CONTEXT(N'v:nvarchar'); SELECT @i AS int_value, @s AS text_value"],
    ],
  },
  {
    name: 'keys', sessions: ['a'], steps: [
      ['batch', 'a', 'varchar literal key', `EXEC sp_set_session_context 'vk', 1; ${probe("N'vk'")}`],
      ['batch', 'a', 'integer key', `EXEC sp_set_session_context 5, 1; ${probe("N'5'")}`],
      ['batch', 'a', 'mixed case key', "EXEC sp_set_session_context N'CaseKey', 1; SELECT SESSION_CONTEXT(N'CaseKey') AS exact_case, SESSION_CONTEXT(N'casekey') AS lower_case, SESSION_CONTEXT(N'CASEKEY') AS upper_case"],
      ['batch', 'a', 'mixed case overwrite', "EXEC sp_set_session_context N'casekey', 2; SELECT SESSION_CONTEXT(N'CaseKey') AS exact_case, SESSION_CONTEXT(N'casekey') AS lower_case"],
      ['batch', 'a', 'trailing space key', "EXEC sp_set_session_context N'pad ', 1; SELECT SESSION_CONTEXT(N'pad') AS unpadded, SESSION_CONTEXT(N'pad ') AS padded, SESSION_CONTEXT(N'pad  ') AS double_padded"],
      ['batch', 'a', 'accent key', "EXEC sp_set_session_context N'café', 1; SELECT SESSION_CONTEXT(N'café') AS accented, SESSION_CONTEXT(N'cafe') AS plain"],
      ['batch', 'a', 'unicode key', `EXEC sp_set_session_context N'ключ', 1; ${probe("N'ключ'")}`],
      ['batch', 'a', 'empty key', `EXEC sp_set_session_context N'', 1; ${probe("N''")}`],
      ['batch', 'a', 'null key', 'EXEC sp_set_session_context NULL, 1'],
      ['batch', 'a', 'null key variable', 'DECLARE @k SYSNAME; EXEC sp_set_session_context @k, 1'],
      ['batch', 'a', '128 character key', `EXEC sp_set_session_context N'${'k'.repeat(128)}', 1; SELECT SESSION_CONTEXT(N'${'k'.repeat(128)}') AS value`],
      ['batch', 'a', '129 character key literal', `EXEC sp_set_session_context N'${'m'.repeat(129)}', 1; SELECT SESSION_CONTEXT(N'${'m'.repeat(128)}') AS prefix_value, SESSION_CONTEXT(N'${'m'.repeat(129)}') AS full_value`],
      ['batch', 'a', '129 character key variable', `DECLARE @k NVARCHAR(200)=REPLICATE(N'n',129); EXEC sp_set_session_context @k, 1; SELECT SESSION_CONTEXT(LEFT(@k,128)) AS prefix_value, SESSION_CONTEXT(@k) AS full_value`],
      ['batch', 'a', '129 character read key', `SELECT SESSION_CONTEXT(N'${'k'.repeat(129)}') AS value`],
      ['batch', 'a', 'nvarchar max key variable', "DECLARE @k NVARCHAR(MAX)=N'maxkey'; EXEC sp_set_session_context @k, 1; SELECT SESSION_CONTEXT(N'maxkey') AS value"],
    ],
  },
  {
    name: 'overwrite, NULL and arguments', sessions: ['a'], steps: [
      ['batch', 'a', 'set int', `EXEC sp_set_session_context N'k', 1; ${probe("N'k'")}`],
      ['batch', 'a', 'overwrite with nvarchar', `EXEC sp_set_session_context N'k', N'two'; ${probe("N'k'")}`],
      ['batch', 'a', 'overwrite with NULL', `EXEC sp_set_session_context N'k', NULL; ${probe("N'k'")}`],
      ['batch', 'a', 'set after NULL', `EXEC sp_set_session_context N'k', 3; ${probe("N'k'")}`],
      ['batch', 'a', 'typed NULL variable', `DECLARE @v INT; EXEC sp_set_session_context N'typed null', @v; ${probe("N'typed null'")}`],
      ['batch', 'a', 'named parameters', `EXEC sp_set_session_context @key=N'named', @value=5, @read_only=0; ${probe("N'named'")}`],
      ['batch', 'a', 'named parameters reordered', `EXEC sp_set_session_context @value=6, @key=N'reordered'; ${probe("N'reordered'")}`],
      ['batch', 'a', 'unknown parameter name', "EXEC sp_set_session_context @name=N'bad', @value=1; SELECT SESSION_CONTEXT(N'bad') AS bad_value"],
      ['batch', 'a', 'missing value', "EXEC sp_set_session_context N'missing value'"],
      ['batch', 'a', 'missing key and value', 'EXEC sp_set_session_context'],
      ['batch', 'a', 'too many arguments', "EXEC sp_set_session_context N'extra', 1, 0, 1"],
      ['batch', 'a', 'default value keyword', `EXEC sp_set_session_context N'default value', DEFAULT; ${probe("N'default value'")}`],
      ['batch', 'a', 'default key keyword', "EXEC sp_set_session_context DEFAULT, 1"],
      ['batch', 'a', 'return status', "DECLARE @r INT; EXEC @r = sp_set_session_context N'status', 1; SELECT @r AS return_status"],
      ['batch', 'a', 'read_only NULL', `EXEC sp_set_session_context N'ro null', 1, NULL; EXEC sp_set_session_context N'ro null', 2; ${probe("N'ro null'")}`],
      ['batch', 'a', 'read_only 2', `EXEC sp_set_session_context N'ro two', 1, 2; EXEC sp_set_session_context N'ro two', 2; ${probe("N'ro two'")}`],
      ['batch', 'a', 'read_only text', "EXEC sp_set_session_context N'ro text', 1, N'yes'"],
      ['batch', 'a', 'qualified procedure name', `EXEC sys.sp_set_session_context N'qualified', 1; EXEC master.sys.sp_set_session_context N'qualified master', 1; SELECT SESSION_CONTEXT(N'qualified') AS qualified, SESSION_CONTEXT(N'qualified master') AS qualified_master`],
      ['batch', 'a', 'metadata', "SELECT o.name, o.type, o.type_desc FROM sys.all_objects o WHERE o.name=N'sp_set_session_context'; SELECT p.parameter_id, p.name, t.name AS type_name, p.max_length, p.is_output, p.has_default_value FROM sys.all_parameters p JOIN sys.types t ON t.user_type_id=p.user_type_id WHERE p.object_id=OBJECT_ID(N'sys.sp_set_session_context') ORDER BY p.parameter_id"],
    ],
  },
  {
    name: 'read_only keys', sessions: ['a'], steps: [
      ['batch', 'a', 'set read_only', `EXEC sp_set_session_context N'ro', 1, 1; ${probe("N'ro'")}`],
      ['batch', 'a', 'overwrite read_only', `EXEC sp_set_session_context N'ro', 2; ${probe("N'ro'")}`],
      ['batch', 'a', 'overwrite read_only with read_only 0', "EXEC sp_set_session_context N'ro', 2, 0"],
      ['batch', 'a', 'overwrite read_only with same value', "EXEC sp_set_session_context N'ro', 1, 1"],
      ['batch', 'a', 'overwrite read_only with NULL', `EXEC sp_set_session_context N'ro', NULL; ${probe("N'ro'")}`],
      ['batch', 'a', 'read_only NULL value', "EXEC sp_set_session_context N'ro null value', NULL, 1; EXEC sp_set_session_context N'ro null value', 1; SELECT SESSION_CONTEXT(N'ro null value') AS value"],
      ['batch', 'a', 'promote writable key', `EXEC sp_set_session_context N'promote', 1; EXEC sp_set_session_context N'promote', 2, 1; EXEC sp_set_session_context N'promote', 3; ${probe("N'promote'")}`],
      ['batch', 'a', 'read_only case variant', "EXEC sp_set_session_context N'RO', 9; SELECT SESSION_CONTEXT(N'ro') AS lower_key, SESSION_CONTEXT(N'RO') AS upper_key, SESSION_CONTEXT(N'Ro') AS mixed_key"],
      ['batch', 'a', 'batch continues after error', "EXEC sp_set_session_context N'ro', 5; SELECT 1 AS continued, @@ERROR AS error_after"],
      ['batch', 'a', 'try catch', "BEGIN TRY EXEC sp_set_session_context N'ro', 5; SELECT 1 AS not_reached END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number, ERROR_SEVERITY() AS severity, ERROR_STATE() AS state, ERROR_PROCEDURE() AS procedure_name, ERROR_LINE() AS line, ERROR_MESSAGE() AS message END CATCH"],
      ['batch', 'a', 'xact_abort transaction', "SET XACT_ABORT ON; BEGIN TRANSACTION; EXEC sp_set_session_context N'ro', 5; SELECT 1 AS continued"],
      ['batch', 'a', 'after xact_abort', "SELECT @@TRANCOUNT AS transaction_count, XACT_STATE() AS transaction_state; IF @@TRANCOUNT > 0 ROLLBACK; SET XACT_ABORT OFF"],
      ['batch', 'a', 'read_only after errors', probe("N'ro'")],
    ],
  },
  {
    name: 'size limits', sessions: ['a'], steps: [
      ['batch', 'a', 'fill until limit', "DECLARE @v VARCHAR(8000)=REPLICATE('x',8000), @i INT=0, @k SYSNAME, @e INT=0; WHILE @i < 256 BEGIN SET @k=CONCAT(N'fill',@i); EXEC sp_set_session_context @k, @v; SET @e=@@ERROR; IF @e <> 0 BREAK; SET @i+=1; END; SELECT @i AS stored, @e AS last_error"],
      ['batch', 'a', 'count after fill', countFill('fill', 'count after fill')],
      ['batch', 'a', 'add small key at limit', `EXEC sp_set_session_context N'small', 1; ${probe("N'small'")}`],
      ['batch', 'a', 'shrink existing key', `EXEC sp_set_session_context N'fill0', 1; ${probe("N'fill0'")}`],
      ['batch', 'a', 'add small key after shrink', `EXEC sp_set_session_context N'small2', 1; ${probe("N'small2'")}`],
      ['batch', 'a', 'null existing key', `EXEC sp_set_session_context N'fill1', NULL; ${probe("N'fill1'")}`],
      ['batch', 'a', 'add large key after null', `DECLARE @v VARCHAR(8000)=REPLICATE('y',8000); EXEC sp_set_session_context N'large', @v; ${probe("N'large'")}`],
      ['batch', 'a', 'grow existing key at limit', `DECLARE @v VARCHAR(8000)=REPLICATE('z',8000); EXEC sp_set_session_context N'fill0', @v; ${probe("N'fill0'")}`],
      ['batch', 'a', 'count at end', countFill('fill', 'count at end')],
    ],
  },
  {
    name: 'size limit in one statement', sessions: ['a'], steps: [
      ['batch', 'a', 'fill with try catch', "DECLARE @v NVARCHAR(4000)=REPLICATE(N'x',4000), @i INT=0, @k SYSNAME; BEGIN TRY WHILE @i < 256 BEGIN SET @k=CONCAT(N'wide',@i); EXEC sp_set_session_context @k, @v; SET @i+=1; END END TRY BEGIN CATCH SELECT @i AS stored, ERROR_NUMBER() AS number, ERROR_SEVERITY() AS severity, ERROR_STATE() AS state, ERROR_MESSAGE() AS message END CATCH"],
      ['batch', 'a', 'count wide keys', countFill('wide', 'count wide keys')],
    ],
  },
  {
    name: 'key comparison', sessions: ['a', 'b', 'c', 'd'], steps: [
      ['batch', 'a', 'lower key variants', "EXEC sp_set_session_context N'abc', 1; SELECT SESSION_CONTEXT(N'abc') AS abc, SESSION_CONTEXT(N'ABC') AS upper_abc, SESSION_CONTEXT(N'Abc') AS title_abc, SESSION_CONTEXT(N'aBc') AS mixed_abc, SESSION_CONTEXT(N'abC') AS last_upper, SESSION_CONTEXT(N'abc ') AS padded, SESSION_CONTEXT(N' abc') AS leading_space, SESSION_CONTEXT(N'ábc') AS accented"],
      ['batch', 'b', 'mixed key variants', "EXEC sp_set_session_context N'CaseKey', 1; SELECT SESSION_CONTEXT(N'CaseKey') AS exact_case, SESSION_CONTEXT(N'casekey') AS lower_case, SESSION_CONTEXT(N'CASEKEY') AS upper_case, SESSION_CONTEXT(N'caseKey') AS camel_case, SESSION_CONTEXT(N'CaseKey ') AS padded"],
      ['batch', 'b', 'mixed key second writer', "EXEC sp_set_session_context N'CASEKEY', 2; SELECT SESSION_CONTEXT(N'CaseKey') AS exact_case, SESSION_CONTEXT(N'casekey') AS lower_case, SESSION_CONTEXT(N'CASEKEY') AS upper_case"],
      ['batch', 'a', 'lower key more variants', "SELECT SESSION_CONTEXT(N'ABc') AS upper_prefix, SESSION_CONTEXT(N'ABc  ') AS upper_prefix_padded, SESSION_CONTEXT(N'abC ') AS last_upper_padded, SESSION_CONTEXT(N'ab') AS prefix, SESSION_CONTEXT(N'abcd') AS longer"],
      ['batch', 'a', 'five character key variants', "EXEC sp_set_session_context N'hello', 1; SELECT SESSION_CONTEXT(N'hello') AS exact, SESSION_CONTEXT(N'HELLo') AS upper_prefix, SESSION_CONTEXT(N'HeLLo') AS mixed_prefix, SESSION_CONTEXT(N'hellO') AS last_upper, SESSION_CONTEXT(N'HELLO') AS all_upper"],
      ['batch', 'a', 'single character key variants', "EXEC sp_set_session_context N'q', 1; SELECT SESSION_CONTEXT(N'q') AS lower_q, SESSION_CONTEXT(N'Q') AS upper_q"],
      ['batch', 'a', 'two character key variants', "EXEC sp_set_session_context N'aB', 1; SELECT SESSION_CONTEXT(N'aB') AS exact, SESSION_CONTEXT(N'AB') AS upper_first, SESSION_CONTEXT(N'Ab') AS lower_last, SESSION_CONTEXT(N'ab') AS all_lower"],
      ['batch', 'a', 'digit final key variants', "EXEC sp_set_session_context N'key1', 1; SELECT SESSION_CONTEXT(N'key1') AS exact, SESSION_CONTEXT(N'KEY1') AS upper_prefix, SESSION_CONTEXT(N'Key1') AS title"],
      ['batch', 'c', 'upper key variants', "EXEC sp_set_session_context N'XYZ', 1; SELECT SESSION_CONTEXT(N'XYZ') AS upper_xyz, SESSION_CONTEXT(N'xyz') AS lower_xyz, SESSION_CONTEXT(N'Xyz') AS title_xyz"],
      ['batch', 'd', 'read_only variants', "EXEC sp_set_session_context N'ro', 1, 1; EXEC sp_set_session_context N'RO', 2; EXEC sp_set_session_context N'Ro', 3; EXEC sp_set_session_context N'ro ', 4; SELECT SESSION_CONTEXT(N'ro') AS lower_key, SESSION_CONTEXT(N'RO') AS upper_key, SESSION_CONTEXT(N'Ro') AS title_key, SESSION_CONTEXT(N'rO') AS mixed_key"],
    ],
  },
  {
    name: 'size accounting', sessions: ['a'], steps: [
      ['batch', 'a', 'create edge procedure', "CREATE PROCEDURE dbo.ctx_edge @k SYSNAME, @unicode BIT AS BEGIN SET NOCOUNT ON; DECLARE @lo INT=0, @hi INT=CASE WHEN @unicode=1 THEN 4000 ELSE 8000 END, @mid INT, @a VARCHAR(8000), @n NVARCHAR(4000); WHILE @lo < @hi BEGIN SET @mid=(@lo+@hi+1)/2; EXEC sp_set_session_context @k, NULL; BEGIN TRY IF @unicode=1 BEGIN SET @n=REPLICATE(N'x',@mid); EXEC sp_set_session_context @k, @n END ELSE BEGIN SET @a=REPLICATE('x',@mid); EXEC sp_set_session_context @k, @a END; SET @lo=@mid END TRY BEGIN CATCH SET @hi=@mid-1 END CATCH END; EXEC sp_set_session_context @k, NULL; SELECT @k AS key_name, @unicode AS is_unicode, @lo AS max_length END"],
      ['batch', 'a', 'fill 125 values', "SET NOCOUNT ON; DECLARE @v VARCHAR(8000)=REPLICATE('x',8000), @i INT=0, @k SYSNAME; WHILE @i < 125 BEGIN SET @k=CONCAT(N'f',RIGHT(CONCAT('00',@i),3)); EXEC sp_set_session_context @k, @v; SET @i+=1 END; SELECT @@ERROR AS last_error"],
      ['batch', 'a', 'edge one character key', "EXEC dbo.ctx_edge N'e', 0"],
      ['batch', 'a', 'edge same key again', "EXEC dbo.ctx_edge N'e', 0"],
      ['batch', 'a', 'edge ten character key', "EXEC dbo.ctx_edge N'eeeeeeeeee', 0"],
      ['batch', 'a', 'edge one character key after longer', "EXEC dbo.ctx_edge N'e', 0"],
      ['batch', 'a', 'edge unicode value', "EXEC dbo.ctx_edge N'u', 1"],
      ['batch', 'a', 'edge after int value', "EXEC sp_set_session_context N'i', 1; EXEC dbo.ctx_edge N'e', 0"],
      ['batch', 'a', 'edge after null value', "EXEC sp_set_session_context N'i', NULL; EXEC dbo.ctx_edge N'e', 0"],
      ['batch', 'a', 'edge after new null key', "EXEC sp_set_session_context N'nullkey', NULL; EXEC dbo.ctx_edge N'e', 0"],
    ],
  },
  {
    name: 'batch and module scope', sessions: ['a'], steps: [
      ['batch', 'a', 'set in batch', "EXEC sp_set_session_context N'batch', 1"],
      ['batch', 'a', 'read in next batch', probe("N'batch'")],
      ['batch', 'a', 'rolled back transaction', "BEGIN TRANSACTION; EXEC sp_set_session_context N'tx', 1; EXEC sp_set_session_context N'batch', 2; ROLLBACK TRANSACTION; SELECT SESSION_CONTEXT(N'tx') AS tx_value, SESSION_CONTEXT(N'batch') AS batch_value, @@TRANCOUNT AS transaction_count"],
      ['batch', 'a', 'failed batch', "EXEC sp_set_session_context N'before throw', 1; THROW 51000, 'session context probe', 1; EXEC sp_set_session_context N'after throw', 1"],
      ['batch', 'a', 'after failed batch', "SELECT SESSION_CONTEXT(N'before throw') AS before_throw, SESSION_CONTEXT(N'after throw') AS after_throw"],
      ['batch', 'a', 'create setter procedure', "CREATE PROCEDURE dbo.ctx_set @v INT AS BEGIN EXEC sp_set_session_context N'proc', @v; SELECT SESSION_CONTEXT(N'proc') AS inside, SESSION_CONTEXT(N'batch') AS caller_value; END"],
      ['batch', 'a', 'procedure sets value', "EXEC dbo.ctx_set 7; SELECT SESSION_CONTEXT(N'proc') AS after_procedure"],
      ['batch', 'a', 'dynamic sql sets value', "EXEC sp_executesql N'EXEC sp_set_session_context N''dynamic'', 8; SELECT SESSION_CONTEXT(N''batch'') AS inner_value'; SELECT SESSION_CONTEXT(N'dynamic') AS after_dynamic"],
      ['batch', 'a', 'create view', "CREATE VIEW dbo.ctx_view AS SELECT SESSION_CONTEXT(N'proc') AS value"],
      ['batch', 'a', 'read through view', "SELECT value, CONVERT(NVARCHAR(128),SQL_VARIANT_PROPERTY(value,'BaseType')) AS base_type FROM dbo.ctx_view"],
      ['batch', 'a', 'create inline function', "CREATE FUNCTION dbo.ctx_fn(@k SYSNAME) RETURNS TABLE AS RETURN SELECT SESSION_CONTEXT(@k) AS value"],
      ['batch', 'a', 'read through inline function', "SELECT value FROM dbo.ctx_fn(N'proc')"],
      ['batch', 'a', 'create scalar function', "CREATE FUNCTION dbo.ctx_scalar() RETURNS INT AS BEGIN RETURN CAST(SESSION_CONTEXT(N'proc') AS INT) END"],
      ['batch', 'a', 'read through scalar function', 'SELECT dbo.ctx_scalar() AS value'],
      ['batch', 'a', 'create setter function', "CREATE FUNCTION dbo.ctx_bad() RETURNS INT AS BEGIN EXEC sp_set_session_context N'fn', 1; RETURN 1 END"],
      ['batch', 'a', 'call setter function', "SELECT dbo.ctx_bad() AS value; SELECT SESSION_CONTEXT(N'fn') AS fn_value"],
      ['batch', 'a', 'create table with context default', "CREATE TABLE dbo.ctx_rows(id INT NOT NULL CONSTRAINT pk_ctx_rows PRIMARY KEY, tenant INT NULL CONSTRAINT df_ctx_rows_tenant DEFAULT CAST(SESSION_CONTEXT(N'tenant') AS INT), raw_value SQL_VARIANT NULL CONSTRAINT df_ctx_rows_raw DEFAULT SESSION_CONTEXT(N'tenant'))"],
      ['batch', 'a', 'insert without tenant', 'INSERT dbo.ctx_rows(id) VALUES(1); SELECT id, tenant, raw_value FROM dbo.ctx_rows'],
      ['batch', 'a', 'insert with tenant', "EXEC sp_set_session_context N'tenant', 12; INSERT dbo.ctx_rows(id) VALUES(2); SELECT id, tenant, raw_value, CONVERT(NVARCHAR(128),SQL_VARIANT_PROPERTY(raw_value,'BaseType')) AS base_type FROM dbo.ctx_rows ORDER BY id"],
      ['batch', 'a', 'filter by context', "SELECT id FROM dbo.ctx_rows WHERE tenant = CAST(SESSION_CONTEXT(N'tenant') AS INT) ORDER BY id"],
      ['batch', 'a', 'create trigger', "CREATE TRIGGER dbo.ctx_rows_trigger ON dbo.ctx_rows AFTER UPDATE AS BEGIN SET NOCOUNT ON; EXEC sp_set_session_context N'trigger', 1; END"],
      ['batch', 'a', 'fire trigger', "UPDATE dbo.ctx_rows SET tenant=tenant WHERE id=2; SELECT SESSION_CONTEXT(N'trigger') AS after_trigger"],
    ],
  },
  {
    name: 'rpc calls', sessions: ['a'], steps: [
      ['procedure', 'a', 'procedure int', 'sp_set_session_context', [['key', 'NVarChar', 'rpc int'], ['value', 'Int', 42]]],
      ['rpc', 'a', 'read rpc int', probe('@k', 'read rpc int'), [['k', 'NVarChar', 'rpc int']]],
      ['batch', 'a', 'read rpc int in batch', probe("N'rpc int'")],
      ['procedure', 'a', 'procedure bigint', 'sp_set_session_context', [['key', 'NVarChar', 'rpc bigint'], ['value', 'BigInt', '9007199254740993']]],
      ['procedure', 'a', 'procedure nvarchar', 'sp_set_session_context', [['key', 'NVarChar', 'rpc nvarchar'], ['value', 'NVarChar', 'héllo']]],
      ['procedure', 'a', 'procedure varchar', 'sp_set_session_context', [['key', 'NVarChar', 'rpc varchar'], ['value', 'VarChar', 'abc']]],
      ['procedure', 'a', 'procedure nvarchar max', 'sp_set_session_context', [['key', 'NVarChar', 'rpc nvarchar max'], ['value', 'NVarChar', 'x', { length: 5000 }]]],
      ['procedure', 'a', 'procedure decimal', 'sp_set_session_context', [['key', 'NVarChar', 'rpc decimal'], ['value', 'Decimal', -12.34, { precision: 10, scale: 3 }]]],
      ['procedure', 'a', 'procedure float', 'sp_set_session_context', [['key', 'NVarChar', 'rpc float'], ['value', 'Float', 0.1]]],
      ['procedure', 'a', 'procedure bit', 'sp_set_session_context', [['key', 'NVarChar', 'rpc bit'], ['value', 'Bit', true]]],
      ['procedure', 'a', 'procedure varbinary', 'sp_set_session_context', [['key', 'NVarChar', 'rpc varbinary'], ['value', 'VarBinary', { binary: '0102' }]]],
      ['procedure', 'a', 'procedure uniqueidentifier', 'sp_set_session_context', [['key', 'NVarChar', 'rpc guid'], ['value', 'UniqueIdentifier', '6F9619FF-8B86-D011-B42D-00C04FC964FF']]],
      ['procedure', 'a', 'procedure date', 'sp_set_session_context', [['key', 'NVarChar', 'rpc date'], ['value', 'Date', { date: '2024-02-29T00:00:00.000Z' }]]],
      ['procedure', 'a', 'procedure datetime2', 'sp_set_session_context', [['key', 'NVarChar', 'rpc datetime2'], ['value', 'DateTime2', { date: '2024-02-29T12:34:56.789Z' }, { scale: 3 }]]],
      ['procedure', 'a', 'procedure datetimeoffset', 'sp_set_session_context', [['key', 'NVarChar', 'rpc datetimeoffset'], ['value', 'DateTimeOffset', { date: '2024-02-29T12:34:56.789Z' }, { scale: 3 }]]],
      ['procedure', 'a', 'procedure null int', 'sp_set_session_context', [['key', 'NVarChar', 'rpc null'], ['value', 'Int', null]]],
      ['procedure', 'a', 'procedure varchar key', 'sp_set_session_context', [['key', 'VarChar', 'rpc varchar key'], ['value', 'Int', 1]]],
      ['procedure', 'a', 'procedure missing value', 'sp_set_session_context', [['key', 'NVarChar', 'rpc missing']]],
      ['procedure', 'a', 'procedure read_only', 'sp_set_session_context', [['key', 'NVarChar', 'rpc ro'], ['value', 'Int', 1], ['read_only', 'Bit', true]]],
      ['procedure', 'a', 'procedure overwrite read_only', 'sp_set_session_context', [['key', 'NVarChar', 'rpc ro'], ['value', 'Int', 2]]],
      ['batch', 'a', 'read procedure values', ['rpc bigint', 'rpc nvarchar', 'rpc varchar', 'rpc nvarchar max', 'rpc decimal', 'rpc float', 'rpc bit', 'rpc varbinary', 'rpc guid', 'rpc date', 'rpc datetime2', 'rpc datetimeoffset', 'rpc null', 'rpc varchar key', 'rpc missing', 'rpc ro'].map(key => probe(`N'${key}'`)).join('; ')],
      ['rpc', 'a', 'sp_executesql set', "EXEC sp_set_session_context @k, @v /*sp_executesql set*/", [['k', 'NVarChar', 'executesql'], ['v', 'Int', 5]]],
      ['rpc', 'a', 'sp_executesql read', probe('@k', 'sp_executesql read'), [['k', 'NVarChar', 'executesql']]],
      ['rpc', 'a', 'sp_executesql varchar key read', probe('@k', 'sp_executesql varchar key read'), [['k', 'VarChar', 'executesql']]],
      ['rpc', 'a', 'sp_executesql nvarchar max key read', probe('@k', 'sp_executesql nvarchar max key read'), [['k', 'NVarChar', 'executesql', { length: 5000 }]]],
      ['rpc', 'a', 'sp_executesql null key read', probe('@k', 'sp_executesql null key read'), [['k', 'NVarChar', null]]],
      ['rpc', 'a', 'sp_executesql set read_only', "EXEC sp_set_session_context @k, @v, @ro /*sp_executesql set read_only*/", [['k', 'NVarChar', 'executesql ro'], ['v', 'NVarChar', 'first'], ['ro', 'Bit', true]]],
      ['rpc', 'a', 'sp_executesql overwrite read_only', "EXEC sp_set_session_context @k, @v /*sp_executesql overwrite read_only*/", [['k', 'NVarChar', 'executesql ro'], ['v', 'NVarChar', 'second']]],
      ['batch', 'a', 'read sp_executesql values', `${probe("N'executesql'")}; ${probe("N'executesql ro'")}`],
    ],
  },
  {
    name: 'prepared calls', sessions: ['a'], steps: [
      ['prepared', 'a', 'prepared set', "EXEC sp_set_session_context @k, @v, @ro /*prepared set*/", [['k', 'NVarChar'], ['v', 'Int'], ['ro', 'Bit']], [
        { k: 'p1', v: 1, ro: false },
        { k: 'p1', v: 2, ro: true },
        { k: 'p1', v: 3, ro: false },
        { k: 'p2', v: null, ro: false },
        { k: 'p3', v: 4, ro: false },
        { k: null, v: 5, ro: false },
        { k: 'p4', v: 6, ro: false },
      ]],
      ['prepared', 'a', 'prepared read', `SELECT SESSION_CONTEXT(@k) AS value, CONVERT(NVARCHAR(128),SQL_VARIANT_PROPERTY(SESSION_CONTEXT(@k),'BaseType')) AS base_type /*prepared read*/`, [['k', 'NVarChar']], [
        { k: 'p1' }, { k: 'p2' }, { k: 'p3' }, { k: 'p4' }, { k: 'missing' }, { k: null },
      ]],
      ['prepared', 'a', 'prepared nvarchar set', "EXEC sp_set_session_context @k, @v /*prepared nvarchar set*/", [['k', 'NVarChar'], ['v', 'NVarChar']], [
        { k: 'p5', v: 'first' }, { k: 'p5', v: 'second value' }, { k: 'p1', v: 'blocked' },
      ]],
      ['batch', 'a', 'read prepared values', ['p1', 'p2', 'p3', 'p4', 'p5'].map(key => probe(`N'${key}'`)).join('; ')],
    ],
  },
  {
    name: 'connection isolation', sessions: ['a', 'b', 'c'], steps: [
      ['batch', 'a', 'a sets shared', "EXEC sp_set_session_context N'shared', 1; EXEC sp_set_session_context N'a only', 1, 1; SELECT SESSION_CONTEXT(N'shared') AS shared, SESSION_CONTEXT(N'a only') AS a_only"],
      ['batch', 'b', 'b reads', "SELECT SESSION_CONTEXT(N'shared') AS shared, SESSION_CONTEXT(N'a only') AS a_only"],
      ['batch', 'b', 'b sets shared read_only', "EXEC sp_set_session_context N'shared', 2, 1; EXEC sp_set_session_context N'a only', 2; SELECT SESSION_CONTEXT(N'shared') AS shared, SESSION_CONTEXT(N'a only') AS a_only"],
      ['batch', 'a', 'a reads after b', "SELECT SESSION_CONTEXT(N'shared') AS shared, SESSION_CONTEXT(N'a only') AS a_only"],
      ['batch', 'a', 'a overwrites shared', "EXEC sp_set_session_context N'shared', 3; SELECT SESSION_CONTEXT(N'shared') AS shared"],
      ['batch', 'b', 'b overwrites read_only shared', "EXEC sp_set_session_context N'shared', 4; SELECT SESSION_CONTEXT(N'shared') AS shared"],
      ['close', 'a', 'close a'],
      ['open', 'c', 'open c'],
      ['batch', 'c', 'c reads after a closed', "SELECT SESSION_CONTEXT(N'shared') AS shared, SESSION_CONTEXT(N'a only') AS a_only"],
      ['batch', 'b', 'b reads at end', "SELECT SESSION_CONTEXT(N'shared') AS shared, SESSION_CONTEXT(N'a only') AS a_only"],
    ],
  },
  {
    name: 'connection reset', sessions: ['a', 'b'], steps: [
      ['batch', 'a', 'a sets values', "EXEC sp_set_session_context N'reset', 1; EXEC sp_set_session_context N'reset ro', 1, 1; SELECT SESSION_CONTEXT(N'reset') AS reset_value, SESSION_CONTEXT(N'reset ro') AS reset_ro"],
      ['batch', 'b', 'b sets values', "EXEC sp_set_session_context N'reset', 2, 1; SELECT SESSION_CONTEXT(N'reset') AS reset_value"],
      ['reset', 'a', 'reset a'],
      ['batch', 'a', 'a reads after reset', "SELECT SESSION_CONTEXT(N'reset') AS reset_value, SESSION_CONTEXT(N'reset ro') AS reset_ro"],
      ['batch', 'a', 'a sets former read_only key', "EXEC sp_set_session_context N'reset ro', 2; SELECT SESSION_CONTEXT(N'reset ro') AS reset_ro"],
      ['batch', 'b', 'b unaffected by a reset', "SELECT SESSION_CONTEXT(N'reset') AS reset_value"],
      ['rpc', 'a', 'a rpc set before second reset', "EXEC sp_set_session_context @k, @v /*rpc before reset*/", [['k', 'NVarChar', 'rpc reset'], ['v', 'Int', 3]]],
      ['batch', 'a', 'a fills before reset', "DECLARE @v VARCHAR(8000)=REPLICATE('x',8000), @i INT=0, @k SYSNAME, @e INT=0; WHILE @i < 256 BEGIN SET @k=CONCAT(N'resetfill',@i); EXEC sp_set_session_context @k, @v; SET @e=@@ERROR; IF @e <> 0 BREAK; SET @i+=1; END; SELECT @i AS stored, @e AS last_error"],
      ['batch', 'a', 'a opens transaction', "BEGIN TRANSACTION; EXEC sp_set_session_context N'in tx', 1; SELECT @@TRANCOUNT AS transaction_count"],
      ['reset', 'a', 'reset a with open transaction'],
      ['batch', 'a', 'a reads after second reset', `SELECT SESSION_CONTEXT(N'rpc reset') AS rpc_reset, SESSION_CONTEXT(N'in tx') AS in_tx, SESSION_CONTEXT(N'reset ro') AS reset_ro, @@TRANCOUNT AS transaction_count; ${countFill('resetfill', 'after second reset')}`],
      ['batch', 'a', 'a fills after reset', "DECLARE @v VARCHAR(8000)=REPLICATE('x',8000), @i INT=0, @k SYSNAME, @e INT=0; WHILE @i < 256 BEGIN SET @k=CONCAT(N'afterfill',@i); EXEC sp_set_session_context @k, @v; SET @e=@@ERROR; IF @e <> 0 BREAK; SET @i+=1; END; SELECT @i AS stored, @e AS last_error"],
      ['reset', 'b', 'reset b'],
      ['batch', 'b', 'b sets former read_only key', "EXEC sp_set_session_context N'reset', 5; SELECT SESSION_CONTEXT(N'reset') AS reset_value"],
    ],
  },
]

// Converts retained parameter descriptions to tedious values.
function parameterValue(value) {
  if (value && typeof value === 'object' && 'binary' in value) return Buffer.from(value.binary, 'hex')
  if (value && typeof value === 'object' && 'date' in value) return new Date(value.date)
  return value
}

function withParameters(connection, parameters, dispatch) {
  return {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch: request => {
      for (const [name, type, value, options] of parameters) request.addParameter(name, TYPES[type], parameterValue(value), options)
      dispatch(request)
    },
  }
}

// Mirrors capturePrepared from docs/reference-captures.md (PR #312), which is
// not yet on main: one sp_prepare, one sp_execute per value set and one
// sp_unprepare on a single Request. Preparation completes through the
// 'prepared'/'error' events; request.error is cleared before each phase and an
// error object already attributed to an earlier phase is never reported
// again, so only tokens raised while a phase is outstanding are recorded.
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
  for (const [name, type, options] of declarations) request.addParameter(name, TYPES[type], undefined, options)
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
  request.on('doneProc', (_count, _more, status) => { if (result) result.returnStatus = status })
  const phase = start => new Promise(resolve => {
    result = fresh()
    complete = (error, rowCount) => {
      complete = () => {}
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
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

// tedious reset: sets the RESETCONNECTION status bit on the next request, which
// is its initial SET-options batch. Records tokens raised during that request.
function captureReset(connection) {
  return new Promise(resolve => {
    const result = { errors: [], info: [], resetEvents: 0 }
    const onError = token => { if (result.errors.length < PREPARED_LIMITS.messages) result.errors.push(messageFields(token)) }
    const onInfo = token => { if (result.info.length < PREPARED_LIMITS.messages) result.info.push(messageFields(token)) }
    const onReset = () => { result.resetEvents++ }
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    connection.on('resetConnection', onReset)
    connection.reset(error => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      connection.off('resetConnection', onReset)
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      resolve(result)
    })
  })
}

const closeConnection = connection => connection.closed ? undefined : new Promise(resolve => { connection.once('end', resolve); connection.close() })

async function runScenario(config, scenario) {
  const records = []
  const sessions = new Map()
  const open = async label => { sessions.set(label, await connect(config)) }
  const session = label => {
    const connection = sessions.get(label)
    if (!connection) throw new Error(`session ${label} is not open`)
    return connection
  }
  try {
    for (const label of scenario.sessions.filter(label => !scenario.steps.some(([kind, target]) => kind === 'open' && target === label))) await open(label)
    for (const [kind, label, name, text, parameters, valueSets] of scenario.steps) {
      const record = { scenario: scenario.name, name, session: label, kind }
      if (kind === 'batch') Object.assign(record, { sql: text, result: canonical(await capture(session(label), text)) })
      else if (kind === 'rpc') Object.assign(record, { sql: text, parameters, result: canonical(await capture(withParameters(session(label), parameters, request => session(label).execSql(request)), text)) })
      else if (kind === 'procedure') Object.assign(record, { procedure: text, parameters, result: canonical(await capture(withParameters(session(label), parameters, request => session(label).callProcedure(request)), text)) })
      else if (kind === 'prepared') Object.assign(record, { sql: text, declarations: parameters, result: canonical(await capturePrepared(session(label), text, parameters, valueSets)) })
      else if (kind === 'reset') record.result = canonical(await captureReset(session(label)))
      else if (kind === 'close') { await closeConnection(session(label)); sessions.delete(label) }
      else if (kind === 'open') await open(label)
      else throw new Error(`unknown step kind ${kind}`)
      records.push(record)
    }
  } finally {
    for (const connection of sessions.values()) await closeConnection(connection)
  }
  return records
}

async function observe(connection, config) {
  const database = (await capture(connection, 'SELECT DB_NAME() AS name')).sets[0].rows[0][0]
  const scoped = { ...config, options: { ...config.options, database, requestTimeout: 120000 } }
  const records = []
  for (const scenario of scenarios) records.push(...await runScenario(scoped, scenario))
  return records
}

function validate(run) {
  const get = (scenario, name) => {
    const found = run.find(record => record.scenario === scenario && record.name === name)
    assert(found, `missing ${scenario}: ${name}`)
    return found.result
  }
  const rows = (scenario, name, set = 0) => get(scenario, name).sets[set]?.rows
  const errorNumber = (scenario, name) => get(scenario, name).errors[0]?.number ?? null
  // Descriptor: SESSION_CONTEXT returns sql_variant.
  assert.equal(get('function surface', 'missing key').sets[0].columns[0].type, 'Variant')
  assert.deepEqual(rows('function surface', 'missing key'), [[null]])
  // Stored base types.
  const baseType = name => rows('value types', `variable ${name}`)[0][1]
  assert.equal(baseType('int'), 'int')
  assert.equal(baseType('decimal'), 'decimal')
  assert.equal(baseType('datetimeoffset'), 'datetimeoffset')
  assert.equal(baseType('sql_variant int'), 'int')
  // read_only enforcement and connection scoping.
  assert.notEqual(errorNumber('read_only keys', 'overwrite read_only'), null)
  assert.deepEqual(rows('read_only keys', 'read_only after errors')[0].slice(0, 2), [1, 'int'])
  assert.deepEqual(rows('connection isolation', 'b reads'), [[null, null]])
  assert.deepEqual(rows('connection isolation', 'a reads after b'), [[1, 1]])
  assert.deepEqual(rows('connection reset', 'a reads after reset'), [[null, null]])
  assert.deepEqual(rows('connection reset', 'b unaffected by a reset'), [[2]])
  assert.deepEqual(rows('connection reset', 'a reads after second reset', 1), [[0, null, null]])
  // Unsupported value types and argument validation.
  for (const [label] of rejectedTypes) assert.equal(errorNumber('value types', `rejected ${label}`), 15600, label)
  assert.equal(baseType('nvarchar 4000'), 'nvarchar')
  assert.equal(rows('value types', 'literal large integer literal')[0][1], 'numeric')
  assert.equal(errorNumber('value types', 'literal expression argument'), 102)
  assert.equal(errorNumber('function surface', 'varchar key literal'), 8116)
  assert.equal(errorNumber('keys', 'empty key'), 15666)
  assert.equal(errorNumber('keys', '129 character key literal'), 15666)
  assert.equal(errorNumber('keys', 'null key'), 225)
  assert.equal(errorNumber('overwrite, NULL and arguments', 'missing value'), 16903)
  assert.equal(errorNumber('overwrite, NULL and arguments', 'too many arguments'), 16914)
  assert.deepEqual(rows('overwrite, NULL and arguments', 'unknown parameter name'), [[1]])
  // Key matching: collation-insensitive except for the final non-space character.
  assert.deepEqual(rows('key comparison', 'lower key variants'), [[1, null, 1, 1, null, 1, null, null]])
  assert.deepEqual(rows('key comparison', 'five character key variants'), [[1, 1, 1, null, null]])
  assert.deepEqual(rows('key comparison', 'read_only variants'), [[1, 2, 1, 2]])
  // Total size limit.
  assert.deepEqual(rows('size limits', 'fill until limit'), [[125, 15665]])
  assert.equal(errorNumber('size limits', 'fill until limit'), 15665)
  for (const record of run) {
    if (['batch', 'rpc', 'procedure'].includes(record.kind)) assert(record.result.done.length > 0, `${record.scenario}: ${record.name}: no completion`)
    if (record.kind === 'prepared') assert.equal(record.result.prepared, true, `${record.scenario}: ${record.name}: not prepared`)
  }
}

// Refuse before any container starts; the fixture is only checked for existence.
if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    console.log(`started ${container.name}`)
    const runs = []
    for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
      const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, connection => observe(connection, config))
      if (!oneDatabase) validate(run)
      runs.push(run)
    }
    if (!oneDatabase) assertSameCapture(runs[0], runs[1], 'session context observations differ across fresh databases')
    containers.push({ image: container.image, runs })
    console.log(`removing ${container.name}`)
  })
}
if (!oneDatabase) assertSameCapture(containers[0].runs, containers[1].runs, 'session context observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
if (!writeFixture && !oneDatabase) {
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
}
if (retained) assertSameCapture(actual, retained, 'session context observations differ from retained fixture')
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${containers[0].runs[0].length} session context observations` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained ? ' and matched retained fixture' : '') + (writeFixture ? '; wrote new fixture' : ''))
