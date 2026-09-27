#!/usr/bin/env node
// Capture Tedious 20 pool reset against pinned SQL Server 2025.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/session-reset.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/session-reset/capture.json')
const state = `SELECT @@TRANCOUNT AS transaction_count, XACT_STATE() AS transaction_state,
  @@DATEFIRST AS datefirst, CASE WHEN (@@OPTIONS & 512) = 512 THEN 1 ELSE 0 END AS nocount,
  CASE WHEN OBJECT_ID('tempdb..#reset_probe') IS NULL THEN 0 ELSE 1 END AS temp_exists`

// Observe only message metadata and response events; do not retain packet payloads.
async function reset(connection) {
  const result = { messages: [], sets: [], done: [], errors: [], info: [], callback: null }
  const outgoing = connection.messageIo.outgoingMessageStream
  const originalWrite = outgoing.write
  const originalBatch = connection.execSqlBatch
  const onError = error => result.errors.push({ number: error.number, state: error.state, class: error.class, lineNumber: error.lineNumber, message: error.message })
  const onInfo = info => result.info.push({ number: info.number, state: info.state, class: info.class, lineNumber: info.lineNumber, message: info.message })
  outgoing.write = function(message, ...rest) {
    result.messages.push({ type: message.type, resetConnection: message.resetConnection })
    return originalWrite.call(this, message, ...rest)
  }
  connection.execSqlBatch = function(request) {
    request.on('columnMetadata', metadata => result.sets.push({
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })), rows: []
    }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => c.value)))
    for (const name of ['done', 'doneInProc', 'doneProc']) request.on(name, (rowCount, more) => result.done.push({ kind: name, rowCount: rowCount ?? null, more }))
    return originalBatch.call(this, request)
  }
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  try {
    await new Promise(resolve => connection.reset(error => {
      result.callback = error ? { message: error.message, code: error.code ?? null, number: error.number ?? null } : null
      resolve()
    }))
  } finally {
    outgoing.write = originalWrite
    connection.execSqlBatch = originalBatch
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
  return canonical(result)
}

async function observe(connection) {
  const records = []
  async function record(name, sql) {
    const result = canonical(await capture(connection, sql))
    records.push({ name, sql, result })
    return result
  }
  await record('initial state', state)
  await record('settings and temp setup', 'SET DATEFIRST 3; SET NOCOUNT ON; CREATE TABLE #reset_probe (n INT); INSERT #reset_probe VALUES (7)')
  await record('before settings reset', `${state}; SELECT n FROM #reset_probe`)
  records.push({ name: 'settings reset', result: await reset(connection) })
  await record('after settings reset', state)
  await record('reused connection', 'SELECT 42 AS answer')

  await record('create durable table', 'CREATE TABLE dbo.reset_marker (n INT)')
  await record('open transaction and insert', 'BEGIN TRANSACTION; INSERT dbo.reset_marker VALUES (9)')
  await record('before transaction reset', `${state}; SELECT n FROM dbo.reset_marker`)
  records.push({ name: 'transaction reset', result: await reset(connection) })
  await record('after transaction reset', `${state}; SELECT COUNT(*) AS marker_count FROM dbo.reset_marker`)
  await record('reused after transaction reset', 'SELECT 43 AS answer')
  return records
}

function validate(run) {
  const get = name => {
    const item = run.find(item => item.name === name)
    assert(item, `missing ${name}`)
    return item.result
  }
  for (const name of ['settings reset', 'transaction reset']) {
    const result = get(name)
    assert.deepEqual(result.messages, [{ type: 1, resetConnection: true }])
    assert.equal(result.callback, null)
    assert.deepEqual(result.errors, [])
    assert(result.done.length > 0)
  }
  for (const name of ['initial state', 'before settings reset', 'after settings reset', 'before transaction reset', 'after transaction reset']) {
    assert.equal(get(name).errors.length, 0, name)
    assert(get(name).done.length > 0, name)
  }
  const row = name => get(name).sets[0].rows[0]
  assert.equal(row('before settings reset')[2], 3)
  assert.equal(row('before settings reset')[3], 1)
  assert.equal(row('before settings reset')[4], 1)
  assert.equal(row('after settings reset')[4], 0)
  assert.equal(row('before transaction reset')[0], 1)
  assert.equal(row('after transaction reset')[0], 0)
  assert.deepEqual(get('after transaction reset').sets[1].rows, [[0]])
  assert.deepEqual(get('reused connection').sets[0].rows, [[42]])
  assert.deepEqual(get('reused after transaction reset').sets[0].rows, [[43]])
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, observe)
    validate(run)
    runs.push(run)
  }
  assertSameCapture(runs[0], runs[1], 'session reset differs across fresh databases')
  const actual = { image: container.image, runs }
  await writeFile(output, JSON.stringify(actual) + '\n')
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'session reset differs from retained fixture')
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} reset observations in two fresh databases${retained ? ' and matched retained fixture' : ''}`)
})
