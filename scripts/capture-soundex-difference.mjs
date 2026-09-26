#!/usr/bin/env node
// Retain SQL Server SOUNDEX and DIFFERENCE rows, descriptors, diagnostics and
// completions for ordinary batches, sp_executesql RPC and a prepared handle,
// including per-character tables and database compatibility-level variants.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/soundex-difference.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/compatibility/soundex-difference/capture.json')

const quote = value => "'" + value.replaceAll("'", "''") + "'"
const nquote = value => 'N' + quote(value)

// Words exercising the classic rules: code mapping, adjacent duplicates, vowel
// and H/W separators, first-letter coding, case and truncation/padding.
const words = [
  'Robert', 'Rupert', 'Rubin', 'Ashcraft', 'Ashcroft', 'Tymczak', 'Pfister', 'Honeyman',
  'Lee', 'Lloyd', 'Washington', 'Gutierrez', 'Jackson', 'Burroughs', 'Burrows', 'Schmidt',
  'Smith', 'Smythe', 'Green', 'Greene', 'Knight', 'Night', 'Wright', 'Rite',
  'robert', 'ROBERT', 'rObErT', 'A', 'Ab', 'Aeiouy', 'Bb', 'Bbb', 'BBB', 'Bf', 'Bpfv',
  'BAB', 'BEB', 'BIB', 'BOB', 'BUB', 'BYB', 'BHB', 'BWB', 'BHWB', 'BWHB', 'BAHB', 'BHAB',
  'SHC', 'SWC', 'SHAC', 'Sch', 'Tcha', 'Ch', 'Hh', 'Wh', 'Whw', 'Hw', 'H', 'W', 'Y', 'Hb', 'Wb', 'Yb',
  'Bhb', 'bwb', 'Ahb', 'Awb', 'Ohio', 'Hawaii', 'Q', 'Qqq', 'Xa', 'Zz',
  'abcdefghijklmnopqrstuvwxyz', 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'Bcdlmnr', 'Blmnrdt',
  '123', '1abc', 'a1b', 'a1b2c3', 'ab1', ' abc', '  Robert', 'Robert ', 'Rob ert', 'Rob-ert',
  "O'Brien", "O'Brian", 'OBrien', 'Mc Donald', 'McDonald', 'MacDonald', 'Smith-Jones', 'Smith Jones',
  '-abc', '.abc', '_abc', '#abc', '@abc', '[abc', 'a.b.c', 'a-b-c', 'a b c', 'ab cd', 'abc.', '',
  ' ', '   ', '-', '1', '0', 'Robert\tSmith', '\tRobert', 'Robert\t', '\nRobert', 'R obert',
]
const unicodeWords = [
  'Émile', 'émile', 'Emile', 'Müller', 'Mueller', 'Muller', 'Ñandu', 'Nandu', 'ßa', 'Straße', 'Strasse',
  'Æsir', 'æb', 'Øst', 'Ost', 'Åse', 'Çelik', 'Celik', 'Ðb', 'Þor', 'Łódź', 'Lodz', 'Škoda', 'Skoda',
  'Œuvre', 'Ÿves', 'Rébert', 'Robért', 'Robért', 'Москва', 'Moskva', '東京', 'Tokyo', 'αβγ', 'Αθήνα',
  'שלום', 'مرحبا', 'ＲＯＢＥＲＴ', 'Ｒobert', 'ｒobert', 'Róbert', 'Kelvin', 'Kelvin',
  'İstanbul', 'Istanbul', 'ıb', 'ﬁsh', 'fish', 'R😀bert', '😀Robert', 'Robert😀',
  '　Robert', ' Robert', '﻿Robert', 'Rob​ert',
]

const sourceRows = [...words, ...unicodeWords]
const valuesList = sourceRows.map((w, i) => `(${i + 1},${nquote(w)})`).join(',')

// Words for the DIFFERENCE matrix whose SOUNDEX codes differ in controlled
// positions: first letter B or P, followed by digits via vowel separators.
const representative = { 1: 'B', 2: 'C', 3: 'D', 4: 'L', 5: 'M', 6: 'R' }
const digitStrings = ['', '1', '11', '111', '12', '123', '132', '213', '231', '312', '321', '2', '22', '23', '3', '456', '1234']
const matrixWords = ['B', 'P'].flatMap(first => digitStrings.map(digits => first + [...digits].map(d => 'A' + representative[d]).join('')))
const matrixList = matrixWords.map((w, i) => `(${i + 1},${quote(w)})`).join(',')
const pairWords = ['Robert', 'Rupert', 'Rubin', 'Smith', 'Smythe', 'Green', 'Greene', 'Ashcraft', 'Tymczak', 'Lee', 'A', '', '1abc', ' ', 'Москва']
const pairList = pairWords.map((w, i) => `(${i + 1},${nquote(w)})`).join(',') + `,(${pairWords.length + 1},NULL)`

const numbers = limit => `WITH n(i) AS (SELECT 0 UNION ALL SELECT i+1 FROM n WHERE i<${limit})`
const charTable = `${numbers(255)} SELECT i,SOUNDEX(CHAR(i)+'BD') AS lead,SOUNDEX('B'+CHAR(i)+'B') AS mid_b,SOUNDEX('B'+CHAR(i)+'D') AS mid_d,SOUNDEX('BD'+CHAR(i)) AS tail,SOUNDEX(CHAR(i)) AS alone FROM n ORDER BY i OPTION(MAXRECURSION 300)`
const ncharTable = `${numbers(591)} SELECT i,SOUNDEX(NCHAR(i)+N'BD') AS lead,SOUNDEX(N'B'+NCHAR(i)+N'B') AS mid_b,SOUNDEX(N'B'+NCHAR(i)+N'D') AS mid_d,SOUNDEX(N'BD'+NCHAR(i)) AS tail,SOUNDEX(NCHAR(i)) AS alone FROM n ORDER BY i OPTION(MAXRECURSION 600)`
const extraCodePoints = [0x391, 0x3B1, 0x3A3, 0x410, 0x416, 0x436, 0x5D0, 0x627, 0x1E9E, 0x1EA0, 0x2002, 0x200B, 0x2126, 0x212A, 0x212B, 0x3000, 0x3042, 0x6771, 0xFB01, 0xFEFF, 0xFF21, 0xFF22, 0xFF41, 0xFF42, 0xD83D, 0xDE00, 0xFFFD, 0xFFFF]
const ncharExtra = `SELECT i,SOUNDEX(NCHAR(i)+N'BD') AS lead,SOUNDEX(N'B'+NCHAR(i)+N'B') AS mid_b,SOUNDEX(N'B'+NCHAR(i)+N'D') AS mid_d,SOUNDEX(N'BD'+NCHAR(i)) AS tail,SOUNDEX(NCHAR(i)) AS alone FROM (VALUES ${extraCodePoints.map(c => `(${c})`).join(',')}) d(i) ORDER BY i`
const letterDifference = `${numbers(25)} SELECT CHAR(65+i) AS letter,SOUNDEX('A'+CHAR(65+i)) AS after_a,SOUNDEX(CHAR(65+i)) AS alone,SOUNDEX(CHAR(97+i)+'b') AS lower_first,DIFFERENCE(CHAR(65+i),'A') AS diff_a FROM n ORDER BY i`

const compactWords = ['Ashcraft', 'Ashcroft', 'Tymczak', 'Pfister', 'BHB', 'BWB', 'BAB', 'SHC', 'Sch', 'Hh', 'Wh', 'Robert', 'Rupert', 'Lee', '1abc', ' abc', 'a1b', 'ab cd', '', 'Émile', 'Москва', 'R😀bert']
const compactList = compactWords.map((w, i) => `(${i + 1},${nquote(w)})`).join(',')
const compactQuery = `SELECT d.id,SOUNDEX(CAST(d.w AS VARCHAR(40))) AS v,SOUNDEX(d.w) AS n,DIFFERENCE(CAST(d.w AS VARCHAR(40)),'Ashcraft') AS diff_ashcraft,DIFFERENCE(d.w,N'Robert') AS diff_robert,DIFFERENCE(d.w,N'BAB') AS diff_bab,DIFFERENCE(d.w,N'SAC') AS diff_sac,DIFFERENCE(N'BAB',d.w) AS bab_diff FROM (VALUES ${compactList}) d(id,w) ORDER BY d.id`
const compactLiterals = "SELECT SOUNDEX('Ashcraft') AS ashcraft,SOUNDEX('Tymczak') AS tymczak,SOUNDEX('BHB') AS bhb,SOUNDEX('Pfister') AS pfister,SOUNDEX(N'Ashcraft') AS n_ashcraft,DIFFERENCE('Ashcraft','Ashcroft') AS d1,DIFFERENCE('Robert','Rupert') AS d2,DIFFERENCE('','') AS d3,DIFFERENCE('BHB','BAB') AS d4,DIFFERENCE('SHC','SAC') AS d5,DIFFERENCE('Pfister','Pister') AS d6,DIFFERENCE('Bb','Bab') AS d7"

const cases = [
  ['environment', "SELECT CAST(SERVERPROPERTY('ProductMajorVersion') AS INT) AS major,CAST(SERVERPROPERTY('Collation') AS NVARCHAR(128)) AS server_collation,DATABASEPROPERTYEX(DB_NAME(),'Collation') AS database_collation,(SELECT compatibility_level FROM sys.databases WHERE database_id=DB_ID()) AS compatibility_level"],
  // Literal forms and typing.
  ['soundex varchar literal', "SELECT SOUNDEX('Robert') AS value"],
  ['soundex nvarchar literal', "SELECT SOUNDEX(N'Robert') AS value"],
  ['soundex char literal', "SELECT SOUNDEX(CAST('Robert' AS CHAR(20))) AS value"],
  ['soundex nchar literal', "SELECT SOUNDEX(CAST(N'Robert' AS NCHAR(20))) AS value"],
  ['soundex varchar max', "SELECT SOUNDEX(CAST('Robert' AS VARCHAR(MAX))) AS value"],
  ['soundex nvarchar max', "SELECT SOUNDEX(CAST(N'Robert' AS NVARCHAR(MAX))) AS value"],
  ['soundex varchar 8000 long', "SELECT SOUNDEX(CAST(REPLICATE('b',8000) AS VARCHAR(8000))) AS value"],
  ['soundex varchar max long', "SELECT SOUNDEX(REPLICATE(CAST('Robert' AS VARCHAR(MAX)),2000)) AS value,SOUNDEX('Rob'+REPLICATE(CAST(' ' AS VARCHAR(MAX)),9000)+'ert') AS spaced"],
  ['soundex nvarchar max long', "SELECT SOUNDEX(REPLICATE(CAST(N'Robert' AS NVARCHAR(MAX)),2000)) AS value"],
  ['soundex untyped null', "SELECT SOUNDEX(NULL) AS value"],
  ['soundex typed null varchar', "SELECT SOUNDEX(CAST(NULL AS VARCHAR(10))) AS value"],
  ['soundex typed null nvarchar', "SELECT SOUNDEX(CAST(NULL AS NVARCHAR(10))) AS value"],
  ['soundex empty and blanks', "SELECT SOUNDEX('') AS empty_value,SOUNDEX(' ') AS space_value,SOUNDEX(N'') AS n_empty,SOUNDEX(N'   ') AS n_spaces,SOUNDEX(CHAR(9)) AS tab_value"],
  ['soundex int input', "SELECT SOUNDEX(123) AS value"],
  ['soundex decimal input', "SELECT SOUNDEX(CAST(1.5 AS DECIMAL(5,1))) AS value"],
  ['soundex date input', "SELECT SOUNDEX(CAST('2024-01-02' AS DATE)) AS value"],
  ['soundex varbinary input', "SELECT SOUNDEX(0x526F62657274) AS value"],
  ['soundex uniqueidentifier input', "SELECT SOUNDEX(CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER)) AS value"],
  ['soundex text input', "SELECT SOUNDEX(CAST('Robert' AS TEXT)) AS value"],
  ['soundex ntext input', "SELECT SOUNDEX(CAST(N'Robert' AS NTEXT)) AS value"],
  ['soundex xml input', "SELECT SOUNDEX(CAST('<a/>' AS XML)) AS value"],
  ['soundex sql_variant input', "SELECT SOUNDEX(CAST('Robert' AS SQL_VARIANT)) AS value"],
  ['soundex no arguments', "SELECT SOUNDEX() AS value"],
  ['soundex two arguments', "SELECT SOUNDEX('a','b') AS value"],
  ['soundex unnamed column', "SELECT SOUNDEX('Robert')"],
  ['soundex collate clauses', "SELECT SOUNDEX('Robert' COLLATE Latin1_General_BIN2) AS bin2,SOUNDEX('robert' COLLATE Latin1_General_CS_AS) AS cs,SOUNDEX(N'Émile' COLLATE Latin1_General_CI_AI) AS ai,SOUNDEX(N'ımre' COLLATE Turkish_CI_AS) AS turkish_dotless,SOUNDEX(N'imre' COLLATE Turkish_CI_AS) AS turkish_dotted"],
  ['soundex result collation', "SELECT SQL_VARIANT_PROPERTY(SOUNDEX('Robert'),'BaseType') AS base_type,SQL_VARIANT_PROPERTY(SOUNDEX('Robert'),'MaxLength') AS max_length,SQL_VARIANT_PROPERTY(SOUNDEX(N'Robert'),'BaseType') AS n_base_type,SQL_VARIANT_PROPERTY(SOUNDEX(N'Robert'),'MaxLength') AS n_max_length,SQL_VARIANT_PROPERTY(SOUNDEX('Robert' COLLATE Latin1_General_BIN2),'Collation') AS bin2_collation,SQL_VARIANT_PROPERTY(SOUNDEX(N'Robert'),'Collation') AS n_collation"],
  ['soundex result collation conflict', "SELECT CASE WHEN SOUNDEX('Robert' COLLATE Latin1_General_BIN2)=SOUNDEX('Rupert' COLLATE Latin1_General_CS_AS) THEN 1 ELSE 0 END AS same"],
  ['soundex code page varchar', "SELECT SOUNDEX(CAST(N'Жук' COLLATE Cyrillic_General_CI_AS AS VARCHAR(10))) AS cyrillic,SOUNDEX(CAST(N'Émile' COLLATE Latin1_General_100_CI_AS_SC_UTF8 AS VARCHAR(10))) AS utf8,SOUNDEX(CAST(N'Émile' AS VARCHAR(10))) AS cp1252,SOUNDEX(CAST(N'Łódź' AS VARCHAR(10))) AS best_fit"],
  ['soundex implicit conversion to varchar', "SELECT CAST(N'Москва' AS VARCHAR(10)) AS converted,SOUNDEX(CAST(N'Москва' AS VARCHAR(10))) AS converted_soundex,SOUNDEX(N'Москва') AS n_soundex"],
  ['soundex select into descriptor', "SELECT SOUNDEX('Robert') AS v,SOUNDEX(N'Robert') AS n,SOUNDEX(CAST('Robert' AS VARCHAR(MAX))) AS vmax,SOUNDEX(CAST(NULL AS VARCHAR(10))) AS typed_null,DIFFERENCE('Robert','Rupert') AS d,DIFFERENCE(CAST(NULL AS VARCHAR(10)),'a') AS d_null INTO #soundex_into; SELECT c.name,t.name AS type_name,c.max_length,c.is_nullable,c.collation_name FROM tempdb.sys.columns c JOIN tempdb.sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('tempdb..#soundex_into') ORDER BY c.column_id; DROP TABLE #soundex_into"],
  ['soundex describe first result set', "SELECT name,system_type_name,max_length,is_nullable,collation_name FROM sys.dm_exec_describe_first_result_set(N'SELECT SOUNDEX(''Robert'') AS v,SOUNDEX(N''Robert'') AS n,DIFFERENCE(''a'',''b'') AS d,SOUNDEX(v) AS col_v,SOUNDEX(n) AS col_n,DIFFERENCE(v,n) AS col_d FROM dbo.soundex_src',NULL,0) ORDER BY column_ordinal"],
  // Column evaluation over the shared word list.
  ['soundex source words', "SELECT id,w,SOUNDEX(v) AS v,SOUNDEX(n) AS n,SOUNDEX(vm) AS vm,SOUNDEX(nm) AS nm,SOUNDEX(c) AS c FROM dbo.soundex_src ORDER BY id"],
  ['soundex literal words varchar', `SELECT d.id,SOUNDEX(CAST(d.w AS VARCHAR(40))) AS value FROM (VALUES ${valuesList}) d(id,w) ORDER BY d.id`],
  ['soundex per character varchar', charTable],
  ['soundex per character nvarchar', ncharTable],
  ['soundex per character nvarchar extra', ncharExtra],
  ['soundex letter table', letterDifference],
  ['soundex computed column properties', "SELECT COLUMNPROPERTY(OBJECT_ID('dbo.soundex_computed'),'s','IsDeterministic') AS s_deterministic,COLUMNPROPERTY(OBJECT_ID('dbo.soundex_computed'),'s','IsPrecise') AS s_precise,COLUMNPROPERTY(OBJECT_ID('dbo.soundex_computed'),'d','IsDeterministic') AS d_deterministic,COLUMNPROPERTY(OBJECT_ID('dbo.soundex_computed'),'d','IsPrecise') AS d_precise,c.is_persisted,c.definition FROM sys.computed_columns c WHERE c.object_id=OBJECT_ID('dbo.soundex_computed') ORDER BY c.column_id"],
  ['soundex computed column rows', "SELECT id,s,d FROM dbo.soundex_computed ORDER BY id"],
  ['soundex in predicate', "SELECT id FROM dbo.soundex_src WHERE SOUNDEX(v)='R163' ORDER BY id"],
  ['soundex group by', "SELECT SOUNDEX(v) AS code,COUNT(*) AS n FROM dbo.soundex_src WHERE id<=24 GROUP BY SOUNDEX(v) ORDER BY code"],
  // DIFFERENCE.
  ['difference literal pairs', "SELECT DIFFERENCE('Green','Greene') AS a,DIFFERENCE('Robert','Rupert') AS b,DIFFERENCE('Robert','Rubin') AS c,DIFFERENCE('Smith','Smythe') AS d,DIFFERENCE('Robert','Smith') AS e,DIFFERENCE('abc','xyz') AS f"],
  ['difference nvarchar', "SELECT DIFFERENCE(N'Green',N'Greene') AS a,DIFFERENCE(N'Robert','Rupert') AS b,DIFFERENCE(CAST('Robert' AS VARCHAR(MAX)),CAST(N'Rupert' AS NVARCHAR(MAX))) AS c"],
  ['difference empty and blanks', "SELECT DIFFERENCE('','') AS a,DIFFERENCE('',' ') AS b,DIFFERENCE('','A') AS c,DIFFERENCE('1','2') AS d,DIFFERENCE('A','1') AS e,DIFFERENCE(' ','Robert') AS f"],
  ['difference untyped nulls', "SELECT DIFFERENCE(NULL,'a') AS a,DIFFERENCE('a',NULL) AS b,DIFFERENCE(NULL,NULL) AS c"],
  ['difference typed nulls', "SELECT DIFFERENCE(CAST(NULL AS VARCHAR(10)),'a') AS a,DIFFERENCE(N'a',CAST(NULL AS NVARCHAR(10))) AS b"],
  ['difference int input', "SELECT DIFFERENCE(1,2) AS value"],
  ['difference text input', "SELECT DIFFERENCE(CAST('a' AS TEXT),'a') AS value"],
  ['difference one argument', "SELECT DIFFERENCE('a') AS value"],
  ['difference three arguments', "SELECT DIFFERENCE('a','b','c') AS value"],
  ['difference unnamed column', "SELECT DIFFERENCE('Robert','Rupert')"],
  ['difference collation conflict', "SELECT DIFFERENCE('Robert' COLLATE Latin1_General_BIN2,'Rupert' COLLATE Latin1_General_CS_AS) AS value"],
  ['difference word pairs', `SELECT a.id AS a_id,b.id AS b_id,SOUNDEX(a.w) AS a_code,SOUNDEX(b.w) AS b_code,DIFFERENCE(a.w,b.w) AS value FROM (VALUES ${pairList}) a(id,w) CROSS JOIN (VALUES ${pairList}) b(id,w) ORDER BY a.id,b.id`],
  ['difference code matrix', `SELECT a.id AS a_id,b.id AS b_id,a.w AS a_word,b.w AS b_word,SOUNDEX(a.w) AS a_code,SOUNDEX(b.w) AS b_code,DIFFERENCE(a.w,b.w) AS value FROM (VALUES ${matrixList}) a(id,w) CROSS JOIN (VALUES ${matrixList}) b(id,w) ORDER BY a.id,b.id`],
  ['difference columns', "SELECT a.id,DIFFERENCE(a.v,'Robert') AS v_robert,DIFFERENCE(a.n,N'Robert') AS n_robert,DIFFERENCE(a.vm,a.nm) AS vm_nm FROM dbo.soundex_src a ORDER BY a.id"],
]

const setup = [
  ['create source', 'CREATE TABLE dbo.soundex_src(id INT CONSTRAINT soundex_src_pk PRIMARY KEY,w NVARCHAR(40),v VARCHAR(40),n NVARCHAR(40),vm VARCHAR(MAX),nm NVARCHAR(MAX),c CHAR(40))'],
  ['insert source', `INSERT dbo.soundex_src(id,w,v,n,vm,nm,c) SELECT id,w,CAST(w AS VARCHAR(40)),w,CAST(w AS VARCHAR(MAX)),CAST(w AS NVARCHAR(MAX)),CAST(w AS CHAR(40)) FROM (VALUES ${valuesList},(${sourceRows.length + 1},NULL)) d(id,w)`],
  ['create computed', 'CREATE TABLE dbo.soundex_computed(id INT CONSTRAINT soundex_computed_pk PRIMARY KEY,v VARCHAR(40),s AS SOUNDEX(v) PERSISTED,d AS DIFFERENCE(v,\'Robert\') PERSISTED)'],
  ['insert computed', "INSERT dbo.soundex_computed(id,v) VALUES (1,'Robert'),(2,'Ashcraft'),(3,''),(4,NULL)"],
  ['index computed', 'CREATE INDEX soundex_computed_s ON dbo.soundex_computed(s)'],
]

const compatibilityLevels = [100, 110, 120, 130, 140, 150, 160, 170]

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
  // tedious keeps the first error on the Request and passes it to every later
  // callback; only an error object not already reported belongs to this phase.
  const reported = new Set()
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
      if (error && !reported.has(error) && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      if (error) reported.add(error)
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

const long = 'Robert' + ' '.repeat(8994)
const rpcSoundex = 'SELECT SOUNDEX(@value) AS value'
const rpcCases = [
  ['rpc soundex nvarchar', TYPES.NVarChar, 'Robert', { length: 20 }],
  ['rpc soundex varchar', TYPES.VarChar, 'Robert', { length: 20 }],
  ['rpc soundex nvarchar accented', TYPES.NVarChar, 'Émile', { length: 20 }],
  ['rpc soundex varchar accented', TYPES.VarChar, 'Émile', { length: 20 }],
  ['rpc soundex nvarchar cyrillic', TYPES.NVarChar, 'Москва', { length: 20 }],
  ['rpc soundex nvarchar h rule', TYPES.NVarChar, 'Ashcraft', { length: 20 }],
  ['rpc soundex nvarchar empty', TYPES.NVarChar, '', { length: 20 }],
  ['rpc soundex nvarchar leading digit', TYPES.NVarChar, '1abc', { length: 20 }],
  ['rpc soundex null nvarchar', TYPES.NVarChar, null, { length: 20 }],
  ['rpc soundex nvarchar max', TYPES.NVarChar, long, { length: 9000 }],
  ['rpc soundex varchar max', TYPES.VarChar, long, { length: 9000 }],
  ['rpc soundex int', TYPES.Int, 123, undefined],
]
const rpcDifference = 'SELECT DIFFERENCE(@a,@b) AS value,SOUNDEX(@a) AS a_code,SOUNDEX(@b) AS b_code'
const rpcDifferenceCases = [
  ['rpc difference nvarchar', TYPES.NVarChar, 'Robert', TYPES.NVarChar, 'Rupert'],
  ['rpc difference mixed', TYPES.VarChar, 'Green', TYPES.NVarChar, 'Greene'],
  ['rpc difference null', TYPES.NVarChar, 'Robert', TYPES.NVarChar, null],
  ['rpc difference empty', TYPES.NVarChar, '', TYPES.NVarChar, ''],
]

async function observe(connection) {
  const records = []
  // Every recorded statement carries its record name, so no two programs
  // (including sp_executesql calls and statements repeated per compatibility
  // level) share a server plan-cache entry.
  async function record(name, text, parameters) {
    const sql = `${text} /* ${name} */`
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
  for (const [name, type, value, options] of rpcCases) await record(name, rpcSoundex, [['value', type, value, options]])
  for (const [name, aType, a, bType, b] of rpcDifferenceCases) await record(name, rpcDifference, [['a', aType, a, { length: 20 }], ['b', bType, b, { length: 20 }]])
  const preparedSql = 'SELECT SOUNDEX(@value) AS code,DIFFERENCE(@value,@other) AS difference /* prepared soundex and difference */'
  const handle = await prepared(connection, preparedSql, [
    ['value', TYPES.NVarChar, { length: 20 }],
    ['other', TYPES.NVarChar, { length: 20 }],
  ], [
    { value: 'Robert', other: 'Rupert' },
    { value: 'Ashcraft', other: 'Ashcroft' },
    { value: '', other: '' },
    { value: null, other: 'Robert' },
    { value: 'Émile', other: 'Emile' },
    { value: 'Robert', other: 'Rupert' },
  ])
  records.push({ name: 'prepared soundex and difference', sql: preparedSql, prepared: handle })
  // Compatibility levels last; the isolated database is dropped afterwards.
  for (const level of compatibilityLevels) {
    await record(`compatibility ${level} set`, `ALTER DATABASE CURRENT SET COMPATIBILITY_LEVEL=${level}; SELECT compatibility_level FROM sys.databases WHERE database_id=DB_ID()`)
    await record(`compatibility ${level} words`, compactQuery)
    await record(`compatibility ${level} literals`, compactLiterals)
    await record(`compatibility ${level} rpc`, rpcDifference, [['a', TYPES.NVarChar, 'BHB', { length: 20 }], ['b', TYPES.NVarChar, 'BAB', { length: 20 }]])
    if (level === 100) {
      await record('compatibility 100 per character varchar', charTable)
      await record('compatibility 100 per character nvarchar', ncharTable)
      await record('compatibility 100 source words', "SELECT id,SOUNDEX(v) AS v,SOUNDEX(n) AS n FROM dbo.soundex_src ORDER BY id")
      await record('compatibility 100 code matrix', cases.find(([name]) => name === 'difference code matrix')[1])
    }
  }
  return records
}

function validate(run) {
  assert.equal(run.length, setup.length + cases.length + rpcCases.length + rpcDifferenceCases.length + 1 + compatibilityLevels.length * 4 + 4)
  const get = name => run.find(record => record.name === name)?.result
  for (const [name] of setup) assert.equal(get(name).errors.length, 0, name)
  assert.deepEqual(get('soundex varchar literal').sets[0].rows, [['R163']])
  assert.deepEqual(get('difference literal pairs').sets[0].rows[0].slice(0, 2), [4, 4])
  assert.equal(get('soundex per character varchar').sets[0].rows.length, 256)
  assert.equal(get('soundex per character nvarchar').sets[0].rows.length, 592)
  assert.equal(get('difference code matrix').sets[0].rows.length, matrixWords.length ** 2)
  assert.equal(get('soundex source words').sets[0].rows.length, sourceRows.length + 1)
  for (const level of compatibilityLevels) assert.deepEqual(get(`compatibility ${level} set`).sets[0].rows, [[level]], String(level))
  for (const record of run) {
    const results = record.prepared ? [record.prepared.prepare, ...record.prepared.executions.map(x => x.result), record.prepared.unprepare].filter(Boolean) : [record.result]
    for (const result of results) assert(result.done.length > 0, `${record.name}: no completion`)
  }
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    console.error(`started owned container ${container.name}`)
    const runs = []
    for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
      const run = await isolatedReference({
        ...config, options: { ...config.options, requestTimeout: 120000 },
      }, observe)
      validate(run)
      runs.push(run)
    }
    if (!oneDatabase) assertSameCapture(runs[0], runs[1], 'SOUNDEX/DIFFERENCE observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) {
  assertSameCapture(containers[1].runs[0], containers[0].runs[0], 'SOUNDEX/DIFFERENCE observations differ across containers (database 0)')
  assertSameCapture(containers[1].runs[1], containers[0].runs[0], 'SOUNDEX/DIFFERENCE observations differ across containers (database 1)')
}
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
if (!writeFixture && !oneDatabase) {
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'SOUNDEX/DIFFERENCE observations differ from retained fixture')
}
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${containers[0].runs[0].length} SOUNDEX/DIFFERENCE observations` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained ? ' and matched retained fixture' : '') + (writeFixture ? '; wrote new retained fixture' : ''))
