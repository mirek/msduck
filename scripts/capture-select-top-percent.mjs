#!/usr/bin/env node
// First-party SQL Server SELECT TOP PERCENT / WITH TIES reference evidence.
import assert from 'node:assert/strict'
import { mkdir, readFile, stat, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/select-top-percent.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const selfTest = args.includes('--self-test')
const positional = args.filter(arg => !['--write-fixture', '--self-test'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (selfTest && (writeFixture || positional.length)) throw new Error('--self-test takes no other arguments')
const output = resolve(positional[0] ?? 'artifacts/compatibility/select-top-percent/capture.json')
const fixturePath = fileURLToPath(fixture)

async function rejectFixtureOutput() {
  if (output === fixturePath) throw new Error('output path must not be the retained fixture')
  const info = async path => {
    try { return await stat(path) }
    catch (error) { if (error.code === 'ENOENT') return null; throw error }
  }
  // stat follows symlinks and also identifies hard links to the fixture.
  const [outInfo, fixtureInfo] = await Promise.all([info(output), info(fixturePath)])
  if (outInfo && fixtureInfo && outInfo.dev === fixtureInfo.dev && outInfo.ino === fixtureInfo.ino) {
    throw new Error('output path aliases the retained fixture')
  }
}

const source = 'dbo.top_percent_source'
const cases = [
  ['zero percent', `SELECT TOP (0) PERCENT id,score FROM ${source} ORDER BY id`],
  ['one percent', `SELECT TOP (1) PERCENT id,score FROM ${source} ORDER BY id`],
  ['fraction below boundary', `SELECT TOP (14.285) PERCENT id,score FROM ${source} ORDER BY id`],
  ['fraction above boundary', `SELECT TOP (14.286) PERCENT id,score FROM ${source} ORDER BY id`],
  ['quarter percent', `SELECT TOP (25) PERCENT id,score FROM ${source} ORDER BY id`],
  ['third percent', `SELECT TOP (33.333) PERCENT id,score FROM ${source} ORDER BY id`],
  ['half percent', `SELECT TOP (50) PERCENT id,score FROM ${source} ORDER BY id`],
  ['full percent', `SELECT TOP (100) PERCENT id,score FROM ${source} ORDER BY id`],
  ['above full percent', `SELECT TOP (101) PERCENT id,score FROM ${source} ORDER BY id`],
  ['negative percent', `SELECT TOP (-1) PERCENT id,score FROM ${source} ORDER BY id`],
  ['null percent', `SELECT TOP (CAST(NULL AS FLOAT)) PERCENT id,score FROM ${source} ORDER BY id`],
  ['fractional percent', `SELECT TOP (1.5) PERCENT id,score FROM ${source} ORDER BY id`],
  ['empty percent', `SELECT TOP (50) PERCENT id,score FROM ${source} WHERE 1=0 ORDER BY id`],
  ['aggregate percent', `SELECT TOP (50) PERCENT COUNT(*) AS total FROM ${source}`],
  ['count zero ties', `SELECT TOP (0) WITH TIES id,score FROM ${source} ORDER BY score DESC`],
  ['count two ties', `SELECT TOP (2) WITH TIES id,score FROM ${source} ORDER BY score DESC`],
  ['count three ties', `SELECT TOP (3) WITH TIES id,score FROM ${source} ORDER BY score DESC`],
  ['count two unique', `SELECT TOP (2) WITH TIES id,score FROM ${source} ORDER BY score DESC,id`],
  ['quarter percent ties', `SELECT TOP (25) PERCENT WITH TIES id,score FROM ${source} ORDER BY score DESC`],
  ['half percent ties', `SELECT TOP (50) PERCENT WITH TIES id,score FROM ${source} ORDER BY score DESC`],
  ['full percent ties', `SELECT TOP (100) PERCENT WITH TIES id,score FROM ${source} ORDER BY score DESC`],
  ['empty ties', `SELECT TOP (2) WITH TIES id,score FROM ${source} WHERE 1=0 ORDER BY score DESC`],
  ['ties without order', `SELECT TOP (2) WITH TIES id,score FROM ${source}`],
  ['percent ties without order', `SELECT TOP (25) PERCENT WITH TIES id,score FROM ${source}`],
  ['negative count ties', `SELECT TOP (-1) WITH TIES id,score FROM ${source} ORDER BY id`],
  ['null count ties', `SELECT TOP (CAST(NULL AS INT)) WITH TIES id,score FROM ${source} ORDER BY id`],
  ['text percent', `SELECT TOP ('abc') PERCENT id,score FROM ${source} ORDER BY id`],
  ['int variable percent', `DECLARE @p INT=25; SELECT TOP (@p) PERCENT id,score FROM ${source} ORDER BY id`],
  ['float variable percent ties', `DECLARE @p FLOAT=25; SELECT TOP (@p) PERCENT WITH TIES id,score FROM ${source} ORDER BY score DESC`],
  ['nested percent', `SELECT id FROM (SELECT TOP (50) PERCENT id FROM ${source} ORDER BY id DESC) AS q ORDER BY id`],
  ['set branch percent', `SELECT id FROM (SELECT TOP (50) PERCENT id FROM ${source} ORDER BY id) AS q UNION ALL SELECT 99 ORDER BY id`],
]

function rpc(connection, sql, parameters) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      for (const [name, type, value] of parameters) request.addParameter(name, type, value)
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

function resultListeners(connection, request, result) {
  const onError = error => result.errors.push({ number: error.number, state: error.state, class: error.class, lineNumber: error.lineNumber, message: error.message })
  const onInfo = info => result.info.push({ number: info.number, state: info.state, class: info.class, lineNumber: info.lineNumber, message: info.message })
  const onMetadata = metadata => result.sets.push({
    columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })), rows: [],
  })
  const onRow = row => result.sets.at(-1).rows.push(row.map(c => c.value))
  const onDone = Object.fromEntries(['done', 'doneInProc', 'doneProc'].map(kind => [kind, (rowCount, more, status) => {
    result.done.push({ kind, rowCount: rowCount ?? null, more })
    if (kind === 'doneProc') result.returnStatus = status
  }]))
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  request.on('columnMetadata', onMetadata)
  request.on('row', onRow)
  for (const [kind, listener] of Object.entries(onDone)) request.on(kind, listener)
  return () => {
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
    request.off('columnMetadata', onMetadata)
    request.off('row', onRow)
    for (const [kind, listener] of Object.entries(onDone)) request.off(kind, listener)
  }
}

async function prepared(connection, sql, values) {
  let complete = () => {}
  const request = new Request(sql, (...args) => complete(...args))
  request.addParameter('p', TYPES.Float)
  await new Promise((resolve, reject) => {
    request.once('prepared', resolve)
    request.once('error', reject)
    connection.prepare(request)
  })
  const executions = []
  try {
    for (const value of values) {
      const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
      const detach = resultListeners(connection, request, result)
      try {
        await new Promise(resolve => {
          complete = (error, rowCount) => {
            result.rowCount = rowCount
            if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          }
          request.error = undefined
          connection.execute(request, { p: value })
        })
      } finally { detach() }
      executions.push(canonical(result))
    }
  } finally {
    await new Promise((resolve, reject) => {
      complete = error => error ? reject(error) : resolve()
      request.error = undefined
      connection.unprepare(request)
    })
  }
  return executions
}

function summary(run) {
  assert.equal(run.length, cases.length + 9)
  assert.equal(new Set(run.map(item => item.name)).size, run.length)
  const version = run.find(item => item.name === 'server identity')?.result.sets[0].rows[0][0]
  assert.match(version, /^17\./)
  const result = name => {
    const value = run.find(item => item.name === name)?.result
    assert(value, `missing ${name}`)
    return value
  }
  for (const [name, count] of [
    ['zero percent', 0], ['one percent', 1], ['fraction below boundary', 1],
    ['fraction above boundary', 2], ['quarter percent', 2],
    ['third percent', 3], ['half percent', 4], ['full percent', 7],
    ['fractional percent', 1], ['empty percent', 0],
    ['count zero ties', 0], ['count two ties', 3], ['count three ties', 3],
    ['count two unique', 2], ['quarter percent ties', 3],
    ['half percent ties', 5], ['full percent ties', 7], ['empty ties', 0],
    ['prepared percent 0', 3], ['prepared percent 2', 5], ['prepared percent 3', 3],
  ]) {
    const value = result(name)
    assert.deepEqual(value.errors, [], name)
    assert.equal(value.sets.length, 1, name)
    assert.equal(value.sets[0].rows.length, count, name)
    assert.equal(value.done[0].rowCount, count, name)
  }
  for (const [name, number, state, severity] of [
    ['above full percent', 1031, 1, 15], ['negative percent', 1031, 1, 15],
    ['null percent', 1014, 1, 15], ['ties without order', 1062, 1, 15],
    ['percent ties without order', 1062, 1, 15],
    ['negative count ties', 127, 1, 15], ['null count ties', 1060, 1, 15],
    ['text percent', 8114, 5, 16], ['RPC percent null', 1014, 1, 15],
    ['prepared percent 1', 1014, 1, 15],
  ]) {
    const value = result(name)
    assert.deepEqual(value.errors.map(error => [error.number, error.state, error.class]), [[number, state, severity]], name)
    assert.equal(value.done.at(-1)?.rowCount, null, name)
  }
  assert.equal(result('zero percent').sets[0].columns.length, 2)
  assert.equal(result('empty percent').sets[0].columns.length, 2)
  assert.equal(result('null percent').sets.length, 0)
  assert.equal(result('prepared percent 1').sets[0].columns.length, 2)
  assert.deepEqual(result('count two ties').sets[0].rows.map(row => row[0]).sort(), [1, 2, 3])
  assert.deepEqual(result('quarter percent ties').sets[0].rows.map(row => row[0]).sort(), [1, 2, 3])
  assert.deepEqual(result('half percent ties').sets[0].rows.map(row => row[0]).sort(), [1, 2, 3, 4, 5])
  assert.deepEqual(stableResult('prepared percent 0', result('prepared percent 0')), stableResult('prepared percent 3', result('prepared percent 3')))
  return run.map(({ name, result }) => {
    assert(result.done.length > 0, `${name}: missing completion`)
    if (hasUnspecifiedTies(name)) {
      for (const set of result.sets) {
        let previous = Infinity
        let seenNull = false
        for (const [, score] of set.rows) {
          if (score === null) { seenNull = true; continue }
          assert(!seenNull && score <= previous, `${name}: score groups are not descending`)
          previous = score
        }
      }
    }
    return { name, result: stableResult(name, result) }
  })
}

function hasUnspecifiedTies(name) {
  return (name.includes('ties') && !name.includes('unique')) || name.startsWith('prepared percent ')
}

function stableResult(name, result) {
  // SQL Server does not specify order among equal ORDER BY keys. Raw captures
  // retain their order; only the stability/replay comparisons sort each
  // contiguous equal-score group, leaving the order of score groups intact.
  const stable = structuredClone(result)
  if (hasUnspecifiedTies(name)) {
    for (const set of stable.sets) {
      for (let start = 0; start < set.rows.length;) {
        let end = start + 1
        while (end < set.rows.length && Object.is(set.rows[end][1], set.rows[start][1])) ++end
        const group = set.rows.slice(start, end).sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b)))
        set.rows.splice(start, end - start, ...group)
        start = end
      }
    }
  }
  return stable
}

async function observe(connection) {
  const run = []
  async function record(name, sql, parameters) {
    const result = canonical(await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql)))
    run.push({ name, sql, result })
  }
  await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
  await record('create source', `CREATE TABLE ${source}(id INT NOT NULL PRIMARY KEY,score INT NULL)`)
  await record('seed source', `INSERT ${source}(id,score) VALUES (1,10),(2,9),(3,9),(4,8),(5,8),(6,7),(7,NULL)`)
  for (const [name, sql] of cases) await record(name, sql)
  await record('RPC percent first', `SELECT TOP (@p) PERCENT id,score FROM ${source} ORDER BY id`, [['p', TYPES.Float, 25]])
  await record('RPC percent null', `SELECT TOP (@p) PERCENT id,score FROM ${source} ORDER BY id`, [['p', TYPES.Float, null]])
  const preparedSql = `SELECT TOP (@p) PERCENT WITH TIES id,score FROM ${source} ORDER BY score DESC`
  const preparedValues = [25, null, 50, 25]
  const preparedResults = await prepared(connection, preparedSql, preparedValues)
  for (let index = 0; index < preparedValues.length; ++index) {
    run.push({ name: `prepared percent ${index}`, sql: preparedSql, parameter: preparedValues[index], result: preparedResults[index] })
  }
  assertSameCapture(stableResult('prepared percent 0', preparedResults[0]), stableResult('prepared percent 3', preparedResults[3]), 'prepared replay differs')
  summary(run)
  return run
}

if (selfTest) {
  const result = rows => ({ sets: [{ rows }] })
  const expected = stableResult('prepared percent 0', result([[1, 10], [2, 9], [3, 9], [4, 8], [5, 8]]))
  const tieSwap = stableResult('prepared percent 0', result([[1, 10], [3, 9], [2, 9], [5, 8], [4, 8]]))
  const groupSwap = stableResult('prepared percent 0', result([[3, 9], [2, 9], [1, 10], [4, 8], [5, 8]]))
  assert.deepEqual(tieSwap, expected, 'equal-score order is unspecified')
  assert.notDeepEqual(groupSwap, expected, 'score-group order must remain visible')
  console.log('Tie stability comparisons preserve score-group order')
} else {
  await rejectFixtureOutput()
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(resolve(output, '..'), { recursive: true })
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let repeat = 0; repeat < 2; ++repeat) runs.push(await isolatedReference(config, observe))
    const actual = { image: container.image, runs }
    const summaries = runs.map(summary)
    assertSameCapture(summaries[0], summaries[1], 'SELECT TOP behavior differs across fresh databases')
    let retained
    try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    if (retained) {
      assert.equal(actual.image, retained.image)
      assertSameCapture(summaries, retained.runs.map(summary), 'SELECT TOP behavior differs from retained fixture')
    }
    await writeFile(output, JSON.stringify(actual) + '\n')
    if (writeFixture) await writeNewFixture(fixture, actual)
    console.log(`Captured ${runs[0].length} SELECT TOP observations in two fresh databases${retained ? ' and matched retained invariants' : ''}`)
  })
}
