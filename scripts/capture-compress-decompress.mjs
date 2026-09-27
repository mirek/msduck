#!/usr/bin/env node
// Retain SQL Server COMPRESS/DECOMPRESS rows, descriptors, diagnostics and
// completions for ordinary batches, sp_executesql RPC and sp_prepare/sp_execute.
//
// Large values are retained as { kind: 'digest', ... } records (length and
// SHA-256) instead of full bytes; see boundValue. Small values stay exact.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, realpath, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { gunzipSync } from 'node:zlib'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/compress-decompress.json', import.meta.url)
const args = process.argv.slice(2)
const flags = ['--write-fixture', '--one-database', '--check-fixture']
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const checkFixture = args.includes('--check-fixture')
const positional = args.filter(arg => !flags.includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && (oneDatabase || checkFixture)) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/compress-decompress-reference-v1/capture.json')

// Values longer than this many bytes (binary) or UTF-16 code units (strings)
// are replaced by their length and SHA-256 so the fixture stays bounded.
const FULL_VALUE_LIMIT = 256
function boundValue(value) {
  if (Buffer.isBuffer(value) && value.length > FULL_VALUE_LIMIT) {
    return { kind: 'digest', type: 'binary', length: value.length, sha256: createHash('sha256').update(value).digest('hex'), head: value.subarray(0, 16).toString('hex') }
  }
  if (typeof value === 'string' && value.length > FULL_VALUE_LIMIT) {
    return { kind: 'digest', type: 'string', length: value.length, sha256Utf16le: createHash('sha256').update(Buffer.from(value, 'utf16le')).digest('hex'), head: value.slice(0, 16) }
  }
  return value
}
function boundRows(result) {
  for (const set of result.sets) set.rows = set.rows.map(row => row.map(boundValue))
  return result
}

// Hand-built GZIP members (generated once with Node zlib and hard-coded so the
// statement text never depends on the client's zlib). Each decompresses to
// 'hello' under a conforming RFC 1952 reader unless noted.
const gzip = {
  'os 0': '0x1f8b0800000000000000cb48cdc9c9070086a6103605000000',
  'os 3 (unix)': '0x1f8b0800000000000003cb48cdc9c9070086a6103605000000',
  'nonzero mtime': '0x1f8b080000105e5f0003cb48cdc9c9070086a6103605000000',
  'FTEXT flag': '0x1f8b0801000000000003cb48cdc9c9070086a6103605000000',
  'FNAME field': '0x1f8b0808000000000003612e74787400cb48cdc9c9070086a6103605000000',
  'FCOMMENT field': '0x1f8b0810000000000003686900cb48cdc9c9070086a6103605000000',
  'FEXTRA field': '0x1f8b08040000000000030600414202000102cb48cdc9c9070086a6103605000000',
  'FHCRC valid': '0x1f8b0802000000000003a777cb48cdc9c9070086a6103605000000',
  'FHCRC wrong': '0x1f8b08020000000000030000cb48cdc9c9070086a6103605000000',
  'reserved flag bit': '0x1f8b0820000000000003cb48cdc9c9070086a6103605000000',
  'compression method 7': '0x1f8b0700000000000003cb48cdc9c9070086a6103605000000',
  'wrong magic': '0x1f8c0800000000000003cb48cdc9c9070086a6103605000000',
  'wrong crc32': '0x1f8b0800000000000003cb48cdc9c907000000000005000000',
  'wrong isize': '0x1f8b0800000000000003cb48cdc9c9070086a6103609000000',
  'missing trailer': '0x1f8b0800000000000003cb48cdc9c90700',
  'half trailer': '0x1f8b0800000000000003cb48cdc9c9070086a61036',
  'trailing garbage': '0x1f8b0800000000000003cb48cdc9c9070086a610360500000078797a',
  'two members (helloworld)': '0x1f8b0800000000000003cb48cdc9c9070086a61036050000001f8b08000000000000032bcf2fca4901004311773a05000000',
  'stored block': '0x1f8b0800000000000003010500faff68656c6c6f86a6103605000000',
  'empty member (empty)': '0x1f8b080000000000000303000000000000000000',
  'zlib wrapper': '0x789ccb48cdc9c90700062c0215',
  'raw deflate': '0xcb48cdc9c90700',
  'header only': '0x1f8b0800000000000003',
  'truncated deflate data': '0x1f8b0800000000000003cb48',
  'truncated inside header': '0x1f8b0800000000',
  'magic and method only': '0x1f8b08',
  'magic only': '0x1f8b',
  'first magic byte only': '0x1f',
  'empty member wrong isize': '0x1f8b080000000000000303000000000001000000',
  'single byte': '0x00',
}

// Deterministic incompressible data built server-side: SHA-256 of each
// big-endian INT 0..blocks-1, concatenated in order (the same bytes as the
// client-side clientNoise below). Set-based, so it emits one DONE token.
const noise = (blocks, name = '@noise') => `DECLARE ${name} VARBINARY(MAX)=(SELECT CONVERT(VARBINARY(MAX),STRING_AGG(CONVERT(VARCHAR(MAX),HASHBYTES('SHA2_256',CAST(value AS VARBINARY(4))),2),'') WITHIN GROUP (ORDER BY value),2) FROM GENERATE_SERIES(0,${blocks - 1}));`
// Server-side summary of a large compress/round trip without returning the value.
const summary = (expr, back = 'VARBINARY(MAX)') => `SELECT DATALENGTH(${expr}) AS input_bytes, DATALENGTH(COMPRESS(${expr})) AS compressed_bytes, HASHBYTES('SHA2_256',COMPRESS(${expr})) AS compressed_sha256, SUBSTRING(COMPRESS(${expr}),1,10) AS gzip_header, CASE WHEN CAST(DECOMPRESS(COMPRESS(${expr})) AS ${back})=${expr} THEN 1 ELSE 0 END AS round_trip, DATALENGTH(DECOMPRESS(COMPRESS(${expr}))) AS decompressed_bytes`

const setup = [
  ['create source', "CREATE TABLE dbo.compress_src(id INT NOT NULL CONSTRAINT pk_compress_src PRIMARY KEY, a VARCHAR(20) NULL, n NVARCHAR(20) NULL, b VARBINARY(20) NULL, am VARCHAR(MAX) NULL, u8 VARCHAR(20) COLLATE Latin1_General_100_CI_AS_SC_UTF8 NULL)"],
  ['insert source', "INSERT dbo.compress_src(id,a,n,b,am,u8) VALUES (1,'abc',N'abc',0x616263,'abc','abc'),(2,'',N'',0x,'',''),(3,NULL,NULL,NULL,NULL,NULL),(4,'caf'+CHAR(233),N'caf'+NCHAR(233),0x636166e9,REPLICATE(CAST('ab' AS VARCHAR(MAX)),5000),N'caf'+NCHAR(233))"],
]

const cases = [
  ['environment', "SELECT CAST(SERVERPROPERTY('ProductMajorVersion') AS INT) AS major, CAST(SERVERPROPERTY('Collation') AS NVARCHAR(128)) AS server_collation, DATABASEPROPERTYEX(DB_NAME(),'Collation') AS database_collation"],

  // COMPRESS exact bytes per input family.
  ['compress varchar', "SELECT COMPRESS('abc') AS value"],
  ['compress varchar typed', "SELECT COMPRESS(CAST('abc' AS VARCHAR(10))) AS value"],
  ['compress nvarchar', "SELECT COMPRESS(N'abc') AS value"],
  ['compress varbinary', "SELECT COMPRESS(0x616263) AS value"],
  ['compress varchar max', "SELECT COMPRESS(CAST('abc' AS VARCHAR(MAX))) AS value"],
  ['compress nvarchar max', "SELECT COMPRESS(CAST(N'abc' AS NVARCHAR(MAX))) AS value"],
  ['compress varbinary max', "SELECT COMPRESS(CAST(0x616263 AS VARBINARY(MAX))) AS value"],
  ['compress char padded', "SELECT COMPRESS(CAST('abc' AS CHAR(5))) AS value"],
  ['compress nchar padded', "SELECT COMPRESS(CAST(N'abc' AS NCHAR(5))) AS value"],
  ['compress binary padded', "SELECT COMPRESS(CAST(0x616263 AS BINARY(5))) AS value"],
  ['compress utf8 collation', "SELECT COMPRESS(CAST(N'caf'+NCHAR(233) AS VARCHAR(20)) COLLATE Latin1_General_100_CI_AS_SC_UTF8) AS value"],
  ['compress cp1252 accent', "SELECT COMPRESS('caf'+CHAR(233)) AS value"],
  ['compress nvarchar supplementary', "SELECT COMPRESS(NCHAR(0xD83D)+NCHAR(0xDE00)) AS value"],
  ['compress single byte', "SELECT COMPRESS(0x00) AS value"],
  ['compress hello', "SELECT COMPRESS('hello') AS value"],
  ['compress trailing spaces', "SELECT COMPRESS('abc   ') AS value"],
  ['compress repetitive 100', "SELECT COMPRESS(REPLICATE('a',100)) AS value"],
  ['compress repetitive text', "SELECT COMPRESS(REPLICATE('The quick brown fox jumps over the lazy dog. ',20)) AS value"],
  ['compress 8000 bounded', "SELECT COMPRESS(REPLICATE('x',8000)) AS value"],
  ['compress nvarchar 4000 bounded', "SELECT COMPRESS(REPLICATE(N'xy',2000)) AS value"],
  ['compress noise 256', `${noise(8)} SELECT COMPRESS(@noise) AS value`],
  ['compress deterministic twice', "SELECT CASE WHEN COMPRESS('abc')=COMPRESS('abc') THEN 1 ELSE 0 END AS same, CASE WHEN COMPRESS('abc')=COMPRESS(0x616263) THEN 1 ELSE 0 END AS varchar_equals_binary, CASE WHEN COMPRESS('abc')=COMPRESS(N'abc') THEN 1 ELSE 0 END AS varchar_equals_nvarchar"],
  ['compress nested', "SELECT COMPRESS(COMPRESS('abc')) AS value"],
  ['compress datalength', "SELECT DATALENGTH(COMPRESS('')) AS empty_bytes, DATALENGTH(COMPRESS('a')) AS one_bytes, DATALENGTH(COMPRESS('abc')) AS three_bytes, DATALENGTH(COMPRESS(REPLICATE('a',1000))) AS repeat_bytes"],

  // Empty and NULL.
  ['compress empty varchar', "SELECT COMPRESS('') AS value"],
  ['compress empty nvarchar', "SELECT COMPRESS(N'') AS value"],
  ['compress empty varbinary', "SELECT COMPRESS(0x) AS value"],
  ['compress empty varchar max', "SELECT COMPRESS(CAST('' AS VARCHAR(MAX))) AS value"],
  ['compress typed null varchar', "SELECT COMPRESS(CAST(NULL AS VARCHAR(10))) AS value"],
  ['compress typed null nvarchar max', "SELECT COMPRESS(CAST(NULL AS NVARCHAR(MAX))) AS value"],
  ['compress typed null varbinary', "SELECT COMPRESS(CAST(NULL AS VARBINARY(10))) AS value"],
  ['compress untyped null', "SELECT COMPRESS(NULL) AS value"],

  // Input type acceptance.
  ['compress int', "SELECT COMPRESS(1) AS value"],
  ['compress decimal', "SELECT COMPRESS(1.5) AS value"],
  ['compress float', "SELECT COMPRESS(CAST(1 AS FLOAT)) AS value"],
  ['compress bit', "SELECT COMPRESS(CAST(1 AS BIT)) AS value"],
  ['compress datetime', "SELECT COMPRESS(CAST('2024-01-02' AS DATETIME)) AS value"],
  ['compress date', "SELECT COMPRESS(CAST('2024-01-02' AS DATE)) AS value"],
  ['compress uniqueidentifier', "SELECT COMPRESS(CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER)) AS value"],
  ['compress xml', "SELECT COMPRESS(CAST('<a/>' AS XML)) AS value"],
  ['compress text', "SELECT COMPRESS(CAST('abc' AS TEXT)) AS value"],
  ['compress ntext', "SELECT COMPRESS(CAST(N'abc' AS NTEXT)) AS value"],
  ['compress image', "SELECT COMPRESS(CAST(0x616263 AS IMAGE)) AS value"],
  ['compress sql_variant', "SELECT COMPRESS(CAST('abc' AS SQL_VARIANT)) AS value"],
  ['compress rowversion-like', "SELECT COMPRESS(CAST(0x00000000000007D1 AS ROWVERSION)) AS value"],
  ['compress json type', "SELECT COMPRESS(CAST(N'{\"a\":1}' AS JSON)) AS value"],
  ['compress no arguments', "SELECT COMPRESS() AS value"],
  ['compress two arguments', "SELECT COMPRESS('a','b') AS value"],

  // DECOMPRESS of SQL Server output and round trips.
  ['decompress compress varchar', "SELECT DECOMPRESS(COMPRESS('abc')) AS value"],
  ['decompress compress nvarchar', "SELECT DECOMPRESS(COMPRESS(N'abc')) AS value"],
  ['decompress compress empty', "SELECT DECOMPRESS(COMPRESS('')) AS value, DATALENGTH(DECOMPRESS(COMPRESS(''))) AS bytes"],
  ['round trip varchar max', "SELECT CAST(DECOMPRESS(COMPRESS('abc')) AS VARCHAR(MAX)) AS value"],
  ['round trip nvarchar max', "SELECT CAST(DECOMPRESS(COMPRESS(N'abc')) AS NVARCHAR(MAX)) AS value"],
  ['round trip varchar bounded', "SELECT CAST(DECOMPRESS(COMPRESS('abc')) AS VARCHAR(10)) AS value"],
  ['round trip varchar truncating', "SELECT CAST(DECOMPRESS(COMPRESS('abcdef')) AS VARCHAR(3)) AS value"],
  ['round trip nvarchar as varchar', "SELECT CAST(DECOMPRESS(COMPRESS(N'abc')) AS VARCHAR(MAX)) AS value, DATALENGTH(CAST(DECOMPRESS(COMPRESS(N'abc')) AS VARCHAR(MAX))) AS bytes"],
  ['round trip varchar as nvarchar', "SELECT CAST(DECOMPRESS(COMPRESS('abcd')) AS NVARCHAR(MAX)) AS value"],
  ['round trip odd bytes as nvarchar', "SELECT CAST(DECOMPRESS(COMPRESS('abc')) AS NVARCHAR(MAX)) AS value, DATALENGTH(CAST(DECOMPRESS(COMPRESS('abc')) AS NVARCHAR(MAX))) AS bytes"],
  ['round trip cp1252 accent', "SELECT CAST(DECOMPRESS(COMPRESS('caf'+CHAR(233))) AS VARCHAR(MAX)) AS value"],
  ['round trip utf8 collation', "SELECT DECOMPRESS(COMPRESS(CAST(N'caf'+NCHAR(233) AS VARCHAR(20)) COLLATE Latin1_General_100_CI_AS_SC_UTF8)) AS bytes, CAST(DECOMPRESS(COMPRESS(CAST(N'caf'+NCHAR(233) AS VARCHAR(20)) COLLATE Latin1_General_100_CI_AS_SC_UTF8)) AS VARCHAR(MAX)) COLLATE Latin1_General_100_CI_AS_SC_UTF8 AS value"],
  ['round trip supplementary', "SELECT CAST(DECOMPRESS(COMPRESS(NCHAR(0xD83D)+NCHAR(0xDE00))) AS NVARCHAR(MAX)) AS value"],
  ['round trip char padding', "SELECT CAST(DECOMPRESS(COMPRESS(CAST('abc' AS CHAR(5)))) AS VARCHAR(MAX)) AS value"],
  ['round trip int cast', "SELECT CAST(DECOMPRESS(COMPRESS(CAST(CAST(258 AS INT) AS VARBINARY(4)))) AS INT) AS value"],
  ['round trip implicit varchar', "DECLARE @v VARCHAR(MAX); SET @v=DECOMPRESS(COMPRESS('abc')); SELECT @v AS value"],
  ['round trip nested', "SELECT CAST(DECOMPRESS(DECOMPRESS(COMPRESS(COMPRESS('abc')))) AS VARCHAR(MAX)) AS value"],
  ['decompress untyped null', "SELECT DECOMPRESS(NULL) AS value"],
  ['decompress typed null', "SELECT DECOMPRESS(CAST(NULL AS VARBINARY(MAX))) AS value"],
  ['decompress empty', "SELECT DECOMPRESS(0x) AS value"],
  ['decompress varchar input', "SELECT DECOMPRESS('abc') AS value"],
  ['decompress nvarchar input', "SELECT DECOMPRESS(N'abc') AS value"],
  ['decompress varchar gzip bytes', "SELECT DECOMPRESS(CAST(COMPRESS('abc') AS VARCHAR(MAX))) AS value"],
  ['decompress int input', "SELECT DECOMPRESS(1) AS value"],
  ['decompress binary padded', "SELECT DECOMPRESS(CAST(0x1f8b0800000000000003cb48cdc9c9070086a6103605000000 AS BINARY(30))) AS value"],
  ['decompress no arguments', "SELECT DECOMPRESS() AS value"],

  // Descriptors and storage types.
  ['describe', "EXEC sp_describe_first_result_set N'SELECT COMPRESS(''abc'') AS c, COMPRESS(N''abc'') AS n, COMPRESS(CAST(0x61 AS VARBINARY(MAX))) AS m, DECOMPRESS(0x) AS d, COMPRESS(CAST(NULL AS VARCHAR(1))) AS z'"],
  ['select into', "SELECT COMPRESS('abc') AS c, DECOMPRESS(COMPRESS('abc')) AS d, COMPRESS(CAST(NULL AS VARCHAR(1))) AS z INTO dbo.compress_into; SELECT c.name,t.name AS type_name,c.max_length,c.is_nullable FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('dbo.compress_into') ORDER BY c.column_id"],
  ['computed column persisted', "CREATE TABLE dbo.compress_computed(id INT NOT NULL CONSTRAINT pk_compress_computed PRIMARY KEY, v VARCHAR(20) NULL, c AS COMPRESS(v) PERSISTED)"],
  ['computed column nonpersisted', "CREATE TABLE dbo.compress_computed_np(id INT NOT NULL CONSTRAINT pk_compress_computed_np PRIMARY KEY, v VARCHAR(20) NULL, c AS COMPRESS(v), d AS DECOMPRESS(COMPRESS(v))); SELECT name, COLUMNPROPERTY(object_id,name,'IsDeterministic') AS deterministic, COLUMNPROPERTY(object_id,name,'IsPrecise') AS precise FROM sys.columns WHERE object_id=OBJECT_ID('dbo.compress_computed_np') AND name IN ('c','d') ORDER BY column_id"],

  // Column sources.
  ['column compress', "SELECT id, COMPRESS(a) AS a, COMPRESS(n) AS n, COMPRESS(b) AS b, COMPRESS(u8) AS u8 FROM dbo.compress_src ORDER BY id"],
  ['column round trip', "SELECT id, CAST(DECOMPRESS(COMPRESS(a)) AS VARCHAR(MAX)) AS a, CAST(DECOMPRESS(COMPRESS(n)) AS NVARCHAR(MAX)) AS n, DECOMPRESS(COMPRESS(b)) AS b, DATALENGTH(COMPRESS(am)) AS am_compressed, CASE WHEN CAST(DECOMPRESS(COMPRESS(am)) AS VARCHAR(MAX))=am THEN 1 ELSE 0 END AS am_round_trip FROM dbo.compress_src ORDER BY id"],
  ['column stored compressed', "SELECT id, COMPRESS(n) AS z INTO dbo.compress_stored FROM dbo.compress_src; SELECT id, CAST(DECOMPRESS(z) AS NVARCHAR(MAX)) AS value FROM dbo.compress_stored ORDER BY id"],

  // Large inputs; values beyond FULL_VALUE_LIMIT are retained as digests.
  ['large repetitive varchar summary', summary("REPLICATE(CAST('a' AS VARCHAR(MAX)),100000)", 'VARCHAR(MAX)')],
  ['large repetitive varchar value', "SELECT COMPRESS(REPLICATE(CAST('a' AS VARCHAR(MAX)),100000)) AS value"],
  ['large text varchar summary', summary("REPLICATE(CAST('The quick brown fox jumps over the lazy dog. ' AS VARCHAR(MAX)),20000)", 'VARCHAR(MAX)')],
  ['large text nvarchar summary', summary("REPLICATE(CAST(N'The quick brown fox jumps over the lazy dog. ' AS NVARCHAR(MAX)),20000)", 'NVARCHAR(MAX)')],
  ['large text nvarchar value', "SELECT COMPRESS(REPLICATE(CAST(N'The quick brown fox jumps over the lazy dog. ' AS NVARCHAR(MAX)),20000)) AS value"],
  ['large noise summary', `${noise(2048)} ${summary('@noise')}`],
  ['large noise value', `${noise(2048)} SELECT COMPRESS(@noise) AS value`],
  ['large noise round trip value', `${noise(2048)} SELECT DECOMPRESS(COMPRESS(@noise)) AS value`],
  ['large 1 MB repetitive summary', summary("REPLICATE(CAST('0123456789abcdef' AS VARCHAR(MAX)),65536)", 'VARCHAR(MAX)')],
  ['large 8 MB zeros summary', summary("CAST(REPLICATE(CAST(CHAR(0) AS VARCHAR(MAX)),8388608) AS VARBINARY(MAX))")],
  ['large round trip nvarchar value', "SELECT CAST(DECOMPRESS(COMPRESS(REPLICATE(CAST(N'xyz' AS NVARCHAR(MAX)),10000))) AS NVARCHAR(MAX)) AS value"],
]

for (const [name, bytes] of Object.entries(gzip)) {
  cases.push([`decompress foreign ${name}`, `SELECT DECOMPRESS(${bytes}) AS value, CAST(DECOMPRESS(${bytes}) AS VARCHAR(MAX)) AS text`])
}

function rpc(connection, sql, parameters) {
  const transport = {
    on: (...a) => connection.on(...a),
    off: (...a) => connection.off(...a),
    execSqlBatch(request) {
      for (const [name, type, value, options] of parameters) request.addParameter(name, type, value, options)
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

// Deterministic client-generated large inputs for RPC and prepared calls.
const repeated = (text, count) => text.repeat(count)
const clientNoise = bytes => {
  const chunks = []
  for (let i = 0; chunks.length * 32 < bytes; i++) {
    const b = Buffer.alloc(4); b.writeInt32BE(i)
    chunks.push(createHash('sha256').update(b).digest())
  }
  return Buffer.concat(chunks).subarray(0, bytes)
}
const gzipHello = Buffer.from('1f8b0800000000000003cb48cdc9c9070086a6103605000000', 'hex')
const gzip42 = Buffer.from('1f8b08000000000002033331020088b0243202000000', 'hex')

const rpcCases = [
  ['rpc compress varchar', 'SELECT COMPRESS(@v) AS value /*rpc varchar*/', [['v', TYPES.VarChar, 'abc', { length: 20 }]]],
  ['rpc compress nvarchar', 'SELECT COMPRESS(@v) AS value /*rpc nvarchar*/', [['v', TYPES.NVarChar, 'abc', { length: 20 }]]],
  ['rpc compress varbinary', 'SELECT COMPRESS(@v) AS value /*rpc varbinary*/', [['v', TYPES.VarBinary, Buffer.from('abc'), { length: 20 }]]],
  ['rpc compress nvarchar max', 'SELECT COMPRESS(@v) AS value /*rpc nvarchar max*/', [['v', TYPES.NVarChar, 'abc', { length: Infinity }]]],
  ['rpc compress varbinary max', 'SELECT COMPRESS(@v) AS value /*rpc varbinary max*/', [['v', TYPES.VarBinary, Buffer.from('abc'), { length: Infinity }]]],
  ['rpc compress empty', 'SELECT COMPRESS(@v) AS value /*rpc empty*/', [['v', TYPES.VarChar, '', { length: 20 }]]],
  ['rpc compress null', 'SELECT COMPRESS(@v) AS value /*rpc null*/', [['v', TYPES.NVarChar, null, { length: 20 }]]],
  ['rpc compress int', 'SELECT COMPRESS(@v) AS value /*rpc int*/', [['v', TYPES.Int, 1]]],
  ['rpc compress large text', 'SELECT COMPRESS(@v) AS value, DATALENGTH(@v) AS input_bytes /*rpc large text*/', [['v', TYPES.VarChar, repeated('lorem ipsum ', 10000), { length: Infinity }]]],
  ['rpc compress large noise', 'SELECT COMPRESS(@v) AS value, DATALENGTH(@v) AS input_bytes, HASHBYTES(\'SHA2_256\',@v) AS input_sha256 /*rpc large noise*/', [['v', TYPES.VarBinary, clientNoise(65536), { length: Infinity }]]],
  ['rpc decompress valid', 'SELECT DECOMPRESS(@v) AS value /*rpc decompress valid*/', [['v', TYPES.VarBinary, gzipHello, { length: 100 }]]],
  ['rpc decompress invalid', 'SELECT DECOMPRESS(@v) AS value /*rpc decompress invalid*/', [['v', TYPES.VarBinary, Buffer.from('abc'), { length: 100 }]]],
  ['rpc decompress null', 'SELECT DECOMPRESS(@v) AS value /*rpc decompress null*/', [['v', TYPES.VarBinary, null, { length: 100 }]]],
  ['rpc round trip nvarchar', 'SELECT CAST(DECOMPRESS(COMPRESS(@v)) AS NVARCHAR(MAX)) AS value /*rpc round trip*/', [['v', TYPES.NVarChar, 'café \u{1F600}', { length: 40 }]]],
]

// Replicates the owner-authored capturePrepared semantics of PR #312
// (scripts/lib/reference.mjs on work/prepared-capture-helper-v1), which is not
// on main yet: one reusable Request; sp_prepare completion via the 'prepared'
// or 'error' event; request.error cleared before each phase; only tokens raised
// while a phase is outstanding recorded; a callback error recorded only when
// the phase had no server error and it was not attributed earlier; bounded
// rows and messages per phase.
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
  for (const [name, type, options] of declarations) request.addParameter(name, type, undefined, options)
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

// Each prepared statement has unique text so it never shares a plan with the
// batch or RPC cases.
const preparedCases = [
  ['prepared compress nvarchar', 'SELECT COMPRESS(@v) AS c, CAST(DECOMPRESS(COMPRESS(@v)) AS NVARCHAR(MAX)) AS r /*prepared compress nvarchar*/',
    [['v', TYPES.NVarChar, { length: 4000 }]],
    [{ v: 'abc' }, { v: '' }, { v: null }, { v: repeated('xy', 1500) }, { v: 'abc' }]],
  ['prepared compress varbinary max', 'SELECT COMPRESS(@v) AS c, DATALENGTH(COMPRESS(@v)) AS n /*prepared compress varbinary max*/',
    [['v', TYPES.VarBinary, { length: Infinity }]],
    [{ v: Buffer.from('abc') }, { v: Buffer.alloc(0) }, { v: clientNoise(20000) }, { v: null }]],
  ['prepared decompress', 'SELECT DECOMPRESS(@v) AS d, CAST(DECOMPRESS(@v) AS VARCHAR(MAX)) AS t /*prepared decompress*/',
    [['v', TYPES.VarBinary, { length: 8000 }]],
    [{ v: gzipHello }, { v: Buffer.from('abc') }, { v: null }, { v: gzipHello.subarray(0, 12) }, { v: gzipHello }]],
  ['prepared decompress int conversion', 'SELECT CAST(CAST(DECOMPRESS(@v) AS VARCHAR(MAX)) AS INT) AS i /*prepared decompress int*/',
    [['v', TYPES.VarBinary, { length: 8000 }]],
    [{ v: gzip42 }, { v: gzipHello }, { v: gzip42 }]],
]

const describeValue = value => {
  const bounded = boundValue(value)
  if (bounded !== value) return bounded
  return canonical(value)
}
const describeParameter = ([name, type, value, options]) => ({
  name, type: type.name, value: describeValue(value),
  ...(options ? { options: { ...options, ...(options.length === Infinity ? { length: 'max' } : {}) } } : {}),
})

async function observe(connection) {
  const records = []
  for (const [name, sql] of [...setup, ...cases]) {
    records.push({ name, sql, result: canonical(boundRows(await capture(connection, sql))) })
  }
  for (const [name, sql, parameters] of rpcCases) {
    records.push({ name, protocol: 'sp_executesql', sql, parameters: parameters.map(describeParameter), result: canonical(boundRows(await rpc(connection, sql, parameters))) })
  }
  for (const [name, sql, declarations, valueSets] of preparedCases) {
    const outcome = await capturePrepared(connection, sql, declarations, valueSets)
    const phase = result => canonical(boundRows(result))
    records.push({
      name, protocol: 'sp_prepare/sp_execute/sp_unprepare', sql,
      declarations: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, options: { ...options, ...(options.length === Infinity ? { length: 'max' } : {}) } })),
      prepare: phase(outcome.prepare),
      prepared: outcome.prepared,
      executions: outcome.executions.map(({ values, result }) => ({
        values: Object.fromEntries(Object.entries(values).map(([k, v]) => [k, describeValue(v)])),
        result: phase(result),
      })),
      unprepare: outcome.unprepare && phase(outcome.unprepare),
    })
  }
  return records
}

// Targeted checks on individual values; never pass whole captures to assert.
function validate(run) {
  assert.equal(run.length, setup.length + cases.length + rpcCases.length + preparedCases.length)
  const get = name => {
    const found = run.find(record => record.name === name)
    assert(found, name + ': missing record')
    return found
  }
  const cell = (name, column = 0) => get(name).result.sets[0]?.rows[0]?.[column]
  const hex = value => value?.kind === 'binary' ? value.value : undefined
  for (const record of run) {
    if (record.result) assert(record.result.done.length > 0, record.name + ': no completion')
    else for (const execution of record.executions) assert(execution.result.done.length > 0, record.name + ': no completion')
  }
  for (const record of run) {
    if (!record.name.startsWith('compress ') || record.result.errors.length) continue
    const value = hex(record.result.sets[0]?.rows[0]?.[0])
    if (value === undefined || value === '') continue
    assert(value.startsWith('1f8b0800000000000400'), record.name + ': unexpected GZIP header')
    gunzipSync(Buffer.from(value, 'hex'))
  }
  assert.equal(Buffer.from(gunzipSync(Buffer.from(hex(cell('compress varchar')), 'hex'))).toString('latin1'), 'abc')
  assert.equal(gunzipSync(Buffer.from(hex(cell('compress nvarchar')), 'hex')).toString('hex'), '610062006300')
  assert.equal(hex(cell('compress varchar')), hex(cell('compress varbinary')))
  assert.equal(cell('round trip varchar max'), 'abc')
  assert.equal(cell('round trip nvarchar max'), 'abc')
  assert.equal(cell('compress typed null varchar'), null)
  assert.equal(hex(cell('decompress compress varchar')), '616263')
  assert.equal(cell('large noise summary', 4), 1)
  assert.equal(hex(cell('compress empty varchar')), '')
  assert.equal(cell('decompress foreign empty member (empty)'), null)
  assert.equal(hex(cell('decompress empty')), '')
  for (const [name, number] of [
    ['compress int', 8116], ['compress xml', 8116], ['compress no arguments', 174],
    ['decompress varchar input', 8116], ['decompress foreign wrong crc32', 9826],
    ['decompress foreign zlib wrapper', 9826], ['computed column persisted', 4936],
  ]) assert.equal(get(name).result.errors[0]?.number, number, name)
  for (const name of ['compress varchar', 'decompress compress varchar']) {
    assert.equal(get(name).result.sets[0].columns[0].type, 'VarBinary', name)
  }
}

// Replicates refuseFixtureOutput from owner PR #312 (not on main yet): resolve
// both paths through realpath, including symlinked directories and a
// not-yet-existing file, and refuse output that aliases the retained fixture.
async function canonicalPath(path) {
  const absolute = resolve(path instanceof URL ? fileURLToPath(path) : path)
  try { return await realpath(absolute) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return resolve(await canonicalPath(dirname(absolute)), basename(absolute))
  }
}
async function refuseFixtureOutput(path, retainedFixture) {
  if (await canonicalPath(path) === await canonicalPath(retainedFixture)) throw new Error('refusing to write capture output over retained fixture ' + fileURLToPath(retainedFixture))
}

let containers
let retained
if (checkFixture) {
  // Bounded offline check of the retained fixture: validate every run and
  // require all four to match, without starting a container.
  retained = JSON.parse(await readFile(fixture, 'utf8'))
  containers = retained.containers
  assert.equal(containers.length, 2)
  for (const container of containers) {
    assert.equal(container.runs.length, 2)
    for (const run of container.runs) validate(run)
    assertSameCapture(container.runs[0], container.runs[1], 'retained runs differ across fresh databases')
  }
  assertSameCapture(containers[0].runs[0], containers[1].runs[0], 'retained runs differ across containers')
  console.log(`Retained fixture validated: ${containers[0].runs[0].length} COMPRESS/DECOMPRESS observations in four matching captures`)
} else {
  await refuseFixtureOutput(output, fixture)
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(resolve(output, '..'), { recursive: true })
  containers = []
  const count = oneDatabase ? 1 : 2
  for (let containerIndex = 0; containerIndex < count; containerIndex++) {
    await withReferenceContainer(async (config, container) => {
      console.error(`started owned container ${container.name}`)
      const runs = []
      for (let databaseIndex = 0; databaseIndex < count; databaseIndex++) {
        const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 300000 } }, observe)
        if (!oneDatabase) validate(run)
        runs.push(run)
      }
      if (!oneDatabase) assertSameCapture(runs[0], runs[1], 'COMPRESS/DECOMPRESS observations differ across fresh databases')
      containers.push({ image: container.image, runs })
    })
  }
  if (!oneDatabase) assertSameCapture(containers[0].runs[0], containers[1].runs[0], 'COMPRESS/DECOMPRESS observations differ across containers')
  const actual = { containers }
  await writeFile(output, JSON.stringify(actual) + '\n')
  if (!writeFixture && !oneDatabase) {
    try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    if (retained) assertSameCapture(actual, retained, 'COMPRESS/DECOMPRESS observations differ from retained fixture')
  }
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${containers[0].runs[0].length} COMPRESS/DECOMPRESS observations` +
    (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
    (retained ? ' and matched retained fixture' : ''))
}
