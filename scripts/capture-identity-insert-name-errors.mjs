#!/usr/bin/env node
// Capture SQL Server's SET IDENTITY_INSERT target-name resolution diagnostics.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/identity-insert-name-errors.json', import.meta.url)
const fixtureSha256 = '4bae40ae8637a86a86a4807a45d6137129fa6c6da7c4c54fab0c4505d7da805c'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-identity-insert-name-errors.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/identity-insert-name-errors/capture.json')

async function existingFile(path, inspect = stat) {
  try { return await inspect(path) }
  catch (error) { if (error.code === 'ENOENT') return null; throw error }
}

async function canonicalOutput(path) {
  try { return await realpath(path) }
  catch (error) {
    if (error.code !== 'ENOENT') throw error
    return join(await realpath(dirname(path)), basename(path))
  }
}

const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['create dbo plain', 'CREATE TABLE dbo.plain(v INT)'],
  ['create dbo identity', 'CREATE TABLE dbo.ident(id INT IDENTITY(1,1))'],
  ['create other schema', 'CREATE SCHEMA other'],
  ['create other plain', 'CREATE TABLE other.plain(v INT)'],
  ['unqualified plain ON', 'SET IDENTITY_INSERT plain ON'],
  ['unqualified missing ON', 'SET IDENTITY_INSERT missing ON'],
  ['qualified plain ON', 'SET IDENTITY_INSERT dbo.plain ON'],
  ['qualified missing ON', 'SET IDENTITY_INSERT dbo.missing ON'],
  ['bracketed plain ON', 'SET IDENTITY_INSERT [plain] ON'],
  ['bracketed missing ON', 'SET IDENTITY_INSERT [missing] ON'],
  ['bracketed qualified plain ON', 'SET IDENTITY_INSERT [dbo].[plain] ON'],
  ['bracketed qualified missing ON', 'SET IDENTITY_INSERT [dbo].[missing] ON'],
  ['case-varied plain ON', 'SET IDENTITY_INSERT [DbO].[PLAIN] ON'],
  ['case-varied missing ON', 'SET IDENTITY_INSERT [DbO].[MISSING] ON'],
  ['other schema plain ON', 'SET IDENTITY_INSERT other.plain ON'],
  ['other schema missing ON', 'SET IDENTITY_INSERT other.missing ON'],
  ['unqualified identity ON', 'SET IDENTITY_INSERT ident ON'],
  ['bracketed identity OFF', 'SET IDENTITY_INSERT [DbO].[IDENT] OFF'],
  ['session reusable', 'SELECT 1 AS reusable'],
].map(([name, sql]) => ({ name, sql }))

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
    const result = await capture(connection, sql)
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

async function observe(connection) {
  const records = []
  for (const { name, sql } of plan) {
    records.push({ name, sql, result: canonical(await captureTokens(connection, sql)) })
    console.log(name)
  }
  return records
}

function validate(run) {
  assertSameCapture(run.map(({ name, sql }) => ({ name, sql })), plan, 'capture plan changed')
  const targetNames = new Map([
    ['unqualified plain ON', 'plain'], ['unqualified missing ON', 'missing'],
    ['qualified plain ON', 'dbo.plain'], ['qualified missing ON', 'dbo.missing'],
    ['bracketed plain ON', 'plain'], ['bracketed missing ON', 'missing'],
    ['bracketed qualified plain ON', 'dbo.plain'],
    ['bracketed qualified missing ON', 'dbo.missing'],
    ['case-varied plain ON', 'DbO.PLAIN'], ['case-varied missing ON', 'DbO.MISSING'],
    ['other schema plain ON', 'other.plain'], ['other schema missing ON', 'other.missing'],
  ])
  for (const { name, result } of run) {
    for (const key of ['sets', 'done', 'doneTokens', 'events', 'errors', 'info']) assert(Array.isArray(result[key]), `${name}: missing ${key}`)
    assert(result.done.length > 0, `${name}: missing completion`)
    assert.equal(result.doneTokens.length, result.done.length, `${name}: incomplete completion`)
    for (const token of result.doneTokens) {
      assert(Number.isInteger(token.status), `${name}: missing raw DONE status`)
      assert(Number.isInteger(token.command), `${name}: missing raw DONE command`)
    }
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete set`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
    const requested = targetNames.get(name)
    if (requested) {
      const missing = name.includes('missing')
      const expected = missing
        ? [1088, 11, 16, `Cannot find the object "${requested}" because it does not exist or you do not have permissions.`]
        : [8106, 1, 16, `Table '${requested}' does not have the identity property. Cannot perform SET operation.`]
      assertSameCapture(result.errors.map(({ number, state, class: severity, message }) => [number, state, severity, message]),
        [expected], `${name}: diagnostic changed`)
      assertSameCapture(result.events.map(({ kind }) => kind), ['ERROR', 'DONE'], `${name}: event order changed`)
      assertSameCapture(result.doneTokens.map(({ kind, status, command }) => [kind, status, command]),
        [['DONE', 2, 253]], `${name}: error completion changed`)
      assert.equal(result.sets.length, 0, `${name}: unexpected result set`)
    } else if (name === 'unqualified identity ON' || name === 'bracketed identity OFF') {
      assert.equal(result.errors.length, 0, `${name}: unexpected error`)
      assertSameCapture(result.events.map(({ kind }) => kind), ['DONE'], `${name}: event order changed`)
      assertSameCapture(result.doneTokens.map(({ kind, status, command }) => [kind, status, command]),
        [['DONE', 0, name.endsWith('ON') ? 183 : 184]], `${name}: SET completion changed`)
    } else {
      assert.equal(result.errors.length, 0, `${name}: unexpected setup or reuse error`)
    }
  }
  const reusable = run.at(-1).result.sets[0]
  assertSameCapture(reusable.rows, [[1]], 'session did not remain reusable')
  assertSameCapture(reusable.columns.map(({ name, type, length, flags }) => [name, type, length, flags]),
    [['reusable', 'Int', null, 32]], 'reuse descriptor changed')
}

async function checkFixture() {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'reference repetitions missing')
  for (const run of retained.runs) validate(run)
  assertSameCapture(retained.runs[0], retained.runs[1], 'reference runs differ')
  console.log(`Checked ${retained.runs[0].length} target-name observations in two retained runs`)
}

if (check) await checkFixture()
else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if ((await existingFile(output, lstat))?.isSymbolicLink()) throw Error('capture output must not be a symbolic link')
  const fixturePath = fileURLToPath(fixture)
  if (await canonicalOutput(output) === await canonicalOutput(fixturePath)) throw Error('capture output must not be the retained fixture')
  const [outputFile, retainedFile] = await Promise.all([existingFile(output), existingFile(fixturePath)])
  if (outputFile && retainedFile && outputFile.dev === retainedFile.dev && outputFile.ino === retainedFile.ino) throw Error('capture output must not be a hard link to the retained fixture')
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) throw Error('reference image must be pinned')
  await withReferenceContainer(async (config, container) => {
    assert.equal(container.image, referenceImage, 'reference image must be pinned')
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      const run = await isolatedReference(config, observe)
      validate(run)
      runs.push(run)
      await writeFile(output, JSON.stringify({ image: container.image, runs }) + '\n')
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
    console.log(`Captured ${runs[0].length} target-name observations in two fresh databases`)
  })
}
