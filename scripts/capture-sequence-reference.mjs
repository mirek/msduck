#!/usr/bin/env node
// Raw SQL Server 2025 evidence for user-defined sequence semantics.
import assert from 'node:assert/strict'
import { access, mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { Request } from 'tedious'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/sequence-reference.json', import.meta.url)
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-sequence-reference.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/sequence-reference/capture.json')
const catalog = name => `SELECT name,TYPE_NAME(user_type_id) AS type_name,start_value,increment AS increment_value,minimum_value,maximum_value,current_value,is_cycling,is_cached,is_exhausted,CONVERT(NVARCHAR(128),SQL_VARIANT_PROPERTY(start_value,'BaseType')) AS start_base_type,CONVERT(NVARCHAR(128),SQL_VARIANT_PROPERTY(increment,'BaseType')) AS increment_base_type,CONVERT(NVARCHAR(128),SQL_VARIANT_PROPERTY(minimum_value,'BaseType')) AS minimum_base_type,CONVERT(NVARCHAR(128),SQL_VARIANT_PROPERTY(maximum_value,'BaseType')) AS maximum_base_type,CONVERT(NVARCHAR(128),SQL_VARIANT_PROPERTY(current_value,'BaseType')) AS current_base_type FROM sys.sequences WHERE name='${name}'`
const cases = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version", 'A'],
  ['create ascending', 'CREATE SEQUENCE dbo.msduck_seq AS BIGINT START WITH 10 INCREMENT BY 2 MINVALUE 10 MAXVALUE 16 NO CYCLE NO CACHE', 'A'],
  ['catalog before allocation', catalog('msduck_seq'), 'A'],
  ['first value', 'SELECT NEXT VALUE FOR dbo.msduck_seq AS value', 'A'],
  ['repeated reference per row', 'SELECT NEXT VALUE FOR dbo.msduck_seq AS first_value,NEXT VALUE FOR dbo.msduck_seq AS second_value', 'A'],
  ['ordered two rows', 'SELECT n,NEXT VALUE FOR dbo.msduck_seq OVER (ORDER BY n) AS value FROM (VALUES (1),(2)) v(n) ORDER BY n', 'A'],
  ['exhaustion', 'SELECT NEXT VALUE FOR dbo.msduck_seq AS value', 'A'],
  ['catalog after exhaustion', catalog('msduck_seq'), 'A'],
  ['restart ascending', 'ALTER SEQUENCE dbo.msduck_seq RESTART WITH 10', 'A'],
  ['allocation inside rollback', 'BEGIN TRANSACTION; SELECT NEXT VALUE FOR dbo.msduck_seq AS value; ROLLBACK TRANSACTION', 'A'],
  ['second session next after rollback', 'SELECT NEXT VALUE FOR dbo.msduck_seq AS value', 'B'],
  ['first session observes shared allocation', 'SELECT NEXT VALUE FOR dbo.msduck_seq AS value', 'A'],
  ['cycle at maximum', 'ALTER SEQUENCE dbo.msduck_seq RESTART WITH 16 CYCLE', 'A'],
  ['cycle maximum value', 'SELECT NEXT VALUE FOR dbo.msduck_seq AS value', 'A'],
  ['cycle wraps to minimum', 'SELECT NEXT VALUE FOR dbo.msduck_seq AS value', 'B'],
  ['catalog after cycle', catalog('msduck_seq'), 'A'],
  ['create descending', 'CREATE SEQUENCE dbo.msduck_desc AS SMALLINT START WITH 0 INCREMENT BY -1 MINVALUE -2 MAXVALUE 0 NO CYCLE NO CACHE', 'A'],
  ['descending rows', 'SELECT n,NEXT VALUE FOR dbo.msduck_desc OVER (ORDER BY n) AS value FROM (VALUES (1),(2),(3)) v(n) ORDER BY n', 'A'],
  ['descending exhaustion', 'SELECT NEXT VALUE FOR dbo.msduck_desc AS value', 'A'],
  ['descending catalog', catalog('msduck_desc'), 'A'],
  ['drop descending', 'DROP SEQUENCE dbo.msduck_desc', 'A'],
  ['drop ascending', 'DROP SEQUENCE dbo.msduck_seq', 'A'],
  ['catalog after drop', "SELECT name FROM sys.sequences WHERE name IN ('msduck_seq','msduck_desc') ORDER BY name", 'A'],
  ['connection reusable after errors', 'SELECT 1 AS reusable', 'A'],
]

function captureEvents(connection, sql) {
  return new Promise(resolve => {
    const result = { sets: [], done: [], errors: [], info: [], events: [], returnStatus: null }
    const request = new Request(sql, (error, rowCount) => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.rowCount = rowCount
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      resolve(result)
    })
    const onError = error => {
      result.errors.push({ number: error.number, state: error.state, class: error.class, lineNumber: error.lineNumber, message: error.message })
      result.events.push({ kind: 'ERROR', number: error.number, state: error.state, class: error.class })
    }
    const onInfo = info => {
      result.info.push({ number: info.number, state: info.state, class: info.class, lineNumber: info.lineNumber, message: info.message })
      result.events.push({ kind: 'INFO', number: info.number })
    }
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    request.on('columnMetadata', metadata => {
      result.sets.push({
        columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })), rows: [],
      })
      result.events.push({ kind: 'COLMETADATA', columns: metadata.length })
    })
    request.on('row', row => {
      result.sets.at(-1).rows.push(row.map(c => c.value))
      result.events.push({ kind: 'ROW' })
    })
    for (const name of ['done', 'doneInProc', 'doneProc']) request.on(name, (rowCount, more) => {
      result.done.push({ kind: name, rowCount: rowCount ?? null, more })
      result.events.push({ kind: name.toUpperCase(), rowCount: rowCount ?? null, more })
    })
    request.on('doneProc', (_count, _more, status) => { result.returnStatus = status })
    try { connection.execSqlBatch(request) }
    catch (error) {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.errors.push({ message: error.message, number: error.number ?? null })
      resolve(result)
    }
  })
}

function shape(result, name) {
  for (const key of ['sets', 'done', 'errors', 'info']) assert(Array.isArray(result?.[key]), `${name}: missing ${key}`)
  assert(result.done.length > 0, `${name}: missing completion`)
  assert(Array.isArray(result.events) && result.events.length > 0, `${name}: missing event order`)
  for (const set of result.sets) {
    assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: missing descriptors or rows`)
    for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
  }
}

function validate(run) {
  assertSameCapture(run.map(({ name, sql, session }) => [name, sql, session]), cases, 'sequence case list')
  const get = name => run.find(item => item.name === name).result
  for (const { name, result } of run) shape(result, name)
  for (const name of ['exhaustion', 'descending exhaustion']) {
    const result = get(name)
    assert.equal(result.errors.length, 1, `${name}: expected error`)
    assertSameCapture(
      [result.errors[0].number, result.errors[0].state, result.errors[0].class],
      [11728, 1, 16], `${name}: error identity`,
    )
    assert.equal(result.sets.length, 1, `${name}: descriptor before error`)
    assertSameCapture(result.sets[0].rows, [], `${name}: no partial row`)
    assertSameCapture(result.done, [{ kind: 'done', rowCount: null, more: false }], `${name}: final completion`)
    assertSameCapture(result.events, [
      { kind: 'COLMETADATA', columns: 1 },
      { kind: 'ERROR', number: 11728, state: 1, class: 16 },
      { kind: 'DONE', rowCount: null, more: false },
    ], `${name}: metadata, error and DONE order`)
  }
  for (const { name, result } of run) {
    if (!['exhaustion', 'descending exhaustion'].includes(name)) assert.equal(result.errors.length, 0, `${name}: unexpected error`)
  }
  const rows = name => get(name).sets[0].rows
  assertSameCapture(rows('first value'), [['10']], 'first sequence value')
  assertSameCapture(rows('repeated reference per row'), [['12', '12']], 'one allocation per row')
  assertSameCapture(rows('ordered two rows'), [[1, '14'], [2, '16']], 'ordered allocation')
  assertSameCapture(rows('allocation inside rollback'), [['10']], 'rollback allocation')
  assertSameCapture(rows('second session next after rollback'), [['12']], 'rollback does not restore sequence')
  assertSameCapture(rows('first session observes shared allocation'), [['14']], 'sequence is database-wide')
  assertSameCapture(rows('cycle maximum value'), [['16']], 'cycle at maximum')
  assertSameCapture(rows('cycle wraps to minimum'), [['10']], 'cycle wraps to minimum')
  const bigintTypes = Array(5).fill('bigint')
  assertSameCapture(rows('catalog before allocation'), [['msduck_seq', 'bigint', '10', '2', '10', '16', '10', false, false, false, ...bigintTypes]], 'initial catalog')
  assertSameCapture(rows('catalog after exhaustion'), [['msduck_seq', 'bigint', '10', '2', '10', '16', '16', false, false, true, ...bigintTypes]], 'exhausted catalog')
  assertSameCapture(rows('catalog after cycle'), [['msduck_seq', 'bigint', '16', '2', '10', '16', '10', true, false, false, ...bigintTypes]], 'cycled catalog')
  assertSameCapture(rows('descending rows'), [[1, 0], [2, -1], [3, -2]], 'descending sequence')
  assertSameCapture(rows('descending catalog'), [['msduck_desc', 'smallint', 0, -1, -2, 0, -2, false, false, true, ...Array(5).fill('smallint')]], 'descending catalog')
  assertSameCapture(rows('catalog after drop'), [], 'dropped catalog rows')
  assertSameCapture(rows('connection reusable after errors'), [[1]], 'connection reuse')
  assert.equal(get('first value').sets[0].columns[0].type, 'BigInt')
  assert.equal(get('descending rows').sets[0].columns[1].type, 'SmallInt')
  for (const name of ['catalog before allocation', 'catalog after exhaustion', 'catalog after cycle', 'descending catalog']) {
    assertSameCapture(get(name).sets[0].columns.slice(2, 7).map(column => column.type),
      Array(5).fill('Variant'), `${name}: native SQL_VARIANT catalog descriptors`)
    assertSameCapture(get(name).sets[0].columns.slice(10, 15).map(column => column.type),
      Array(5).fill('NVarChar'), `${name}: base-type probe descriptors`)
  }
  assert.equal(get('exhaustion').sets[0].columns[0].type, 'BigInt')
  assert.equal(get('descending exhaustion').sets[0].columns[0].type, 'SmallInt')
  assertSameCapture(get('allocation inside rollback').done.map(({ kind, rowCount, more }) => [kind, rowCount, more]), [
    ['done', null, true], ['done', 1, true], ['done', null, false],
  ], 'transaction completion tokens')
}

async function observe(primary, config) {
  const database = (await capture(primary, 'SELECT DB_NAME() AS database_name')).sets[0].rows[0][0]
  const secondary = await connect({ ...config, options: { ...config.options, database } })
  try {
    const run = []
    for (const [name, sql, session] of cases) {
      const result = canonical(await captureEvents(session === 'A' ? primary : secondary, sql))
      run.push({ name, sql, session, result })
      console.log(name)
    }
    validate(run)
    return run
  } finally {
    if (!secondary.closed) await new Promise(resolve => { secondary.once('end', resolve); secondary.close() })
  }
}

if (check) {
  const retained = JSON.parse(await readFile(fixture, 'utf8'))
  assert.equal(retained.image, referenceImage)
  assert.equal(retained.freshDatabases, 2)
  assert.equal(retained.independentContainers, 2)
  validate(retained.results)
  console.log(`checked ${retained.results.length} sequence observations`)
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  try { await access(output); throw Error(`refusing to overwrite ${output}`) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  await mkdir(dirname(output), { recursive: true })
  const runs = []
  let image
  for (let index = 0; index < 2; index++) {
    await withReferenceContainer(async (config, container) => {
      assert.equal(container.image, referenceImage, 'unexpected SQL Server image')
      image ??= container.image
      assert.equal(container.image, image)
      const run = await isolatedReference(config, primary => observe(primary, config))
      if (runs.length) assertSameCapture(run, runs[0], 'independent sequence captures differ')
      runs.push(run)
    })
  }
  const actual = { image, freshDatabases: 2, independentContainers: 2, results: runs[0] }
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'fresh sequence capture differs from retained fixture')
  await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${actual.results.length} sequence observations in two fresh containers${retained ? '; matched retained fixture' : ''}`)
}
