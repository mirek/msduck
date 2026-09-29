#!/usr/bin/env node
// Replay the pinned SQL Server capture against this revision of msduck.
import assert from 'node:assert/strict'
import { execFileSync, spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Connection } from 'tedious'
import { capture, canonical, differences } from './lib/compatibility.mjs'

const fixturePath = fileURLToPath(new URL('../reference/statistical-aggregates.json', import.meta.url))
const root = fileURLToPath(new URL('../', import.meta.url))
const executablePath = resolve(root, 'target/debug/msduck')
const fixtureSha256 = 'ddd57b260ee6db3ccb82c128bab345f5410818491d8585afd8355d2fbe150fa1'
const outputPath = fileURLToPath(new URL('../artifacts/compatibility/statistical-precision/comparison.json', import.meta.url))
if (process.argv.length !== 2) throw Error('usage: node scripts/compare-statistical-reference.mjs')
const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim()
const sourceRevision = git('rev-parse', 'HEAD')
assert.match(sourceRevision, /^[0-9a-f]{40}$/, 'Git head is not a full commit SHA')
assert.equal(git('status', '--porcelain=v1'), '', 'Git checkout is dirty')
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])

function floatBits(value) {
  const bytes = Buffer.alloc(8)
  bytes.writeDoubleBE(value)
  return `0x${bytes.toString('hex')}`
}

function annotateDifference(difference, reference) {
  const match = /^\/sets\/(\d+)\/rows\/(\d+)\/(\d+)$/.exec(difference.path)
  const column = match && reference.sets[Number(match[1])]?.columns[Number(match[3])]
  const local = difference.local
  const expected = difference.reference
  if (column?.type === 'FloatN' && column.length === 8 && typeof local === 'number' && typeof expected === 'number') {
    return { ...difference, kind: 'float-bits', localBits: floatBits(local), referenceBits: floatBits(expected) }
  }
  return { ...difference, kind: 'other' }
}

async function captureTokens(connection, sql) {
  const raw = []
  const doneTokens = []
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
    const result = canonical(await capture(connection, sql))
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

const fixtureBytes = await readFile(fixturePath)
assert.equal(createHash('sha256').update(fixtureBytes).digest('hex'), fixtureSha256, 'reference fixture changed')
const fixture = JSON.parse(fixtureBytes)
assert.equal(fixture.runs.length, 2, 'reference fixture must have two independent runs')
assert.deepEqual(fixture.runs[0], fixture.runs[1], 'reference runs differ')
const plan = fixture.runs[0]
const executableHash = createHash('sha256')
for await (const bytes of createReadStream(executablePath)) executableHash.update(bytes)
const { server, port } = await startServer()
let connection
try {
  connection = new Connection({
    server: '127.0.0.1',
    authentication: { type: 'default', options: { userName: 'sa', password: 'development' } },
    options: { port, encrypt: false, database: 'master', connectTimeout: 10000, requestTimeout: 30000 },
  })
  connection.on('error', () => {})
  await new Promise((accept, reject) => connection.connect(error => error ? reject(error) : accept()))
  const cases = []
  for (const { name, sql, result: reference } of plan) {
    if (name === 'server version') continue // ProductVersion is product-specific.
    const local = await captureTokens(connection, sql)
    const delta = differences(local, reference).map(difference => annotateDifference(difference, reference))
    cases.push({ name, sql, reference, local, differences: delta })
    console.log(`${delta.length ? 'different' : 'match'}: ${name} (${delta.length})`)
  }
  const summary = {
    matched: cases.filter(item => !item.differences.length).length,
    different: cases.filter(item => item.differences.length).length,
    floatBitDifferences: cases.flatMap(item => item.differences).filter(item => item.kind === 'float-bits').length,
    otherDifferences: cases.flatMap(item => item.differences).filter(item => item.kind === 'other').length,
  }
  const output = {
    sourceRevision, referenceImage: fixture.image, referenceSha256: fixtureSha256,
    executableSha256: executableHash.digest('hex'),
    excluded: [{ name: 'server version', reason: 'ProductVersion is product-specific' }],
    summary, cases,
  }
  await mkdir(dirname(outputPath), { recursive: true })
  await writeFile(outputPath, `${JSON.stringify(output, null, 2)}\n`)
  console.log(JSON.stringify({ output: outputPath, summary }))
} finally {
  connection?.close()
  server.kill('SIGTERM')
}
