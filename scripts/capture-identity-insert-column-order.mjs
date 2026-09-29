#!/usr/bin/env node
// Capture SQL Server's IDENTITY_INSERT with an identity column after the first physical column.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/identity-insert-column-order.json', import.meta.url)
const fixtureSha256 = '339a0cfaa7396922bda74fdc5806296dcf9ac04db85d0273aeea3bcf78d9e9c4'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-identity-insert-column-order.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/identity-insert-column-order/capture.json')

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

const snapshot = "SELECT v,id,note FROM dbo.late ORDER BY id; SELECT CONVERT(VARCHAR(40),IDENT_CURRENT('dbo.late')) AS current_value, @@TRANCOUNT AS tran_count, XACT_STATE() AS xact_state"
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['create late identity', 'CREATE TABLE dbo.late(v INT NOT NULL, id INT IDENTITY(10,2) PRIMARY KEY, note NVARCHAR(20) NULL)'],
  ['baseline generated', "INSERT dbo.late(v,note) VALUES(1,N'base')"],
  ['baseline state', snapshot],
  ['OFF explicit first in list', "INSERT dbo.late(id,v,note) VALUES(30,2,N'off-first')"],
  ['OFF explicit second in list', "INSERT dbo.late(v,id,note) VALUES(2,30,N'off-second')"],
  ['OFF positional VALUES', "INSERT dbo.late VALUES(2,30,N'positional')"],
  ['state after OFF errors', snapshot],
  ['ON quoted alias', 'SET IDENTITY_INSERT [DbO].[LATE] ON'],
  ['ON omitted identity', "INSERT dbo.late(v,note) VALUES(2,N'omitted')"],
  ['state after ON omitted', snapshot],
  ['ON explicit first in list', "INSERT dbo.late(id,v,note) VALUES(30,3,N'first')"],
  ['state after first explicit', snapshot],
  ['ON explicit second in list', "INSERT dbo.late(v,id,note) VALUES(4,50,N'second')"],
  ['state after second explicit', snapshot],
  ['ON positional VALUES', "INSERT dbo.late VALUES(5,60,N'positional')"],
  ['state after ON positional', snapshot],
  ['ON quoted alias reordered OUTPUT', "INSERT [DbO].[LaTe]([note],[v],[id]) OUTPUT inserted.id,inserted.v,inserted.note VALUES(N'alias',5,70)"],
  ['state after alias OUTPUT', snapshot],
  ['ON explicit lower value', "INSERT dbo.late(v,id,note) VALUES(6,20,N'low')"],
  ['state after lower explicit', snapshot],
  ['OFF quoted alias', 'SET IDENTITY_INSERT [dbo].[late] OFF'],
  ['generated after OFF', "INSERT dbo.late(v,note) VALUES(7,N'generated')"],
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
    'OFF explicit first in list': [544, 195, "Cannot insert explicit value for identity column in table 'late' when IDENTITY_INSERT is set to OFF."],
    'OFF explicit second in list': [544, 195, "Cannot insert explicit value for identity column in table 'late' when IDENTITY_INSERT is set to OFF."],
    'OFF positional VALUES': [8101, 253, "An explicit value for the identity column in table 'dbo.late' can only be specified when a column list is used and IDENTITY_INSERT is ON."],
    'ON omitted identity': [545, 195, "Explicit value must be specified for identity column in table 'late' either when IDENTITY_INSERT is set to ON or when a replication user is inserting into a NOT FOR REPLICATION identity column."],
    'ON positional VALUES': [8101, 253, "An explicit value for the identity column in table 'dbo.late' can only be specified when a column list is used and IDENTITY_INSERT is ON."],
  }
  for (const { name, result } of run) {
    const expected = errors[name]
    if (expected) {
      const [number, command, message] = expected
      assertSameCapture(result.errors.map(({ number, state, class: severity, message }) =>
        [number, state, severity, message]), [[number, 1, 16, message]], `${name}: diagnostic changed`)
      assertSameCapture(result.doneTokens.map(({ kind, status, command: actual, rowCount }) =>
        [kind, status, actual, rowCount]), [['DONE', 2, command, null]], `${name}: error completion changed`)
      assertSameCapture(result.sets, [], `${name}: error published a result set`)
    } else assertSameCapture(result.errors, [], `${name}: unexpected error`)
  }
  for (const [name, command] of [['ON quoted alias', 183], ['OFF quoted alias', 184]]) {
    assertSameCapture(get(name).doneTokens.map(({ kind, status, command: actual }) => [kind, status, actual]),
      [['DONE', 0, command]], `${name}: setting completion changed`)
  }
  const states = [
    ['baseline state', [[1, 10, 'base']], '10'],
    ['state after OFF errors', [[1, 10, 'base']], '10'],
    ['state after ON omitted', [[1, 10, 'base']], '10'],
    ['state after first explicit', [[1, 10, 'base'], [3, 30, 'first']], '30'],
    ['state after second explicit', [[1, 10, 'base'], [3, 30, 'first'], [4, 50, 'second']], '50'],
    ['state after ON positional', [[1, 10, 'base'], [3, 30, 'first'], [4, 50, 'second']], '50'],
    ['state after alias OUTPUT', [[1, 10, 'base'], [3, 30, 'first'], [4, 50, 'second'], [5, 70, 'alias']], '70'],
    ['state after lower explicit', [[1, 10, 'base'], [6, 20, 'low'], [3, 30, 'first'], [4, 50, 'second'], [5, 70, 'alias']], '70'],
    ['final state', [[1, 10, 'base'], [6, 20, 'low'], [3, 30, 'first'], [4, 50, 'second'], [5, 70, 'alias'], [7, 72, 'generated']], '72'],
  ]
  for (const [name, rows, current] of states) {
    assertSameCapture(get(name).sets.map(set => set.rows), [rows, [[current, 0, 1]]], `${name}: rows or allocator changed`)
    assert.equal(get(name).sets[0].columns[1].flags,
      ['baseline state', 'state after OFF errors', 'final state'].includes(name) ? 16 : 24,
      `${name}: stored identity descriptor flags changed`)
    assertSameCapture(get(name).doneTokens.map(({ kind, status, command, rowCount }) =>
      [kind, status, command, rowCount]),
    [['DONE', 17, 193, rows.length], ['DONE', 16, 193, 1]], `${name}: state completion changed`)
  }
  const shape = columns => columns.map(({ name, type, length, flags }) => [name, type, length, flags])
  assertSameCapture(get('baseline state').sets.map(set => shape(set.columns)), [
    [['v', 'Int', null, 8], ['id', 'Int', null, 16], ['note', 'NVarChar', 40, 9]],
    [['current_value', 'VarChar', 40, 33], ['tran_count', 'Int', null, 32], ['xact_state', 'IntN', 2, 33]],
  ], 'state descriptors changed')
  const output = get('ON quoted alias reordered OUTPUT')
  assertSameCapture(output.sets.map(set => set.rows), [[[70, 5, 'alias']]], 'reordered OUTPUT row changed')
  assertSameCapture(output.sets.map(set => shape(set.columns)), [
    [['id', 'Int', null, 24], ['v', 'Int', null, 8], ['note', 'NVarChar', 40, 9]],
  ], 'reordered OUTPUT descriptors changed')
  assertSameCapture(output.events.map(event => event.kind), ['COLMETADATA', 'ROW', 'DONE'], 'reordered OUTPUT token order changed')
  assertSameCapture(output.doneTokens.map(({ kind, status, command, rowCount }) =>
    [kind, status, command, rowCount]), [['DONE', 16, 195, 1]], 'reordered OUTPUT completion changed')
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
  console.log(`Checked ${retained.runs[0].length} IDENTITY_INSERT column-order observations in two retained runs`)
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
    console.log(`Captured ${runs[0].length} IDENTITY_INSERT column-order observations in two fresh databases`)
  })
}
