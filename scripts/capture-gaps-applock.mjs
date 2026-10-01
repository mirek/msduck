#!/usr/bin/env node
// SQL Server evidence for application locks (issue #713): sp_getapplock,
// sp_releaseapplock, APPLOCK_MODE and APPLOCK_TEST, including their
// diagnostics, completion tokens and cross-session contention.
//
// Usage:
//   capture-gaps-applock.mjs [output]          capture two fresh pinned containers
//   capture-gaps-applock.mjs --write-fixture   also write reference/gaps-applock.json
//   capture-gaps-applock.mjs --check           validate the retained fixture
//   capture-gaps-applock.mjs --compare PORT    run the observations against a local
//                                              msduck server and diff them with the fixture
//
// Timings are reduced to comparisons, so captures are stable.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { isDeepStrictEqual } from 'node:util'
import { Connection, Request } from 'tedious'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { assertSameCapture, describeFirstDifference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/gaps-applock.json', import.meta.url)
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const compareAt = args.indexOf('--compare')
const comparePort = compareAt >= 0 ? Number(args[compareAt + 1]) : null
const paths = args.filter((arg, i) => !arg.startsWith('--') && !(compareAt >= 0 && i === compareAt + 1))
const output = resolve(paths[0] ?? 'artifacts/compatibility/gaps-applock-reference/capture.json')

// Completion, RETURNSTATUS and diagnostic tokens are recorded in stream order
// before tedious parses them. The parser's options object belongs to one
// connection, so concurrent connections keep separate sinks.
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const recorded = new Map([[0xFD, 'done'], [0xFE, 'doneProc'], [0xFF, 'doneInProc'], [0x79, 'returnStatus'], [0xAA, 'error'], [0xAB, 'info'], [0x81, 'metadata'], [0xD1, 'row']])
const sinks = new WeakMap()
const readToken = StreamParser.prototype.readToken
StreamParser.prototype.readToken = function (type) {
  return recorded.has(type) ? record(this, type) : readToken.call(this, type)
}
function record(parser, type) {
  const at = parser.position
  const kind = recorded.get(type)
  const need = kind === 'returnStatus' ? 4 : kind.startsWith('done') ? 12 : 0
  if (parser.buffer.length < at + need) return parser.waitForChunk().then(() => record(parser, type))
  const sink = sinks.get(parser.options)
  if (sink) {
    if (kind.startsWith('done')) sink.push([kind, parser.buffer.readUInt16LE(at), parser.buffer.readUInt16LE(at + 2), Number(parser.buffer.readBigUInt64LE(at + 4))])
    else if (kind === 'returnStatus') sink.push([kind, parser.buffer.readInt32LE(at)])
    else sink.push([kind])
  }
  return readToken.call(parser, type)
}

function connect(config) {
  return new Promise((resolve, reject) => {
    const connection = new Connection(config)
    connection.on('error', () => {})
    connection.connect(error => error ? reject(error) : resolve(connection))
  })
}
const close = connection => new Promise(resolve => {
  if (connection.closed) return resolve()
  connection.once('end', resolve)
  connection.close()
})

// One batch: descriptors, rows, diagnostics (with procedure and line) and the
// raw token sequence. `elapsed` is returned separately and never recorded.
function run(connection, sql) {
  return new Promise(resolve => {
    const result = { sets: [], messages: [], tokens: [] }
    sinks.set(connection.config.options, result.tokens)
    const diagnostic = kind => m => result.messages.push({ kind, number: m.number, state: m.state, class: m.class, procName: m.procName, lineNumber: m.lineNumber, message: m.message })
    const onError = diagnostic('error')
    const onInfo = diagnostic('info')
    const started = Date.now()
    const request = new Request(sql, error => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      sinks.delete(connection.config.options)
      if (error && !result.messages.some(m => m.kind === 'error')) result.transport = error.message
      resolve({ result, elapsed: Date.now() - started })
    })
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    request.on('columnMetadata', metadata => result.sets.push({
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, nullable: (c.flags & 1) === 1 })), rows: [],
    }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => c.value)))
    connection.execSqlBatch(request)
  })
}

const scalar = r => r.result.sets.at(-1)?.rows[0]?.[0]
const get = (resource, mode, owner = 'Session', timeout = 0, extra = '') =>
  `DECLARE @rc int; EXEC @rc = sp_getapplock @Resource = N'${resource}', @LockMode = '${mode}', @LockOwner = '${owner}', @LockTimeout = ${timeout}${extra}; SELECT @rc AS rc`
const release = (resource, owner = 'Session', extra = '') =>
  `DECLARE @rc int; EXEC @rc = sp_releaseapplock @Resource = N'${resource}', @LockOwner = '${owner}'${extra}; SELECT @rc AS rc`
const modes = ['IntentShared', 'Shared', 'Update', 'IntentExclusive', 'Exclusive']

// Single-session batches: argument forms, diagnostics and token streams.
const batches = [
  ['repro', `EXEC sp_getapplock @Resource=N'foo', @LockMode='Exclusive', @LockOwner='Session', @LockTimeout=0;`],
  ['repro mode', `SELECT APPLOCK_MODE('public', 'foo', 'Session') AS m, APPLOCK_TEST('public', 'foo', 'Exclusive', 'Session') AS t`],
  ['positional', `DECLARE @rc int; EXEC @rc = sp_getapplock N'foo', 'Exclusive', 'Session', 0; SELECT @rc AS rc, APPLOCK_MODE('public', 'foo', 'Session') AS m`],
  ['release counted 1', `DECLARE @rc int; EXEC @rc = sp_releaseapplock N'foo', 'Session'; SELECT @rc AS rc, APPLOCK_MODE('public', 'foo', 'Session') AS m`],
  ['release counted 2', `DECLARE @rc int; EXEC @rc = sp_releaseapplock N'foo', 'Session'; SELECT @rc AS rc, APPLOCK_MODE('public', 'foo', 'Session') AS m`],
  ['release not held', `DECLARE @rc int; EXEC @rc = sp_releaseapplock N'foo', 'Session'; SELECT @rc AS rc, @@ERROR AS e, @@ROWCOUNT AS c`],
  ['release not held bare', `EXEC sp_releaseapplock @Resource = N'foo', @LockOwner = 'Session'`],
  ['get release tokens', `EXEC sp_getapplock N'foo', 'Shared', 'Session', 0; SELECT @@ROWCOUNT AS c; EXEC sp_releaseapplock N'foo', 'Session'`],
  ['default timeout tokens', `EXEC sp_getapplock N'foo', 'Shared', 'Session'; EXEC sp_releaseapplock N'foo', 'Session'`],
  ['default keyword', `EXEC sp_getapplock N'foo', 'Shared', 'Session', DEFAULT, DEFAULT; EXEC sp_releaseapplock N'foo', 'Session', DEFAULT`],
  ['lowercase names and values', `EXEC sp_getapplock @resource = N'foo', @lockmode = 'shared ', @lockowner = 'session'; EXEC sp_releaseapplock @RESOURCE = N'foo', @LOCKOWNER = 'SESSION'`],
  ['qualified names', `EXEC dbo.sp_getapplock N'foo', 'Shared', 'Session'; EXEC sys.sp_releaseapplock N'foo', 'Session'`],
  ['variable arguments', `DECLARE @r nvarchar(300) = N'foo', @m varchar(20) = 'Update', @o varchar(20) = 'Session', @t int = 0, @rc int; EXEC @rc = sp_getapplock @r, @m, @o, @t; SELECT @rc AS rc, APPLOCK_MODE('public', @r, @o) AS m; EXEC @rc = sp_releaseapplock @r, @o; SELECT @rc AS rc`],
  ['string timeout', `DECLARE @rc int; EXEC @rc = sp_getapplock N'foo', 'Shared', 'Session', '0'; SELECT @rc AS rc; EXEC sp_releaseapplock N'foo', 'Session'`],
  ['return into varchar', `DECLARE @rc varchar(10); EXEC @rc = sp_getapplock N'foo', 'Shared', 'Session', 0; SELECT @rc AS rc; EXEC sp_releaseapplock N'foo', 'Session'`],
  ['nocount tokens', `SET NOCOUNT ON; EXEC sp_getapplock N'n', 'Shared', 'Session', 0; EXEC sp_releaseapplock N'n', 'Session'; EXEC sp_releaseapplock N'n', 'Session'; EXEC sp_getapplock N'n', 'Bogus', 'Session'; SET NOCOUNT OFF`],
  ['transaction owner outside transaction', `DECLARE @rc int; EXEC @rc = sp_getapplock @Resource = N'foo', @LockMode = 'Exclusive'; SELECT @rc AS rc, @@ERROR AS e`],
  ['transaction owner outside transaction bare', `EXEC sp_getapplock N'foo', 'Shared'`],
  ['invalid mode', `DECLARE @rc int; EXEC @rc = sp_getapplock N'foo', 'Bogus', 'Session'; SELECT @rc AS rc, @@ERROR AS e`],
  ['invalid mode bare', `EXEC sp_getapplock N'foo', 'Bogus', 'Session'`],
  ['null mode', `EXEC sp_getapplock N'foo', NULL, 'Session'`],
  ['invalid owner', `EXEC sp_getapplock N'foo', 'Shared', 'Bogus'`],
  ['null owner', `EXEC sp_getapplock N'foo', 'Shared', NULL`],
  ['release invalid owner', `EXEC sp_releaseapplock N'foo', 'Bogus'`],
  ['release transaction outside transaction', `EXEC sp_releaseapplock N'foo'`],
  ['null resource', `EXEC sp_getapplock NULL, 'Shared', 'Session'`],
  ['missing resource', `EXEC sp_getapplock @LockMode = 'Shared', @LockOwner = 'Session'`],
  ['release null resource', `EXEC sp_releaseapplock NULL, 'Session'`],
  ['invalid timeout', `EXEC sp_getapplock N'foo', 'Shared', 'Session', -2`],
  ['invalid timeout before resource and principal', `EXEC sp_getapplock NULL, 'Shared', 'Session', -2, 'nobody'`],
  ['resource before principal', `EXEC sp_getapplock NULL, 'Shared', 'Session', 0, 'nobody'`],
  ['unknown principal', `DECLARE @rc int; EXEC @rc = sp_getapplock N'foo', 'Shared', 'Session', 0, 'nobody'; SELECT @rc AS rc`],
  ['empty principal', `EXEC sp_getapplock N'foo', 'Shared', 'Session', 0, ''`],
  ['null principal', `EXEC sp_getapplock N'foo', 'Shared', 'Session', 0, NULL`],
  ['release unknown principal', `EXEC sp_releaseapplock N'foo', 'Session', 'nobody'`],
  ['fixed principals', `DECLARE @rc int, @all int = 0; EXEC @rc = sp_getapplock N'p', 'Shared', 'Session', 0, 'dbo'; SET @all += @rc; EXEC @rc = sp_getapplock N'p', 'Shared', 'Session', 0, 'guest'; SET @all += @rc; EXEC @rc = sp_getapplock N'p', 'Shared', 'Session', 0, 'sys'; SET @all += @rc; EXEC @rc = sp_getapplock N'p', 'Shared', 'Session', 0, 'INFORMATION_SCHEMA'; SET @all += @rc; EXEC @rc = sp_getapplock N'p', 'Shared', 'Session', 0, 'db_owner'; SET @all += @rc; EXEC @rc = sp_getapplock N'p', 'Shared', 'Session', 0, 'DB_DenyDataReader'; SET @all += @rc; SELECT @all AS total, APPLOCK_MODE('DBO', 'p', 'Session') AS dbo, APPLOCK_MODE('public', 'p', 'Session') AS pub`],
  ['principals are separate', `DECLARE @rc int; EXEC @rc = sp_releaseapplock N'p', 'Session', 'dbo'; EXEC @rc = sp_releaseapplock N'p', 'Session', 'guest'; EXEC @rc = sp_releaseapplock N'p', 'Session', 'sys'; EXEC @rc = sp_releaseapplock N'p', 'Session', 'information_schema'; EXEC @rc = sp_releaseapplock N'p', 'Session', 'db_owner'; EXEC @rc = sp_releaseapplock N'p', 'Session', 'db_denydatareader'; SELECT @rc AS rc, APPLOCK_MODE('dbo', 'p', 'Session') AS m`],
  ['unknown parameter', `EXEC sp_getapplock @Resource = N'foo', @LockMode = 'Shared', @LockOwner = 'Session', @Bogus = 1`],
  ['positional after named', `EXEC sp_getapplock @Resource = N'foo', 'Shared'`],
  ['too many arguments', `EXEC sp_getapplock N'foo', 'Shared', 'Session', 0, 'public', 7`],
  ['missing mode', `EXEC sp_getapplock N'foo'`],
  ['invalid timeout text', `EXEC sp_getapplock N'foo', 'Shared', 'Session', 'abc'`],
  ['error in try', `BEGIN TRY EXEC sp_releaseapplock N'foo', 'Session'; SELECT 'after' AS a END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n, ERROR_PROCEDURE() AS p, ERROR_SEVERITY() AS s, ERROR_STATE() AS st END CATCH`],
  ['info in try', `BEGIN TRY EXEC sp_getapplock N'foo', 'Bogus', 'Session'; SELECT 'after' AS a END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n END CATCH`],
  ['transaction lock', `BEGIN TRAN; DECLARE @rc int; EXEC @rc = sp_getapplock N't', 'Shared'; SELECT @rc AS rc, APPLOCK_MODE('public', 't', 'Transaction') AS t, APPLOCK_MODE('public', 't', 'Session') AS s, APPLOCK_MODE('public', 't', NULL) AS d; EXEC @rc = sp_getapplock N't', 'Exclusive', 'Session', 0; SELECT @rc AS rc`],
  ['nested commit keeps transaction lock', `BEGIN TRAN; DECLARE @rc int; EXEC @rc = sp_getapplock N't', 'Shared'; COMMIT; SELECT APPLOCK_MODE('public', 't', 'Transaction') AS t, @@TRANCOUNT AS c`],
  ['transaction release counted', `DECLARE @rc int; EXEC @rc = sp_releaseapplock N't'; SELECT @rc AS rc, APPLOCK_MODE('public', 't', 'Transaction') AS m; EXEC @rc = sp_releaseapplock N't'; SELECT @rc AS rc, APPLOCK_MODE('public', 't', 'Transaction') AS m; EXEC @rc = sp_getapplock N't', 'Update'; COMMIT; SELECT @@TRANCOUNT AS c, APPLOCK_MODE('public', 't', 'Session') AS s`],
  ['release session lock', release('t')],
  ['rollback releases transaction lock', `BEGIN TRAN; DECLARE @rc int; EXEC @rc = sp_getapplock N'r', 'Exclusive'; ROLLBACK; BEGIN TRAN; SELECT APPLOCK_MODE('public', 'r', 'Transaction') AS m; ROLLBACK`],
  ['mode outside transaction', `SELECT APPLOCK_MODE('public', 'foo', 'Transaction') AS m`],
  ['test outside transaction', `SELECT APPLOCK_TEST('public', 'foo', 'Exclusive', 'Transaction') AS t`],
  ['mode null owner outside transaction', `SELECT APPLOCK_MODE('public', 'foo', NULL) AS m`],
  ['mode invalid owner', `SELECT APPLOCK_MODE('public', 'foo', 'Bogus') AS m`],
  ['test invalid owner', `SELECT APPLOCK_TEST('public', 'foo', 'Shared', 'Bogus') AS t`],
  ['test invalid mode', `SELECT APPLOCK_TEST('public', 'foo', 'Bogus', 'Session') AS t`],
  ['test union mode', `SELECT APPLOCK_TEST('public', 'foo', 'SharedIntentExclusive', 'Session') AS t`],
  ['mode unknown principal', `SELECT APPLOCK_MODE('nobody', 'foo', 'Session') AS m`],
  ['test unknown principal', `SELECT APPLOCK_TEST('nobody', 'foo', 'Shared', 'Session') AS t`],
  ['mode null literal principal', `SELECT APPLOCK_MODE(NULL, 'foo', 'Session') AS m`],
  ['mode null literal resource', `SELECT APPLOCK_MODE('public', NULL, 'Session') AS m`],
  ['test null literal mode', `SELECT APPLOCK_TEST('public', 'foo', NULL, 'Session') AS t`],
  ['mode int resource', `SELECT APPLOCK_MODE('public', 1, 'Session') AS m`],
  ['mode null variable resource', `DECLARE @v nvarchar(10) = NULL; SELECT APPLOCK_MODE('public', @v, 'Session') AS m`],
  ['mode null variable principal', `DECLARE @v nvarchar(10) = NULL; SELECT APPLOCK_MODE(@v, 'foo', 'Session') AS m`],
  ['test null variable mode', `DECLARE @v nvarchar(10) = NULL; SELECT APPLOCK_TEST('public', 'foo', @v, 'Session') AS t`],
  ['test null variable resource', `DECLARE @v nvarchar(10) = NULL; SELECT APPLOCK_TEST('public', @v, 'Shared', 'Session') AS t`],
  ['test null variable principal', `DECLARE @v nvarchar(10) = NULL; SELECT APPLOCK_TEST(@v, 'foo', 'Shared', 'Session') AS t`],
  ['function descriptors', `SELECT APPLOCK_MODE('public', 'foo', 'session ') AS m, APPLOCK_TEST('public', 'foo', 'exclusive', 'SESSION') AS t, APPLOCK_MODE('public', 'foo', 'Session') + N'x' AS mx, APPLOCK_TEST('public', 'foo', 'Shared', 'Session') + 1 AS tx`],
  ['function declarations', `SELECT name, system_type_name, max_length, is_nullable FROM sys.dm_exec_describe_first_result_set(N'SELECT APPLOCK_MODE(''public'', ''foo'', ''Session'') AS m, APPLOCK_TEST(''public'', ''foo'', ''Shared'', ''Session'') AS t', NULL, 0) ORDER BY column_ordinal`],
  ['function in condition', `IF APPLOCK_MODE('public', 'foo', 'Session') = 'NoLock' SELECT 1 AS free ELSE SELECT 0 AS free`],
  ['resource is case and space sensitive', `DECLARE @rc int; EXEC @rc = sp_getapplock N'Case', 'Exclusive', 'Session', 0; SELECT APPLOCK_MODE('public', 'case', 'Session') AS lower, APPLOCK_MODE('public', 'Case ', 'Session') AS spaced, APPLOCK_MODE('public', 'Case', 'Session') AS exact; EXEC @rc = sp_releaseapplock N'Case', 'Session'`],
  ['empty resource', `DECLARE @rc int; EXEC @rc = sp_getapplock N'', 'Shared', 'Session'; SELECT @rc AS rc, APPLOCK_MODE('public', '', 'Session') AS m; EXEC @rc = sp_releaseapplock N'', 'Session'; SELECT @rc AS rc`],
  ['long resource truncated', `DECLARE @rc int, @long nvarchar(300) = REPLICATE(N'a', 255) + N'b'; EXEC @rc = sp_getapplock @long, 'Exclusive', 'Session', 0; SELECT @rc AS rc, APPLOCK_MODE('public', REPLICATE(N'a', 255), 'Session') AS short, APPLOCK_MODE('public', @long, 'Session') AS long; EXEC @rc = sp_releaseapplock @long, 'Session'; SELECT @rc AS rc`],
  ['long resource release message', `DECLARE @long nvarchar(300) = REPLICATE(N'a', 255) + N'b'; EXEC sp_releaseapplock @long, 'Session'`],
]

// Unions of two and three requests by one owner.
for (const a of modes) for (const b of modes) {
  batches.push([`union ${a} ${b}`, `DECLARE @rc int; EXEC @rc = sp_getapplock N'u', '${a}', 'Session'; EXEC @rc = sp_getapplock N'u', '${b}', 'Session'; SELECT APPLOCK_MODE('public', 'u', 'Session') AS m; EXEC @rc = sp_releaseapplock N'u', 'Session'; SELECT APPLOCK_MODE('public', 'u', 'Session') AS m; EXEC @rc = sp_releaseapplock N'u', 'Session'`])
}
batches.push(['union Shared IntentExclusive Update', `DECLARE @rc int; EXEC @rc = sp_getapplock N'u', 'Shared', 'Session'; EXEC @rc = sp_getapplock N'u', 'IntentExclusive', 'Session'; EXEC @rc = sp_getapplock N'u', 'Update', 'Session'; SELECT APPLOCK_MODE('public', 'u', 'Session') AS m; EXEC @rc = sp_releaseapplock N'u', 'Session'; EXEC @rc = sp_releaseapplock N'u', 'Session'; EXEC @rc = sp_releaseapplock N'u', 'Session'`])

async function observe(config, database) {
  const options = { ...config.options, database }
  const open = () => connect({ ...config, options })
  const observations = []
  const note = (name, value) => observations.push({ name, ...value })
  const a = await open(), b = await open(), c = await open()
  try {
    for (const [name, sql] of batches) note(name, (await run(a, sql)).result)
    for (const name of ['IF @@TRANCOUNT > 0 ROLLBACK']) await run(a, name)

    // Compatibility: A holds a (possibly converted) mode, B requests each mode.
    const held = { IntentShared: ['IntentShared'], Shared: ['Shared'], Update: ['Update'], IntentExclusive: ['IntentExclusive'], SharedIntentExclusive: ['Shared', 'IntentExclusive'], UpdateIntentExclusive: ['Update', 'IntentExclusive'], Exclusive: ['Exclusive'] }
    const matrix = {}
    for (const [mode, sequence] of Object.entries(held)) {
      for (const step of sequence) await run(a, get('m', step))
      matrix[mode] = { held: scalar(await run(a, `SELECT APPLOCK_MODE('public', 'm', 'Session')`)) }
      for (const request of modes) {
        const test = scalar(await run(b, `SELECT APPLOCK_TEST('public', 'm', '${request}', 'Session')`))
        const granted = scalar(await run(b, get('m', request)))
        if (granted === 0) await run(b, release('m'))
        matrix[mode][request] = [granted, test]
      }
      for (const _ of sequence) await run(a, release('m'))
    }
    note('compatibility', { matrix })

    // Contention, timeouts and waits.
    note('holder', (await run(a, get('foo', 'Exclusive'))).result)
    note('contended timeout 0', (await run(b, get('foo', 'Exclusive'))).result)
    let timed = await run(b, get('foo', 'Exclusive', 'Session', 400))
    note('contended timeout 400', { ...timed.result, waited: timed.elapsed >= 350 })
    note('contended view', (await run(b, `SELECT APPLOCK_TEST('public', 'foo', 'Exclusive', 'Session') AS x, APPLOCK_TEST('public', 'foo', 'IntentShared', 'Session') AS is_, APPLOCK_MODE('public', 'foo', 'Session') AS m`)).result)
    note('other session cannot release', (await run(b, release('foo'))).result)
    note('other principal is free', (await run(b, `${get('foo', 'Exclusive', 'Session', 0, ", @DbPrincipal = 'dbo'")}; EXEC sp_releaseapplock N'foo', 'Session', 'dbo'`)).result)
    note('other case is free', (await run(b, `${get('FOO', 'Exclusive')}; EXEC sp_releaseapplock N'FOO', 'Session'`)).result)
    note('other database is free', (await run(c, `USE master; ${get('foo', 'Exclusive')}; EXEC sp_releaseapplock N'foo', 'Session'; USE [${database}]`)).result)
    let waiting = run(b, get('foo', 'Exclusive', 'Session', 10000))
    await delay(300)
    note('holder releases', (await run(a, release('foo'))).result)
    timed = await waiting
    note('waiter granted after wait', { ...timed.result, waited: timed.elapsed >= 200 })
    note('waiter releases', (await run(b, release('foo'))).result)

    // Session end releases session locks.
    note('closing holder', (await run(c, get('s', 'Exclusive'))).result)
    waiting = run(b, get('s', 'Exclusive', 'Session', 10000))
    await delay(300)
    await close(c)
    timed = await waiting
    note('granted when holder disconnects', { ...timed.result, waited: timed.elapsed >= 200 })
    await run(b, release('s'))

    // Transaction locks are released at the outermost COMMIT or ROLLBACK.
    await run(a, `BEGIN TRAN; ${get('t', 'Exclusive', 'Transaction')}`)
    note('transaction lock blocks', (await run(b, get('t', 'Shared'))).result)
    await run(a, 'BEGIN TRAN; COMMIT')
    note('inner commit keeps lock', (await run(b, get('t', 'Shared'))).result)
    waiting = run(b, get('t', 'Shared', 'Session', 10000))
    await delay(300)
    await run(a, 'ROLLBACK')
    timed = await waiting
    note('rollback grants waiter', { ...timed.result, waited: timed.elapsed >= 200 })
    await run(b, release('t'))

    // Same-session owners do not block each other.
    note('same session owners', (await run(a, `BEGIN TRAN; ${get('o', 'Exclusive', 'Transaction')}; EXEC sp_getapplock N'o', 'Exclusive', 'Session', 0; SELECT APPLOCK_MODE('public', 'o', 'Transaction') AS t, APPLOCK_MODE('public', 'o', 'Session') AS s; ROLLBACK; SELECT APPLOCK_MODE('public', 'o', 'Session') AS s`)).result)
    note('other session sees session lock', (await run(b, `SELECT APPLOCK_TEST('public', 'o', 'Shared', 'Session') AS t`)).result)
    await run(a, release('o'))

    // A queued request blocks later compatible requests.
    await run(a, get('q', 'Shared'))
    const c2 = await open()
    waiting = run(b, get('q', 'Exclusive', 'Session', 1500))
    await delay(300)
    note('queued behind waiter', (await run(c2, get('q', 'Shared'))).result)
    note('test sees waiter', (await run(c2, `SELECT APPLOCK_TEST('public', 'q', 'Shared', 'Session') AS t`)).result)
    note('waiter times out', (await waiting).result)
    note('compatible after timeout', (await run(c2, `${get('q', 'Shared')}; EXEC sp_releaseapplock N'q', 'Session'`)).result)
    await run(a, release('q'))

    // Conversion: both share, one converts.
    await run(a, get('cv', 'Shared'))
    await run(b, get('cv', 'Shared'))
    note('conversion timeout 0', (await run(a, `${get('cv', 'Exclusive')}; SELECT APPLOCK_MODE('public', 'cv', 'Session') AS m`)).result)
    waiting = run(a, get('cv', 'Exclusive', 'Session', 10000))
    await delay(300)
    note('new request behind conversion', (await run(c2, get('cv', 'Shared'))).result)
    await run(b, release('cv'))
    timed = await waiting
    note('conversion granted', { ...timed.result, waited: timed.elapsed >= 200 })
    note('converted lock counted', (await run(a, `SELECT APPLOCK_MODE('public', 'cv', 'Session') AS m; ${release('cv')}; SELECT APPLOCK_MODE('public', 'cv', 'Session') AS m; EXEC sp_releaseapplock N'cv', 'Session'; SELECT APPLOCK_MODE('public', 'cv', 'Session') AS m`)).result)

    // Deadlock: exactly one waiter is chosen as the victim and gets -3
    // without an error; the victim keeps its locks and its transaction. Which
    // waiter SQL Server's lock monitor picks is a cost decision, so the
    // victim is recorded by role, not by session.
    const deadlock = async (label, owner, setup = '') => {
      await run(a, `${setup}${get('d1', 'Exclusive', owner)}`)
      await run(b, `${setup}${get('d2', 'Exclusive', owner)}`)
      const suffix = owner === 'Transaction' ? ', @@TRANCOUNT AS c' : ''
      const waits = [run(a, `${get('d2', 'Exclusive', owner, 20000)}${suffix}`)]
      await delay(300)
      waits.push(run(b, `${get('d1', 'Exclusive', owner, 20000)}${suffix}`))
      const first = await Promise.race(waits.map((wait, index) => wait.then(() => index)))
      const victim = await waits[first]
      const survivorConnection = first === 0 ? b : a
      const victimConnection = first === 0 ? a : b
      await run(victimConnection, owner === 'Transaction' ? 'ROLLBACK' : `EXEC sp_releaseapplock N'${first === 0 ? 'd1' : 'd2'}', 'Session'`)
      note(`deadlock ${label} victim`, victim.result)
      note(`deadlock ${label} survivor`, (await waits[1 - first]).result)
      await run(survivorConnection, owner === 'Transaction' ? 'ROLLBACK' : `EXEC sp_releaseapplock N'd1', 'Session'; EXEC sp_releaseapplock N'd2', 'Session'`)
    }
    await deadlock('session', 'Session')
    await deadlock('transaction', 'Transaction', 'BEGIN TRAN; ')
    await close(c2)
  } finally {
    await Promise.all([a, b, c].map(close))
  }
  return observations
}

function validate(observations) {
  const find = name => {
    const found = observations.find(o => o.name === name)
    assert(found, `${name}: missing observation`)
    return found
  }
  const rows = name => find(name).sets.map(set => set.rows)
  assertSameCapture(rows('positional'), [[[0, 'Exclusive']]], 'positional call changed')
  assertSameCapture(rows('release not held'), [[[-999, 0, 1]]], 'release failure changed')
  assertSameCapture(find('compatibility').matrix.SharedIntentExclusive.IntentShared, [0, 1], 'compatibility changed')
  assertSameCapture(rows('contended timeout 0'), [[[-1]]], 'timeout changed')
  assertSameCapture(rows('waiter granted after wait'), [[[1]]], 'wait changed')
  assertSameCapture(rows('deadlock session victim'), [[[-3]]], 'deadlock victim changed')
  assertSameCapture(rows('deadlock transaction victim'), [[[-3, 1]]], 'deadlock transaction changed')
}

const database = 'applock_capture'
async function inContainer(config) {
  const admin = await connect(config)
  await run(admin, `CREATE DATABASE [${database}]`)
  await close(admin)
  return observe(config, database)
}

if (check) {
  const retained = JSON.parse(await readFile(fixture, 'utf8'))
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(createHash('sha256').update(JSON.stringify(retained.runs[0])).digest('hex'), retained.captureSha256, 'capture checksum changed')
  for (const observations of retained.runs) validate(observations)
  assertSameCapture(retained.runs[0], retained.runs[1], 'independent runs differ')
  console.log(`Checked ${retained.runs[0].length} application lock observations in two retained runs`)
} else if (comparePort !== null) {
  // Development aid: report every observation that differs from SQL Server.
  const retained = JSON.parse(await readFile(fixture, 'utf8'))
  const config = { server: '127.0.0.1', authentication: { type: 'default', options: { userName: 'sa', password: '' } }, options: { port: comparePort, encrypt: false, requestTimeout: 30000 } }
  const admin = await connect(config)
  await run(admin, `IF DB_ID('${database}') IS NULL CREATE DATABASE [${database}]`)
  await close(admin)
  const actual = await observe(config, database)
  let differences = 0
  for (const expected of retained.runs[0]) {
    const found = actual.find(o => o.name === expected.name)
    if (found && isDeepStrictEqual(found, expected)) continue
    differences++
    console.log(`${expected.name}: ${found ? describeFirstDifference(found, expected) : 'missing'}`)
  }
  console.log(`${differences} of ${retained.runs[0].length} observations differ`)
  process.exitCode = differences ? 1 : 0
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    await withReferenceContainer(async (config, container) => {
      assert.equal(container.image, referenceImage, 'reference image must be pinned')
      const observations = await inContainer(config)
      validate(observations)
      if (runs.length) assertSameCapture(observations, runs[0], 'independent reference containers differ')
      runs.push(observations)
    }, { docker: labelled })
  }
  const actual = { image: referenceImage, captureSha256: createHash('sha256').update(JSON.stringify(runs[0])).digest('hex'), runs }
  await writeFile(output, JSON.stringify(actual, null, 1) + '\n')
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} application lock observations in two fresh containers`)
}

// Containers carry this task's label so parallel workers can tell them apart.
async function labelled(args, env) {
  const { execFile } = await import('node:child_process')
  const { promisify } = await import('node:util')
  const command = args[0] === 'run' ? ['run', '--label', 'msduck.task=gaps-applock-v1', ...args.slice(1)] : args
  return (await promisify(execFile)('docker', command, { env: { ...process.env, ...env }, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()
}
