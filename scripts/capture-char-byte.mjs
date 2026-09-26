#!/usr/bin/env node
// Exact CHAR byte, descriptor and RPC observations from pinned SQL Server.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/char-byte.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/char-byte/char-byte.json')

function prepared(connection, value) {
  return new Promise(resolve => {
    const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
    const request = new Request('SELECT CHAR(@n) AS c, CONVERT(VARBINARY(1), CHAR(@n)) AS raw', (error, rowCount) => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.rowCount = rowCount
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      resolve(canonical(result))
    })
    const onError = error => result.errors.push({ number: error.number, state: error.state, class: error.class, lineNumber: error.lineNumber, message: error.message })
    const onInfo = info => result.info.push({ number: info.number, state: info.state, class: info.class, lineNumber: info.lineNumber, message: info.message })
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    request.on('columnMetadata', metadata => result.sets.push({
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })), rows: []
    }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => c.value)))
    for (const name of ['done', 'doneInProc', 'doneProc']) request.on(name, (rowCount, more) => result.done.push({ kind: name, rowCount: rowCount ?? null, more }))
    request.on('doneProc', (_count, _more, status) => { result.returnStatus = status })
    request.addParameter('n', TYPES.Int, value)
    connection.execSql(request)
  })
}

function validate(run) {
  const get = name => {
    const item = run.find(item => item.name === name)
    assert(item, `missing ${name}`)
    return item.result
  }
  const identity = get('server identity')
  assert.match(identity.sets[0].rows[0][0], /^17\./)
  const matrix = get('all bytes')
  assert.deepEqual(matrix.errors, [])
  assert.equal(matrix.sets.length, 1)
  assert.equal(matrix.sets[0].rows.length, 256)
  assert.deepEqual(matrix.sets[0].columns.map(column => [column.name, column.type, column.length]), [
    ['code', 'IntN', 4], ['c', 'Char', 1], ['raw', 'VarBinary', 1],
    ['ascii_code', 'IntN', 4], ['unicode_code', 'IntN', 4], ['byte_length', 'IntN', 4]
  ])
  for (const [index, row] of matrix.sets[0].rows.entries()) {
    assert.equal(row[0], index)
    assert.deepEqual(row[2], { kind: 'binary', value: index.toString(16).padStart(2, '0') })
    assert.equal(row[3], index)
    assert.equal(row[5], 1)
  }
  for (const name of ['boundaries', 'fractions and text', 'empty metadata', 'invalid text', 'int overflow']) {
    const item = get(name)
    assert(item.done.length > 0, name)
  }
  for (const [index, value] of [65, 0, 128, 129, 255, 256, null, 65].entries()) {
    const item = get(index === 7 ? 'rpc 65 replay' : `rpc ${String(value)}`)
    assert.equal(item.sets.length, 1)
    assert.equal(item.sets[0].rows.length, 1)
    assert(item.done.length > 0)
  }
}

async function observe(connection) {
  const run = []
  async function record(name, sql) {
    run.push({ name, sql, result: canonical(await capture(connection, sql)) })
  }
  await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
  await record('all bytes', `WITH n AS (SELECT 0 AS v UNION ALL SELECT v+1 FROM n WHERE v<255)
SELECT v AS code, CHAR(v) AS c, CONVERT(VARBINARY(1), CHAR(v)) AS raw,
       ASCII(CHAR(v)) AS ascii_code, UNICODE(CHAR(v)) AS unicode_code,
       DATALENGTH(CHAR(v)) AS byte_length FROM n ORDER BY v OPTION (MAXRECURSION 0)`)
  await record('boundaries', 'SELECT CHAR(-1) AS negative,CHAR(0) AS zero,CHAR(255) AS maximum,CHAR(256) AS over_maximum,CHAR(NULL) AS null_code')
  await record('fractions and text', "SELECT CHAR(65.9) AS fractional,CHAR('66') AS numeric_text,CHAR('') AS empty_text")
  await record('empty metadata', 'SELECT CHAR(65) AS c WHERE 1=0')
  await record('invalid text', "SELECT CHAR('bad') AS c")
  await record('int overflow', 'SELECT CHAR(2147483648) AS c')
  for (const [index, value] of [65, 0, 128, 129, 255, 256, null, 65].entries()) {
    run.push({ name: index === 7 ? 'rpc 65 replay' : `rpc ${String(value)}`, sql: 'SELECT CHAR(@n) AS c, CONVERT(VARBINARY(1), CHAR(@n)) AS raw', parameter: value, result: await prepared(connection, value) })
  }
  validate(run)
  return run
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, observe))
  }
  assertSameCapture(runs[0], runs[1], 'raw CHAR observations differ across fresh databases')
  const actual = { image: container.image, runs }
  await writeFile(output, JSON.stringify(actual) + '\n')
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'raw CHAR observations differ from retained fixture')
  if (writeFixture) {
    await writeNewFixture(fixture, actual)
  }
  console.log(`Captured ${runs[0].length} CHAR observations in two fresh databases${retained ? ' and matched retained raw fixture' : ''}`)
})
