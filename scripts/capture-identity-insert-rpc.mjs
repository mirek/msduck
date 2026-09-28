#!/usr/bin/env node
// First-party SQL Server 2025 evidence for IDENTITY_INSERT RPC and allocator scope.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/identity-insert-rpc.json', import.meta.url)
const fixtureSha256 = 'ebd50b7da9e5ef01270fd0c6007e470e66f23cc97383b36188a804ca8203d8f2'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-identity-insert-rpc.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/identity-insert-rpc/capture.json')

async function existingFile(path, inspect = stat) {
  try { return await inspect(path) }
  catch (error) {
    if (error.code === 'ENOENT') return null
    throw error
  }
}

async function canonicalOutput(path) {
  try { return await realpath(path) }
  catch (error) {
    if (error.code !== 'ENOENT') throw error
    return join(await realpath(dirname(path)), basename(path))
  }
}

// Capture a batch or parameterized sp_executesql, including raw DONE words.
async function captureTokens(connection, sql, mode = 'batch', parameters = {}) {
  const doneTokens = []
  const raw = []
  const events = []
  const createParser = connection.createTokenStreamParser
  const readToken = StreamParser.prototype.readToken
  const recordRaw = (parser, type) => {
    assert(parser.options.tdsVersion >= '7_2', 'unexpected TDS version')
    const at = parser.position
    if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordRaw(parser, type))
    raw.push({ kind: doneKinds.get(type), status: parser.buffer.readUInt16LE(at), command: parser.buffer.readUInt16LE(at + 2) })
    return readToken.call(parser, type)
  }
  StreamParser.prototype.readToken = function (type) {
    return doneKinds.has(type) ? recordRaw(this, type) : readToken.call(this, type)
  }
  connection.createTokenStreamParser = function (message, handler) {
    const parser = createParser.call(this, message, handler)
    assert.equal(typeof parser.parser?.prependListener, 'function', 'Tedious token stream unavailable')
    parser.parser.prependListener('data', token => {
      if (typeof token.name === 'string') events.push({ kind: token.name })
      if (['DONE', 'DONEINPROC', 'DONEPROC'].includes(token.name)) doneTokens.push({
        kind: token.name, more: token.more, sqlError: token.sqlError,
        attention: token.attention, serverError: token.serverError,
        rowCount: token.rowCount ?? null, command: token.curCmd,
      })
    })
    return parser
  }
  const transport = mode === 'rpc' ? {
    on: (...values) => connection.on(...values),
    off: (...values) => connection.off(...values),
    execSqlBatch(request) {
      for (const [name, value] of Object.entries(parameters)) request.addParameter(name, TYPES.Int, value)
      connection.execSql(request)
    },
  } : connection
  try {
    const result = await capture(transport, sql)
    assert.equal(doneTokens.length, result.done.length, 'incomplete decoded DONE tokens')
    assert.equal(raw.length, doneTokens.length, 'incomplete raw DONE words')
    for (let index = 0; index < doneTokens.length; index++) {
      assert.equal(raw[index].kind, doneTokens[index].kind, 'raw and decoded DONE kinds differ')
      assert.equal(raw[index].command, doneTokens[index].command, 'raw and decoded DONE commands differ')
      doneTokens[index].status = raw[index].status
    }
    return { ...result, doneTokens, events }
  } finally {
    connection.createTokenStreamParser = createParser
    StreamParser.prototype.readToken = readToken
  }
}

const message = token => ({ number: token.number, state: token.state, class: token.class, lineNumber: token.lineNumber, message: token.message })
const column = value => ({ name: value.colName, type: value.type.name, length: value.dataLength ?? null, precision: value.precision ?? null, scale: value.scale ?? null, flags: value.flags, collation: canonical(value.collation ?? null) })

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

// Tedious sends real sp_prepare/sp_execute/sp_unprepare RPCs. A reusable
// Request reports prepare through 'prepared', then execute/unprepare callbacks.
function preparedRequest(connection, sql) {
  let current
  let complete = () => {}
  const request = new Request(sql, (error, rowCount) => complete(error, rowCount))
  request.addParameter('marker', TYPES.Int)
  const errors = token => current?.errors.push(message(token))
  const info = token => current?.info.push(message(token))
  connection.on('errorMessage', errors)
  connection.on('infoMessage', info)
  request.on('columnMetadata', columns => current?.sets.push({ columns: columns.map(column), rows: [] }))
  request.on('row', values => current?.sets.at(-1).rows.push(values.map(value => value.value)))
  for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => {
    current?.done.push({ kind, rowCount: rowCount ?? null, more })
  })
  const statusTokens = returnStatusTokens(connection)
  const phase = start => new Promise(resolve => {
    const raw = []
    const events = []
    const createParser = connection.createTokenStreamParser
    const readToken = StreamParser.prototype.readToken
    const recordRaw = (parser, type) => {
      assert(parser.options.tdsVersion >= '7_2', 'unexpected TDS version')
      const at = parser.position
      if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordRaw(parser, type))
      raw.push({ kind: doneKinds.get(type), status: parser.buffer.readUInt16LE(at), command: parser.buffer.readUInt16LE(at + 2) })
      return readToken.call(parser, type)
    }
    StreamParser.prototype.readToken = function (type) {
      return doneKinds.has(type) ? recordRaw(this, type) : readToken.call(this, type)
    }
    connection.createTokenStreamParser = function (message, handler) {
      const parser = createParser.call(this, message, handler)
      assert.equal(typeof parser.parser?.prependListener, 'function', 'Tedious token stream unavailable')
      parser.parser.prependListener('data', token => {
        if (typeof token.name === 'string') events.push({ kind: token.name })
      })
      return parser
    }
    const restore = () => {
      connection.createTokenStreamParser = createParser
      StreamParser.prototype.readToken = readToken
      statusTokens.sink = null
    }
    current = { sets: [], done: [], errors: [], info: [], returnStatus: null }
    statusTokens.sink = value => { current.returnStatus = value }
    complete = (error, rowCount) => {
      restore()
      const finished = current
      current = undefined
      complete = () => {}
      finished.rowCount = rowCount
      if (error && !finished.errors.length) finished.errors.push({ number: error.number ?? null, message: error.message })
      assert.equal(raw.length, finished.done.length, 'prepared DONE token count mismatch')
      finished.doneTokens = finished.done.map((token, index) => ({
        ...token,
        kind: token.kind === 'done' ? 'DONE' : token.kind === 'doneInProc' ? 'DONEINPROC' : 'DONEPROC',
        status: raw[index].status,
        command: raw[index].command,
      }))
      for (let index = 0; index < raw.length; index++) {
        assert.equal(raw[index].kind, finished.doneTokens[index].kind, 'prepared DONE kind mismatch')
      }
      finished.events = events
      resolve(canonical(finished))
    }
    request.error = undefined
    start()
  })
  return {
    async prepare() {
      const onPrepared = () => complete(undefined, undefined)
      const onError = error => complete(error, undefined)
      const result = await phase(() => {
        request.once('prepared', onPrepared)
        request.once('error', onError)
        connection.prepare(request)
      })
      request.off('prepared', onPrepared)
      request.off('error', onError)
      return { result, handle: request.handle ?? null }
    },
    execute(values) { return phase(() => connection.execute(request, values)) },
    unprepare() { return phase(() => connection.unprepare(request)) },
    close() {
      statusTokens.sink = null
      connection.off('errorMessage', errors)
      connection.off('infoMessage', info)
    },
  }
}

const alphaState = "SELECT id,v FROM dbo.alpha ORDER BY id; SELECT CONVERT(VARCHAR(40),IDENT_CURRENT('dbo.alpha')) AS current_value, CONVERT(VARCHAR(40),SCOPE_IDENTITY()) AS scoped, CONVERT(VARCHAR(40),@@IDENTITY) AS session_last"
const descendingState = "SELECT id,v FROM dbo.descending_ids ORDER BY id DESC; SELECT CONVERT(VARCHAR(40),IDENT_CURRENT('dbo.descending_ids')) AS current_value"
const preparedSql = 'SET IDENTITY_INSERT dbo.alpha ON; SELECT @marker AS marker'

async function observe(connection) {
  const records = []
  async function record(name, sql, mode = 'batch', parameters = {}) {
    const result = canonical(await captureTokens(connection, sql, mode, parameters))
    records.push({ name, sql, mode, ...(mode === 'rpc' ? { parameters } : {}), result })
    console.log(name)
    return result
  }
  function preparedRecord(name, result) {
    records.push({ name, sql: preparedSql, mode: 'prepared', result })
    console.log(name)
  }

  await record('server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version")
  await record('create alpha', 'CREATE TABLE dbo.alpha(id INT IDENTITY(1,1) PRIMARY KEY, v INT)')
  await record('create descending', 'CREATE TABLE dbo.descending_ids(id INT IDENTITY(0,-2) PRIMARY KEY, v INT)')
  await record('baseline alpha', 'INSERT dbo.alpha(v) VALUES(1)')
  await record('baseline descending', 'INSERT dbo.descending_ids(v) VALUES(1)')
  await record('baseline state', `${alphaState}; ${descendingState}`)

  await record('RPC set alpha ON', 'SET IDENTITY_INSERT dbo.alpha ON; SELECT @marker AS marker', 'rpc', { marker: 7 })
  await record('outer explicit after RPC ON', 'INSERT dbo.alpha(id,v) VALUES(100,2)')
  await record('state after RPC ON', alphaState)
  await record('RPC set alpha OFF', 'SET IDENTITY_INSERT dbo.alpha OFF; SELECT @marker AS marker', 'rpc', { marker: 8 })
  await record('outer explicit after RPC OFF', 'INSERT dbo.alpha(id,v) VALUES(101,3)')
  await record('outer automatic after RPC OFF', 'INSERT dbo.alpha(v) VALUES(4)')
  await record('state after RPC OFF', alphaState)

  await record('outer set alpha ON', 'SET IDENTITY_INSERT dbo.alpha ON')
  await record('RPC set alpha OFF inside outer ON', 'SET IDENTITY_INSERT dbo.alpha OFF; SELECT @marker AS marker', 'rpc', { marker: 10 })
  await record('outer explicit after nested RPC OFF', 'INSERT dbo.alpha(id,v) VALUES(110,11)')
  await record('RPC explicit while outer ON', 'INSERT dbo.alpha(id,v) VALUES(@id,5); SELECT CONVERT(VARCHAR(40),SCOPE_IDENTITY()) AS scoped, CONVERT(VARCHAR(40),@@IDENTITY) AS session_last', 'rpc', { id: 120 })
  await record('outer identity after RPC explicit', alphaState)
  await record('outer set alpha OFF', 'SET IDENTITY_INSERT dbo.alpha OFF')
  await record('outer automatic after RPC explicit', 'INSERT dbo.alpha(v) VALUES(6)')

  const prepared = preparedRequest(connection, preparedSql)
  try {
    const { result, handle } = await prepared.prepare()
    preparedRecord('prepare SET ON', { ...result, handle })
    await record('outer explicit after prepare only', 'INSERT dbo.alpha(id,v) VALUES(130,7)')
    if (handle !== null) {
      preparedRecord('execute prepared SET ON', await prepared.execute({ marker: 9 }))
      await record('outer explicit after prepared execute', 'INSERT dbo.alpha(id,v) VALUES(130,8)')
      preparedRecord('unprepare SET ON', await prepared.unprepare())
      await record('outer explicit after unprepare', 'INSERT dbo.alpha(id,v) VALUES(131,9)')
    }
  } finally { prepared.close() }
  await record('outer set alpha OFF after prepare', 'SET IDENTITY_INSERT dbo.alpha OFF')
  await record('outer automatic after prepare', 'INSERT dbo.alpha(v) VALUES(10)')
  await record('alpha state after prepare', alphaState)

  await record('descending ON', 'SET IDENTITY_INSERT dbo.descending_ids ON')
  await record('descending explicit low', 'INSERT dbo.descending_ids(id,v) VALUES(-20,2)')
  await record('descending explicit high after low', 'INSERT dbo.descending_ids(id,v) VALUES(5,3)')
  await record('descending OFF', 'SET IDENTITY_INSERT dbo.descending_ids OFF')
  await record('descending generated after high', 'INSERT dbo.descending_ids(v) VALUES(4)')
  await record('descending state', descendingState)
  await record('descending ON for rollback', 'SET IDENTITY_INSERT dbo.descending_ids ON')
  await record('descending explicit rollback', 'BEGIN TRANSACTION; INSERT dbo.descending_ids(id,v) VALUES(-100,5); ROLLBACK TRANSACTION')
  await record('descending OFF after rollback', 'SET IDENTITY_INSERT dbo.descending_ids OFF')
  await record('descending state after rollback', descendingState)
  await record('descending generated after rollback', 'INSERT dbo.descending_ids(v) VALUES(6)')
  await record('descending final state', descendingState)
  await record('session reusable', 'SELECT @@TRANCOUNT AS tran_count, XACT_STATE() AS xact_state, 1 AS reusable')
  return records
}

function validate(run) {
  assert.equal(run.length, 42, 'case count changed')
  const get = name => {
    const entry = run.find(item => item.name === name)
    assert(entry, `missing ${name}`)
    return entry.result
  }
  for (const entry of run) {
    const { result, name } = entry
    for (const key of ['sets', 'done', 'errors', 'info']) assert(Array.isArray(result[key]), `${name}: missing ${key}`)
    assert(Array.isArray(result.doneTokens) && Array.isArray(result.events), `${name}: missing ordered wire tokens`)
    assert.equal(result.doneTokens.length, result.done.length, `${name}: incomplete DONE words`)
    assert(result.doneTokens.length > 0, `${name}: no completion`)
    for (const token of result.doneTokens) assert(Number.isInteger(token.status), `${name}: missing DONE status`)
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete set`)
      for (const descriptor of set.columns) assert(typeof descriptor.type === 'string' && typeof descriptor.flags === 'number', `${name}: incomplete descriptor`)
    }
  }
  const explicitOff = [
    'outer explicit after RPC ON', 'outer explicit after RPC OFF',
    'outer explicit after prepare only', 'outer explicit after prepared execute',
    'outer explicit after unprepare',
  ]
  for (const entry of run) {
    if (explicitOff.includes(entry.name)) {
      assert.equal(entry.result.errors.length, 1, `${entry.name}: expected 544`)
      assert.equal(entry.result.errors[0].number, 544, `${entry.name}: wrong diagnostic`)
      assertSameCapture(entry.result.doneTokens.map(({ kind, status, command }) => [kind, status, command]),
        [['DONE', 2, 195]], `${entry.name}: wrong error completion`)
    } else assert.equal(entry.result.errors.length, 0, `${entry.name}: unexpected error`)
  }
  const rows = (name, set = 0) => get(name).sets[set].rows
  assertSameCapture(rows('baseline state', 3), [['0']], 'descending seed changed')
  assertSameCapture(rows('RPC set alpha ON'), [[7]], 'RPC parameter or result changed')
  assertSameCapture(get('RPC set alpha ON').sets[0].columns.map(({ name, type, length, flags }) => [name, type, length, flags]),
    [['marker', 'IntN', 4, 33]], 'RPC marker descriptor changed')
  assertSameCapture(get('RPC set alpha ON').doneTokens.map(({ kind, status, command }) => [kind, status, command]),
    [['DONEINPROC', 1, 183], ['DONEINPROC', 17, 193], ['DONEPROC', 0, 224]], 'RPC SET ON completion changed')
  assertSameCapture(get('RPC set alpha OFF').doneTokens.map(({ kind, status, command }) => [kind, status, command]),
    [['DONEINPROC', 1, 184], ['DONEINPROC', 17, 193], ['DONEPROC', 0, 224]], 'RPC SET OFF completion changed')
  assertSameCapture(get('RPC set alpha OFF inside outer ON').doneTokens.map(({ kind, status, command }) => [kind, status, command]),
    [['DONEINPROC', 1, 184], ['DONEINPROC', 17, 193], ['DONEPROC', 0, 224]], 'nested RPC SET OFF completion changed')
  assertSameCapture(rows('state after RPC ON', 1), [['1', '0', '0']], 'RPC ON leaked to caller')
  assertSameCapture(rows('state after RPC OFF', 1), [['2', '2', '2']], 'RPC OFF changed caller allocation')
  assertSameCapture(rows('outer identity after RPC explicit').some(row => row[0] === 110 && row[1] === 11), true,
    'RPC OFF leaked into caller ON setting')
  assertSameCapture(rows('RPC explicit while outer ON'), [['120', '120']], 'RPC scope identity changed')
  assertSameCapture(rows('outer identity after RPC explicit', 1), [['120', '110', '120']],
    'caller scope or session identity changed')
  assert.equal(get('prepare SET ON').handle, 1, 'prepared handle changed')
  assert.equal(get('prepare SET ON').returnStatus, 8182, 'prepare return status changed')
  assertSameCapture(get('prepare SET ON').events.map(({ kind }) => kind),
    ['RETURNSTATUS', 'RETURNVALUE', 'DONEPROC'], 'prepare token order changed')
  assertSameCapture(rows('execute prepared SET ON'), [[9]], 'prepared execution did not run')
  assertSameCapture(get('execute prepared SET ON').doneTokens.map(({ kind, status, command }) => [kind, status, command]),
    [['DONEINPROC', 1, 183], ['DONEINPROC', 17, 193], ['DONEPROC', 0, 224]],
    'prepared SET completion changed')
  assertSameCapture(rows('alpha state after prepare', 1), [['122', '122', '122']],
    'prepared setting leaked or allocator changed')
  assertSameCapture(rows('descending state'), [[5, 3], [0, 1], [-20, 2], [-22, 4]],
    'negative-increment high/low allocation changed')
  assertSameCapture(rows('descending state', 1), [['-22']], 'descending current value changed')
  assertSameCapture(rows('descending state after rollback'), rows('descending state'),
    'rolled-back explicit row remained')
  assertSameCapture(rows('descending state after rollback', 1), [['-100']],
    'rolled-back explicit value failed to advance allocator')
  assertSameCapture(rows('descending final state'), [[5, 3], [0, 1], [-20, 2], [-22, 4], [-102, 6]],
    'post-rollback descending allocation changed')
  assertSameCapture(rows('descending final state', 1), [['-102']], 'descending final current changed')
  assertSameCapture(get('session reusable').sets[0].rows, [[0, 0, 1]], 'session not reusable')
}

async function checkFixture() {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'reference repetitions missing')
  for (const run of retained.runs) validate(run)
  assertSameCapture(retained.runs[0], retained.runs[1], 'reference runs differ')
  console.log(`Checked ${retained.runs[0].length} IDENTITY_INSERT RPC observations in two retained runs`)
}

if (check) await checkFixture()
else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if ((await existingFile(output, lstat))?.isSymbolicLink()) throw Error('capture output must not be a symbolic link')
  const fixturePath = fileURLToPath(fixture)
  if (await canonicalOutput(output) === await canonicalOutput(fixturePath)) throw Error('capture output must not be the retained fixture')
  const [outputFile, retainedFile] = await Promise.all([existingFile(output), existingFile(fixturePath)])
  if (outputFile && retainedFile && outputFile.dev === retainedFile.dev && outputFile.ino === retainedFile.ino) {
    throw Error('capture output must not be a hard link to the retained fixture')
  }
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) {
    throw Error('reference image must be pinned')
  }
  await withReferenceContainer(async (config, container) => {
    assert.equal(container.image, referenceImage, 'reference image must be pinned')
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      const run = await isolatedReference(config, observe)
      runs.push(run)
      await writeFile(output, JSON.stringify({ image: container.image, runs }) + '\n')
      validate(run)
    }
    assertSameCapture(runs[0], runs[1], 'fresh-database captures differ')
    const actual = { image: container.image, runs }
    await writeFile(output, JSON.stringify(actual) + '\n')
    let retained
    try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    if (retained) {
      assert.equal(actual.image, retained.image, 'reference image changed')
      assertSameCapture(actual.runs, retained.runs, 'live capture differs from retained fixture')
    }
    if (writeFixture) await writeNewFixture(fixture, actual)
    console.log(`Captured ${runs[0].length} IDENTITY_INSERT RPC observations in two fresh databases`)
  })
}
