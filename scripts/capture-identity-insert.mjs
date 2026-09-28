#!/usr/bin/env node
// Capture SQL Server's session-local IDENTITY_INSERT and allocator behavior.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/identity-insert.json', import.meta.url)
const fixtureSha256 = 'b4ad94e51ba1d5c11ba9c983126ce88f5052ec1816846743140621a9ff77ef18'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-identity-insert.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/identity-insert/capture.json')

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

const snapshot = "SELECT id,v FROM dbo.alpha ORDER BY id; SELECT id,v FROM dbo.beta ORDER BY id; SELECT CONVERT(VARCHAR(40),IDENT_CURRENT('dbo.alpha')) AS alpha_current, CONVERT(VARCHAR(40),IDENT_CURRENT('dbo.beta')) AS beta_current"
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['create alpha', 'CREATE TABLE dbo.alpha(id INT IDENTITY(1,1) PRIMARY KEY, v INT)'],
  ['create beta', 'CREATE TABLE dbo.beta(id INT IDENTITY(10,2) PRIMARY KEY, v INT)'],
  ['baseline inserts', 'INSERT dbo.alpha(v) VALUES(1); INSERT dbo.beta(v) VALUES(10)'],
  ['baseline snapshot', snapshot],
  ['A alpha on', 'SET IDENTITY_INSERT dbo.alpha ON'],
  ['A explicit without column list', 'INSERT dbo.alpha VALUES(100,2)'],
  ['A high explicit', 'INSERT dbo.alpha(id,v) VALUES(100,3)'],
  ['A high snapshot', snapshot],
  ['B explicit while off', 'INSERT dbo.alpha(id,v) VALUES(200,4)', 'B'],
  ['B alpha on', 'SET IDENTITY_INSERT [dbo].[alpha] ON', 'B'],
  ['B explicit while on', 'INSERT dbo.alpha(id,v) VALUES(200,5)', 'B'],
  ['A beta on conflicts', 'SET IDENTITY_INSERT dbo.beta ON'],
  ['A alpha still on', 'INSERT dbo.alpha(id,v) VALUES(5,6)'],
  ['A automatic while on', 'INSERT dbo.alpha(v) VALUES(7)'],
  ['B beta on conflicts', 'SET IDENTITY_INSERT dbo.beta ON', 'B'],
  ['B alpha off', 'SET IDENTITY_INSERT dbo.alpha OFF', 'B'],
  ['B explicit after off', 'INSERT dbo.alpha(id,v) VALUES(201,8)', 'B'],
  ['A alpha off', 'SET IDENTITY_INSERT dbo.alpha OFF'],
  ['A explicit after off', 'INSERT dbo.alpha(id,v) VALUES(202,9)'],
  ['A automatic after off', 'INSERT dbo.alpha(v) VALUES(9)'],
  ['A beta on after alpha off', 'SET IDENTITY_INSERT dbo.beta ON'],
  ['A beta explicit high', 'INSERT dbo.beta(id,v) VALUES(50,11)'],
  ['A beta automatic while on', 'INSERT dbo.beta(v) VALUES(12)'],
  ['A beta off', 'SET IDENTITY_INSERT dbo.beta OFF'],
  ['A beta automatic after off', 'INSERT dbo.beta(v) VALUES(12)'],
  ['before rollback probe', snapshot],
  ['transaction setting rollback', 'BEGIN TRANSACTION; SET IDENTITY_INSERT dbo.alpha ON; ROLLBACK TRANSACTION'],
  ['explicit after setting rollback', 'INSERT dbo.alpha(id,v) VALUES(300,13)'],
  ['transaction status', 'SELECT @@TRANCOUNT AS tran_count, XACT_STATE() AS xact_state'],
  ['after rollback probe', snapshot],
  ['A alpha off after rollback', 'SET IDENTITY_INSERT dbo.alpha OFF'],
  ['A explicit after final off', 'INSERT dbo.alpha(id,v) VALUES(301,14)'],
  ['session reusable', 'SELECT 1 AS reusable', 'B'],
].map(([name, sql, session = 'A']) => ({ name, sql, session }))

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

async function observe(primary, config) {
  const database = (await capture(primary, 'SELECT DB_NAME() AS name')).sets[0].rows[0][0]
  const secondary = await connect({ ...config, options: { ...config.options, database } })
  const records = []
  try {
    for (const { name, sql, session } of plan) {
      const result = canonical(await captureTokens(session === 'A' ? primary : secondary, sql))
      records.push({ name, sql, session, result })
      console.log(name)
    }
    return records
  } finally {
    if (!secondary.closed) await new Promise(resolve => { secondary.once('end', resolve); secondary.close() })
  }
}

function validate(run) {
  assertSameCapture(run.map(({ name, sql, session }) => ({ name, sql, session })), plan, 'capture plan changed')
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
  const expectedErrors = {
    'A explicit without column list': [8101, 253],
    'B explicit while off': [544, 195],
    'A beta on conflicts': [8107, 253],
    'A automatic while on': [545, 195],
    'B beta on conflicts': [8107, 253],
    'B explicit after off': [544, 195],
    'A explicit after off': [544, 195],
    'A beta automatic while on': [545, 195],
    'A explicit after final off': [544, 195],
  }
  for (const [name, [number, command]] of Object.entries(expectedErrors)) {
    const result = get(name)
    assert.equal(result.errors.length, 1, `${name}: wrong error count`)
    assert.equal(result.errors[0].number, number, `${name}: wrong diagnostic`)
    assert.equal(result.errors[0].state, 1, `${name}: wrong state`)
    assert.equal(result.errors[0].class, 16, `${name}: wrong severity`)
    assertSameCapture(result.doneTokens.map(({ kind, status, command: actual }) => [kind, status, actual]),
      [['DONE', 2, command]], `${name}: wrong error completion`)
  }
  for (const name of ['A high explicit', 'B explicit while on', 'A alpha still on',
    'A automatic after off', 'A beta explicit high', 'A beta automatic after off',
    'transaction setting rollback', 'explicit after setting rollback', 'session reusable']) {
    assert.equal(get(name).errors.length, 0, `${name}: unexpected error`)
  }
  const row = name => get(name).sets.at(-1).rows[0]
  assertSameCapture(row('baseline snapshot'), ['1', '10'], 'initial IDENT_CURRENT')
  assertSameCapture(row('A high snapshot'), ['100', '10'], 'explicit high did not reseed')
  assertSameCapture(row('before rollback probe'), ['201', '52'], 'high/low and cross-session allocation changed')
  assertSameCapture(get('before rollback probe').sets.slice(0, 2).map(set => set.rows), [
    [[1, 1], [5, 6], [100, 3], [200, 5], [201, 9]],
    [[10, 10], [50, 11], [52, 12]],
  ], 'identity rows changed')
  assertSameCapture(get('transaction status').sets[0].rows, [[0, 0]], 'transaction left active')
  assertSameCapture(row('after rollback probe'), ['300', '52'], 'setting rollback did not retain ON')
  assertSameCapture(get('after rollback probe').sets[0].rows.at(-1), [300, 13], 'post-rollback explicit row missing')
  const current = get('before rollback probe').sets[2].columns
  assertSameCapture(current.map(({ name, type, length, flags }) => [name, type, length, flags]), [
    ['alpha_current', 'VarChar', 40, 33], ['beta_current', 'VarChar', 40, 33],
  ], 'typed current-value descriptors changed')
  for (const [name, command] of [['A alpha on', 183], ['B alpha on', 183], ['A alpha off', 184],
    ['B alpha off', 184], ['A beta on after alpha off', 183], ['A beta off', 184]]) {
    assertSameCapture(get(name).doneTokens.map(({ kind, status, command: actual }) => [kind, status, actual]),
      [['DONE', 0, command]], `${name}: setting completion changed`)
  }
}

async function checkFixture() {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'reference repetitions missing')
  for (const run of retained.runs) validate(run)
  assertSameCapture(stableRun(retained.runs[0]), stableRun(retained.runs[1]), 'reference runs differ')
  console.log(`Checked ${retained.runs[0].length} IDENTITY_INSERT observations in two retained runs`)
}

// Error 8107 names the random fresh database. Retain each raw name in the
// fixture; ignore only that generated prefix when checking run equivalence.
function stableRun(run) {
  return run.map(entry => ({ ...entry, result: {
    ...entry.result,
    errors: entry.result.errors.map(error => ({ ...error,
      message: error.number === 8107
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
      const run = await isolatedReference(config, primary => observe(primary, config))
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
    console.log(`Captured ${runs[0].length} IDENTITY_INSERT observations in two fresh databases`)
  })
}
