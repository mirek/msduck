#!/usr/bin/env node
// Owner-controlled SQL Server 2025 evidence for TDS RESETCONNECTIONSKIPTRAN.
import assert from 'node:assert/strict'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/session-reset-skiptran.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/session-reset-skiptran/capture.json')

async function canonicalTarget(path) {
  let current = path
  const missing = []
  while (true) {
    try { return resolve(await realpath(current), ...missing.reverse()) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    const parent = dirname(current)
    if (parent === current) throw Error('cannot resolve output path')
    missing.push(basename(current)); current = parent
  }
}
async function assertSeparateOutput() {
  for (let part = output; ; part = dirname(part)) {
    let info
    try { info = await lstat(part) } catch (error) { if (error.code !== 'ENOENT') throw error }
    assert.ok(!info?.isSymbolicLink(), 'output path contains a symlink')
    if (dirname(part) === part) break
  }
  const retained = fileURLToPath(fixture)
  assert.notEqual(await canonicalTarget(output), await canonicalTarget(retained), 'output aliases retained fixture')
  let outputFile, retainedFile
  try { outputFile = await stat(output) } catch (error) { if (error.code !== 'ENOENT') throw error }
  try { retainedFile = await stat(retained) } catch (error) { if (error.code !== 'ENOENT') throw error }
  assert.ok(!outputFile || !retainedFile || outputFile.dev !== retainedFile.dev || outputFile.ino !== retainedFile.ino, 'output hard-links retained fixture')
  assert.ok(!outputFile, 'refusing to overwrite capture output')
}

const state = `SELECT @@TRANCOUNT AS transaction_count, XACT_STATE() AS transaction_state,
  @@DATEFIRST AS datefirst, CASE WHEN (@@OPTIONS & 512) = 512 THEN 1 ELSE 0 END AS nocount,
  CASE WHEN OBJECT_ID('tempdb..#skiptran_probe') IS NULL THEN 0 ELSE 1 END AS temp_exists`

// Tedious's writer has no public SKIPTRAN knob. During one post-login Batch,
// ask it to write RESETCONNECTION, then replace only the first packet's status
// byte with SKIPTRAN. Record the eight-byte header fields, never the payload.
async function skiptran(connection, sql) {
  const outgoing = connection.messageIo.outgoingMessageStream
  const originalPush = outgoing.push
  const originalWrite = outgoing.write
  const packets = []
  const messages = []
  outgoing.write = function(message, ...rest) {
    messages.push({ type: message.type, resetConnection: message.resetConnection })
    return originalWrite.call(this, message, ...rest)
  }
  outgoing.push = function(buffer, ...rest) {
    if (Buffer.isBuffer(buffer)) {
      assert.equal(buffer.length, buffer.readUInt16BE(2), 'TDS packet length')
      assert.equal(buffer.readUInt8(0), 1, 'only one post-login SQL Batch is instrumented')
      if (packets.length === 0) {
        assert.equal(buffer.readUInt8(1) & 0x08, 0x08, 'Tedious reset bit before substitution')
        buffer[1] = (buffer[1] & ~0x08) | 0x10
      } else buffer[1] &= ~0x08
      packets.push({ type: buffer.readUInt8(0), status: buffer.readUInt8(1),
        length: buffer.readUInt16BE(2), packetId: buffer.readUInt8(6) })
    }
    return originalPush.call(this, buffer, ...rest)
  }
  connection.resetConnectionOnNextRequest = true
  try {
    const result = canonical(await capture(connection, sql))
    assertSameCapture(messages, [{ type: 1, resetConnection: true }], 'instrumented message metadata')
    assert.ok(packets.length > 0, 'missing outgoing Batch packet')
    assert.equal(packets[0].status & 0x18, 0x10, 'first packet has SKIPTRAN alone')
    assert.equal(packets.at(-1).status & 0x01, 0x01, 'last packet has EOM')
    return { messages, packets, result }
  } finally {
    outgoing.push = originalPush
    outgoing.write = originalWrite
    connection.resetConnectionOnNextRequest = false
  }
}

// Keep the server handle inside Tedious, since its numeric value is incidental
// and may differ between the four fresh databases. Retain every response event.
function preparedHandle(connection, records) {
  let complete = () => {}
  const request = new Request('SELECT @p + 1 AS answer', (...args) => complete(...args))
  request.addParameter('p', TYPES.Int, undefined)
  async function phase(name, start, preparing = false) {
    const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
    const onError = error => result.errors.push({ number: error.number, state: error.state, class: error.class, lineNumber: error.lineNumber, message: error.message })
    const onInfo = info => result.info.push({ number: info.number, state: info.state, class: info.class, lineNumber: info.lineNumber, message: info.message })
    const onMetadata = metadata => result.sets.push({
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null,
        precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags,
        collation: canonical(c.collation ?? null) })), rows: []
    })
    const onRow = row => result.sets.at(-1).rows.push(row.map(c => c.value))
    const onDone = {}
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    request.on('columnMetadata', onMetadata)
    request.on('row', onRow)
    for (const kind of ['done', 'doneInProc', 'doneProc']) {
      onDone[kind] = (rowCount, more) => result.done.push({ kind, rowCount: rowCount ?? null, more })
      request.on(kind, onDone[kind])
    }
    const onStatus = (_count, _more, status) => { result.returnStatus = status }
    request.on('doneProc', onStatus)
    let resolvePhase = () => {}
    const onPrepared = () => resolvePhase()
    const onPrepareError = error => {
      if (!result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null, code: error.code ?? null })
      resolvePhase()
    }
    try {
      await new Promise(resolve => {
        resolvePhase = resolve
        complete = (error, rowCount) => {
          result.rowCount = rowCount
          if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null, code: error.code ?? null })
          resolve()
        }
        if (preparing) {
          request.once('prepared', onPrepared)
          request.once('error', onPrepareError)
        }
        request.error = undefined
        start()
      })
    } finally {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      request.off('columnMetadata', onMetadata)
      request.off('row', onRow)
      for (const [kind, listener] of Object.entries(onDone)) request.off(kind, listener)
      request.off('doneProc', onStatus)
      if (preparing) {
        request.off('prepared', onPrepared)
        request.off('error', onPrepareError)
      }
    }
    records.push({ name, session: 'primary', result: canonical(result) })
  }
  return {
    prepare: () => phase('prepare handle', () => connection.prepare(request), true),
    execute: name => phase(name, () => connection.execute(request, { p: 41 }))
  }
}

async function observe(config, primary) {
  const records = []
  const initial = canonical(await capture(primary, 'SELECT DB_NAME() AS database_name'))
  assert.equal(initial.errors.length, 0)
  const database = initial.sets[0].rows[0][0]
  assert.match(database, /^msduck_audit_[0-9a-f]+$/)
  const secondary = await connect({ ...config, options: { ...config.options, database } })
  async function record(name, connection, sql) {
    const result = canonical(await capture(connection, sql))
    records.push({ name, session: connection === primary ? 'primary' : 'secondary', sql, result })
    return result
  }
  try {
    const handle = preparedHandle(primary, records)
    await record('initial state', primary, state)
    await record('create durable table', primary, 'CREATE TABLE dbo.skiptran_marker(n INT)')
    await record('create local temp and settings', primary,
      'CREATE TABLE #skiptran_probe(n INT); INSERT #skiptran_probe VALUES(7); SET DATEFIRST 3; SET NOCOUNT ON')
    await handle.prepare()
    assert.equal(records.at(-1).result.errors.length, 0, 'sp_prepare')
    await handle.execute('execute before reset')
    await record('begin transaction and insert', primary,
      'BEGIN TRANSACTION; INSERT dbo.skiptran_marker VALUES(9)')
    await record('before skiptran', primary, `${state}; SELECT n FROM #skiptran_probe`)
    await record('dirty view before skiptran', secondary,
      'SELECT COUNT(*) AS marker_count FROM dbo.skiptran_marker WITH (NOLOCK)')
    records.push({ name: 'skiptran request', sql: state, result: await skiptran(primary, state) })
    await record('after skiptran', primary, state)
    await record('dirty view after skiptran', secondary,
      'SELECT COUNT(*) AS marker_count FROM dbo.skiptran_marker WITH (NOLOCK)')
    await handle.execute('execute after skiptran')
    await record('temp lookup after skiptran', primary,
      "SELECT CASE WHEN OBJECT_ID('tempdb..#skiptran_probe') IS NULL THEN 0 ELSE 1 END AS temp_exists")
    await record('reused inside transaction', primary, 'SELECT 42 AS answer')
    await record('cleanup transaction', primary, 'IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION; SELECT @@TRANCOUNT AS transaction_count')
    await record('committed view after rollback', secondary,
      'SELECT COUNT(*) AS marker_count FROM dbo.skiptran_marker')
    await record('reused after rollback', primary, 'SELECT 43 AS answer')
    return records
  } finally {
    // Never let a preserved transaction keep the isolated database open.
    try { await capture(primary, 'IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION') }
    finally {
      if (!secondary.closed) await new Promise(resolve => { secondary.once('end', resolve); secondary.close() })
    }
  }
}

function validate(run) {
  assert.equal(run.length, 17)
  const get = name => {
    const item = run.find(entry => entry.name === name)
    assert.ok(item, `missing ${name}`)
    return item.result
  }
  const skip = get('skiptran request')
  assertSameCapture(skip.messages, [{ type: 1, resetConnection: true }], 'outgoing message')
  assert.equal(skip.packets[0].status & 0x18, 0x10)
  assert.equal(skip.result.errors.length, 0, 'SKIPTRAN request')
  assertSameCapture(get('before skiptran').sets[0].rows[0].slice(0, 5), [1, 1, 3, 1, 1], 'before state')
  assertSameCapture(get('dirty view before skiptran').sets[0].rows, [[1]], 'dirty row before reset')
  assertSameCapture(get('dirty view after skiptran').sets[0].rows, [[1]], 'transaction preserved')
  assertSameCapture(get('committed view after rollback').sets[0].rows, [[0]], 'rollback cleanup')
  assertSameCapture(get('reused inside transaction').sets[0].rows, [[42]], 'reuse inside transaction')
  assertSameCapture(get('reused after rollback').sets[0].rows, [[43]], 'reuse after rollback')
}

await assertSeparateOutput()
if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(dirname(output), { recursive: true })
const captures = []
let image
for (let container = 0; container < 2; container++) {
  await withReferenceContainer(async (config, info) => {
    image ??= info.image
    assert.equal(info.image, image)
    for (let database = 0; database < 2; database++) {
      const run = await isolatedReference(config, primary => observe(config, primary))
      validate(run)
      if (captures.length) assertSameCapture(run, captures[0], 'fresh SKIPTRAN captures differ')
      captures.push(run)
    }
  })
}
const actual = { image, freshDatabases: captures.length, independentContainers: 2, results: captures[0] }
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (retained) assertSameCapture(actual, retained, 'SKIPTRAN capture differs from fixture')
await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${actual.results.length} SKIPTRAN observations in four fresh databases and two containers${retained ? '; matched retained fixture' : ''}`)
