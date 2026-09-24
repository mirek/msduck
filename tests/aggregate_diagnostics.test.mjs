import assert from 'node:assert/strict'
import { readFile, mkdir, writeFile } from 'node:fs/promises'
import { test } from 'node:test'
import { Request, TYPES } from 'tedious'
import { start, query } from './support/client.mjs'
import { capture, canonical, differences } from '../scripts/lib/compatibility.mjs'

async function orderedCapture(connection, sql) {
  const events = []
  const info = message => events.push({ kind: 'info', number: message.number })
  const error = message => events.push({ kind: 'error', number: message.number })
  connection.on('infoMessage', info)
  connection.on('errorMessage', error)
  // Adapt the capture helper's connection interface without mutating a shared
  // connection method or evaluating the SQL twice.
  const observed = {
    on: connection.on.bind(connection),
    off: connection.off.bind(connection),
    execSqlBatch(request) {
      request.on('columnMetadata', () => events.push({ kind: 'metadata' }))
      request.on('row', () => events.push({ kind: 'row' }))
      for (const kind of ['done', 'doneInProc', 'doneProc']) {
        request.on(kind, (count, more) => events.push({ kind, rowCount: count ?? null, more }))
      }
      connection.execSqlBatch(request)
    },
  }
  try { return canonical({ ...await capture(observed, sql), events }) }
  finally {
    connection.off('infoMessage', info)
    connection.off('errorMessage', error)
  }
}

const fixture = JSON.parse(await readFile(new URL('../reference/aggregate-warnings.json', import.meta.url), 'utf8'))
const boundaries = JSON.parse(await readFile(new URL('../reference/aggregate-warning-boundaries.json', import.meta.url), 'utf8'))

async function replayBoundaries(t, samples, artifact) {
  const connection = await start(t)
  const records = []
  for (const sample of samples) {
    for (const sql of [...sample.setup, `SET ANSI_WARNINGS ${sample.mode}`]) {
      assert.deepEqual((await capture(connection, sql)).errors, [], sql)
    }
    const actual = []
    for (const _ of sample.executions) {
      const result = await orderedCapture(connection, sample.sql)
      const state = await orderedCapture(connection, 'SELECT @@ERROR AS last_error,@@ROWCOUNT AS last_rowcount,@@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state')
      const contents = await orderedCapture(connection, sample.followup)
      actual.push({ result, state, contents })
    }
    records.push({ id: sample.id, actual, expected: sample.executions, differences: differences(actual, sample.executions) })
  }
  await mkdir('artifacts/compatibility', { recursive: true })
  await writeFile(`artifacts/compatibility/${artifact}.json`, JSON.stringify(records, null, 2) + '\n')
  assert.deepEqual(records.flatMap(record => record.differences.map(difference => ({ id: record.id, ...difference }))), [])
}

test('window diagnostics match consumed frames including empty frames and COUNT', async t => {
  await replayBoundaries(t, boundaries.results.filter(sample => sample.id.includes('-window-')), 'aggregate-window-boundaries')
})

test('DML and scalar assignment diagnostics match reference state and completion order', async t => {
  const selected = new Set(['insert-select', 'insert-empty-source', 'insert-grouped', 'update-scalar', 'update-no-targets', 'delete-subquery', 'select-into', 'select-assignment', 'set-subquery', 'declare-subquery'])
  await replayBoundaries(t, boundaries.results.filter(sample => selected.has(sample.id.slice(sample.mode.length + 1))), 'aggregate-consumer-boundaries')
})

test('prepared aggregate executions keep diagnostic state isolated and honor setting changes', async t => {
  const connection = await start(t)
  const info = []
  connection.on('infoMessage', message => info.push(message.number))
  let complete = () => {}
  const request = new Request('SELECT MIN(v) AS result FROM (VALUES(@v),(2)) d(v)', error => complete(error))
  request.addParameter('v', TYPES.Int)
  await new Promise((resolve, reject) => {
    request.once('prepared', resolve)
    request.once('error', reject)
    connection.prepare(request)
  })
  assert.deepEqual(info, [], 'preparation must not observe inputs')
  for (const [mode, value, expected, warnings] of [
    ['ON', null, 2, [8153]],
    ['ON', 1, 1, []],
    ['OFF', null, 2, []],
    ['ON', null, 2, [8153]],
  ]) {
    await query(connection, `SET ANSI_WARNINGS ${mode}`)
    info.length = 0
    const rows = []
    const row = cells => rows.push(cells.map(cell => cell.value))
    request.on('row', row)
    try {
      await new Promise((resolve, reject) => {
        complete = error => error ? reject(error) : resolve()
        request.error = undefined
        connection.execute(request, { v: value })
      })
    } finally { request.off('row', row) }
    assert.deepEqual(rows, [[expected]])
    assert.deepEqual(info, warnings)
  }
  await new Promise((resolve, reject) => {
    complete = error => error ? reject(error) : resolve()
    connection.unprepare(request)
  })
})

for (const mode of ['ON', 'OFF']) {
  test(`aggregate warnings ANSI_WARNINGS ${mode} match SQL Server results and event order`, async t => {
    const connection = await start(t)
    await query(connection, `SET ANSI_WARNINGS ${mode}`)
    const results = []
    for (const sample of fixture.results.filter(sample => sample.mode === mode)) {
      const actual = await orderedCapture(connection, sample.sql)
      results.push({ id: sample.id, sql: sample.sql, actual, expected: sample.result, differences: differences(actual, sample.result) })
    }
    await mkdir('artifacts/compatibility', { recursive: true })
    await writeFile(`artifacts/compatibility/aggregate-warnings-${mode}.json`, JSON.stringify(results, null, 2) + '\n')
    assert.deepEqual(results.flatMap(result => result.differences.map(difference => ({ id: result.id, ...difference }))), [])
  })
}
