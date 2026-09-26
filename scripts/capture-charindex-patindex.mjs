#!/usr/bin/env node
// Retain SQL Server CHARINDEX/PATINDEX rows, descriptors, diagnostics and completions.
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { isDeepStrictEqual } from 'node:util'
import { TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { capturePrepared, isolatedReference } from './lib/reference.mjs'

const fixture = new URL('../reference/charindex-patindex.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/charindex-patindex/capture.json')

// U+1F600 is a supplementary character (UTF-16 surrogate pair D83D DE00).
const emoji = "NCHAR(0xD83D)+NCHAR(0xDE00)"
const cases = [
  ['environment', "SELECT CAST(SERVERPROPERTY('ProductMajorVersion') AS INT) AS major, CAST(SERVERPROPERTY('Collation') AS NVARCHAR(128)) AS server_collation, DATABASEPROPERTYEX(DB_NAME(),'Collation') AS database_collation"],
  // CHARINDEX values
  ['charindex basic', "SELECT CHARINDEX('b','abcb') AS value"],
  ['charindex not found', "SELECT CHARINDEX('z','abc') AS value"],
  ['charindex multi-character', "SELECT CHARINDEX('cb','abcbcb') AS value"],
  ['charindex find longer than search', "SELECT CHARINDEX('abcd','abc') AS value"],
  ['charindex null find', "SELECT CHARINDEX(NULL,'abc') AS value"],
  ['charindex typed null find', "SELECT CHARINDEX(CAST(NULL AS VARCHAR(10)),'abc') AS value"],
  ['charindex null search', "SELECT CHARINDEX('a',CAST(NULL AS VARCHAR(10))) AS value"],
  ['charindex null start', "SELECT CHARINDEX('a','abc',NULL) AS value"],
  ['charindex all null', "SELECT CHARINDEX(NULL,NULL) AS value"],
  ['charindex all null with start', "SELECT CHARINDEX(NULL,NULL,NULL) AS value"],
  ['charindex empty find', "SELECT CHARINDEX('','abc') AS value"],
  ['charindex empty find with start', "SELECT CHARINDEX('','abc',2) AS value"],
  ['charindex empty search', "SELECT CHARINDEX('a','') AS value"],
  ['charindex both empty', "SELECT CHARINDEX('','') AS value"],
  ['charindex start 0', "SELECT CHARINDEX('a','aba',0) AS value"],
  ['charindex start negative', "SELECT CHARINDEX('a','aba',-5) AS value"],
  ['charindex start 1', "SELECT CHARINDEX('a','aba',1) AS value"],
  ['charindex start 2', "SELECT CHARINDEX('a','aba',2) AS value"],
  ['charindex start at last', "SELECT CHARINDEX('a','aba',3) AS value"],
  ['charindex start past length', "SELECT CHARINDEX('a','aba',4) AS value"],
  ['charindex start far beyond', "SELECT CHARINDEX('a','aba',1000000) AS value"],
  ['charindex start int max', "SELECT CHARINDEX('a','aba',2147483647) AS value"],
  ['charindex start bigint', "SELECT CHARINDEX('a','aba',CAST(2 AS BIGINT)) AS value"],
  ['charindex start bigint beyond int', "SELECT CHARINDEX('a','aba',CAST(3000000000 AS BIGINT)) AS value"],
  ['charindex start bigint negative', "SELECT CHARINDEX('a','aba',CAST(-3000000000 AS BIGINT)) AS value"],
  ['charindex start bigint literal', "SELECT CHARINDEX('a','aba',3000000000) AS value"],
  ['charindex start smallint', "SELECT CHARINDEX('a','aba',CAST(2 AS SMALLINT)) AS value"],
  ['charindex start tinyint', "SELECT CHARINDEX('a','aba',CAST(2 AS TINYINT)) AS value"],
  ['charindex start decimal', "SELECT CHARINDEX('a','aba',2.9) AS value"],
  ['charindex start float', "SELECT CHARINDEX('a','aba',CAST(2.9 AS FLOAT)) AS value"],
  ['charindex start numeric string', "SELECT CHARINDEX('a','aba','2') AS value"],
  ['charindex start invalid string', "SELECT CHARINDEX('a','aba','x') AS value"],
  ['charindex start bit', "SELECT CHARINDEX('a','aba',CAST(1 AS BIT)) AS value"],
  ['charindex start date rejected', "SELECT CHARINDEX('a','aba',CAST('2024-01-01' AS DATE)) AS value"],
  ['charindex start decimal 2.1 on aab', "SELECT CHARINDEX('a','aab',2.1) AS value"],
  ['charindex start decimal 2.5 on aab', "SELECT CHARINDEX('a','aab',2.5) AS value"],
  ['charindex start decimal 2.9 on aab', "SELECT CHARINDEX('a','aab',2.9) AS value"],
  ['charindex start decimal beyond int', "SELECT CHARINDEX('a','aab',3000000000.0) AS value"],
  ['charindex start money', "SELECT CHARINDEX('a','aab',CAST(2.9 AS MONEY)) AS value"],
  ['charindex max start bigint beyond int', "SELECT CHARINDEX('a',CAST('aab' AS VARCHAR(MAX)),CAST(3000000000 AS BIGINT)) AS value"],
  ['charindex max start bigint negative', "SELECT CHARINDEX('b',CAST('aab' AS VARCHAR(MAX)),CAST(-3000000000 AS BIGINT)) AS value"],
  // CHARINDEX type families
  ['charindex nvarchar', "SELECT CHARINDEX(N'b',N'abc') AS value"],
  ['charindex mixed varchar find nvarchar search', "SELECT CHARINDEX('b',N'abc') AS value"],
  ['charindex mixed nvarchar find varchar search', "SELECT CHARINDEX(N'b','abc') AS value"],
  ['charindex char padded', "SELECT CHARINDEX('b',CAST('ab' AS CHAR(10))) AS value"],
  ['charindex char padded space', "SELECT CHARINDEX(' ',CAST('ab' AS CHAR(10))) AS value"],
  ['charindex varchar max search', "SELECT CHARINDEX('b',CAST('abc' AS VARCHAR(MAX))) AS value"],
  ['charindex nvarchar max search', "SELECT CHARINDEX(N'b',CAST(N'abc' AS NVARCHAR(MAX))) AS value"],
  ['charindex varchar max find', "SELECT CHARINDEX(CAST('b' AS VARCHAR(MAX)),'abc') AS value"],
  ['charindex nvarchar max find', "SELECT CHARINDEX(CAST(N'b' AS NVARCHAR(MAX)),N'abc') AS value"],
  ['charindex max null search', "SELECT CHARINDEX('b',CAST(NULL AS VARCHAR(MAX))) AS value"],
  ['charindex max start bigint', "SELECT CHARINDEX('b',CAST('abcb' AS VARCHAR(MAX)),CAST(3 AS BIGINT)) AS value"],
  ['charindex max beyond 8000', "SELECT CHARINDEX('b',REPLICATE(CAST('a' AS VARCHAR(MAX)),9000)+'b') AS value"],
  ['charindex max start beyond 8000', "SELECT CHARINDEX('a',REPLICATE(CAST('a' AS VARCHAR(MAX)),9000),8999) AS value"],
  ['charindex nvarchar max beyond 4000', "SELECT CHARINDEX(N'b',REPLICATE(CAST(N'a' AS NVARCHAR(MAX)),5000)+N'b') AS value"],
  // trailing spaces
  ['charindex trailing space find', "SELECT CHARINDEX('a ','a') AS value"],
  ['charindex trailing space find matched', "SELECT CHARINDEX('a ','ba b') AS value"],
  ['charindex space only find', "SELECT CHARINDEX(' ','ab ') AS value"],
  ['charindex space only find absent', "SELECT CHARINDEX(' ','ab') AS value"],
  ['charindex trailing space search', "SELECT CHARINDEX('b','a  b  ') AS value"],
  ['charindex nvarchar trailing space find', "SELECT CHARINDEX(N'a ',N'a') AS value"],
  // collations
  ['charindex default case insensitive', "SELECT CHARINDEX('B','abc') AS value"],
  ['charindex case sensitive', "SELECT CHARINDEX('B','abc' COLLATE Latin1_General_CS_AS) AS value"],
  ['charindex case sensitive on find', "SELECT CHARINDEX('B' COLLATE Latin1_General_CS_AS,'abc') AS value"],
  ['charindex accent sensitive default', "SELECT CHARINDEX(N'e',N'caf'+NCHAR(233)) AS value"],
  ['charindex accent insensitive', "SELECT CHARINDEX(N'e',N'caf'+NCHAR(233) COLLATE Latin1_General_CI_AI) AS value"],
  ['charindex bin2 case', "SELECT CHARINDEX('B','abcB' COLLATE Latin1_General_BIN2) AS value"],
  ['charindex bin2 accent', "SELECT CHARINDEX(N'e',N'caf'+NCHAR(233)+N'e' COLLATE Latin1_General_BIN2) AS value"],
  ['charindex collation conflict', "SELECT CHARINDEX('a' COLLATE Latin1_General_CS_AS,'abc' COLLATE Latin1_General_CI_AS) AS value"],
  ['charindex expansion ss', "SELECT CHARINDEX(N'ss',N'stra'+NCHAR(223)+N'e') AS value"],
  ['charindex expansion ss bin2', "SELECT CHARINDEX(N'ss',N'stra'+NCHAR(223)+N'e' COLLATE Latin1_General_BIN2) AS value"],
  ['charindex ignorable character', "SELECT CHARINDEX(N'ab',N'a'+NCHAR(0x00AD)+N'b') AS value"],
  ['charindex ignorable character bin2', "SELECT CHARINDEX(N'ab',N'a'+NCHAR(0x00AD)+N'b' COLLATE Latin1_General_BIN2) AS value"],
  ['charindex utf8 varchar', "SELECT CHARINDEX('b',CAST(N'"+"é"+"b' COLLATE Latin1_General_100_CI_AS_SC_UTF8 AS VARCHAR(10)) COLLATE Latin1_General_100_CI_AS_SC_UTF8) AS value"],
  // surrogate pairs
  ['charindex after surrogate pair', "SELECT CHARINDEX(N'b',"+emoji+"+N'b') AS value"],
  ['charindex after surrogate pair sc', "SELECT CHARINDEX(N'b',"+emoji+"+N'b' COLLATE Latin1_General_100_CI_AS_SC) AS value"],
  ['charindex find surrogate pair', "SELECT CHARINDEX("+emoji+",N'a'+"+emoji+") AS value"],
  ['charindex find surrogate pair sc', "SELECT CHARINDEX("+emoji+",N'a'+"+emoji+" COLLATE Latin1_General_100_CI_AS_SC) AS value"],
  ['charindex find high surrogate', "SELECT CHARINDEX(NCHAR(0xD83D),N'a'+"+emoji+") AS value"],
  ['charindex find high surrogate sc', "SELECT CHARINDEX(NCHAR(0xD83D),N'a'+"+emoji+" COLLATE Latin1_General_100_CI_AS_SC) AS value"],
  ['charindex start inside surrogate sc', "SELECT CHARINDEX(N'b',"+emoji+"+N'b' COLLATE Latin1_General_100_CI_AS_SC,2) AS value"],
  ['charindex find surrogate bin2', "SELECT CHARINDEX(NCHAR(0xDE00),N'a'+"+emoji+" COLLATE Latin1_General_BIN2) AS value"],
  // other argument types
  ['charindex int arguments', "SELECT CHARINDEX(2,12345) AS value"],
  ['charindex int search', "SELECT CHARINDEX('3',12345) AS value"],
  ['charindex int search nvarchar find', "SELECT CHARINDEX(N'3',12345) AS value"],
  ['charindex decimal search', "SELECT CHARINDEX('.',CAST(12.5 AS DECIMAL(5,2))) AS value"],
  ['charindex int find', "SELECT CHARINDEX(3,'12345') AS value"],
  ['charindex binary arguments', "SELECT CHARINDEX(0x62,0x616263) AS value"],
  ['charindex varbinary max', "SELECT CHARINDEX(0x62,CAST(0x616263 AS VARBINARY(MAX))) AS value"],
  ['charindex text search', "SELECT CHARINDEX('b',CAST('abc' AS TEXT)) AS value"],
  ['charindex ntext search', "SELECT CHARINDEX(N'b',CAST(N'abc' AS NTEXT)) AS value"],
  ['charindex xml rejected', "SELECT CHARINDEX('a',CAST('<a/>' AS XML)) AS value"],
  ['charindex uniqueidentifier', "SELECT CHARINDEX('-',CAST('00000000-0000-0000-0000-000000000000' AS UNIQUEIDENTIFIER)) AS value"],
  ['charindex datetime', "SELECT CHARINDEX('2024',CAST('2024-01-02' AS DATETIME)) AS value"],
  ['charindex too few arguments', "SELECT CHARINDEX('a') AS value"],
  ['charindex too many arguments', "SELECT CHARINDEX('a','abc',1,1) AS value"],
  // PATINDEX values
  ['patindex basic', "SELECT PATINDEX('%b%','abcb') AS value"],
  ['patindex not found', "SELECT PATINDEX('%z%','abc') AS value"],
  ['patindex no wildcard exact', "SELECT PATINDEX('abc','abc') AS value"],
  ['patindex no wildcard partial', "SELECT PATINDEX('b','abc') AS value"],
  ['patindex prefix only', "SELECT PATINDEX('a%','abc') AS value"],
  ['patindex suffix only', "SELECT PATINDEX('%c','abc') AS value"],
  ['patindex suffix only miss', "SELECT PATINDEX('%b','abc') AS value"],
  ['patindex percent only', "SELECT PATINDEX('%','abc') AS value"],
  ['patindex percent only empty', "SELECT PATINDEX('%','') AS value"],
  ['patindex empty pattern', "SELECT PATINDEX('','abc') AS value"],
  ['patindex empty pattern empty', "SELECT PATINDEX('','') AS value"],
  ['patindex empty search', "SELECT PATINDEX('%a%','') AS value"],
  ['patindex null pattern', "SELECT PATINDEX(NULL,'abc') AS value"],
  ['patindex null search', "SELECT PATINDEX('%a%',NULL) AS value"],
  ['patindex typed null search', "SELECT PATINDEX('%a%',CAST(NULL AS NVARCHAR(10))) AS value"],
  ['patindex underscore', "SELECT PATINDEX('%b_d%','abcd') AS value"],
  ['patindex underscore only', "SELECT PATINDEX('_','a') AS value"],
  ['patindex underscore leading', "SELECT PATINDEX('%_c%','abc') AS value"],
  ['patindex range', "SELECT PATINDEX('%[c-e]%','abxd') AS value"],
  ['patindex set', "SELECT PATINDEX('%[xyz]%','abcz') AS value"],
  ['patindex negated set', "SELECT PATINDEX('%[^a]%','aab') AS value"],
  ['patindex negated range', "SELECT PATINDEX('%[^a-c]%','abcd') AS value"],
  ['patindex digit', "SELECT PATINDEX('%[0-9]%','ab3c') AS value"],
  ['patindex bracket percent', "SELECT PATINDEX('%[%]%','ab%c') AS value"],
  ['patindex bracket underscore', "SELECT PATINDEX('%[_]%','ab_c') AS value"],
  ['patindex bracket open bracket', "SELECT PATINDEX('%[[]%','ab[c') AS value"],
  ['patindex close bracket literal', "SELECT PATINDEX('%]%','ab]c') AS value"],
  ['patindex bracket close bracket', "SELECT PATINDEX('%[]]%','ab]c') AS value"],
  ['patindex bracket caret literal', "SELECT PATINDEX('%[a^]%','x^') AS value"],
  ['patindex bracket dash literal', "SELECT PATINDEX('%[-a]%','x-') AS value"],
  ['patindex bracket trailing dash', "SELECT PATINDEX('%[a-]%','x-') AS value"],
  ['patindex reversed range', "SELECT PATINDEX('%[z-a]%','m') AS value"],
  ['patindex empty brackets', "SELECT PATINDEX('%[]%','ab[]c') AS value"],
  ['patindex unclosed bracket', "SELECT PATINDEX('%[a%','x[a') AS value"],
  ['patindex escape clause rejected', "SELECT PATINDEX('%!%%' ESCAPE '!','a%b') AS value"],
  ['patindex backslash not escape', "SELECT PATINDEX('%\\%%','a\\b') AS value"],
  ['patindex multiple percent', "SELECT PATINDEX('%b%d%','abcd') AS value"],
  // PATINDEX types and spaces
  ['patindex nvarchar', "SELECT PATINDEX(N'%b%',N'abc') AS value"],
  ['patindex mixed pattern', "SELECT PATINDEX('%b%',N'abc') AS value"],
  ['patindex varchar max', "SELECT PATINDEX('%b%',CAST('abc' AS VARCHAR(MAX))) AS value"],
  ['patindex nvarchar max', "SELECT PATINDEX(N'%b%',CAST(N'abc' AS NVARCHAR(MAX))) AS value"],
  ['patindex max pattern', "SELECT PATINDEX(CAST('%b%' AS VARCHAR(MAX)),'abc') AS value"],
  ['patindex max beyond 8000', "SELECT PATINDEX('%b%',REPLICATE(CAST('a' AS VARCHAR(MAX)),9000)+'b') AS value"],
  ['patindex char padded suffix', "SELECT PATINDEX('%b',CAST('ab' AS CHAR(10))) AS value"],
  ['patindex trailing space search suffix', "SELECT PATINDEX('%b','ab  ') AS value"],
  ['patindex trailing space pattern', "SELECT PATINDEX('ab ','ab') AS value"],
  ['patindex trailing space both', "SELECT PATINDEX('%b ','ab ') AS value"],
  ['patindex nvarchar trailing space search suffix', "SELECT PATINDEX(N'%b',N'ab  ') AS value"],
  ['patindex space class', "SELECT PATINDEX('%[ ]%','ab c') AS value"],
  // PATINDEX collations
  ['patindex default case insensitive', "SELECT PATINDEX('%B%','abc') AS value"],
  ['patindex case sensitive', "SELECT PATINDEX('%B%','abc' COLLATE Latin1_General_CS_AS) AS value"],
  ['patindex range case insensitive', "SELECT PATINDEX('%[A-C]%','xb') AS value"],
  ['patindex range case sensitive', "SELECT PATINDEX('%[A-C]%','xb' COLLATE Latin1_General_CS_AS) AS value"],
  ['patindex range bin2', "SELECT PATINDEX('%[A-C]%','xbB' COLLATE Latin1_General_BIN2) AS value"],
  ['patindex range sql collation lowercase', "SELECT PATINDEX('%[a-c]%','xB' COLLATE SQL_Latin1_General_CP1_CS_AS) AS value"],
  ['patindex accent sensitive default', "SELECT PATINDEX(N'%e%',N'caf'+NCHAR(233)) AS value"],
  ['patindex accent insensitive', "SELECT PATINDEX(N'%e%',N'caf'+NCHAR(233) COLLATE Latin1_General_CI_AI) AS value"],
  ['patindex accent range', "SELECT PATINDEX(N'%[a-f]%',N'x'+NCHAR(233)) AS value"],
  ['patindex accent range bin2', "SELECT PATINDEX(N'%[a-f]%',N'x'+NCHAR(233) COLLATE Latin1_General_BIN2) AS value"],
  ['patindex collation conflict', "SELECT PATINDEX('%a%' COLLATE Latin1_General_CS_AS,'abc' COLLATE Latin1_General_CI_AS) AS value"],
  // PATINDEX surrogate pairs
  ['patindex after surrogate pair', "SELECT PATINDEX(N'%b%',"+emoji+"+N'b') AS value"],
  ['patindex after surrogate pair sc', "SELECT PATINDEX(N'%b%',"+emoji+"+N'b' COLLATE Latin1_General_100_CI_AS_SC) AS value"],
  ['patindex underscore surrogate', "SELECT PATINDEX(N'_b',"+emoji+"+N'b') AS value"],
  ['patindex underscore surrogate sc', "SELECT PATINDEX(N'_b',"+emoji+"+N'b' COLLATE Latin1_General_100_CI_AS_SC) AS value"],
  ['patindex two underscores surrogate', "SELECT PATINDEX(N'__b',"+emoji+"+N'b') AS value"],
  ['patindex two underscores surrogate sc', "SELECT PATINDEX(N'__b',"+emoji+"+N'b' COLLATE Latin1_General_100_CI_AS_SC) AS value"],
  ['patindex find surrogate pair', "SELECT PATINDEX(N'%'+"+emoji+"+N'%',N'a'+"+emoji+") AS value"],
  // PATINDEX other argument types
  ['patindex int search', "SELECT PATINDEX('%3%',12345) AS value"],
  ['patindex int pattern', "SELECT PATINDEX(3,'3') AS value"],
  ['patindex binary search', "SELECT PATINDEX('%b%',0x616263) AS value"],
  ['patindex text search', "SELECT PATINDEX('%b%',CAST('abc' AS TEXT)) AS value"],
  ['patindex ntext search', "SELECT PATINDEX(N'%b%',CAST(N'abc' AS NTEXT)) AS value"],
  ['patindex xml rejected', "SELECT PATINDEX('%a%',CAST('<a/>' AS XML)) AS value"],
  ['patindex start argument rejected', "SELECT PATINDEX('%a%','abc',1) AS value"],
  ['patindex too few arguments', "SELECT PATINDEX('%a%') AS value"],
  // column-sourced descriptors and aggregation over rows
  ['charindex columns', "SELECT id,CHARINDEX(f,s) AS vc,CHARINDEX(nf,ns) AS nv,CHARINDEX(f,ms) AS mx,CHARINDEX(f,s,st) AS st FROM dbo.ci_src ORDER BY id"],
  ['patindex columns', "SELECT id,PATINDEX(p,s) AS vc,PATINDEX(np,ns) AS nv,PATINDEX(p,ms) AS mx FROM dbo.ci_src ORDER BY id"],
  ['charindex not null column descriptor', "SELECT CHARINDEX(f,s) AS value FROM dbo.ci_nn"],
  ['patindex not null column descriptor', "SELECT PATINDEX(p,s) AS value FROM dbo.ci_nn"],
  ['charindex where filter', "SELECT id FROM dbo.ci_src WHERE CHARINDEX(f,s)>0 ORDER BY id"],
  ['patindex where filter', "SELECT id FROM dbo.ci_src WHERE PATINDEX(p,s)>0 ORDER BY id"],
  ['charindex result metadata', "SELECT name,system_type_name,is_nullable FROM sys.dm_exec_describe_first_result_set(N'SELECT CHARINDEX(''a'',''b'') AS a,CHARINDEX(N''a'',CAST(N''b'' AS NVARCHAR(MAX))) AS b,PATINDEX(''%a%'',''b'') AS c,PATINDEX(''%a%'',CAST(''b'' AS VARCHAR(MAX))) AS d,CHARINDEX(f,s) AS e,CHARINDEX(f,ms) AS f FROM dbo.ci_nn',NULL,0) ORDER BY column_ordinal"],
]

const setup = [
  ['create source', 'CREATE TABLE dbo.ci_src(id INT PRIMARY KEY,f VARCHAR(10),s VARCHAR(20),nf NVARCHAR(10),ns NVARCHAR(20),ms VARCHAR(MAX),st INT,p VARCHAR(20),np NVARCHAR(20))'],
  ['insert source', "INSERT dbo.ci_src VALUES (1,'b','abcb',N'b',N'abcb','abcb',3,'%b%',N'%b%'),(2,'z','abc',N'z',N'abc','abc',1,'%z%',N'%z%'),(3,NULL,'abc',NULL,N'abc','abc',NULL,NULL,NULL),(4,'a',NULL,N'a',NULL,NULL,-1,'%a%',N'%a%'),(5,'','abc',N'',N'abc','abc',0,'',N'')"],
  ['create not null source', 'CREATE TABLE dbo.ci_nn(f VARCHAR(10) NOT NULL,s VARCHAR(20) NOT NULL,ms VARCHAR(MAX) NOT NULL,p VARCHAR(20) NOT NULL)'],
  ['insert not null source', "INSERT dbo.ci_nn VALUES ('b','abc','abc','%b%')"],
]

// sp_executesql RPC: [name, sql, parameters]
const rpcCases = [
  ['rpc charindex varchar', 'SELECT CHARINDEX(@find,@search) AS value', [['find', TYPES.VarChar, 'b', { length: 10 }], ['search', TYPES.VarChar, 'abcb', { length: 20 }]]],
  ['rpc charindex nvarchar', 'SELECT CHARINDEX(@find,@search) AS value', [['find', TYPES.NVarChar, 'b', { length: 10 }], ['search', TYPES.NVarChar, 'abcb', { length: 20 }]]],
  ['rpc charindex nvarchar max', 'SELECT CHARINDEX(@find,@search) AS value', [['find', TYPES.NVarChar, 'b', { length: 10 }], ['search', TYPES.NVarChar, 'abcb', { length: Infinity }]]],
  ['rpc charindex varchar max', 'SELECT CHARINDEX(@find,@search) AS value', [['find', TYPES.VarChar, 'b', { length: 10 }], ['search', TYPES.VarChar, 'abcb', { length: Infinity }]]],
  ['rpc charindex null find', 'SELECT CHARINDEX(@find,@search) AS value', [['find', TYPES.NVarChar, null, { length: 10 }], ['search', TYPES.NVarChar, 'abcb', { length: 20 }]]],
  ['rpc charindex empty find', 'SELECT CHARINDEX(@find,@search) AS value', [['find', TYPES.NVarChar, '', { length: 10 }], ['search', TYPES.NVarChar, 'abcb', { length: 20 }]]],
  ['rpc charindex int start', 'SELECT CHARINDEX(@find,@search,@start) AS value', [['find', TYPES.NVarChar, 'b', { length: 10 }], ['search', TYPES.NVarChar, 'abcb', { length: 20 }], ['start', TYPES.Int, 3]]],
  ['rpc charindex zero start', 'SELECT CHARINDEX(@find,@search,@start) AS value', [['find', TYPES.NVarChar, 'b', { length: 10 }], ['search', TYPES.NVarChar, 'abcb', { length: 20 }], ['start', TYPES.Int, 0]]],
  ['rpc charindex negative start', 'SELECT CHARINDEX(@find,@search,@start) AS value', [['find', TYPES.NVarChar, 'b', { length: 10 }], ['search', TYPES.NVarChar, 'abcb', { length: 20 }], ['start', TYPES.Int, -7]]],
  ['rpc charindex null start', 'SELECT CHARINDEX(@find,@search,@start) AS value', [['find', TYPES.NVarChar, 'b', { length: 10 }], ['search', TYPES.NVarChar, 'abcb', { length: 20 }], ['start', TYPES.Int, null]]],
  ['rpc charindex bigint start', 'SELECT CHARINDEX(@find,@search,@start) AS value', [['find', TYPES.NVarChar, 'b', { length: 10 }], ['search', TYPES.NVarChar, 'abcb', { length: 20 }], ['start', TYPES.BigInt, '3']]],
  ['rpc charindex bigint start beyond int', 'SELECT CHARINDEX(@find,@search,@start) AS value', [['find', TYPES.NVarChar, 'b', { length: 10 }], ['search', TYPES.NVarChar, 'abcb', { length: 20 }], ['start', TYPES.BigInt, '3000000000']]],
  ['rpc charindex trailing space find', 'SELECT CHARINDEX(@find,@search) AS value', [['find', TYPES.NVarChar, 'a ', { length: 10 }], ['search', TYPES.NVarChar, 'a', { length: 20 }]]],
  ['rpc charindex surrogate', 'SELECT CHARINDEX(@find,@search) AS value', [['find', TYPES.NVarChar, 'b', { length: 10 }], ['search', TYPES.NVarChar, '\u{1F600}b', { length: 20 }]]],
  ['rpc charindex collate parameter', 'SELECT CHARINDEX(@find,@search COLLATE Latin1_General_CS_AS) AS value', [['find', TYPES.NVarChar, 'B', { length: 10 }], ['search', TYPES.NVarChar, 'abc', { length: 20 }]]],
  ['rpc charindex int parameter search', 'SELECT CHARINDEX(@find,@search) AS value', [['find', TYPES.NVarChar, '3', { length: 10 }], ['search', TYPES.Int, 12345]]],
  ['rpc patindex varchar', 'SELECT PATINDEX(@pattern,@search) AS value', [['pattern', TYPES.VarChar, '%[c-e]%', { length: 20 }], ['search', TYPES.VarChar, 'abxd', { length: 20 }]]],
  ['rpc patindex nvarchar max', 'SELECT PATINDEX(@pattern,@search) AS value', [['pattern', TYPES.NVarChar, '%b%', { length: 20 }], ['search', TYPES.NVarChar, 'abc', { length: Infinity }]]],
  ['rpc patindex null pattern', 'SELECT PATINDEX(@pattern,@search) AS value', [['pattern', TYPES.NVarChar, null, { length: 20 }], ['search', TYPES.NVarChar, 'abc', { length: 20 }]]],
  ['rpc patindex empty pattern', 'SELECT PATINDEX(@pattern,@search) AS value', [['pattern', TYPES.NVarChar, '', { length: 20 }], ['search', TYPES.NVarChar, 'abc', { length: 20 }]]],
  ['rpc patindex trailing space search', 'SELECT PATINDEX(@pattern,@search) AS value', [['pattern', TYPES.NVarChar, '%b', { length: 20 }], ['search', TYPES.NVarChar, 'ab  ', { length: 20 }]]],
  ['rpc patindex bracket escape', 'SELECT PATINDEX(@pattern,@search) AS value', [['pattern', TYPES.NVarChar, '%[%]%', { length: 20 }], ['search', TYPES.NVarChar, 'ab%c', { length: 20 }]]],
  ['rpc patindex surrogate underscore', 'SELECT PATINDEX(@pattern,@search) AS value', [['pattern', TYPES.NVarChar, '_b', { length: 20 }], ['search', TYPES.NVarChar, '\u{1F600}b', { length: 20 }]]],
]

// sp_prepare/sp_execute: one handle per statement, executed with each value set.
const preparedCases = [
  ['prepared charindex nvarchar start', 'SELECT CHARINDEX(@find,@search,@start) AS value',
    [['find', TYPES.NVarChar, { length: 10 }], ['search', TYPES.NVarChar, { length: 20 }], ['start', TYPES.Int]],
    [{ find: 'b', search: 'abcb', start: 1 }, { find: 'b', search: 'abcb', start: 3 }, { find: 'b', search: 'abcb', start: 0 }, { find: 'b', search: 'abcb', start: -1 }, { find: 'b', search: 'abcb', start: 99 }, { find: null, search: 'abcb', start: 1 }, { find: '', search: 'abcb', start: 1 }, { find: 'b', search: 'abcb', start: null }]],
  ['prepared charindex bigint start', 'SELECT CHARINDEX(@find,@search,@start) AS value',
    [['find', TYPES.VarChar, { length: 10 }], ['search', TYPES.VarChar, { length: 20 }], ['start', TYPES.BigInt]],
    [{ find: 'b', search: 'abcb', start: '3' }, { find: 'b', search: 'abcb', start: '3000000000' }, { find: 'b', search: 'abcb', start: '-3000000000' }]],
  ['prepared charindex varchar max', 'SELECT CHARINDEX(@find,@search) AS value',
    [['find', TYPES.VarChar, { length: 10 }], ['search', TYPES.VarChar, { length: Infinity }]],
    [{ find: 'b', search: 'abcb' }, { find: 'b', search: null }]],
  ['prepared patindex nvarchar', 'SELECT PATINDEX(@pattern,@search) AS value',
    [['pattern', TYPES.NVarChar, { length: 20 }], ['search', TYPES.NVarChar, { length: 20 }]],
    [{ pattern: '%b%', search: 'abc' }, { pattern: '%[^a]%', search: 'aab' }, { pattern: '_b', search: '\u{1F600}b' }, { pattern: '%b', search: 'ab  ' }, { pattern: null, search: 'abc' }, { pattern: '', search: '' }]],
]

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

// The shared helper attributes only errors raised during each execution. The
// retained shape omits rowCount for preparation and unpreparation.
async function prepared(connection, sql, declarations, executions) {
  const handle = await capturePrepared(connection, sql, declarations, executions)
  const { rowCount: _prepareCount, ...preparation } = handle.prepare
  if (!handle.prepared) return { preparation: canonical(preparation), prepared: false, executions: [] }
  const { rowCount: _unprepareCount, ...unpreparation } = handle.unprepare
  return {
    preparation: canonical(preparation), prepared: true,
    executions: handle.executions.map(({ values, result }) => ({ values, result: canonical(result) })),
    unpreparation: canonical(unpreparation),
  }
}

async function observe(connection) {
  const records = []
  const describe = parameters => parameters.map(([name, type, value, options]) => ({ name, type: type.name, value, ...(options ? { options: { ...options, ...(options.length === Infinity ? { length: 'max' } : {}) } } : {}) }))
  for (const [name, sql] of [...setup, ...cases]) records.push({ name, sql, result: canonical(await capture(connection, sql)) })
  for (const [name, sql, parameters] of rpcCases) records.push({ name, sql, protocol: 'sp_executesql', parameters: describe(parameters), result: canonical(await rpc(connection, sql, parameters)) })
  for (const [name, sql, declarations, executions] of preparedCases) {
    records.push({
      name, sql, protocol: 'sp_prepare/sp_execute/sp_unprepare',
      parameters: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, ...(options ? { options: { ...options, ...(options.length === Infinity ? { length: 'max' } : {}) } } : {}) })),
      ...(await prepared(connection, sql, declarations, executions)),
    })
  }
  records.push({ name: 'reuse', sql: 'SELECT @@TRANCOUNT AS transaction_count, 1 AS reusable', result: canonical(await capture(connection, 'SELECT @@TRANCOUNT AS transaction_count, 1 AS reusable')) })
  return records
}

function validate(run) {
  assert.equal(run.length, setup.length + cases.length + rpcCases.length + preparedCases.length + 1)
  const get = name => {
    const record = run.find(record => record.name === name)
    assert(record, 'missing ' + name)
    return record.result
  }
  for (const [name] of setup) assert.equal(get(name).errors.length, 0, name)
  const value = name => get(name).sets[0]?.rows[0]?.[0]
  const column = name => get(name).sets[0]?.columns[0]
  for (const record of run) {
    if (record.result) assert(record.result.done.length > 0, record.name + ': no completion')
    else for (const execution of record.executions) assert(execution.result.done.length > 0, record.name + ': no completion')
  }
  assert.equal(value('charindex basic'), 2)
  assert.equal(value('charindex not found'), 0)
  assert.equal(value('charindex null find'), null)
  assert.equal(value('patindex basic'), 2)
  for (const [name, expected] of [
    ['charindex empty find', 0], ['charindex start 0', 1], ['charindex start negative', 1],
    ['charindex start past length', 0], ['charindex start decimal 2.9 on aab', 2],
    ['charindex trailing space find', 0], ['charindex case sensitive', 0], ['charindex bin2 case', 4],
    ['charindex after surrogate pair', 3], ['charindex after surrogate pair sc', 2],
    ['patindex empty pattern', 0], ['patindex empty pattern empty', 1], ['patindex no wildcard partial', 0],
    ['patindex trailing space search suffix', 2], ['patindex nvarchar trailing space search suffix', 0],
    ['patindex bracket close bracket', 0], ['patindex underscore surrogate sc', 1],
  ]) assert.equal(value(name), expected, name)
  for (const [name, type, length] of [
    ['charindex basic', 'IntN', 4], ['charindex varchar max find', 'IntN', 4],
    ['charindex varchar max search', 'IntN', 8], ['charindex nvarchar max search', 'IntN', 8],
    ['charindex varbinary max', 'IntN', 8], ['patindex nvarchar max', 'IntN', 8], ['patindex max pattern', 'IntN', 4],
    ['charindex start bigint', 'IntN', 4], ['charindex not null column descriptor', 'IntN', 4],
  ]) {
    assert.equal(column(name).type, type, name)
    assert.equal(column(name).length, length, name)
    assert.equal(column(name).flags, 33, name)
  }
  for (const [name, number] of [
    ['charindex start bigint beyond int', 8115], ['charindex start float', 8116],
    ['charindex start numeric string', 8116], ['charindex int find', 8116],
    ['charindex collation conflict', 468], ['charindex xml rejected', 257],
    ['charindex too few arguments', 189], ['patindex null search', 8116],
    ['patindex int search', 8116], ['patindex escape clause rejected', 156],
    ['patindex start argument rejected', 174], ['rpc charindex bigint start beyond int', 8115],
  ]) assert.equal(get(name).errors[0]?.number, number, name)
  assert.equal(get('reuse').errors.length, 0)
  return { value, column, get }
}

// Node 24 inspects both operands while building a failed assert.equal or
// assert.deepEqual AssertionError, even with a custom message. For multi-MB
// captures that grew a process to ~123 GB. Compare whole captures with
// isDeepStrictEqual and report only the first differing record name.
function requireEqual(left, right, message) {
  if (isDeepStrictEqual(left, right)) return
  const a = left?.containers ? left.containers.flatMap(c => c.runs.flat()) : left
  const b = right?.containers ? right.containers.flatMap(c => c.runs.flat()) : right
  const index = Array.isArray(a) && Array.isArray(b)
    ? Array.from({ length: Math.max(a.length, b.length) }, (_, i) => i).find(i => !isDeepStrictEqual(a[i], b[i])) ?? -1
    : -1
  throw new Error(message + (index >= 0 ? ` (first difference at record ${index}: ${JSON.stringify(a[index]?.name ?? b[index]?.name ?? null)})` : ''))
}

const fixtureExists = existsSync(fixture)
if (writeFixture && fixtureExists) throw new Error('refusing to overwrite retained fixture ' + fixture.pathname)
await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < 2; containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let databaseIndex = 0; databaseIndex < 2; databaseIndex++) {
      const run = await isolatedReference({
        ...config, options: { ...config.options, requestTimeout: 120000 },
      }, observe)
      validate(run)
      runs.push(run)
    }
    requireEqual(runs[0], runs[1], 'CHARINDEX/PATINDEX observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
requireEqual(containers[0].runs[0], containers[1].runs[0], 'CHARINDEX/PATINDEX observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
if (fixtureExists) requireEqual(actual, JSON.parse(await readFile(fixture, 'utf8')), 'CHARINDEX/PATINDEX observations differ from retained fixture')
if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
console.log('Captured ' + containers[0].runs[0].length + ' CHARINDEX/PATINDEX programs in four fresh databases across two containers' + (fixtureExists ? ' and matched retained fixture' : ''))
