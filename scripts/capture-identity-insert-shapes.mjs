#!/usr/bin/env node
// Capture SQL Server's IDENTITY_INSERT INSERT-shape diagnostic precedence.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/identity-insert-shapes.json', import.meta.url)
const fixtureSha256 = '425141ff3b10e7af1c0d7e6ee5615d5f7afb9d1bd4fc87e34892b79cb357dcd8'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-identity-insert-shapes.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/identity-insert-shapes/capture.json')

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
  ['create alpha', 'CREATE TABLE dbo.alpha(id INT IDENTITY(10,2) PRIMARY KEY, v INT NOT NULL)'],
  ['create source', 'CREATE TABLE dbo.source_rows(v INT NOT NULL)'],
  ['populate source', 'INSERT dbo.source_rows(v) VALUES(2)'],
  ['baseline insert', 'INSERT dbo.alpha(v) VALUES(1)'],
  ['baseline state', snapshot],
  ['OFF positional VALUES', 'INSERT dbo.alpha VALUES(20,2)'],
  ['state after OFF positional VALUES', snapshot],
  ['OFF positional DEFAULT', 'INSERT dbo.alpha VALUES(DEFAULT,3)'],
  ['state after OFF positional DEFAULT', snapshot],
  ['OFF explicit conversion', "INSERT dbo.alpha(id,v) VALUES(30,CONVERT(INT,'bad'))"],
  ['state after OFF explicit conversion', snapshot],
  ['alpha ON', 'SET IDENTITY_INSERT dbo.alpha ON'],
  ['ON positional VALUES', 'INSERT dbo.alpha VALUES(40,4)'],
  ['state after ON positional VALUES', snapshot],
  ['ON positional DEFAULT', 'INSERT dbo.alpha VALUES(DEFAULT,5)'],
  ['state after ON positional DEFAULT', snapshot],
  ['ON omitted INSERT SELECT', 'INSERT dbo.alpha(v) SELECT v FROM dbo.source_rows'],
  ['state after ON omitted SELECT', snapshot],
  ['ON duplicate identity columns', 'INSERT dbo.alpha(id,id,v) VALUES(50,52,6)'],
  ['state after ON duplicate columns', snapshot],
  ['ON invalid column', 'INSERT dbo.alpha(id,missing,v) VALUES(60,7,8)'],
  ['state after ON invalid column', snapshot],
  ['ON explicit conversion', "INSERT dbo.alpha(id,v) VALUES(70,CONVERT(INT,'bad'))"],
  ['state after ON explicit conversion', snapshot],
  ['ON omitted conversion', "INSERT dbo.alpha(v) SELECT CONVERT(INT,'bad')"],
  ['state after ON omitted conversion', snapshot],
  ['ON explicit INSERT SELECT', 'INSERT dbo.alpha(id,v) SELECT 80,v FROM dbo.source_rows'],
  ['state after ON explicit SELECT', snapshot],
  ['alpha OFF', 'SET IDENTITY_INSERT dbo.alpha OFF'],
  ['OFF omitted INSERT SELECT', 'INSERT dbo.alpha(v) SELECT v FROM dbo.source_rows'],
  ['state after OFF omitted SELECT', snapshot],
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
  const positional = "An explicit value for the identity column in table 'dbo.alpha' can only be specified when a column list is used and IDENTITY_INSERT is ON."
  const omitted = "Explicit value must be specified for identity column in table 'alpha' either when IDENTITY_INSERT is set to ON or when a replication user is inserting into a NOT FOR REPLICATION identity column."
  const errors = {
    'OFF positional VALUES': [8101, 1, 16, positional, 253],
    'OFF positional DEFAULT': [8101, 1, 16, positional, 253],
    'OFF explicit conversion': [544, 1, 16, "Cannot insert explicit value for identity column in table 'alpha' when IDENTITY_INSERT is set to OFF.", 195],
    'ON positional VALUES': [8101, 1, 16, positional, 253],
    'ON positional DEFAULT': [8101, 1, 16, positional, 253],
    'ON omitted INSERT SELECT': [545, 1, 16, omitted, 195],
    'ON duplicate identity columns': [264, 1, 16, "The column name 'id' is specified more than once in the SET clause or column list of an INSERT. A column cannot be assigned more than one value in the same clause. Modify the clause to make sure that a column is updated only once. If this statement updates or inserts columns into a view, column aliasing can conceal the duplication in your code.", 253],
    'ON invalid column': [207, 1, 16, "Invalid column name 'missing'.", 253],
    'ON explicit conversion': [245, 1, 16, "Conversion failed when converting the varchar value 'bad' to data type int.", 253],
    'ON omitted conversion': [545, 1, 16, omitted, 195],
  }
  for (const { name, result } of run) {
    if (Object.hasOwn(errors, name)) {
      const [number, state, severity, message, command] = errors[name]
      assertSameCapture(result.errors.map(({ number, state, class: actualClass, message }) =>
        [number, state, actualClass, message]), [[number, state, severity, message]], `${name}: diagnostic changed`)
      assertSameCapture(result.doneTokens.map(({ kind, status, command: actual, rowCount }) =>
        [kind, status, actual, rowCount]), [['DONE', 2, command, null]], `${name}: error completion changed`)
    } else assert.equal(result.errors.length, 0, `${name}: unexpected error`)
  }
  for (const [name, command] of [['alpha ON', 183], ['alpha OFF', 184]]) {
    assertSameCapture(get(name).doneTokens.map(({ kind, status, command: actual }) => [kind, status, actual]),
      [['DONE', 0, command]], `${name}: setting completion changed`)
  }
  const state = name => get(name).sets.map(set => set.rows)
  const baseline = [[[10, 1]], [['10', 0, 1]]]
  for (const name of [
    'baseline state', 'state after OFF positional VALUES', 'state after OFF positional DEFAULT',
    'state after OFF explicit conversion', 'state after ON positional VALUES',
    'state after ON positional DEFAULT', 'state after ON omitted SELECT',
    'state after ON duplicate columns', 'state after ON invalid column',
  ]) assertSameCapture(state(name), baseline, `${name}: baseline state changed`)
  for (const name of ['state after ON explicit conversion', 'state after ON omitted conversion']) {
    assertSameCapture(state(name), [[[10, 1]], [['70', 0, 1]]], `${name}: failed-write allocation changed`)
  }
  assertSameCapture(state('state after ON explicit SELECT'),
    [[[10, 1], [80, 2]], [['80', 0, 1]]], 'explicit SELECT state changed')
  for (const name of ['state after OFF omitted SELECT', 'final state']) {
    assertSameCapture(state(name), [[[10, 1], [80, 2], [82, 2]], [['82', 0, 1]]],
      `${name}: generated SELECT state changed`)
  }
  assertSameCapture(get('session reusable').sets.map(set => set.rows), [[[1]]], 'session became unusable')
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
  console.log(`Checked ${retained.runs[0].length} IDENTITY_INSERT shape observations in two retained runs`)
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
    console.log(`Captured ${runs[0].length} IDENTITY_INSERT shape observations in two fresh databases`)
  })
}
