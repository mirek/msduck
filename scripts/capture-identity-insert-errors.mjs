#!/usr/bin/env node
// Capture SQL Server's IDENTITY_INSERT error and allocator behavior.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/identity-insert-errors.json', import.meta.url)
const fixtureSha256 = '45ae04f29a976b6b38866ea4c8142a8d28c354792b787c816d59d6471fa9b6ee'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-identity-insert-errors.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/identity-insert-errors/capture.json')

async function canonicalOutput(path) {
  try { return await realpath(path) }
  catch (error) {
    if (error.code !== 'ENOENT') throw error
    return join(await realpath(dirname(path)), basename(path))
  }
}

async function existingFile(path, inspect = stat) {
  try { return await inspect(path) }
  catch (error) {
    if (error.code === 'ENOENT') return null
    throw error
  }
}

const snapshot = "SELECT id,v FROM dbo.alpha ORDER BY id; SELECT CONVERT(VARCHAR(40),IDENT_CURRENT('dbo.alpha')) AS current_value, @@TRANCOUNT AS tran_count, XACT_STATE() AS xact_state"
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['create alpha', 'CREATE TABLE dbo.alpha(id INT IDENTITY(10,2) PRIMARY KEY, v INT NOT NULL CONSTRAINT uq_alpha_v UNIQUE, CONSTRAINT ck_alpha_v CHECK(v>0))'],
  ['create beta', 'CREATE TABLE dbo.beta(id INT IDENTITY(1,1) PRIMARY KEY, v INT)'],
  ['create plain', 'CREATE TABLE dbo.plain(v INT)'],
  ['baseline insert', 'INSERT dbo.alpha(v) VALUES(1)'],
  ['baseline state', snapshot],
  ['alpha ON', 'SET IDENTITY_INSERT dbo.alpha ON'],
  ['alpha ON repeated', 'SET IDENTITY_INSERT dbo.alpha ON'],
  ['beta OFF while alpha ON', 'SET IDENTITY_INSERT dbo.beta OFF'],
  ['plain ON', 'SET IDENTITY_INSERT dbo.plain ON'],
  ['missing ON', 'SET IDENTITY_INSERT dbo.missing ON'],
  ['state after setting errors', snapshot],
  ['DEFAULT VALUES while ON', 'INSERT dbo.alpha DEFAULT VALUES'],
  ['state after DEFAULT VALUES', snapshot],
  ['explicit high', 'INSERT dbo.alpha(id,v) VALUES(50,2)'],
  ['state after explicit high', snapshot],
  ['failed UNIQUE', 'INSERT dbo.alpha(id,v) VALUES(100,2)'],
  ['state after UNIQUE', snapshot],
  ['failed CHECK', 'INSERT dbo.alpha(id,v) VALUES(200,-1)'],
  ['state after CHECK', snapshot],
  ['failed NOT NULL', 'INSERT dbo.alpha(id,v) VALUES(300,NULL)'],
  ['state after NOT NULL', snapshot],
  ['failed conversion', "INSERT dbo.alpha(id,v) VALUES(400,CONVERT(INT,'bad'))"],
  ['state after conversion', snapshot],
  ['begin rollback probe', 'BEGIN TRANSACTION'],
  ['explicit inside transaction', 'INSERT dbo.alpha(id,v) VALUES(500,3)'],
  ['state inside transaction', snapshot],
  ['rollback explicit', 'ROLLBACK TRANSACTION'],
  ['state after rollback', snapshot],
  ['alpha OFF', 'SET IDENTITY_INSERT dbo.alpha OFF'],
  ['alpha OFF repeated', 'SET IDENTITY_INSERT dbo.alpha OFF'],
  ['generated after failures', 'INSERT dbo.alpha(v) VALUES(4)'],
  ['final state', snapshot],
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
    const result = canonical(await captureTokens(connection, sql))
    records.push({ name, sql, result })
    console.log(name)
  }
  return records
}

function validate(run) {
  assertSameCapture(run.map(({ name, sql }) => ({ name, sql })), plan, 'capture plan changed')
  const get = name => {
    const entry = run.find(item => item.name === name)
    assert(entry, `missing ${name}`)
    return entry.result
  }
  for (const { name, result } of run) {
    for (const key of ['sets', 'done', 'doneTokens', 'events', 'errors', 'info']) assert(Array.isArray(result[key]), `${name}: missing ${key}`)
    assert(result.done.length > 0, `${name}: missing completion`)
    assert.equal(result.doneTokens.length, result.done.length, `${name}: incomplete completion`)
    for (const token of result.doneTokens) assert(Number.isInteger(token.status), `${name}: missing DONE status`)
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete result set`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
  }
  const errors = {
    'plain ON': [8106, 1, 16, 253],
    'missing ON': [1088, 11, 16, 253],
    'DEFAULT VALUES while ON': [545, 1, 16, 195],
    'failed UNIQUE': [2627, 1, 14, 195],
    'failed CHECK': [547, 0, 16, 195],
    'failed NOT NULL': [515, 2, 16, 195],
    'failed conversion': [245, 1, 16, 253],
  }
  for (const { name, result } of run) {
    if (Object.hasOwn(errors, name)) {
      const [number, state, severity, command] = errors[name]
      assert.equal(result.errors.length, 1, `${name}: wrong error count`)
      assertSameCapture([result.errors[0].number, result.errors[0].state, result.errors[0].class],
        [number, state, severity], `${name}: wrong diagnostic`)
      assert.equal(typeof result.errors[0].message, 'string', `${name}: missing message`)
      assertSameCapture(result.doneTokens.map(({ kind, status, command: actual }) => [kind, status, actual]),
        [['DONE', 2, command]], `${name}: wrong error completion`)
    } else assert.equal(result.errors.length, 0, `${name}: unexpected error`)
  }
  for (const [name, command] of [
    ['alpha ON', 183], ['alpha ON repeated', 183], ['beta OFF while alpha ON', 184],
    ['alpha OFF', 184], ['alpha OFF repeated', 184],
  ]) assertSameCapture(get(name).doneTokens.map(({ kind, status, command: actual }) => [kind, status, actual]),
    [['DONE', 0, command]], `${name}: setting completion changed`)
  const state = name => get(name).sets.map(set => set.rows)
  const base = [[[10, 1]], [['10', 0, 1]]]
  for (const name of ['baseline state', 'state after setting errors', 'state after DEFAULT VALUES']) {
    assertSameCapture(state(name), base, `${name}: unexpected allocation or row`)
  }
  const row50 = [[10, 1], [50, 2]]
  for (const [name, current] of [
    ['state after explicit high', '50'], ['state after UNIQUE', '100'],
    ['state after CHECK', '200'], ['state after NOT NULL', '300'],
    ['state after conversion', '400'], ['state after rollback', '500'],
  ]) assertSameCapture(state(name), [row50, [[current, 0, 1]]], `${name}: allocator or rows changed`)
  assertSameCapture(state('state inside transaction'),
    [[[10, 1], [50, 2], [500, 3]], [['500', 1, 1]]], 'transaction state changed')
  assertSameCapture(state('final state'),
    [[[10, 1], [50, 2], [502, 4]], [['502', 0, 1]]], 'post-failure generation changed')
  assertSameCapture(state('session reusable'), [[[1]]], 'session became unusable')
  assertSameCapture(get('baseline state').sets[1].columns.map(({ name, type, length, flags }) =>
    [name, type, length, flags]), [
    ['current_value', 'VarChar', 40, 33], ['tran_count', 'Int', null, 32],
    ['xact_state', 'IntN', 2, 33],
  ], 'state descriptors changed')
}

async function checkFixture() {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'reference repetitions missing')
  for (const run of retained.runs) validate(run)
  assertSameCapture(stableRun(retained.runs[0]), stableRun(retained.runs[1]), 'reference runs differ')
  console.log(`Checked ${retained.runs[0].length} IDENTITY_INSERT error observations in two retained runs`)
}

// CHECK and NOT NULL diagnostics include the random fresh database name.
// Both raw names remain in the fixture; only equivalence checks mask them.
function stableRun(run) {
  return run.map(entry => ({ ...entry, result: {
    ...entry.result,
    errors: entry.result.errors.map(error => ({ ...error,
      message: [547, 515].includes(error.number)
        ? error.message.replace(/msduck_audit_[0-9a-f]{32}/g, '<fresh-database>')
        : error.message,
    })),
  } }))
}

if (check) await checkFixture()
else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if ((await existingFile(output, lstat))?.isSymbolicLink()) {
    throw Error('capture output must not be a symbolic link')
  }
  const fixturePath = fileURLToPath(fixture)
  if (await canonicalOutput(output) === await canonicalOutput(fixturePath)) {
    throw Error('capture output must not be the retained fixture')
  }
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
    assertSameCapture(stableRun(runs[0]), stableRun(runs[1]), 'fresh-database captures differ')
    const actual = { image: container.image, runs }
    await writeFile(output, JSON.stringify(actual) + '\n')
    let retained
    try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    if (retained) {
      assert.equal(actual.image, retained.image, 'reference image changed')
      assertSameCapture(actual.runs.map(stableRun), retained.runs.map(stableRun), 'live capture differs from retained fixture')
    }
    if (writeFixture) await writeNewFixture(fixture, actual)
    console.log(`Captured ${runs[0].length} IDENTITY_INSERT error observations in two fresh databases`)
  })
}
