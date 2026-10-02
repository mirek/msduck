#!/usr/bin/env node
// First-party ground truth; captured SQL and diagnostics are data.
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { TYPES } from 'tedious'
import { captureBatch, captureRpc } from './capture-order-token.mjs'
import { canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/quoted-session-identifiers.json', import.meta.url)
const source = 'dbo.quoted_session_columns'
const projection = '[@@OPTIONS], [@p], [@@TRANCOUNT]'
export const queries = [
  ['brackets', `SELECT ${projection} FROM ${source} WHERE id=1`],
  ['double quotes', `SELECT "@@OPTIONS", "@p", "@@TRANCOUNT" FROM ${source} WHERE id=1`],
  ['qualified', `SELECT q.[@@OPTIONS], q.[@p], q.[@@TRANCOUNT] FROM ${source} AS q WHERE q.id=1`],
  ['parentheses', `SELECT ([@@OPTIONS]) AS text_value, ([@p]) AS int_value, ([@@TRANCOUNT]) AS small_value FROM ${source} WHERE id=1`],
  ['empty', `SELECT ${projection} FROM ${source} WHERE id=-1`],
  ['null values', `SELECT ${projection} FROM ${source} WHERE id=2`],
  ['derived', `SELECT ${projection} FROM (SELECT ${projection}, id FROM ${source}) AS q WHERE id=1`],
  ['cte', `WITH q AS (SELECT ${projection}, id FROM ${source}) SELECT ${projection} FROM q WHERE id=1`],
  ['mixed counters', `SELECT @@OPTIONS AS live_options, [@@OPTIONS] AS stored_options, @@TRANCOUNT AS live_transactions, [@@TRANCOUNT] AS stored_transactions FROM ${source} WHERE id=1`],
  ['local variable', `DECLARE @p INT=42; SELECT @p AS parameter_value, [@p] AS column_value FROM ${source} WHERE id=1`],
  ['missing quoted counter', `SELECT [@@MISSING] FROM ${source}`],
  ['missing quoted parameter', `SELECT [@missing] FROM ${source}`],
  ['missing parameter', `SELECT @missing FROM ${source}`],
]

async function parameterRpc(connection, sql, value) {
  // The full observer owns its Request. Attach a typed input only to this
  // sequential invocation, then restore the connection even on failure.
  const original = connection.execSql
  connection.execSql = function (request) {
    request.addParameter('p', TYPES.Int, value)
    return original.call(this, request)
  }
  try { return await captureRpc(connection, sql) }
  finally { connection.execSql = original }
}

export async function observe(connection) {
  const records = []
  const step = async (name, mode, sql, action) => {
    records.push({ name, mode, sql, result: canonical(await action()) })
  }
  const setup = `SET QUOTED_IDENTIFIER ON; CREATE TABLE ${source}(id INT NOT NULL PRIMARY KEY, [@@OPTIONS] NVARCHAR(20) NULL, [@p] INT NULL, [@@TRANCOUNT] SMALLINT NOT NULL); INSERT ${source} VALUES(1,N'stored-options',7,12),(2,NULL,NULL,13)`
  await step('setup', 'batch', setup, () => captureBatch(connection, setup))
  for (const [name, sql] of queries) {
    for (const mode of ['batch', 'rpc']) {
      await step(name, mode, sql, () => mode === 'batch' ? captureBatch(connection, sql) : captureRpc(connection, sql))
    }
  }
  for (const [name, value, predicate] of [['bound parameter', 42, 'id=1'], ['null parameter', null, 'id=1'], ['empty parameter', 42, 'id=-1']]) {
    const sql = `SELECT @p AS parameter_value, [@p] AS column_value FROM ${source} WHERE ${predicate}`
    await step(name, 'rpc', sql, () => parameterRpc(connection, sql, value))
    records.at(-1).parameter = { name: 'p', type: 'Int', value }
  }
  return records
}

export function verify(records) {
  if (records.length !== 30) throw new Error('quoted identifier capture is incomplete')
  for (const record of records) {
    // The controlled source has two rows and every result query selects at
    // most one. Check the complete observation before retaining the fixture.
    if (record.result.sets.length > 1 || record.result.errors.length > 1 || record.result.info.length > 1
        || record.result.done.length > 4 || record.result.events.length > 16
        || record.result.sets.some(set => set.columns.length > 4 || set.rows.length > 1)) {
      throw new Error(`${record.name} exceeds the controlled capture bounds`)
    }
    const expectedError = record.name === 'missing parameter' ? 137 : record.name.startsWith('missing quoted') ? 207 : null
    if (expectedError !== null) {
      assertSameCapture(record.result.errors.map(error => error.number), [expectedError], `${record.name} error number`)
    } else if (record.result.errors.length) throw new Error(`${record.name} unexpectedly failed`)
    if (['brackets', 'double quotes', 'qualified', 'parentheses', 'derived', 'cte'].includes(record.name)) {
      assertSameCapture(record.result.sets.map(set => set.rows), [[['stored-options', 7, 12]]], `${record.name} stored values`)
    }
    if (record.name === 'null values') assertSameCapture(record.result.sets.map(set => set.rows), [[[null, null, 13]]], 'NULL source values')
    if (record.name === 'bound parameter' || record.name === 'local variable') assertSameCapture(record.result.sets.map(set => set.rows), [[[42, 7]]], 'variable and column stay distinct')
    if (record.name === 'null parameter') assertSameCapture(record.result.sets.map(set => set.rows), [[[null, 7]]], 'NULL variable and column stay distinct')
    if (record.name === 'empty' || record.name === 'empty parameter') {
      if (record.result.sets.length !== 1 || record.result.sets[0].rows.length !== 0) throw new Error('missing empty-result descriptors')
    }
    if (record.name === 'mixed counters') {
      const rows = record.result.sets.map(set => set.rows)
      if (typeof rows[0]?.[0]?.[0] !== 'number') throw new Error('live options counter is not numeric')
      assertSameCapture(rows[0][0].slice(1), ['stored-options', 0, 12], 'live and stored counters stay distinct')
    }
  }
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2)
  const writeFixture = args.includes('--write-fixture')
  const paths = args.filter(arg => arg !== '--write-fixture')
  if (paths.length > 1) throw new Error('expected at most one output path')
  const output = resolve(paths[0] ?? 'artifacts/compatibility/quoted-session-identifiers/capture.json')
  if (writeFixture) {
    await refuseExistingFixture(fixture)
    await mkdir(new URL('../reference/', import.meta.url), { recursive: true })
  }
  await mkdir(dirname(output), { recursive: true })
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      const records = await isolatedReference(config, observe)
      verify(records)
      runs.push(records)
    }
    assertSameCapture(runs[0], runs[1], 'quoted identifier observations differ across fresh databases')
    const capture = { image: container.image, runs }
    await writeFile(output, JSON.stringify(capture) + '\n')
    let retained
    try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    if (retained) assertSameCapture(capture, retained, 'quoted identifier observations differ from retained fixture')
    if (writeFixture) await writeNewFixture(fixture, capture)
    console.log(`Captured ${runs[0].length} complete observations in two fresh databases`)
  })
}
