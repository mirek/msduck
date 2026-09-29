#!/usr/bin/env node
// SQL Server reference evidence; compare raw observations without tolerances.
import assert from 'node:assert/strict'
import { execFileSync, spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical, differences } from './lib/compatibility.mjs'

const root = fileURLToPath(new URL('../', import.meta.url))
const fixture = new URL('../reference/float-aggregates.json', import.meta.url)
const fixtureSha256 = 'e19bac07a0d08caf68fc94101a3ee1dee891faf6598002af44d8d93800492ce2'
const args = process.argv.slice(2)
const modes = args.filter(arg => ['--check', '--write-fixture', '--compare'].includes(arg))
const paths = args.filter(arg => !modes.includes(arg))
if (modes.length > 1 || paths.length > 1 || paths.some(arg => arg.startsWith('--'))) throw Error('usage: capture-float-aggregate-reference.mjs [--check | --write-fixture | --compare] [output]')
const mode = modes[0]
const output = resolve(paths[0] ?? `artifacts/compatibility/float-aggregates/${mode === '--compare' ? 'comparison' : 'capture'}.json`)
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const functions = ['SUM', 'AVG', 'MIN', 'MAX']
const value = (item, type) => item === null ? `CAST(NULL AS ${type})` : `CAST('${item}' AS ${type})`
const source = (type, values) => `(VALUES ${values.map((item, index) => `(${index + 1},${value(item, type)})`).join(',')}) s(id,n)`
const aggregate = (from, distinct = false, suffix = '') => `SELECT ${functions.map(name => `${name}(${distinct ? 'DISTINCT ' : ''}n) AS ${name.toLowerCase()}`).join(',')} FROM ${from} ${suffix}`
const window = (from, over) => `SELECT id,${functions.map(name => `${name}(n) OVER (${over}) AS ${name.toLowerCase()}`).join(',')} FROM ${from} ORDER BY id`
const plan = [['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"]]
for (const type of ['REAL', 'FLOAT(24)', 'FLOAT(53)']) {
  const from = source(type, ['0.1', '0.2', '0.3', null, '0.3'])
  plan.push(
    [`${type} fractional`, aggregate(from)],
    [`${type} distinct`, aggregate(from, true)],
    [`${type} singleton`, aggregate(source(type, ['3.25']))],
    [`${type} nulls`, aggregate(source(type, [null, null]))],
    [`${type} empty`, aggregate(from, false, 'WHERE 1=0')],
    [`${type} signed zero`, aggregate(source(type, ['-0.0', '0.0', '-0.0']))],
    [`${type} grouped`, `SELECT id%2 AS g,${functions.map(name => `${name}(n) AS ${name.toLowerCase()}`).join(',')} FROM ${from} GROUP BY id%2 ORDER BY g`],
    [`${type} ordered window`, window(from, 'ORDER BY id ROWS UNBOUNDED PRECEDING')],
    [`${type} bounded window`, window(from, 'ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING')],
    [`${type} empty window`, window(from, 'ORDER BY id ROWS BETWEEN 1 FOLLOWING AND 1 FOLLOWING')],
    [`${type} cancellation first`, aggregate(source(type, ['1e20', '-1e20', '3']))],
    [`${type} cancellation last`, aggregate(source(type, ['1e20', '3', '-1e20']))],
  )
}
plan.push(
  ['FLOAT adjacent precision', aggregate(source('FLOAT', ['9007199254740992', '1', '1', '1']))],
  ['FLOAT adjacent ordered', window(source('FLOAT', ['9007199254740992', '1', '1', '1']), 'ORDER BY id ROWS UNBOUNDED PRECEDING')],
  ['REAL wide sum', aggregate(source('REAL', ['3e38', '3e38']))],
  ['FLOAT sum overflow', `SELECT SUM(n) AS total FROM ${source('FLOAT', ['1e308', '1e308'])}`],
  ['reuse after sum overflow', 'SELECT 1 AS reusable'],
  ['FLOAT average overflow', `SELECT AVG(n) AS average FROM ${source('FLOAT', ['1e308', '1e308'])}`],
  ['reuse after average overflow', 'SELECT 1 AS reusable'],
  ['FLOAT cancellation overflow', `SELECT SUM(n) AS total FROM ${source('FLOAT', ['1e308', '1e308', '-1e308'])}`],
  ['session reusable', 'SELECT 1 AS reusable'],
)

async function existingFile(path, inspect = stat) {
  try { return await inspect(path) } catch (error) { if (error.code === 'ENOENT') return null; throw error }
}

async function canonicalOutput(path) {
  try { return await realpath(path) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return join(await realpath(dirname(path)), basename(path))
  }
}

async function captureTokens(connection, sql) {
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

function floatBits(result) {
  return result.sets.map(set => set.rows.map(row => row.map((value, index) => {
    const column = set.columns[index]
    if (!['FloatN', 'Real', 'Float'].includes(column.type) || value === null) return null
    const width = column.type === 'Real' ? 4 : column.type === 'Float' ? 8 : column.length
    assert([4, 8].includes(width), 'unexpected floating descriptor width')
    const number = typeof value === 'number' ? value : value?.kind === 'number' ? Number(value.value) : null
    assert.equal(typeof number, 'number', 'nonnumeric float result')
    const bytes = Buffer.alloc(width)
    if (width === 4) bytes.writeFloatBE(number); else bytes.writeDoubleBE(number)
    return bytes.toString('hex')
  })))
}

function validate(run) {
  assertSameCapture(run.map(({ name, sql }) => [name, sql]), plan, 'capture plan changed')
  for (const { name, result, bits } of run) {
    for (const field of ['sets', 'done', 'doneTokens', 'events', 'errors', 'info']) assert(Array.isArray(result[field]), `${name}: missing ${field}`)
    assert(result.done.length > 0, `${name}: missing completion`)
    assert.equal(result.doneTokens.length, result.done.length, `${name}: completion mismatch`)
    for (const token of result.doneTokens) assert(Number.isInteger(token.status), `${name}: missing raw DONE status`)
    for (const set of result.sets) {
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
    // JSON serializes -0 as 0; the separate bit field retains the wire sign.
    const zeros = sets => sets.map(set => set.map(row => row.map(bit =>
      ['80000000', '8000000000000000'].includes(bit) ? '0'.repeat(bit.length) : bit)))
    assertSameCapture(zeros(bits), zeros(floatBits(result)), `${name}: bits disagree with decoded rows`)
  }
  const byName = new Map(run.map(record => [record.name, record]))
  assertSameCapture(byName.get('server version').result.sets[0].rows, [['17.0.4065.4']], 'reference engine version changed')
  for (const { name, result } of run) {
    if (!/^(REAL|FLOAT\(24\)|FLOAT\(53\)) /.test(name)) continue
    assert.equal(result.errors.length, 0, `${name}: reference query failed`)
    assert.equal(result.sets.length, 1, `${name}: result set missing`)
    const offset = name.includes('window') || name.endsWith('grouped') ? 1 : 0
    const columns = result.sets[0].columns.slice(offset)
    assert.equal(columns.length, 4, `${name}: aggregate descriptor missing`)
    assertSameCapture(columns.map(column => [column.type, column.length]),
      [['FloatN', 8], ['FloatN', 8], ['FloatN', name.startsWith('FLOAT(53)') ? 8 : 4], ['FloatN', name.startsWith('FLOAT(53)') ? 8 : 4]], `${name}: aggregate declarations changed`)
  }
  for (const name of ['reuse after sum overflow', 'reuse after average overflow', 'session reusable']) {
    assertSameCapture(run.find(item => item.name === name).result.sets[0].rows, [[1]], `${name}: connection reuse failed`)
  }
  for (const name of ['FLOAT sum overflow', 'FLOAT average overflow', 'FLOAT cancellation overflow']) {
    assertSameCapture(run.find(item => item.name === name).result.errors.map(error => [error.number, error.state, error.class]), [[8115, 2, 16]], `${name}: overflow identity changed`)
  }
}

async function observe(connection) {
  const run = []
  for (const [name, sql] of plan) {
    const result = await captureTokens(connection, sql)
    run.push({ name, sql, result, bits: floatBits(result) })
    console.log(name)
  }
  return run
}

// Parsed JSON rows lose -0. Recover only that sign from retained wire bits
// before comparing rows; bit comparisons still distinguish either sign exactly.
function restoreZeroSign(result, bits) {
  const restored = structuredClone(result)
  restored.sets.forEach((set, si) => set.rows.forEach((row, ri) => row.forEach((value, ci) => {
    if (value === 0 && ['80000000', '8000000000000000'].includes(bits[si]?.[ri]?.[ci])) row[ci] = -0
  })))
  return restored
}

async function retainedFixture() {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'independent runs missing')
  for (const run of retained.runs) validate(run)
  assertSameCapture(retained.runs[0], retained.runs[1], 'independent runs differ')
  assert.equal(createHash('sha256').update(JSON.stringify(retained.runs[0])).digest('hex'), retained.captureSha256, 'capture checksum changed')
  return retained
}

async function protectOutput() {
  await mkdir(dirname(output), { recursive: true })
  if ((await existingFile(output, lstat))?.isSymbolicLink()) throw Error('capture output must not be a symbolic link')
  const fixturePath = fileURLToPath(fixture)
  if (await canonicalOutput(output) === await canonicalOutput(fixturePath)) throw Error('capture output must not be the retained fixture')
  const [outputFile, retainedFile] = await Promise.all([existingFile(output), existingFile(fixturePath)])
  if (outputFile && retainedFile && outputFile.dev === retainedFile.dev && outputFile.ino === retainedFile.ino) throw Error('capture output must not be a hard link to the retained fixture')
}

if (mode === '--check') {
  const retained = await retainedFixture()
  console.log(`Checked ${retained.runs[0].length} approximate aggregate observations in two retained runs`)
} else if (mode === '--compare') {
  const retained = await retainedFixture()
  await protectOutput()
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim()
  const sourceRevision = git('rev-parse', 'HEAD')
  assert.equal(git('status', '--porcelain=v1'), '', 'comparison requires a clean tracked checkout')
  execFileSync('cargo', ['build', '--workspace', '--all-targets', '--locked'], { cwd: root, stdio: 'inherit' })
  assert.equal(git('rev-parse', 'HEAD'), sourceRevision, 'revision changed during build')
  assert.equal(git('status', '--porcelain=v1'), '', 'build changed checkout')
  const executable = resolve(root, process.env.CARGO_TARGET_DIR ?? 'target', 'debug/msduck')
  const hash = createHash('sha256')
  for await (const bytes of createReadStream(executable)) hash.update(bytes)
  const server = spawn(executable, ['--listen', '127.0.0.1:0'], { cwd: root, stdio: ['ignore', 'ignore', 'pipe'] })
  let connection
  try {
    const port = await new Promise((accept, reject) => {
      const timeout = setTimeout(() => reject(Error('server startup timeout')), 30000)
      server.once('error', error => { clearTimeout(timeout); reject(error) })
      server.once('exit', code => { clearTimeout(timeout); reject(Error(`server exited: ${code}`)) })
      let logs = ''
      server.stderr.on('data', bytes => {
        logs = (logs + bytes.toString()).slice(-4096)
        const match = /listening on 127\.0\.0\.1:(\d+)/.exec(logs)
        if (match) { clearTimeout(timeout); accept(Number(match[1])) }
      })
    })
    connection = await connect({ server: '127.0.0.1', authentication: { type: 'default', options: { userName: 'sa', password: 'development' } }, options: { port, encrypt: false, database: 'master', requestTimeout: 30000 } })
    const local = await observe(connection)
    const cases = local.filter(item => item.name !== 'server version').map(item => {
      const reference = retained.runs[0].find(record => record.name === item.name)
      return { name: item.name, sql: item.sql, local: item.result, localBits: item.bits, reference: reference.result, referenceBits: reference.bits,
        differences: differences(item.result, restoreZeroSign(reference.result, reference.bits)), bitDifferences: differences(item.bits, reference.bits) }
    })
    const summary = { cases: cases.length, matched: cases.filter(item => !item.differences.length && !item.bitDifferences.length).length,
      floatBitDifferences: cases.reduce((sum, item) => sum + item.bitDifferences.length, 0), otherDifferences: cases.reduce((sum, item) => sum + item.differences.length, 0) }
    await writeFile(output, JSON.stringify({ sourceRevision, executableSha256: hash.digest('hex'), referenceSha256: fixtureSha256, referenceImage: retained.image,
      excluded: [{ name: 'server version', reason: 'ProductVersion is product-specific' }], summary, cases }, null, 2) + '\n', { flag: 'wx' })
    console.log(JSON.stringify(summary))
  } finally {
    if (connection && !connection.closed) await new Promise(resolve => { connection.once('end', resolve); connection.close() })
    if (server.exitCode === null && server.signalCode === null) {
      const ended = new Promise(resolve => server.once('exit', resolve))
      server.kill('SIGTERM')
      const timeout = setTimeout(() => server.kill('SIGKILL'), 5000)
      try { await ended } finally { clearTimeout(timeout) }
    }
  }
} else {
  if (mode === '--write-fixture') await refuseExistingFixture(fixture)
  await protectOutput()
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) throw Error('reference image must be pinned')
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    await withReferenceContainer(async (config, container) => {
      assert.equal(container.image, referenceImage, 'reference image must be pinned')
      const run = await isolatedReference(config, observe)
      validate(run)
      if (runs.length) assertSameCapture(run, runs[0], 'independent reference containers differ')
      runs.push(run)
    })
  }
  const actual = { image: referenceImage, captureSha256: createHash('sha256').update(JSON.stringify(runs[0])).digest('hex'), runs }
  if (mode !== '--write-fixture') assertSameCapture(JSON.parse(JSON.stringify(actual)), await retainedFixture(), 'fresh capture differs from retained fixture')
  await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
  if (mode === '--write-fixture') await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} approximate aggregate observations in two fresh containers`)
}
