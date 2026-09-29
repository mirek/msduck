#!/usr/bin/env node
// Capture SQL Server's allocator behavior when an explicit identity value fails conversion.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/identity-insert-conversion.json', import.meta.url)
const fixtureSha256 = 'ff35b24b314cdd6529bfdcb56392a5f584e10f97fb4f7bd5674d746b4d922cab'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-identity-insert-conversion.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/identity-insert-conversion/capture.json')

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

const snapshot = "SELECT id,v FROM dbo.conversion ORDER BY id; SELECT CONVERT(VARCHAR(40),IDENT_CURRENT('dbo.conversion')) AS current_value, @@TRANCOUNT AS tran_count, XACT_STATE() AS xact_state"
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['create table', 'CREATE TABLE dbo.conversion(id INT IDENTITY(10,2) PRIMARY KEY, v INT NOT NULL)'],
  ['baseline generated', 'INSERT dbo.conversion(v) VALUES(1)'],
  ['baseline state', snapshot],
  ['setting ON', 'SET IDENTITY_INSERT dbo.conversion ON'],
  ['malformed quoted identity', "INSERT dbo.conversion(id,v) VALUES('bad',2)"],
  ['state after malformed', snapshot],
  ['overflow quoted identity', "INSERT dbo.conversion(id,v) VALUES('2147483648',3)"],
  ['state after overflow', snapshot],
  ['NULL identity', 'INSERT dbo.conversion(id,v) VALUES(NULL,4)'],
  ['state after NULL', snapshot],
  ['malformed converted identity', "INSERT dbo.conversion(id,v) VALUES(CONVERT(INT,'bad'),5)"],
  ['state after converted', snapshot],
  ['explicit success', 'INSERT dbo.conversion(id,v) VALUES(100,6)'],
  ['state after success', snapshot],
  ['setting OFF', 'SET IDENTITY_INSERT dbo.conversion OFF'],
  ['generated after failures', 'INSERT dbo.conversion(v) VALUES(7)'],
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
    const record = run.find(item => item.name === name)
    assert(record, `missing ${name}`)
    return record.result
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
  const failures = {
    'malformed quoted identity': [245, "Conversion failed when converting the varchar value 'bad' to data type int.", 253, ['ERROR', 'DONE']],
    'overflow quoted identity': [248, "The conversion of the varchar value '2147483648' overflowed an int column.", 195, ['ERROR', 'INFO', 'DONE']],
    'NULL identity': [339, 'DEFAULT or NULL are not allowed as explicit identity values.', 253, ['ERROR', 'DONE']],
    'malformed converted identity': [245, "Conversion failed when converting the varchar value 'bad' to data type int.", 253, ['ERROR', 'DONE']],
  }
  for (const { name, result } of run) {
    const expected = failures[name]
    if (expected) {
      const [number, message, command, events] = expected
      assertSameCapture(result.errors.map(({ number, state, class: severity, message }) =>
        [number, state, severity, message]), [[number, 1, 16, message]], `${name}: diagnostic changed`)
      assertSameCapture(result.doneTokens.map(({ kind, status, command: actual, rowCount }) =>
        [kind, status, actual, rowCount]), [['DONE', 2, command, null]], `${name}: completion changed`)
      assertSameCapture(result.events.map(event => event.kind), events, `${name}: event order changed`)
      assertSameCapture(result.sets, [], `${name}: failure published a result set`)
    } else assertSameCapture(result.errors, [], `${name}: unexpected error`)
  }
  const stateCases = [
    ['baseline state', [[10, 1]], '10', 16],
    ['state after malformed', [[10, 1]], '10', 24],
    ['state after overflow', [[10, 1]], '10', 24],
    ['state after NULL', [[10, 1]], '10', 24],
    ['state after converted', [[10, 1]], '10', 24],
    ['state after success', [[10, 1], [100, 6]], '100', 24],
    ['final state', [[10, 1], [100, 6], [102, 7]], '102', 16],
  ]
  const shape = columns => columns.map(({ name, type, length, flags }) => [name, type, length, flags])
  for (const [name, rows, current, identityFlags] of stateCases) {
    const result = get(name)
    assertSameCapture(result.sets.map(set => set.rows), [rows, [[current, 0, 1]]], `${name}: rows or allocator changed`)
    assertSameCapture(result.sets.map(set => shape(set.columns)), [
      [['id', 'Int', null, identityFlags], ['v', 'Int', null, 8]],
      [['current_value', 'VarChar', 40, 33], ['tran_count', 'Int', null, 32], ['xact_state', 'IntN', 2, 33]],
    ], `${name}: descriptors changed`)
    assertSameCapture(result.doneTokens.map(({ kind, status, command, rowCount }) => [kind, status, command, rowCount]),
      [['DONE', 17, 193, rows.length], ['DONE', 16, 193, 1]], `${name}: completion changed`)
  }
  for (const [name, command] of [['setting ON', 183], ['setting OFF', 184]]) {
    assertSameCapture(get(name).doneTokens.map(({ kind, status, command: actual, rowCount }) => [kind, status, actual, rowCount]),
      [['DONE', 0, command, null]], `${name}: setting completion changed`)
  }
  for (const name of ['baseline generated', 'explicit success', 'generated after failures']) {
    assertSameCapture(get(name).doneTokens.map(({ kind, status, command, rowCount }) => [kind, status, command, rowCount]),
      [['DONE', 16, 195, 1]], `${name}: INSERT completion changed`)
  }
  assertSameCapture(get('session reusable').sets.map(set => set.rows), [[[1]]], 'session became unusable')
}

async function checkFixture() {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'reference repetitions missing')
  for (const run of retained.runs) validate(run)
  assertSameCapture(stableRun(retained.runs[0]), stableRun(retained.runs[1]), 'reference runs differ')
  console.log(`Checked ${retained.runs[0].length} IDENTITY_INSERT conversion observations in two retained runs`)
}

// Fresh database names in diagnostics remain raw in the fixture and are masked
// only for run-equivalence checks.
function stableRun(run) {
  return run.map(entry => ({ ...entry, result: {
    ...entry.result,
    errors: entry.result.errors.map(error => ({ ...error,
      message: error.message.replace(/msduck_audit_[0-9a-f]{32}/g, '<fresh-database>'),
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
    console.log(`Captured ${runs[0].length} IDENTITY_INSERT conversion observations in two fresh databases`)
  })
}
