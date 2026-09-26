#!/usr/bin/env node
// Retain SQL Server 2025 REGEXP_LIKE, REGEXP_REPLACE, REGEXP_SUBSTR,
// REGEXP_INSTR, REGEXP_COUNT, REGEXP_MATCHES and REGEXP_SPLIT_TO_TABLE rows,
// descriptors, diagnostics and completions for ordinary batches,
// sp_executesql RPC and prepared handles.
import assert from 'node:assert/strict'
import { mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/regexp-functions.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('--write-fixture requires the full four-database capture')
const output = resolve(positional[0] ?? 'artifacts/compatibility/regexp-functions/capture.json')

// REGEXP_LIKE is a predicate; CASE exposes its three-valued result as a row.
const like = (source, pattern, flags) => {
  const call = `REGEXP_LIKE(${source},${pattern}${flags === undefined ? '' : ',' + flags})`
  return `CASE WHEN ${call} THEN 1 WHEN NOT ${call} THEN 0 ELSE -1 END`
}
// U+1F600 is a supplementary character (UTF-16 surrogate pair D83D DE00).
const emoji = 'NCHAR(0xD83D)+NCHAR(0xDE00)'
const compatibility = level => `DECLARE @sql NVARCHAR(400)=N'ALTER DATABASE '+QUOTENAME(DB_NAME())+N' SET COMPATIBILITY_LEVEL=${level}';EXEC(@sql)`

const setup = [
  ['create source', "CREATE TABLE dbo.regexp_src(id INT PRIMARY KEY,v VARCHAR(20),n NVARCHAR(20),nm NVARCHAR(MAX),cs VARCHAR(20) COLLATE Latin1_General_CS_AS,pattern VARCHAR(20),flags VARCHAR(10))"],
  ['insert source', "INSERT dbo.regexp_src VALUES (1,'abc123',N'abc123',N'abc123','ABC','[0-9]+','c'),(2,'ABC',N'ABC',N'ABC','abc','b','i'),(3,NULL,NULL,NULL,NULL,NULL,NULL),(4,'a,b,,c',N'a,b,,c',N'a,b,,c','A,B',',',NULL)"],
  ['create check constraint', "CREATE TABLE dbo.regexp_check(id INT,code VARCHAR(10) CONSTRAINT regexp_code CHECK (REGEXP_LIKE(code,'^[A-Z]{2}[0-9]$')))"],
  ['check constraint accepts', "INSERT dbo.regexp_check VALUES (1,'AB1'),(2,NULL)"],
  ['check constraint rejects', "INSERT dbo.regexp_check VALUES (3,'ab1')"],
]

const cases = [
  ['environment', "SELECT CAST(SERVERPROPERTY('ProductMajorVersion') AS INT) AS major,CAST(SERVERPROPERTY('Collation') AS NVARCHAR(128)) AS server_collation,DATABASEPROPERTYEX(DB_NAME(),'Collation') AS database_collation,(SELECT compatibility_level FROM sys.databases WHERE name=DB_NAME()) AS compatibility_level"],
  ['builtin catalog', "SELECT name,type FROM sys.all_objects WHERE name LIKE 'REGEXP[_]%' AND is_ms_shipped=1 ORDER BY name"],

  // REGEXP_LIKE
  ['like basic', `SELECT ${like("'abc'", "'^a'")} AS a,${like("'abc'", "'^b'")} AS b,${like("N'abc'", "N'c$'")} AS c`],
  ['like case default and flags', `SELECT ${like("'ABC'", "'^a'")} AS d,${like("'ABC'", "'^a'", "'i'")} AS i,${like("'ABC'", "'^a'", "'c'")} AS c,${like("'ABC'", "'^a'", "'ic'")} AS ic,${like("'ABC'", "'^a'", "'ci'")} AS ci,${like("'ABC'", "'^a'", "'ii'")} AS ii`],
  ['like multiline flag', `SELECT ${like("'a'+CHAR(10)+'b'", "'^b$'")} AS d,${like("'a'+CHAR(10)+'b'", "'^b$'", "'m'")} AS m`],
  ['like dotall flag', `SELECT ${like("'a'+CHAR(10)+'b'", "'a.b'")} AS d,${like("'a'+CHAR(10)+'b'", "'a.b'", "'s'")} AS s,${like("'a'+CHAR(13)+CHAR(10)+'b'", "'a..b'", "'s'")} AS crlf`],
  ['like empty flags', `SELECT ${like("'abc'", "'a'", "''")} AS v`],
  ['like inline flag', `SELECT ${like("'abc'", "'(?i)B'")} AS a,${like("'ABC'", "'(?-i)b'", "'i'")} AS b`],
  ['like invalid flag x', `SELECT ${like("'abc'", "'a'", "'x'")} AS v`],
  ['like invalid flag uppercase', `SELECT ${like("'abc'", "'a'", "'I'")} AS v`],
  ['like invalid flag among valid', `SELECT ${like("'abc'", "'a'", "'imq'")} AS v`],
  ['like nulls', `SELECT ${like('NULL', "'a'")} AS source,${like("'a'", 'NULL')} AS pattern,${like("'a'", "'a'", 'NULL')} AS flags,${like('CAST(NULL AS VARCHAR(10))', "'a'")} AS typed`],
  ['like empty pattern and source', `SELECT ${like("''", "''")} AS a,${like("'abc'", "''")} AS b,${like("''", "'a'")} AS c,${like("''", "'^$'")} AS d`],
  ['like escapes and classes', `SELECT ${like("'abc'", "'\\p{L}+'")} AS unicode_letter,${like("'abc'", "'\\d'")} AS digit,${like("'a b'", "'\\bb\\b'")} AS boundary,${like("'abc'", "'\\z'")} AS end_text,${like("'a.c'", "'\\Q.\\E'")} AS quoted,${like("'abc'", "'[[:alpha:]]'")} AS posix`],
  ['like trailing spaces', `SELECT ${like("CAST('ab' AS CHAR(4))", "'b $'")} AS char_pad,${like("'ab  '", "'b$'")} AS varchar_space,${like("N'ab  '", "N'b$'")} AS nvarchar_space`],
  ['like varchar max source', `SELECT ${like("CAST('abc' AS VARCHAR(MAX))", "'b'")} AS a,${like("CAST(N'abc' AS NVARCHAR(MAX))", "N'b'")} AS b`],
  ['like pattern longer than 8000', `SELECT ${like("'abc'", "REPLICATE('a',9000)")} AS v`],
  ['like max pattern rejected', `SELECT ${like("'abc'", "CAST('b' AS VARCHAR(MAX))")} AS v`],
  ['like nvarchar max pattern rejected', `SELECT ${like("N'abc'", "CAST(N'b' AS NVARCHAR(MAX))")} AS v`],
  ['like nvarchar flags rejected', `SELECT ${like("'ABC'", "'a'", "N'i'")} AS v`],
  ['like int source rejected', `SELECT ${like('1', "'1'")} AS v`],
  ['like int pattern rejected', `SELECT ${like("'1'", '1')} AS v`],
  ['like int flags rejected', `SELECT ${like("'1'", "'1'", '1')} AS v`],
  ['like select list rejected', "SELECT REGEXP_LIKE('abc','^a') AS v"],
  ['like one argument', "SELECT CASE WHEN REGEXP_LIKE('abc') THEN 1 ELSE 0 END AS v"],
  ['like four arguments', "SELECT CASE WHEN REGEXP_LIKE('abc','a','i','c') THEN 1 ELSE 0 END AS v"],
  ['like where filter', "SELECT id FROM dbo.regexp_src WHERE REGEXP_LIKE(v,'[0-9]') OR REGEXP_LIKE(n,'^abc$','i') ORDER BY id"],
  ['like where negated', "SELECT id FROM dbo.regexp_src WHERE NOT REGEXP_LIKE(v,'[0-9]') ORDER BY id"],
  ['like column pattern and flags', "SELECT id,CASE WHEN REGEXP_LIKE(v,pattern,flags) THEN 1 WHEN NOT REGEXP_LIKE(v,pattern,flags) THEN 0 ELSE -1 END AS v FROM dbo.regexp_src ORDER BY id"],
  ['like column collation ignored', "SELECT id,CASE WHEN REGEXP_LIKE(cs,'^a') THEN 1 ELSE 0 END AS v FROM dbo.regexp_src WHERE id IN (1,2) ORDER BY id"],
  ['check constraint rows', 'SELECT id,code FROM dbo.regexp_check ORDER BY id'],

  // Invalid patterns
  ['invalid unclosed group', "SELECT REGEXP_REPLACE('abc','(','x') AS v"],
  ['invalid unclosed class', `SELECT ${like("'abc'", "'['")} AS v`],
  ['invalid lookahead', `SELECT ${like("'abc'", "'a(?=b)'")} AS v`],
  ['invalid backreference', `SELECT ${like("'aa'", "'(a)\\1'")} AS v`],
  ['invalid repetition range', `SELECT ${like("'abc'", "'a{2,1}'")} AS v`],
  ['invalid repetition size', `SELECT ${like("'abc'", "'a{1001}'")} AS v`],
  ['repetition size 1000', `SELECT ${like("'abc'", "'a{0,1000}'")} AS v`],
  ['invalid missing repetition argument', `SELECT ${like("'abc'", "'*'")} AS v`],
  ['invalid unmatched close paren', "SELECT REGEXP_COUNT('abc',')') AS v"],
  ['invalid trailing backslash', "SELECT REGEXP_COUNT('abc','a\\') AS v"],
  ['invalid pattern with null source', "SELECT REGEXP_COUNT(NULL,'(') AS a"],
  ['invalid pattern over empty table', "SELECT REGEXP_COUNT(v,'(') AS a FROM dbo.regexp_src WHERE 1=0"],
  ['invalid pattern from column', "SELECT id,REGEXP_COUNT(v,CASE WHEN id=2 THEN '(' ELSE 'b' END) AS a FROM dbo.regexp_src ORDER BY id"],

  // REGEXP_REPLACE
  ['replace basic', "SELECT REGEXP_REPLACE('abcabc','b','X') AS a,REGEXP_REPLACE('abc','b') AS omitted,REGEXP_REPLACE('abc','z','X') AS no_match"],
  ['replace start and occurrence', "SELECT REGEXP_REPLACE('abcabc','b','X',1,2) AS a,REGEXP_REPLACE('abcabc','b','X',3) AS b,REGEXP_REPLACE('abcabc','b','X',1,0) AS c,REGEXP_REPLACE('abcabc','b','X',1,1) AS d,REGEXP_REPLACE('abcabc','b','X',100) AS e,REGEXP_REPLACE('abcabc','b','X',1,5) AS f,REGEXP_REPLACE('abcabc','b','X',3,2) AS g"],
  ['replace flags', "SELECT REGEXP_REPLACE('ABCabc','b','X',1,0,'i') AS i,REGEXP_REPLACE('ABCabc','b','X',1,0,'c') AS c,REGEXP_REPLACE('a'+CHAR(10)+'b','^b','X',1,0,'m') AS m"],
  ['replace start zero', "SELECT REGEXP_REPLACE('abcabc','b','X',0) AS v"],
  ['replace start negative', "SELECT REGEXP_REPLACE('abcabc','b','X',-1) AS v"],
  ['replace occurrence negative', "SELECT REGEXP_REPLACE('abcabc','b','X',1,-1) AS v"],
  ['replace invalid flag', "SELECT REGEXP_REPLACE('abc','b','X',1,0,'q') AS v"],
  ['replace backreferences', "SELECT REGEXP_REPLACE('abc','(b)','[\\1]') AS backslash_one,REGEXP_REPLACE('abc','(b)','[$1]') AS dollar_one,REGEXP_REPLACE('abc','(b)','[\\0]') AS backslash_zero,REGEXP_REPLACE('abc','(b)','[\\\\]') AS double_backslash,REGEXP_REPLACE('abc','(b)','[\\2]') AS missing_group,REGEXP_REPLACE('abc','b','[&]') AS ampersand"],
  ['replace trailing backslash replacement', "SELECT REGEXP_REPLACE('abc','b','x\\') AS v"],
  ['replace nulls', "SELECT REGEXP_REPLACE('abc','b',NULL) AS replacement,REGEXP_REPLACE(NULL,'b','x') AS source,REGEXP_REPLACE('abc',NULL,'x') AS pattern,REGEXP_REPLACE('abc','b','x',NULL) AS start,REGEXP_REPLACE('abc','b','x',1,NULL) AS occurrence,REGEXP_REPLACE('abc','b','x',1,1,NULL) AS flags"],
  ['replace empty matches', "SELECT REGEXP_REPLACE('abc','','-') AS empty_pattern,REGEXP_REPLACE('','','-') AS both_empty,REGEXP_REPLACE('abc','x*','-') AS star,REGEXP_REPLACE('abc','^','-') AS caret,REGEXP_REPLACE('abc','$','-') AS dollar,REGEXP_REPLACE('','x*','-') AS empty_star"],
  ['replace descriptors', "SELECT REGEXP_REPLACE(CAST('abc' AS VARCHAR(10)),'b','x') AS v10,REGEXP_REPLACE(CAST(N'abc' AS NVARCHAR(10)),'b','x') AS n10,REGEXP_REPLACE(CAST('abc' AS VARCHAR(MAX)),'b','x') AS vmax,REGEXP_REPLACE(CAST(N'abc' AS NVARCHAR(MAX)),'b','x') AS nmax,REGEXP_REPLACE(CAST('abc' AS CHAR(5)),' ','_') AS char5,REGEXP_REPLACE(CAST(N'abc' AS NCHAR(5)),' ','_') AS nchar5"],
  ['replace mixed argument types', "SELECT REGEXP_REPLACE('abc','b',N'Ä') AS nvarchar_replacement,REGEXP_REPLACE(N'abc','b','x') AS nvarchar_source,REGEXP_REPLACE('abc',N'b','x') AS nvarchar_pattern,REGEXP_REPLACE('abc','b',CAST('x' AS VARCHAR(MAX))) AS max_replacement,REGEXP_REPLACE('abc','b',CAST(N'x' AS NVARCHAR(MAX))) AS nmax_replacement"],
  ['replace max pattern rejected', "SELECT REGEXP_REPLACE('abc',CAST('b' AS VARCHAR(MAX)),'x') AS v"],
  ['replace max flags rejected', "SELECT REGEXP_REPLACE('abc','b','x',1,1,CAST('i' AS VARCHAR(MAX))) AS v"],
  ['replace varchar growth truncates', "SELECT LEN(r) AS len,DATALENGTH(r) AS bytes,LEFT(r,3) AS head FROM (SELECT REGEXP_REPLACE(REPLICATE('a',8000),'a','bb') AS r) d"],
  ['replace nvarchar growth truncates', "SELECT LEN(r) AS len,DATALENGTH(r) AS bytes FROM (SELECT REGEXP_REPLACE(REPLICATE(N'a',4000),N'a',N'bb') AS r) d"],
  ['replace max growth', "SELECT LEN(r) AS len,DATALENGTH(r) AS bytes FROM (SELECT REGEXP_REPLACE(REPLICATE(CAST(N'a' AS NVARCHAR(MAX)),10000),N'a',N'bb') AS r) d"],
  ['replace int source rejected', "SELECT REGEXP_REPLACE(12345,'3','x') AS v"],
  ['replace int replacement rejected', "SELECT REGEXP_REPLACE('abc','b',1) AS v"],
  ['replace too many arguments', "SELECT REGEXP_REPLACE('abc','b','x',1,1,'i',1) AS v"],
  ['replace one argument', "SELECT REGEXP_REPLACE('abc') AS v"],
  ['replace numeric string start', "SELECT REGEXP_REPLACE('abcb','b','x','3') AS v"],
  ['replace decimal start rejected', "SELECT REGEXP_REPLACE('abcb','b','x',1.5) AS v"],
  ['replace bigint start', "SELECT REGEXP_REPLACE('abcb','b','x',CAST(3 AS BIGINT)) AS a"],
  ['replace bigint start overflow', "SELECT REGEXP_REPLACE('abcb','b','x',CAST(3000000000 AS BIGINT)) AS a"],
  ['replace unnamed column', "SELECT REGEXP_REPLACE('abc','b','x')"],
  ['replace column source', "SELECT id,REGEXP_REPLACE(v,'[0-9]','#') AS v,REGEXP_REPLACE(nm,'[0-9]','#') AS nm,REGEXP_REPLACE(cs,'b','x') AS cs FROM dbo.regexp_src ORDER BY id"],

  // REGEXP_SUBSTR
  ['substr basic', "SELECT REGEXP_SUBSTR('abc123','[0-9]+') AS a,REGEXP_SUBSTR('abcabc','b',1,2) AS b,REGEXP_SUBSTR('abcabc','b',5) AS c,REGEXP_SUBSTR('abcabc','z') AS d,REGEXP_SUBSTR('abcabc','b',1,3) AS e,REGEXP_SUBSTR('abc','b',10) AS f"],
  ['substr groups', "SELECT REGEXP_SUBSTR('abcabc','(a)(b)',1,1,'c',0) AS g0,REGEXP_SUBSTR('abcabc','(a)(b)',1,1,'c',1) AS g1,REGEXP_SUBSTR('abcabc','(a)(b)',1,1,'c',2) AS g2,REGEXP_SUBSTR('abcabc','(a)(b)',1,1,'c',3) AS g3,REGEXP_SUBSTR('ab','(x)?(b)',1,1,'c',1) AS unmatched_group"],
  ['substr flags', "SELECT REGEXP_SUBSTR('ABC','b',1,1,'i') AS i,REGEXP_SUBSTR('ABC','b',1,1,'c') AS c,REGEXP_SUBSTR('a'+CHAR(10)+'b','a.b',1,1,'s') AS s"],
  ['substr occurrence zero', "SELECT REGEXP_SUBSTR('abcabc','b',1,0) AS v"],
  ['substr start zero', "SELECT REGEXP_SUBSTR('abcabc','b',0) AS v"],
  ['substr group negative', "SELECT REGEXP_SUBSTR('abcabc','(a)',1,1,'c',-1) AS v"],
  ['substr nulls', "SELECT REGEXP_SUBSTR(NULL,'b') AS source,REGEXP_SUBSTR('abc',NULL) AS pattern,REGEXP_SUBSTR('abc','b',NULL) AS start,REGEXP_SUBSTR('abc','b',1,NULL) AS occurrence,REGEXP_SUBSTR('abc','b',1,1,NULL) AS flags,REGEXP_SUBSTR('abc','b',1,1,'c',NULL) AS grp"],
  ['substr empty match', "SELECT REGEXP_SUBSTR('abc','x*') AS a,REGEXP_SUBSTR('','x*') AS b,DATALENGTH(REGEXP_SUBSTR('abc','x*')) AS bytes"],
  ['substr descriptors', "SELECT REGEXP_SUBSTR('abc123','b') AS literal6,REGEXP_SUBSTR(CAST('abc' AS VARCHAR(10)),'b') AS v10,REGEXP_SUBSTR(CAST(N'abc' AS NVARCHAR(10)),'b') AS n10,REGEXP_SUBSTR(CAST('abc' AS VARCHAR(MAX)),'b') AS vmax,REGEXP_SUBSTR(CAST(N'abc' AS NVARCHAR(MAX)),'b') AS nmax,REGEXP_SUBSTR(CAST('abc' AS CHAR(5)),'c.*') AS char5,REGEXP_SUBSTR('abc',N'b') AS nvarchar_pattern"],
  ['substr max beyond 8000', "SELECT REGEXP_SUBSTR(REPLICATE(CAST('a' AS VARCHAR(MAX)),10000)+'bc','b.') AS v"],
  ['substr too many arguments', "SELECT REGEXP_SUBSTR('abc','b',1,1,'c',0,1) AS v"],
  ['substr column source', "SELECT id,REGEXP_SUBSTR(v,'[a-z]+') AS v,REGEXP_SUBSTR(n,'[0-9]+') AS n,REGEXP_SUBSTR(nm,'.$') AS nm FROM dbo.regexp_src ORDER BY id"],

  // REGEXP_INSTR
  ['instr basic', "SELECT REGEXP_INSTR('abc123','[0-9]') AS a,REGEXP_INSTR('abcabc','b',1,2) AS b,REGEXP_INSTR('abcabc','b',3) AS c,REGEXP_INSTR('abc','z') AS d,REGEXP_INSTR('abc','b',10) AS e,REGEXP_INSTR('abcabc','b',1,3) AS f"],
  ['instr return option', "SELECT REGEXP_INSTR('abcabc','b',1,1,0) AS o0,REGEXP_INSTR('abcabc','b',1,1,1) AS o1,REGEXP_INSTR('abc','bc',1,1,1) AS o1_multi,REGEXP_INSTR('abc','c',1,1,1) AS o1_end,REGEXP_INSTR('abc','z',1,1,1) AS o1_missing"],
  ['instr groups', "SELECT REGEXP_INSTR('abcabc','(a)(b)',1,1,0,'c',2) AS g2,REGEXP_INSTR('abcabc','(a)(b)',1,1,1,'c',2) AS g2_end,REGEXP_INSTR('abcabc','(a)(b)',1,1,0,'c',0) AS g0,REGEXP_INSTR('abcabc','(a)(b)',1,1,0,'c',3) AS g3,REGEXP_INSTR('ab','(x)?(b)',1,1,0,'c',1) AS unmatched_group"],
  ['instr flags', "SELECT REGEXP_INSTR('ABC','b',1,1,0,'i') AS i,REGEXP_INSTR('ABC','b',1,1,0,'c') AS c"],
  ['instr return option two', "SELECT REGEXP_INSTR('abcabc','b',1,1,2) AS v"],
  ['instr return option negative', "SELECT REGEXP_INSTR('abcabc','b',1,1,-1) AS v"],
  ['instr occurrence zero', "SELECT REGEXP_INSTR('abcabc','b',1,0) AS v"],
  ['instr start zero', "SELECT REGEXP_INSTR('abcabc','b',0) AS v"],
  ['instr group negative', "SELECT REGEXP_INSTR('abcabc','(a)',1,1,0,'c',-1) AS v"],
  ['instr nulls', "SELECT REGEXP_INSTR(NULL,'b') AS source,REGEXP_INSTR('abc',NULL) AS pattern,REGEXP_INSTR('abc','b',NULL) AS start,REGEXP_INSTR('abc','b',1,NULL) AS occurrence,REGEXP_INSTR('abc','b',1,1,NULL) AS return_option,REGEXP_INSTR('abc','b',1,1,0,NULL) AS flags,REGEXP_INSTR('abc','b',1,1,0,'c',NULL) AS grp"],
  ['instr empty match', "SELECT REGEXP_INSTR('abc','x*') AS a,REGEXP_INSTR('abc','x*',1,1,1) AS b,REGEXP_INSTR('','x*') AS c,REGEXP_INSTR('abc','$') AS d"],
  ['instr descriptors', "SELECT REGEXP_INSTR(CAST('abc' AS VARCHAR(MAX)),'b') AS vmax,REGEXP_INSTR(CAST(N'abc' AS NVARCHAR(MAX)),'b') AS nmax,REGEXP_INSTR(REPLICATE(CAST('a' AS VARCHAR(MAX)),10000)+'b','b') AS beyond"],
  ['instr too many arguments', "SELECT REGEXP_INSTR('abc','b',1,1,0,'c',1,1) AS v"],

  // REGEXP_COUNT
  ['count basic', "SELECT REGEXP_COUNT('abcabc','b') AS a,REGEXP_COUNT('abcabc','b',3) AS b,REGEXP_COUNT('abc','b',10) AS c,REGEXP_COUNT('abc','z') AS d"],
  ['count flags', "SELECT REGEXP_COUNT('ABCabc','b',1,'i') AS i,REGEXP_COUNT('ABCabc','b',1,'c') AS c,REGEXP_COUNT('a'+CHAR(10)+'b','^.',1,'m') AS m"],
  ['count empty matches', "SELECT REGEXP_COUNT('abc','') AS empty_pattern,REGEXP_COUNT('abc','x*') AS star,REGEXP_COUNT('','') AS both_empty,REGEXP_COUNT('abc','',2) AS empty_from_2"],
  ['count start zero', "SELECT REGEXP_COUNT('abc','b',0) AS v"],
  ['count nulls', "SELECT REGEXP_COUNT(NULL,'a') AS source,REGEXP_COUNT('a',NULL) AS pattern,REGEXP_COUNT('a','a',NULL) AS start,REGEXP_COUNT('a','a',1,NULL) AS flags"],
  ['count flag types', "SELECT REGEXP_COUNT('abc','b',1,CAST('i' AS VARCHAR(8000))) AS v8000,REGEXP_COUNT('abc','b',1,REPLICATE('i',30)) AS thirty"],
  ['count thirty-one flags', "SELECT REGEXP_COUNT('abc','b',1,REPLICATE('i',31)) AS v"],
  ['count char flags padded', "SELECT REGEXP_COUNT('abc','b',1,CAST('i' AS CHAR(3))) AS v"],
  ['count nvarchar flags rejected', "SELECT REGEXP_COUNT('abc','b',1,N'i') AS v"],
  ['count start string flag rejected', "SELECT REGEXP_COUNT('abc','b','i') AS v"],
  ['count numeric string start', "SELECT REGEXP_COUNT('abc','b','1') AS v"],
  ['count decimal start rejected', "SELECT REGEXP_COUNT('abc','b',1.9) AS v"],
  ['count bigint start', "SELECT REGEXP_COUNT('abcb','b',CAST(3 AS BIGINT)) AS v"],
  ['count bigint start overflow', "SELECT REGEXP_COUNT('abcb','b',CAST(3000000000 AS BIGINT)) AS v"],
  ['count max pattern rejected', "SELECT REGEXP_COUNT('abc',CAST('b' AS VARCHAR(MAX))) AS v"],
  ['count max source', "SELECT REGEXP_COUNT(REPLICATE(CAST('a' AS VARCHAR(MAX)),10000),'a') AS v"],
  ['count text rejected', "SELECT REGEXP_COUNT(CAST('abc' AS TEXT),'b') AS v"],
  ['count ntext rejected', "SELECT REGEXP_COUNT(CAST(N'abc' AS NTEXT),N'b') AS v"],
  ['count varbinary rejected', "SELECT REGEXP_COUNT(0x616263,'b') AS v"],
  ['count xml rejected', "SELECT REGEXP_COUNT(CAST('<a/>' AS XML),'a') AS v"],
  ['count uniqueidentifier rejected', "SELECT REGEXP_COUNT(NEWID(),'-') AS v"],
  ['count sql_variant rejected', "SELECT REGEXP_COUNT(CAST('a' AS SQL_VARIANT),'a') AS v"],
  ['count one argument', "SELECT REGEXP_COUNT('abc') AS v"],
  ['count five arguments', "SELECT REGEXP_COUNT('abc','b',1,'i',1) AS v"],

  // Collations and characters
  ['collation ignored by default', "SELECT REGEXP_COUNT('ABC' COLLATE Latin1_General_CS_AS,'a') AS cs,REGEXP_COUNT('ABC' COLLATE Latin1_General_CI_AS,'a') AS ci,REGEXP_COUNT('ABC' COLLATE Latin1_General_BIN2,'a') AS bin,REGEXP_COUNT('ABC','a' COLLATE Latin1_General_CI_AS) AS pattern_ci"],
  ['collation with flags', "SELECT REGEXP_COUNT('ABC' COLLATE Latin1_General_CI_AS,'a',1,'c') AS ci_c,REGEXP_COUNT('ABC' COLLATE Latin1_General_CS_AS,'a',1,'i') AS cs_i"],
  ['collation conflict', "SELECT REGEXP_COUNT('ABC' COLLATE Latin1_General_CS_AS,'a' COLLATE Latin1_General_CI_AS) AS v"],
  ['collation result', "SELECT REGEXP_REPLACE('abc' COLLATE Latin1_General_CS_AS,'b','x') AS cs,REGEXP_SUBSTR('abc' COLLATE Latin1_General_BIN2,'b') AS bin,REGEXP_REPLACE(N'abc' COLLATE Japanese_CI_AS,N'b',N'x') AS jp"],
  ['accents', "SELECT REGEXP_COUNT(N'é',N'e') AS e_in_eacute,REGEXP_COUNT(N'É',N'é') AS case_default,REGEXP_COUNT(N'É',N'é',1,'i') AS case_i,REGEXP_COUNT(N'ÉA',N'[a-z]',1,'i') AS ascii_range_i,REGEXP_COUNT(N'ÉA',N'\\w') AS word,REGEXP_COUNT(N'e'+NCHAR(0x301),N'.') AS combining"],
  ['code page characters', "SELECT REGEXP_REPLACE('a'+CHAR(233)+'b',CHAR(233),'x') AS replace_e,REGEXP_COUNT('a'+CHAR(233)+'b','.') AS dots,REGEXP_COUNT(CHAR(200),'\\p{L}') AS letter,REGEXP_COUNT(CHAR(128),NCHAR(0x20AC)) AS euro,REGEXP_INSTR('a'+CHAR(233)+'b','b') AS position"],
  ['utf8 collation', "SELECT REGEXP_INSTR(CAST(N'éb' AS VARCHAR(10)) COLLATE Latin1_General_100_CI_AS_SC_UTF8,'b') AS instr_utf8,REGEXP_COUNT(CAST(N'éb' AS VARCHAR(10)) COLLATE Latin1_General_100_CI_AS_SC_UTF8,'.') AS count_utf8,REGEXP_SUBSTR(CAST(N'éb' AS VARCHAR(10)) COLLATE Latin1_General_100_CI_AS_SC_UTF8,'^.') AS substr_utf8,DATALENGTH(CAST(N'éb' AS VARCHAR(10)) COLLATE Latin1_General_100_CI_AS_SC_UTF8) AS bytes"],
  ['surrogate default collation', `SELECT REGEXP_INSTR(${emoji}+N'b',N'b') AS instr_b,REGEXP_COUNT(${emoji}+N'b',N'.') AS dots,REGEXP_SUBSTR(${emoji}+N'b',N'^.') AS first_dot,LEN(REGEXP_SUBSTR(${emoji}+N'b',N'^.')) AS first_len,REGEXP_INSTR(${emoji}+N'b',N'.',1,1,1) AS first_end`],
  ['surrogate sc collation', `SELECT REGEXP_INSTR(${emoji}+N'b' COLLATE Latin1_General_100_CI_AS_SC,N'b') AS instr_b,REGEXP_COUNT(${emoji}+N'b' COLLATE Latin1_General_100_CI_AS_SC,N'.') AS dots`],
  ['surrogate start and replace', `SELECT REGEXP_INSTR(N'a'+${emoji}+N'b',N'b',3) AS start3,REGEXP_INSTR(N'a'+${emoji}+N'b',N'b',4) AS start4,REGEXP_REPLACE(N'a'+${emoji}+N'b',N'.',N'x') AS dots,REGEXP_SUBSTR(N'a'+${emoji}+N'b',N'.',3) AS substr_start3`],
  ['surrogate pattern forms', `SELECT REGEXP_COUNT(N'a'+${emoji},N'\\x{1F600}') AS hex_escape,REGEXP_COUNT(N'a'+${emoji},N'['+${emoji}+N']') AS class,REGEXP_COUNT(N'a'+${emoji},${emoji}) AS literal,REGEXP_COUNT(N'a'+${emoji},N'\\p{So}') AS symbol`],
  ['lone surrogate', "SELECT REGEXP_COUNT(NCHAR(0xD83D)+N'b',N'.') AS dots,REGEXP_INSTR(NCHAR(0xD83D)+N'b',N'b') AS instr_b,REGEXP_REPLACE(NCHAR(0xD83D)+N'b',N'b',N'x') AS replaced"],
  ['surrogate in varchar pattern', `SELECT REGEXP_COUNT(N'a'+${emoji},CAST(${emoji} AS VARCHAR(10))) AS v`],

  // REGEXP_MATCHES
  ['matches basic', "SELECT * FROM REGEXP_MATCHES('a1b22c333','[0-9]+')"],
  ['matches groups', "SELECT * FROM REGEXP_MATCHES('a1b22','([a-z])([0-9]+)')"],
  ['matches unmatched and named groups', "SELECT * FROM REGEXP_MATCHES('ab','(x)?(?P<second>b)')"],
  ['matches flags', "SELECT * FROM REGEXP_MATCHES('A1b2','[a-z]','i')"],
  ['matches multiline flag', "SELECT * FROM REGEXP_MATCHES('a'+CHAR(10)+'b','^.$','m')"],
  ['matches null source', "SELECT * FROM REGEXP_MATCHES(NULL,'a')"],
  ['matches null pattern', "SELECT * FROM REGEXP_MATCHES('abc',NULL)"],
  ['matches null flags', "SELECT * FROM REGEXP_MATCHES('abc','a',NULL)"],
  ['matches no match', "SELECT * FROM REGEXP_MATCHES('abc','z')"],
  ['matches empty pattern', "SELECT * FROM REGEXP_MATCHES('abc','')"],
  ['matches empty source', "SELECT * FROM REGEXP_MATCHES('','x*')"],
  ['matches json escaping', "SELECT * FROM REGEXP_MATCHES('a\"b\\c'+CHAR(9)+'d','[\"\\\\\\t]')"],
  ['matches descriptors', "SELECT (SELECT TOP 1 match_value FROM REGEXP_MATCHES(CAST('a1' AS VARCHAR(MAX)),'[0-9]')) AS vmax,(SELECT TOP 1 match_value FROM REGEXP_MATCHES(N'a1',N'[0-9]')) AS nvarchar_literal"],
  ['matches varchar max source', "SELECT * FROM REGEXP_MATCHES(CAST('a1' AS VARCHAR(MAX)),'[0-9]')"],
  ['matches nvarchar source', "SELECT * FROM REGEXP_MATCHES(N'a1',N'[0-9]')"],
  ['matches surrogates', `SELECT * FROM REGEXP_MATCHES(N'a'+${emoji}+N'b',N'.')`],
  ['matches invalid pattern', "SELECT * FROM REGEXP_MATCHES('abc','(')"],
  ['matches invalid flag', "SELECT * FROM REGEXP_MATCHES('abc','a','x')"],
  ['matches max pattern rejected', "SELECT * FROM REGEXP_MATCHES('abc',CAST('b' AS VARCHAR(MAX)))"],
  ['matches nvarchar flags rejected', "SELECT * FROM REGEXP_MATCHES('abc','b',N'i')"],
  ['matches int source rejected', "SELECT * FROM REGEXP_MATCHES(1,'1')"],
  ['matches one argument', "SELECT * FROM REGEXP_MATCHES('abc')"],
  ['matches four arguments', "SELECT * FROM REGEXP_MATCHES('abc','b','i',1)"],
  ['matches scalar use rejected', "SELECT REGEXP_MATCHES('a','a') AS v"],
  ['matches cross apply', "SELECT t.id,m.match_id,m.match_value FROM (VALUES(1,'a1b2'),(2,NULL),(3,'x')) t(id,s) CROSS APPLY REGEXP_MATCHES(t.s,'[0-9]') m ORDER BY t.id,m.match_id"],
  ['matches outer apply column source', "SELECT s.id,m.match_id,m.start_position,m.match_value FROM dbo.regexp_src s OUTER APPLY REGEXP_MATCHES(s.nm,'[0-9]') m ORDER BY s.id,m.match_id"],
  // A NULL argument seen by a table-valued REGEXP instance suppresses later rows.
  ['matches apply null source first', "SELECT t.id,m.match_value FROM (VALUES(1,CAST(NULL AS VARCHAR(10))),(2,'a1'),(3,'b2')) t(id,s) CROSS APPLY REGEXP_MATCHES(t.s,'[0-9]') m ORDER BY t.id"],
  ['matches apply null source last', "SELECT t.id,m.match_value FROM (VALUES(1,'a1'),(2,'b2'),(3,CAST(NULL AS VARCHAR(10)))) t(id,s) CROSS APPLY REGEXP_MATCHES(t.s,'[0-9]') m ORDER BY t.id"],
  ['matches apply null pattern first', "SELECT t.id,m.match_value FROM (VALUES(1,'a1',CAST(NULL AS VARCHAR(10))),(2,'a1','[0-9]')) t(id,s,p) CROSS APPLY REGEXP_MATCHES(t.s,t.p) m ORDER BY t.id"],
  ['matches apply no match first', "SELECT t.id,m.match_value FROM (VALUES(1,'zz'),(2,'a1')) t(id,s) CROSS APPLY REGEXP_MATCHES(t.s,'[0-9]') m ORDER BY t.id"],
  ['matches variable null then value', "DECLARE @s NVARCHAR(20)=NULL;SELECT * FROM REGEXP_MATCHES(@s,N'[0-9]');SET @s=N'a1';SELECT * FROM REGEXP_MATCHES(@s,N'[0-9]')"],
  ['scalar apply null source first', "SELECT t.id,REGEXP_COUNT(t.s,'[0-9]') AS c,REGEXP_SUBSTR(t.s,'[0-9]') AS s FROM (VALUES(1,CAST(NULL AS VARCHAR(10))),(2,'a1')) t(id,s) ORDER BY t.id"],
  ['matches column list', "SELECT match_value,start_position FROM REGEXP_MATCHES('xaxbx','x') ORDER BY match_id DESC"],
  ['matches describe first result set', "SELECT column_ordinal,name,is_nullable,system_type_name,max_length,collation_name FROM sys.dm_exec_describe_first_result_set(N'SELECT * FROM REGEXP_MATCHES(''abc'',''b'')',NULL,0) ORDER BY column_ordinal"],

  // REGEXP_SPLIT_TO_TABLE
  ['split basic', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('a,b,,c',',')"],
  ['split regex delimiter', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('a1b22c','[0-9]+')"],
  ['split flags', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('aXbxc','x','i')"],
  ['split leading and trailing', "SELECT * FROM REGEXP_SPLIT_TO_TABLE(',a,',',')"],
  ['split empty source', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('',',')"],
  ['split empty pattern', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('abc','')"],
  ['split empty-matching pattern', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('abc','x*')"],
  ['split no match', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('abc',',')"],
  ['split null source', "SELECT * FROM REGEXP_SPLIT_TO_TABLE(NULL,',')"],
  ['split null pattern', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('abc',NULL)"],
  ['split null flags', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('a,b',',',NULL)"],
  ['split nvarchar source', "SELECT * FROM REGEXP_SPLIT_TO_TABLE(N'a,b',',')"],
  ['split nvarchar max source', "SELECT * FROM REGEXP_SPLIT_TO_TABLE(CAST(N'a,b' AS NVARCHAR(MAX)),N',')"],
  ['split char source', "SELECT * FROM REGEXP_SPLIT_TO_TABLE(CAST('a,b' AS CHAR(5)),',')"],
  ['split surrogates', `SELECT * FROM REGEXP_SPLIT_TO_TABLE(N'a'+${emoji}+N'b',${emoji})`],
  ['split invalid pattern', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('abc','[')"],
  ['split invalid flag', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('abc','b','z')"],
  ['split nvarchar flags rejected', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('abc','b',N'i')"],
  ['split max pattern rejected', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('abc',CAST(',' AS VARCHAR(MAX)))"],
  ['split one argument', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('abc')"],
  ['split outer apply', "SELECT t.id,m.value,m.ordinal FROM (VALUES(1,'a,b'),(2,'c'),(3,NULL)) t(id,s) OUTER APPLY REGEXP_SPLIT_TO_TABLE(t.s,',') m ORDER BY t.id,m.ordinal"],
  ['split apply null source first', "SELECT t.id,m.value,m.ordinal FROM (VALUES(1,CAST(NULL AS VARCHAR(10))),(2,'a,b')) t(id,s) CROSS APPLY REGEXP_SPLIT_TO_TABLE(t.s,',') m ORDER BY t.id,m.ordinal"],
  ['split describe first result set', "SELECT column_ordinal,name,is_nullable,system_type_name,max_length,collation_name FROM sys.dm_exec_describe_first_result_set(N'SELECT * FROM REGEXP_SPLIT_TO_TABLE(N''a,b'','','')',NULL,0) ORDER BY column_ordinal"],
  ['scalar describe first result set', "SELECT column_ordinal,name,is_nullable,system_type_name,max_length,collation_name FROM sys.dm_exec_describe_first_result_set(N'SELECT REGEXP_REPLACE(''abc'',''b'',''x'') AS r,REGEXP_SUBSTR(N''abc'',''b'') AS s,REGEXP_INSTR(''abc'',''b'') AS i,REGEXP_COUNT(''abc'',''b'') AS c',NULL,0) ORDER BY column_ordinal"],
]

// Compatibility level transitions run last in each fresh database.
const levelCases = [
  ['set compatibility 160', compatibility(160)],
  ['compatibility 160 level', 'SELECT compatibility_level FROM sys.databases WHERE name=DB_NAME()'],
  ['compatibility 160 like', `SELECT ${like("'abc'", "'^a'")} AS v`],
  ['compatibility 160 scalars', "SELECT REGEXP_REPLACE('abcabc','b','X') AS r,REGEXP_SUBSTR('abc123','[0-9]+') AS s,REGEXP_INSTR('abc123','[0-9]') AS i,REGEXP_COUNT('abcabc','b') AS c"],
  ['compatibility 160 matches', "SELECT * FROM REGEXP_MATCHES('a1','[0-9]')"],
  ['compatibility 160 split', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('a,b',',')"],
  ['compatibility 160 existing check constraint', "INSERT dbo.regexp_check VALUES (4,'CD2'),(5,'cd2')"],
  ['set compatibility 100', compatibility(100)],
  ['compatibility 100 scalars', "SELECT REGEXP_REPLACE('abcabc','b','X') AS r,REGEXP_COUNT('abcabc','b') AS c"],
  ['compatibility 100 like', `SELECT ${like("'abc'", "'^a'")} AS v`],
  ['set compatibility 170', compatibility(170)],
  ['compatibility 170 like', `SELECT ${like("'abc'", "'^a'")} AS v`],
  ['compatibility 170 split', "SELECT * FROM REGEXP_SPLIT_TO_TABLE('a,b',',')"],
]

// Fresh database names and IDs vary per run; everything else is retained raw.
function scrub(result) {
  for (const message of [...result.errors, ...result.info]) {
    if (typeof message.message !== 'string') continue
    message.message = message.message.replaceAll(/msduck_audit_[0-9a-f]{32}/g, '<fresh-database>').replaceAll(/database ID \d+/g, 'database ID <fresh-database-id>')
  }
  return result
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

const long = 'a'.repeat(9000)
const V = (name, value, length = 20) => [name, TYPES.VarChar, value, { length }]
const N = (name, value, length = 20) => [name, TYPES.NVarChar, value, { length }]
const I = (name, value) => [name, TYPES.Int, value]
const rpcCases = [
  ['rpc like nvarchar', `SELECT ${like('@s', '@p', '@f')} AS v`, [N('s', 'ABC'), N('p', '^a'), V('f', 'i', 10)]],
  ['rpc like null flags', `SELECT ${like('@s', '@p', '@f')} AS v`, [N('s', 'ABC'), N('p', '^a'), V('f', null, 10)]],
  ['rpc like nvarchar flags rejected', `SELECT ${like('@s', '@p', '@f')} AS v`, [N('s', 'ABC'), N('p', '^a'), N('f', 'i', 10)]],
  ['rpc like nvarchar max pattern rejected', `SELECT ${like('@s', '@p')} AS v`, [N('s', 'ABC'), N('p', long, 9000)]],
  ['rpc replace varchar', 'SELECT REGEXP_REPLACE(@s,@p,@r,@start,@occurrence,@f) AS v', [V('s', 'abcabc'), V('p', 'b'), V('r', 'X'), I('start', 1), I('occurrence', 2), V('f', 'c', 10)]],
  ['rpc replace nvarchar', 'SELECT REGEXP_REPLACE(@s,@p,@r) AS v', [N('s', 'abcabc'), N('p', '(b)'), N('r', '[\\1]')]],
  ['rpc replace nvarchar max source', 'SELECT REGEXP_REPLACE(@s,@p,@r) AS v', [N('s', 'abc', 9000), N('p', 'b'), N('r', 'x')]],
  ['rpc replace null start', 'SELECT REGEXP_REPLACE(@s,@p,@r,@start) AS v', [V('s', 'abc'), V('p', 'b'), V('r', 'x'), I('start', null)]],
  ['rpc replace start zero', 'SELECT REGEXP_REPLACE(@s,@p,@r,@start) AS v', [V('s', 'abc'), V('p', 'b'), V('r', 'x'), I('start', 0)]],
  ['rpc replace invalid pattern', 'SELECT REGEXP_REPLACE(@s,@p,@r) AS v', [V('s', 'abc'), V('p', '('), V('r', 'x')]],
  ['rpc substr surrogate', 'SELECT REGEXP_SUBSTR(@s,@p) AS v,REGEXP_INSTR(@s,N\'b\') AS i', [N('s', '\u{1F600}b'), N('p', '^.')]],
  ['rpc substr group', 'SELECT REGEXP_SUBSTR(@s,@p,1,1,\'c\',@g) AS v', [V('s', 'abab'), V('p', '(a)(b)'), I('g', 2)]],
  ['rpc instr return option', 'SELECT REGEXP_INSTR(@s,@p,1,1,@o) AS v', [V('s', 'abc'), V('p', 'b'), I('o', 1)]],
  ['rpc count bigint start', 'SELECT REGEXP_COUNT(@s,@p,@start) AS v', [V('s', 'abcb'), V('p', 'b'), ['start', TYPES.BigInt, 3]]],
  ['rpc count varchar max source', 'SELECT REGEXP_COUNT(@s,@p) AS v', [V('s', long, 9000), V('p', 'a')]],
  ['rpc count null source', 'SELECT REGEXP_COUNT(@s,@p) AS v', [N('s', null), N('p', 'a')]],
  ['rpc matches', 'SELECT * FROM REGEXP_MATCHES(@s,@p,@f)', [N('s', 'A1b2'), N('p', '[a-z]'), V('f', 'i', 10)]],
  ['rpc matches null source', 'SELECT * FROM REGEXP_MATCHES(@s,@p)', [N('s', null), N('p', 'a')]],
  ['rpc split', 'SELECT * FROM REGEXP_SPLIT_TO_TABLE(@s,@p)', [V('s', 'a,b,,c'), V('p', ',')]],
  // Cached-plan sequences: each statement text is unique to its sequence.
  ...[['a1b22'], [null], ['a1b22'], ['a1b22', '('], ['a1b22']].map(([s, p = '[0-9]+'], i) =>
    [`rpc matches cached plan null execution ${i + 1}`, 'SELECT * FROM REGEXP_MATCHES(@s,@p) /*cached null execution*/', [N('s', s), N('p', p)]]),
  ...[[null], ['a,b'], ['a,b']].map(([s], i) =>
    [`rpc split cached plan null first ${i + 1}`, 'SELECT * FROM REGEXP_SPLIT_TO_TABLE(@s,@p) /*cached null first*/', [N('s', s), N('p', ',')]]),
  ...[[null], ['c']].map(([f], i) =>
    [`rpc matches cached plan null flags ${i + 1}`, 'SELECT * FROM REGEXP_MATCHES(@s,@p,@f) /*cached null flags*/', [N('s', 'a1'), N('p', '[0-9]'), V('f', f, 10)]]),
  ...[[null], ['a1b22']].map(([s], i) =>
    [`rpc matches recompile null first ${i + 1}`, 'SELECT * FROM REGEXP_MATCHES(@s,@p) OPTION(RECOMPILE) /*recompile null first*/', [N('s', s), N('p', '[0-9]+')]]),
  ...[[null], ['a1']].map(([s], i) =>
    [`rpc count cached plan null first ${i + 1}`, 'SELECT REGEXP_COUNT(@s,@p) AS c /*cached null first*/', [N('s', s), N('p', '[0-9]')]]),
  ['rpc split nvarchar max source', 'SELECT * FROM REGEXP_SPLIT_TO_TABLE(@s,@p)', [N('s', 'a,b', 9000), N('p', ',')]],
]

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
  // tedious keeps the first execution error on request.error and passes it to
  // every later callback of the same Request; only report a new error object.
  let reported
  const phase = start => new Promise(done => {
    result = fresh()
    complete = (error, rowCount) => {
      result.rowCount = rowCount
      const stale = error !== undefined && error === reported
      if (error) reported = error
      if (error && !stale && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      const finished = result
      result = undefined
      done(canonical(scrub(finished)))
    }
    start()
  })
  // tedious reports sp_prepare completion through 'prepared'/'error' events
  // instead of the request callback; waiting on the callback hangs.
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
    for (const values of valueSets) executions.push({ values: canonical(values), result: await phase(() => connection.execute(request, values)) })
    const unprepare = await phase(() => connection.unprepare(request))
    return { prepare, executions, unprepare }
  } finally {
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

const preparedCases = [
  ['prepared scalars',
    'SELECT REGEXP_REPLACE(@s,@p,N\'#\',@start,0,@f) AS r,REGEXP_SUBSTR(@s,@p,@start,1,@f) AS s,REGEXP_INSTR(@s,@p,@start,1,0,@f) AS i,REGEXP_COUNT(@s,@p,@start,@f) AS c',
    [['s', TYPES.NVarChar, { length: 20 }], ['p', TYPES.NVarChar, { length: 20 }], ['start', TYPES.Int], ['f', TYPES.VarChar, { length: 10 }]],
    [
      { s: 'abc123', p: '[0-9]', start: 1, f: 'c' },
      { s: 'ABCabc', p: 'b', start: 1, f: 'i' },
      { s: 'abc', p: 'b', start: null, f: 'c' },
      { s: null, p: 'b', start: 1, f: 'c' },
      { s: 'abc', p: '(', start: 1, f: 'c' },
      { s: 'abc', p: 'b', start: 1, f: 'x' },
      { s: 'abc', p: 'b', start: 0, f: 'c' },
      { s: '\u{1F600}b', p: 'b', start: 1, f: 'c' },
      { s: 'abc123', p: '[0-9]', start: 1, f: 'c' },
    ]],
  ['prepared like',
    `SELECT ${like('@s', '@p', '@f')} AS v`,
    [['s', TYPES.VarChar, { length: 20 }], ['p', TYPES.VarChar, { length: 20 }], ['f', TYPES.VarChar, { length: 10 }]],
    [
      { s: 'ABC', p: '^a', f: 'i' },
      { s: 'ABC', p: '^a', f: 'c' },
      { s: null, p: '^a', f: 'c' },
      { s: 'ABC', p: '[', f: 'c' },
      { s: 'ABC', p: '^A', f: 'c' },
    ]],
  ['prepared matches',
    'SELECT * FROM REGEXP_MATCHES(@s,@p) /*prepared matches*/',
    [['s', TYPES.NVarChar, { length: 20 }], ['p', TYPES.NVarChar, { length: 20 }]],
    [
      { s: 'a1b22', p: '[0-9]+' },
      { s: null, p: '[0-9]+' },
      { s: 'a1b22', p: '[0-9]+' },
      { s: 'abc', p: '(' },
      { s: 'a1b22', p: '[0-9]+' },
    ]],
  ['prepared split',
    'SELECT * FROM REGEXP_SPLIT_TO_TABLE(@s,@p) /*prepared split*/',
    [['s', TYPES.VarChar, { length: 20 }], ['p', TYPES.VarChar, { length: 20 }]],
    [
      { s: 'a,b,,c', p: ',' },
      { s: 'abc', p: '' },
      { s: null, p: ',' },
      { s: 'a,b', p: ',' },
    ]],
  ['prepared nvarchar flags rejected',
    'SELECT REGEXP_COUNT(@s,@p,1,@f) AS c',
    [['s', TYPES.VarChar, { length: 20 }], ['p', TYPES.VarChar, { length: 20 }], ['f', TYPES.NVarChar, { length: 10 }]],
    [{ s: 'abc', p: 'b', f: 'i' }]],
]

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    const result = canonical(scrub(await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql))))
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
  for (const [name, sql, parameters] of rpcCases) await record(name, sql, parameters)
  for (const [name, sql, declarations, valueSets] of preparedCases) {
    records.push({
      name, sql, protocol: 'sp_prepare/sp_execute/sp_unprepare',
      declarations: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, ...(options ? { options } : {}) })),
      prepared: await prepared(connection, sql, declarations, valueSets),
    })
  }
  for (const [name, sql] of levelCases) await record(name, sql)
  await record('connection reuse', 'SELECT @@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state,1 AS reusable')
  return records
}

const programCount = setup.length + cases.length + rpcCases.length + preparedCases.length + levelCases.length + 1

// Bounded spot checks only: never hand whole records or captures to node:assert.
function validate(run) {
  assert.equal(run.length, programCount)
  const get = name => {
    const found = run.find(record => record.name === name)
    if (!found) throw new Error(`missing record ${name}`)
    return found.result
  }
  const rows = name => JSON.stringify(get(name).sets[0]?.rows ?? null)
  const column = (name, index = 0) => {
    const c = get(name).sets[0]?.columns[index]
    return c ? `${c.type}(${c.length})` : 'none'
  }
  assert.equal(rows('environment'), JSON.stringify([[17, 'SQL_Latin1_General_CP1_CI_AS', 'SQL_Latin1_General_CP1_CI_AS', 170]]))
  assert.equal(rows('like case default and flags'), '[[0,1,0,0,1,1]]')
  assert.equal(rows('like nulls'), '[[-1,-1,-1,-1]]')
  assert.equal(rows('replace start and occurrence'), JSON.stringify([['abcaXc', 'abcaXc', 'aXcaXc', 'aXcabc', 'abcabc', 'abcabc', 'abcabc']]))
  assert.equal(rows('substr basic').slice(0, 17), '[["123","b","b",n')
  assert.equal(rows('count empty matches').slice(0, 4), '[[4,')
  assert.equal(column('replace basic'), 'VarChar(8000)')
  assert.equal(column('substr descriptors'), 'VarChar(6)')
  assert.equal(column('instr basic'), 'IntN(4)')
  assert.equal(rows('split basic'), JSON.stringify([['a', '1'], ['b', '2'], ['', '3'], ['c', '4']]))
  for (const [name, number] of [
    ['check constraint rejects', 547],
    ['like invalid flag x', 19303],
    ['like select list rejected', 156],
    ['like max pattern rejected', 8116],
    ['invalid unclosed group', 19308],
    ['invalid lookahead', 19300],
    ['replace start zero', 19301],
    ['replace occurrence negative', 19301],
    ['substr occurrence zero', 19301],
    ['instr return option two', 19301],
    ['count thirty-one flags', 19302],
    ['count one argument', 189],
    ['collation conflict', 468],
    ['matches invalid pattern', 19308],
    ['matches one argument', 313],
    ['compatibility 160 like', 195],
    ['compatibility 160 matches', 208],
  ]) assert.equal(get(name).errors[0]?.number, number, name)
  assert.equal(get('compatibility 160 scalars').errors.length, 0)
  assert.equal(rows('matches apply null source last'), JSON.stringify([[1, '1'], [2, '2']]))
  assert.equal(get('matches apply null source first').sets[0].rows.length, 0)
  assert.equal(get('rpc matches cached plan null execution 3').sets[0].rows.length, 0)
  assert.equal(get('rpc matches cached plan null execution 5').sets[0].rows.length, 2)
  assert.equal(rows('compatibility 170 like'), '[[1]]')
  for (const record of run) {
    const results = record.prepared ? [record.prepared.prepare, ...record.prepared.executions.map(x => x.result), record.prepared.unprepare].filter(Boolean) : [record.result]
    for (const result of results) assert(result.done.length > 0, `${record.name}: no completion`)
  }
}

// Refuse silent overwrite before starting any container; existence check only.
// Resolves symlinks in the existing part of a path, so an alias of the fixture
// (or of its directory) compares equal even before the file exists. Mirrors
// refuseFixtureOutput from the shared helper proposed in PR #312.
async function canonicalPath(path) {
  const absolute = resolve(path instanceof URL ? fileURLToPath(path) : path)
  try { return await realpath(absolute) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return resolve(await canonicalPath(dirname(absolute)), basename(absolute))
  }
}

// Scratch output must never alias the retained fixture: the unconditional
// artifact write would replace the ground truth and then compare the capture
// with itself. Checked before any directory is created or container started.
// A hard link has a different canonical path but the same file identity, so
// existing files are also compared by device and inode.
async function identity(path) {
  try { const info = await stat(path, { bigint: true }); return `${info.dev}:${info.ino}` } catch (error) {
    if (error.code === 'ENOENT') return null
    throw error
  }
}
const outputIdentity = await identity(output)
if (await canonicalPath(output) === await canonicalPath(fixture) ||
  (outputIdentity !== null && outputIdentity === await identity(fixture))) throw new Error('refusing to write capture output over retained fixture ' + fileURLToPath(fixture))
if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
const counts = oneDatabase ? 1 : 2
const containers = []
for (let containerIndex = 0; containerIndex < counts; containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    console.error(`container ${containerIndex + 1}/${counts}: ${container.name}`)
    const runs = []
    for (let databaseIndex = 0; databaseIndex < counts; databaseIndex++) {
      runs.push(await isolatedReference({
        ...config, options: { ...config.options, requestTimeout: 120000 },
      }, observe))
    }
    containers.push({ image: container.image, runs })
  })
}
const actual = { containers }
// Write the raw artifact first so a failed check can be inspected.
await writeFile(output, JSON.stringify(actual) + '\n')
for (const { runs } of containers) for (const run of runs) validate(run)
if (!oneDatabase) {
  assertSameCapture(containers[0].runs[0], containers[0].runs[1], 'REGEXP observations differ across fresh databases (container 0)')
  assertSameCapture(containers[1].runs[0], containers[1].runs[1], 'REGEXP observations differ across fresh databases (container 1)')
  assertSameCapture(containers[0].runs[0], containers[1].runs[0], 'REGEXP observations differ across containers (database 0)')
  assertSameCapture(containers[0].runs[1], containers[1].runs[1], 'REGEXP observations differ across containers (database 1)')
}
let retained
if (!writeFixture && !oneDatabase) {
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'REGEXP observations differ from retained fixture')
}
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${programCount} REGEXP observations` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained ? ' and matched retained fixture' : '') + (writeFixture ? '; wrote new retained fixture' : ''))
