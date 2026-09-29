#!/usr/bin/env node
// Capture SQL Server's multi-row IDENTITY_INSERT allocation and OUTPUT behavior.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/identity-insert-multirow.json', import.meta.url)
const fixtureSha256 = 'f8d284ef1ab7c5bce67bf67edc32b31c50fdcf97655488b4eefaac467e5c1498'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-identity-insert-multirow.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/identity-insert-multirow/capture.json')

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
  ['create source', 'CREATE TABLE dbo.source_rows(id INT PRIMARY KEY, v INT NOT NULL)'],
  ['baseline insert', 'INSERT dbo.alpha(v) VALUES(1)'],
  ['baseline state', snapshot],
  ['alpha ON', 'SET IDENTITY_INSERT dbo.alpha ON'],
  ['multi VALUES OUTPUT', 'INSERT dbo.alpha(id,v) OUTPUT inserted.id,inserted.v VALUES(20,2),(30,3)'],
  ['state after VALUES', snapshot],
  ['populate source', 'INSERT dbo.source_rows VALUES(40,4),(50,5)'],
  ['multi INSERT SELECT', 'INSERT dbo.alpha(id,v) SELECT id,v FROM dbo.source_rows ORDER BY id'],
  ['state after SELECT', snapshot],
  ['later UNIQUE with OUTPUT', 'INSERT dbo.alpha(id,v) OUTPUT inserted.id,inserted.v VALUES(60,6),(70,2)'],
  ['state after UNIQUE', snapshot],
  ['OFF after UNIQUE', 'SET IDENTITY_INSERT dbo.alpha OFF'],
  ['generated after UNIQUE', 'INSERT dbo.alpha(v) VALUES(7)'],
  ['state after generated UNIQUE', snapshot],
  ['ON after UNIQUE', 'SET IDENTITY_INSERT dbo.alpha ON'],
  ['later CHECK', 'INSERT dbo.alpha(id,v) VALUES(80,8),(90,-1)'],
  ['state after CHECK', snapshot],
  ['OFF after CHECK', 'SET IDENTITY_INSERT dbo.alpha OFF'],
  ['generated after CHECK', 'INSERT dbo.alpha(v) VALUES(9)'],
  ['state after generated CHECK', snapshot],
  ['ON after CHECK', 'SET IDENTITY_INSERT dbo.alpha ON'],
  ['later conversion', "INSERT dbo.alpha(id,v) VALUES(100,10),(110,CONVERT(INT,'bad'))"],
  ['state after conversion', snapshot],
  ['OFF after conversion', 'SET IDENTITY_INSERT dbo.alpha OFF'],
  ['generated after conversion', 'INSERT dbo.alpha(v) VALUES(11)'],
  ['state after generated conversion', snapshot],
  ['ON after conversion', 'SET IDENTITY_INSERT dbo.alpha ON'],
  ['populate failing source', 'INSERT dbo.source_rows VALUES(120,12),(130,2)'],
  ['later SELECT UNIQUE with OUTPUT', 'INSERT dbo.alpha(id,v) OUTPUT inserted.id,inserted.v SELECT id,v FROM dbo.source_rows WHERE id>=120 ORDER BY id'],
  ['state after SELECT UNIQUE', snapshot],
  ['OFF after SELECT UNIQUE', 'SET IDENTITY_INSERT dbo.alpha OFF'],
  ['generated after SELECT UNIQUE', 'INSERT dbo.alpha(v) VALUES(13)'],
  ['state after generated SELECT UNIQUE', snapshot],
  ['ON after SELECT UNIQUE', 'SET IDENTITY_INSERT dbo.alpha ON'],
  ['begin transaction', 'BEGIN TRANSACTION'],
  ['multi VALUES inside transaction', 'INSERT dbo.alpha(id,v) OUTPUT inserted.id,inserted.v VALUES(150,15),(160,16)'],
  ['state inside transaction', snapshot],
  ['rollback transaction', 'ROLLBACK TRANSACTION'],
  ['state after rollback', snapshot],
  ['final OFF', 'SET IDENTITY_INSERT dbo.alpha OFF'],
  ['generated after rollback', 'INSERT dbo.alpha(v) VALUES(17)'],
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
    'later UNIQUE with OUTPUT': [2627, 1, 14, 195],
    'later CHECK': [547, 0, 16, 195],
    'later conversion': [245, 1, 16, 253],
    'later SELECT UNIQUE with OUTPUT': [2627, 1, 14, 195],
  }
  for (const { name, result } of run) {
    if (Object.hasOwn(errors, name)) {
      const [number, state, severity, command] = errors[name]
      assert.equal(result.errors.length, 1, `${name}: wrong error count`)
      assertSameCapture([result.errors[0].number, result.errors[0].state, result.errors[0].class],
        [number, state, severity], `${name}: wrong diagnostic`)
      assert.equal(typeof result.errors[0].message, 'string', `${name}: missing message`)
      assertSameCapture(result.doneTokens.map(({ kind, status, command: actual, rowCount }) => [kind, status, actual, rowCount]),
        [['DONE', 2, command, null]], `${name}: wrong failed completion`)
    } else assert.equal(result.errors.length, 0, `${name}: unexpected error`)
  }
  const rows = name => get(name).sets.map(set => set.rows)
  const b = [[10, 1]]
  const v = [...b, [20, 2], [30, 3]]
  const s = [...v, [40, 4], [50, 5]]
  const u = [...s, [72, 7]]
  const c = [...u, [92, 9]]
  const x = [...c, [112, 11]]
  const q = [...x, [132, 13]]
  for (const [name, table, current, tranCount] of [
    ['baseline state', b, '10', 0], ['state after VALUES', v, '30', 0],
    ['state after SELECT', s, '50', 0], ['state after UNIQUE', s, '70', 0],
    ['state after generated UNIQUE', u, '72', 0], ['state after CHECK', u, '90', 0],
    ['state after generated CHECK', c, '92', 0], ['state after conversion', c, '110', 0],
    ['state after generated conversion', x, '112', 0], ['state after SELECT UNIQUE', x, '130', 0],
    ['state after generated SELECT UNIQUE', q, '132', 0],
    ['state inside transaction', [...q, [150, 15], [160, 16]], '160', 1],
    ['state after rollback', q, '160', 0], ['final state', [...q, [162, 17]], '162', 0],
  ]) assertSameCapture(rows(name), [table, [[current, tranCount, 1]]], `${name}: rows or allocation changed`)
  assertSameCapture(rows('multi VALUES OUTPUT'), [[[20, 2], [30, 3]]], 'successful VALUES OUTPUT changed')
  assertSameCapture(rows('multi INSERT SELECT'), [], 'INSERT SELECT unexpectedly returned rows')
  assertSameCapture(rows('later UNIQUE with OUTPUT'), [[[60, 6]]], 'failed VALUES OUTPUT row changed')
  assertSameCapture(rows('later SELECT UNIQUE with OUTPUT'), [[[120, 12]]], 'failed SELECT OUTPUT row changed')
  assertSameCapture(rows('multi VALUES inside transaction'), [[[150, 15], [160, 16]]], 'transaction OUTPUT changed')
  for (const name of ['multi VALUES OUTPUT', 'later UNIQUE with OUTPUT',
    'later SELECT UNIQUE with OUTPUT', 'multi VALUES inside transaction']) {
    assertSameCapture(get(name).sets[0].columns.map(({ name: column, type, length, flags }) =>
      [column, type, length, flags]), [['id', 'Int', null, 24], ['v', 'Int', null, 8]],
    `${name}: OUTPUT descriptors changed`)
  }
  for (const name of ['later UNIQUE with OUTPUT', 'later SELECT UNIQUE with OUTPUT']) {
    assertSameCapture(get(name).events.map(({ kind }) => kind),
      ['COLMETADATA', 'ROW', 'ERROR', 'INFO', 'DONE'], `${name}: failed OUTPUT event order changed`)
  }
  assertSameCapture(get('later CHECK').events.map(({ kind }) => kind),
    ['ERROR', 'INFO', 'DONE'], 'CHECK event order changed')
  assertSameCapture(get('later conversion').events.map(({ kind }) => kind),
    ['ERROR', 'DONE'], 'conversion event order changed')
  assertSameCapture(get('multi VALUES OUTPUT').doneTokens.map(({ kind, status, command, rowCount }) =>
    [kind, status, command, rowCount]), [['DONE', 16, 195, 2]], 'successful VALUES completion changed')
  assertSameCapture(get('multi INSERT SELECT').doneTokens.map(({ kind, status, command, rowCount }) =>
    [kind, status, command, rowCount]), [['DONE', 16, 195, 2]], 'successful SELECT completion changed')
  assertSameCapture(rows('session reusable'), [[[1]]], 'session became unusable')
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
  console.log(`Checked ${retained.runs[0].length} IDENTITY_INSERT multi-row observations in two retained runs`)
}

// Some constraint diagnostics include the random fresh database name.
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
    console.log(`Captured ${runs[0].length} IDENTITY_INSERT multi-row observations in two fresh databases`)
  })
}
