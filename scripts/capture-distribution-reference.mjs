#!/usr/bin/env node
// Independent SQL Server evidence for PERCENT_RANK and CUME_DIST windows.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/distribution-reference.json', import.meta.url)
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-distribution-reference.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/distribution-reference/capture.json')
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const source = '(VALUES (1,1,NULL),(2,1,10),(3,1,10),(4,1,20),(5,1,30),(6,2,7)) s(id,g,n)'
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['ties and nulls ascending', `SELECT id,PERCENT_RANK() OVER (PARTITION BY g ORDER BY n) AS p,CUME_DIST() OVER (PARTITION BY g ORDER BY n) AS c FROM ${source} ORDER BY id`],
  ['ties and nulls descending', `SELECT id,PERCENT_RANK() OVER (PARTITION BY g ORDER BY n DESC) AS p,CUME_DIST() OVER (PARTITION BY g ORDER BY n DESC) AS c FROM ${source} ORDER BY id`],
  ['all null', 'SELECT id,PERCENT_RANK() OVER (ORDER BY n) AS p,CUME_DIST() OVER (ORDER BY n) AS c FROM (VALUES (1,NULL),(2,NULL)) s(id,n) ORDER BY id'],
  ['singleton', 'SELECT PERCENT_RANK() OVER (ORDER BY n) AS p,CUME_DIST() OVER (ORDER BY n) AS c FROM (VALUES (7)) s(n)'],
  ['empty', `SELECT PERCENT_RANK() OVER (ORDER BY n) AS p,CUME_DIST() OVER (ORDER BY n) AS c FROM ${source} WHERE 1=0`],
  ['named window', `SELECT id,PERCENT_RANK() OVER w AS p,CUME_DIST() OVER w AS c FROM ${source} WINDOW w AS (ORDER BY n) ORDER BY id`],
  ['missing order percent rank', `SELECT PERCENT_RANK() OVER (PARTITION BY g) FROM ${source}`],
  ['missing order cume dist', `SELECT CUME_DIST() OVER (PARTITION BY g) FROM ${source}`],
  ['missing over percent rank', `SELECT PERCENT_RANK() FROM ${source}`],
  ['missing over cume dist', `SELECT CUME_DIST() FROM ${source}`],
  ['frame percent rank', `SELECT PERCENT_RANK() OVER (ORDER BY n ROWS UNBOUNDED PRECEDING) FROM ${source}`],
  ['frame cume dist', `SELECT CUME_DIST() OVER (ORDER BY n ROWS UNBOUNDED PRECEDING) FROM ${source}`],
  ['argument percent rank', `SELECT PERCENT_RANK(1) OVER (ORDER BY n) FROM ${source}`],
  ['argument cume dist', `SELECT CUME_DIST(1) OVER (ORDER BY n) FROM ${source}`],
  ['session reusable', 'SELECT 1 AS reusable'],
]

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

function validate(run) {
  assertSameCapture(run.map(({ name, sql }) => [name, sql]), plan, 'capture plan changed')
  for (const { name, result } of run) {
    for (const field of ['sets', 'done', 'doneTokens', 'events', 'errors', 'info']) assert(Array.isArray(result[field]), `${name}: missing ${field}`)
    assert(result.done.length > 0, `${name}: missing completion`)
    assert.equal(result.doneTokens.length, result.done.length, `${name}: completion mismatch`)
    for (const token of result.doneTokens) assert(Number.isInteger(token.status), `${name}: missing raw DONE status`)
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete result`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
  }
  const get = name => run.find(item => item.name === name).result
  assertSameCapture(get('session reusable').sets[0].rows, [[1]], 'session unusable after errors')
  assertSameCapture(get('server version').sets[0].rows, [['17.0.4065.4']], 'server version changed')
  const rows = name => get(name).sets[0].rows
  assertSameCapture(rows('ties and nulls ascending'), [
    [1, 0, 0.2], [2, 0.25, 0.6], [3, 0.25, 0.6],
    [4, 0.75, 0.8], [5, 1, 1], [6, 0, 1],
  ], 'ascending distribution changed')
  assertSameCapture(rows('ties and nulls descending'), [
    [1, 1, 1], [2, 0.5, 0.8], [3, 0.5, 0.8],
    [4, 0.25, 0.4], [5, 0, 0.2], [6, 0, 1],
  ], 'descending distribution changed')
  assertSameCapture(rows('all null'), [[1, 0, 1], [2, 0, 1]], 'all-NULL distribution changed')
  assertSameCapture(rows('singleton'), [[0, 1]], 'singleton distribution changed')
  assertSameCapture(rows('empty'), [], 'empty distribution changed')
  for (const name of ['ties and nulls ascending', 'ties and nulls descending', 'all null', 'singleton', 'empty', 'named window']) {
    const columns = get(name).sets[0].columns.slice(-2)
    assertSameCapture(columns.map(({ type, length, flags }) => [type, length, flags]),
      [['FloatN', 8, 1], ['FloatN', 8, 1]], `${name}: distribution descriptors changed`)
    assertSameCapture(get(name).doneTokens.map(({ kind, status, command }) => [kind, status, command]),
      [['DONE', 16, 193]], `${name}: successful completion changed`)
  }
  for (const [name, number, state, message] of [
    ['missing order percent rank', 4112, 1, "The function 'PERCENT_RANK' must have an OVER clause with ORDER BY."],
    ['missing order cume dist', 4112, 1, "The function 'CUME_DIST' must have an OVER clause with ORDER BY."],
    ['missing over percent rank', 10753, 3, "The function 'PERCENT_RANK' must have an OVER clause."],
    ['missing over cume dist', 10753, 3, "The function 'CUME_DIST' must have an OVER clause."],
    ['frame percent rank', 10752, 3, "The function 'PERCENT_RANK' may not have a window frame."],
    ['frame cume dist', 10752, 3, "The function 'CUME_DIST' may not have a window frame."],
    ['argument percent rank', 4114, 1, "The function 'PERCENT_RANK' takes exactly 0 argument(s)."],
    ['argument cume dist', 4114, 1, "The function 'CUME_DIST' takes exactly 0 argument(s)."],
  ]) {
    const result = get(name)
    assertSameCapture(result.errors.map(error => [error.number, error.state, error.class, error.message]),
      [[number, state, 15, message]], `${name}: diagnostic changed`)
    assertSameCapture(result.sets, [], `${name}: unexpected descriptor`)
    assertSameCapture(result.events.map(event => event.kind), ['ERROR', 'DONE'], `${name}: token order changed`)
    assertSameCapture(result.doneTokens.map(({ kind, status, command }) => [kind, status, command]),
      [['DONE', 2, 253]], `${name}: error completion changed`)
  }
}

async function observe(connection) {
  const run = []
  for (const [name, sql] of plan) {
    run.push({ name, sql, result: await captureTokens(connection, sql) })
    console.log(name)
  }
  validate(run)
  return run
}

if (check) {
  const bytes = await readFile(fixture)
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'independent runs missing')
  assert.equal(createHash('sha256').update(JSON.stringify(retained.runs[0])).digest('hex'), retained.captureSha256, 'capture checksum changed')
  for (const run of retained.runs) validate(run)
  assertSameCapture(retained.runs[0], retained.runs[1], 'independent runs differ')
  console.log(`Checked ${retained.runs[0].length} distribution observations in two retained runs`)
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if ((await existingFile(output, lstat))?.isSymbolicLink()) throw Error('capture output must not be a symbolic link')
  const fixturePath = fileURLToPath(fixture)
  if (await canonicalOutput(output) === await canonicalOutput(fixturePath)) throw Error('capture output must not be the retained fixture')
  const [outputFile, retainedFile] = await Promise.all([existingFile(output), existingFile(fixturePath)])
  if (outputFile && retainedFile && outputFile.dev === retainedFile.dev && outputFile.ino === retainedFile.ino) throw Error('capture output must not be a hard link to the retained fixture')
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) throw Error('reference image must be pinned')
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    await withReferenceContainer(async (config, container) => {
      assert.equal(container.image, referenceImage, 'reference image must be pinned')
      const run = await isolatedReference(config, observe)
      if (runs.length) assertSameCapture(run, runs[0], 'independent reference containers differ')
      runs.push(run)
    })
  }
  const actual = { image: referenceImage, captureSha256: createHash('sha256').update(JSON.stringify(runs[0])).digest('hex'), runs }
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) } catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'fresh capture differs from retained fixture')
  await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} distribution observations in two fresh containers${retained ? '; matched fixture' : ''}`)
}
