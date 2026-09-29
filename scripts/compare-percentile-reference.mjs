#!/usr/bin/env node
// Preserve full percentile observations; comparison differences are evidence, not failures.
import assert from 'node:assert/strict'
import { execFileSync, spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { mkdir, open, readFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { capture, canonical, differences } from './lib/compatibility.mjs'
import { connect, assertSameCapture } from './lib/reference.mjs'

const root = fileURLToPath(new URL('../', import.meta.url))
const fixturePath = new URL('../reference/percentile-reference.json', import.meta.url)
const fixtureSha256 = 'd0b7e5d459e51e87d89771f4dd0dd3c7039decad109abf0068f3f45fd5f87498'
const executablePath = resolve(root, process.env.CARGO_TARGET_DIR ?? 'target', 'debug/msduck')
const args = process.argv.slice(2)
const check = args[0] === '--check'
if (args.length > 1 || (args[0]?.startsWith('--') && !check)) throw Error('usage: compare-percentile-reference.mjs [--check | output]')
const output = resolve(args[0] && !check ? args[0] : 'artifacts/compatibility/percentile-execution/comparison.json')
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim()

async function captureTokens(connection, action) {
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
  try {
    const result = await action()
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

async function capturePreparedPhase(connection, request, issue, setComplete, prepared = false) {
  const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
  const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
  const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
  const onMetadata = metadata => result.sets.push({ columns: metadata.map(c => ({ name: c.colName, type: c.type.name,
    length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null,
    flags: c.flags, collation: canonical(c.collation ?? null) })), rows: [] })
  const onRow = row => result.sets.at(-1).rows.push(row.map(c => c.value))
  const done = Object.fromEntries(['done', 'doneInProc', 'doneProc'].map(kind => [kind, (rowCount, more, status) => {
    result.done.push({ kind, rowCount: rowCount ?? null, more })
    if (kind === 'doneProc') result.returnStatus = status
  }]))
  connection.on('errorMessage', onError); connection.on('infoMessage', onInfo)
  request.on('columnMetadata', onMetadata); request.on('row', onRow)
  for (const [kind, listener] of Object.entries(done)) request.on(kind, listener)
  try {
    return await captureTokens(connection, async () => {
      if (prepared) {
        await new Promise(resolve => {
          const success = () => { request.off('error', failure); resolve() }
          const failure = error => {
            request.off('prepared', success)
            if (!result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          }
          request.once('prepared', success)
          request.once('error', failure)
          issue()
        })
      } else {
        await new Promise(resolve => {
          request.error = undefined
          setComplete((error, rowCount) => {
            result.rowCount = rowCount
            if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          })
          issue()
        })
      }
      return result
    })
  } finally {
    connection.off('errorMessage', onError); connection.off('infoMessage', onInfo)
    request.off('columnMetadata', onMetadata); request.off('row', onRow)
    for (const [kind, listener] of Object.entries(done)) request.off(kind, listener)
  }
}

async function startServer() {
  const server = spawn(executablePath, ['--listen', '127.0.0.1:0'], { cwd: root, stdio: ['ignore', 'ignore', 'pipe'] })
  let logs = ''
  try {
    const port = await new Promise((accept, reject) => {
      const timeout = setTimeout(() => reject(Error(`server startup timed out: ${logs}`)), 30000)
      server.once('error', error => { clearTimeout(timeout); reject(error) })
      server.once('exit', code => { clearTimeout(timeout); reject(Error(`server exited (${code}): ${logs}`)) })
      server.stderr.on('data', bytes => {
        logs = (logs + bytes.toString()).slice(-4096)
        const match = /listening on 127\.0\.0\.1:(\d+)/.exec(logs)
        if (match) { clearTimeout(timeout); accept(Number(match[1])) }
      })
    })
    return { server, port }
  } catch (error) {
    server.kill('SIGTERM')
    throw error
  }
}


async function observe(connection, plan) {
  const records = []
  for (const entry of plan) {
    const { name, sql, mode } = entry
    if (name === 'server version') continue
    if (mode === 'batch') {
      records.push({ name, sql, mode, result: canonical(await captureTokens(connection, () => capture(connection, sql))) })
    } else {
      assert.equal(mode, 'prepared', 'unknown reference request mode')
      assert(TYPES[entry.type], 'unknown reference parameter type')
      let complete = () => {}
      const setComplete = callback => { complete = callback }
      const request = new Request(sql, (error, count) => complete(error, count))
      request.addParameter('b', TYPES[entry.type], undefined, entry.options ?? undefined)
      const preparation = canonical(await capturePreparedPhase(connection, request, () => connection.prepare(request), setComplete, true))
      const executions = []
      let unpreparation = null
      if (!preparation.errors.length) {
        try {
          for (const value of entry.values) {
            const result = canonical(await capturePreparedPhase(connection, request,
              () => connection.execute(request, { b: value }), setComplete))
            executions.push({ value, result })
          }
        } finally {
          unpreparation = canonical(await capturePreparedPhase(connection, request,
            () => connection.unprepare(request), setComplete))
        }
      }
      records.push({ name, sql, mode, type: entry.type, options: entry.options,
        values: entry.values, preparation, executions, unpreparation })
    }
    console.log(name)
  }
  return records
}

function phases(record) {
  if (record.mode === 'batch') return { batch: record.result }
  return { preparation: record.preparation,
    ...Object.fromEntries(record.executions.map((entry, i) => [`execution-${i}`, entry.result])),
    unpreparation: record.unpreparation }
}
function floatingBits(result) {
  if (result == null) return null
  return result.sets.map(set => set.rows.map(row => row.map((value, i) => {
    const column = set.columns[i]
    if (!['FloatN', 'Float', 'Real'].includes(column.type) || value === null) return null
    const width = column.type === 'Real' ? 4 : column.type === 'Float' ? 8 : column.length
    assert([4, 8].includes(width), 'unknown FLOAT width')
    const number = typeof value === 'number' ? value : value?.kind === 'number' ? Number(value.value) : undefined
    assert.equal(typeof number, 'number', 'unexpected FLOAT value')
    const bytes = Buffer.alloc(width)
    if (width === 4) bytes.writeFloatBE(number); else bytes.writeDoubleBE(number)
    return bytes.toString('hex')
  })))
}
function compare(local, reference) {
  const actual = phases(local), expected = phases(reference)
  const localBits = Object.fromEntries(Object.entries(actual).map(([key, value]) => [key, floatingBits(value)]))
  const referenceBits = Object.fromEntries(Object.entries(expected).map(([key, value]) => [key, floatingBits(value)]))
  return { name: reference.name, sql: reference.sql, local, reference,
    differences: differences(local, reference), localBits, referenceBits,
    bitDifferences: differences(localBits, referenceBits) }
}

const fixtureBytes = await readFile(fixturePath)
assert.equal(createHash('sha256').update(fixtureBytes).digest('hex'), fixtureSha256, 'reference fixture changed')
const fixture = JSON.parse(fixtureBytes)
assert.equal(fixture.runs.length, 2, 'missing independent reference runs')
assertSameCapture(fixture.runs[0], fixture.runs[1], 'independent reference runs differ')
const plan = fixture.runs[0]
assert.equal(plan.length, 37, 'reference plan changed')
assert.equal(new Set(plan.map(entry => entry.name)).size, plan.length, 'duplicate reference names')
assert.equal(plan.filter(entry => entry.name === 'server version').length, 1)
for (const entry of plan) {
  assert(['batch', 'prepared'].includes(entry.mode), 'invalid reference mode')
  if (entry.mode === 'prepared') assert(TYPES[entry.type] && Array.isArray(entry.values), 'invalid reference binding')
}
if (check) {
  console.log('Checked 37 reference records; replay includes 33 batches and 3 prepared requests')
} else {
  const sourceRevision = git('rev-parse', 'HEAD')
  const sourceTree = git('rev-parse', 'HEAD^{tree}')
  assert.equal(git('status', '--porcelain=v1'), '', 'comparison requires a clean tracked checkout')
  const outputRelative = relative(root, output)
  if (outputRelative !== '..' && !outputRelative.startsWith('..' + sep)) {
    try { execFileSync('git', ['check-ignore', '-q', '--', outputRelative], { cwd: root, stdio: 'ignore' }) }
    catch { throw Error('in-checkout output must be Git-ignored; use artifacts/ or an external path') }
  }
  // Exclusive creation refuses every existing file, including fixture aliases,
  // symlinks and hard links, before build or execution. Keep this handle through
  // final write so a concurrent pathname replacement cannot redirect output.
  await mkdir(dirname(output), { recursive: true })
  const file = await open(output, 'wx')
  let server, connection
  try {
    await file.writeFile(JSON.stringify({ status: 'incomplete', sourceRevision, sourceTree, referenceSha256: fixtureSha256 }) + '\n')
    execFileSync('cargo', ['build', '--workspace', '--all-targets', '--locked'], { cwd: root, stdio: 'inherit' })
    assert.equal(git('rev-parse', 'HEAD'), sourceRevision, 'revision changed during build')
    assert.equal(git('status', '--porcelain=v1'), '', 'build changed tracked sources')
    const hash = createHash('sha256')
    for await (const bytes of createReadStream(executablePath)) hash.update(bytes)
    const started = await startServer()
    server = started.server
    connection = await connect({ server: '127.0.0.1',
      authentication: { type: 'default', options: { userName: 'sa', password: 'development' } },
      options: { port: started.port, encrypt: false, database: 'master', connectTimeout: 10000, requestTimeout: 30000 } })
    const local = await observe(connection, plan)
    const cases = local.map(entry => compare(entry, plan.find(reference => reference.name === entry.name)))
    assert.equal(cases.length, 36, 'missing replay request')
    const reuse = canonical(await captureTokens(connection, () => capture(connection, 'SELECT 1 AS reusable')))
    assertSameCapture(reuse.sets.map(set => set.rows), [[[1]]], 'connection reuse failed')
    assert.equal(reuse.errors.length, 0, 'reuse error')
    assert.equal(git('rev-parse', 'HEAD'), sourceRevision, 'revision changed during replay')
    assert.equal(git('status', '--porcelain=v1'), '', 'replay changed tracked sources')
    const summary = { cases: cases.length,
      matched: cases.filter(entry => !entry.differences.length && !entry.bitDifferences.length).length,
      different: cases.filter(entry => entry.differences.length || entry.bitDifferences.length).length,
      differences: cases.reduce((sum, entry) => sum + entry.differences.length, 0),
      floatBitDifferences: cases.reduce((sum, entry) => sum + entry.bitDifferences.length, 0) }
    const result = { status: 'complete', sourceRevision, sourceTree,
      executableSha256: hash.digest('hex'), referenceSha256: fixtureSha256, referenceImage: fixture.image,
      buildCommand: 'cargo build --workspace --all-targets --locked',
      excluded: [{ name: 'server version', reason: 'ProductVersion is product-specific' }], summary, cases, reuse }
    // writeFile uses the current file position: truncate and write explicitly at
    // offset zero to replace the incomplete marker without reopening the path.
    const bytes = Buffer.from(JSON.stringify(result, null, 2) + '\n')
    await file.truncate(0)
    let written = 0
    while (written < bytes.length) written += (await file.write(bytes, written, bytes.length - written, written)).bytesWritten
    console.log(JSON.stringify(summary))
  } finally {
    try {
      if (connection && !connection.closed) await new Promise(resolve => { connection.once('end', resolve); connection.close() })
    } finally {
      if (server && server.exitCode === null && server.signalCode === null) {
        const ended = new Promise(resolve => server.once('exit', resolve))
        server.kill('SIGTERM')
        const timeout = setTimeout(() => server.kill('SIGKILL'), 5000)
        try { await ended } finally { clearTimeout(timeout) }
      }
      await file.close()
    }
  }
}
