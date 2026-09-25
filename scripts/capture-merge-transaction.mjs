#!/usr/bin/env node
// First-party SQL Server evidence for MERGE failures in user transactions.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, isolatedReference } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/merge-transaction.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/merge-transaction/merge-transaction.json')

function bindGeneratedDatabaseNames(run) {
  return run.map(entry => {
    const copy = structuredClone(entry)
    for (const message of [...copy.result.errors, ...copy.result.info]) {
      if (typeof message.message === 'string') {
        message.message = message.message.replaceAll(/msduck_audit_[0-9a-f]{32}/g, '<fresh-database>')
      }
    }
    if (entry.name === 'database name') copy.result.sets[0].rows = [['<fresh-database>']]
    return copy
  })
}

function resultOf(run, name) {
  const entry = run.find(item => item.name === name)
  assert(entry, `missing ${name}`)
  return entry.result
}

function rows(run, name, set = 0) {
  const result = resultOf(run, name)
  assert.deepEqual(result.errors, [], name)
  return result.sets[set].rows
}

function validate(run) {
  for (const scenario of ['check off', 'check on', 'duplicate off', 'duplicate on', 'output off', 'try check off', 'check rollback off']) {
    const failure = resultOf(run, `${scenario}: failing MERGE`)
    if (scenario.startsWith('try ')) {
      assert.deepEqual(failure.errors, [])
      assert.equal(failure.sets[0].rows[0][0], 547)
    } else {
      assert.equal(failure.errors.length, 1)
      assert.equal(failure.errors[0].number, scenario.startsWith('duplicate') ? 8672 : 547)
    }
    assert.deepEqual(rows(run, `${scenario}: after failure`, 0), [[1, 10]], scenario)
    assert.deepEqual(rows(run, `${scenario}: after failure`, 2), [], scenario)
    assert.deepEqual(rows(run, `${scenario}: after reconnect`, 0), [[1, 10]], scenario)
    assert.deepEqual(rows(run, `${scenario}: after reconnect`, 2), [], scenario)
  }
  for (const scenario of ['check off', 'output off', 'try check off', 'check rollback off']) {
    assert.deepEqual(rows(run, `${scenario}: transaction state`), [[1, 1]], scenario)
    assert.deepEqual(rows(run, `${scenario}: after failure`, 1), [[1, 100]], scenario)
  }
  for (const scenario of ['check on', 'duplicate off', 'duplicate on']) {
    assert.deepEqual(rows(run, `${scenario}: transaction state`), [[0, 0]], scenario)
    assert.deepEqual(rows(run, `${scenario}: after failure`, 1), [], scenario)
    assert.equal(resultOf(run, `${scenario}: commit`).errors[0].number, 3902)
  }
  for (const scenario of ['check off', 'output off', 'try check off']) {
    assert.deepEqual(rows(run, `${scenario}: after reconnect`, 1), [[1, 100], [2, 200]], scenario)
  }
  for (const scenario of ['check on', 'duplicate off', 'duplicate on']) {
    assert.deepEqual(rows(run, `${scenario}: after reconnect`, 1), [[2, 200]], scenario)
  }
  assert.deepEqual(rows(run, 'check rollback off: after reconnect', 1), [])
}

async function observe(connection, config, repeat) {
  const run = []
  async function record(name, sql) {
    const result = canonical(await capture(connection, sql))
    run.push({ name, sql, result })
    return result
  }

  const database = await record('database name', 'SELECT DB_NAME() AS database_name')
  assert.deepEqual(database.errors, [])
  const databaseName = database.sets[0].rows[0][0]
  await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")

  const scenarios = [
    { name: 'check off', table: 'merge_tx_check_off', abort: false, kind: 'check' },
    { name: 'check on', table: 'merge_tx_check_on', abort: true, kind: 'check' },
    { name: 'duplicate off', table: 'merge_tx_duplicate_off', abort: false, kind: 'duplicate' },
    { name: 'duplicate on', table: 'merge_tx_duplicate_on', abort: true, kind: 'duplicate' },
    { name: 'output off', table: 'merge_tx_output_off', abort: false, kind: 'output' },
    { name: 'try check off', table: 'merge_tx_try_check_off', abort: false, kind: 'try' },
    { name: 'check rollback off', table: 'merge_tx_check_rollback_off', abort: false, kind: 'check', finish: 'rollback' }
  ]

  for (const scenario of scenarios) {
    const { name, table, abort, kind, finish } = scenario
    const constraint = `CK_${table}_positive`
    const setup = await record(`${name}: create`, `CREATE TABLE dbo.${table}(id INT NOT NULL PRIMARY KEY,n INT NOT NULL CONSTRAINT ${constraint} CHECK(n>0)); CREATE TABLE dbo.${table}_prior(id INT NOT NULL PRIMARY KEY,n INT NOT NULL); CREATE TABLE dbo.${table}_sink([action] NVARCHAR(10) NOT NULL,inserted_id INT NULL); INSERT dbo.${table}(id,n) VALUES(1,10);`)
    assert.deepEqual(setup.errors, [], `${name}: create`)
    const begin = await record(`${name}: begin`, `SET XACT_ABORT ${abort ? 'ON' : 'OFF'}; BEGIN TRANSACTION; INSERT dbo.${table}_prior(id,n) VALUES(1,100);`)
    assert.deepEqual(begin.errors, [], `${name}: begin`)

    const merge = kind === 'duplicate'
      ? `MERGE dbo.${table} AS t USING (VALUES (1,11),(1,12)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n;`
      : `MERGE dbo.${table} AS t USING (VALUES (1,11),(2,-1)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED BY TARGET THEN INSERT(id,n) VALUES(s.id,s.n)${kind === 'output' ? ` OUTPUT $action, inserted.id INTO dbo.${table}_sink([action],inserted_id)` : ''};`
    const sql = kind === 'try'
      ? `BEGIN TRY ${merge} END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS caught, XACT_STATE() AS caught_xact_state, @@TRANCOUNT AS caught_tran_count; END CATCH;`
      : merge
    await record(`${name}: failing MERGE`, sql)
    await record(`${name}: transaction state`, 'SELECT XACT_STATE() AS xact_state, @@TRANCOUNT AS tran_count')
    await record(`${name}: after failure`, `SELECT id,n FROM dbo.${table} ORDER BY id; SELECT id,n FROM dbo.${table}_prior ORDER BY id; SELECT [action],inserted_id FROM dbo.${table}_sink ORDER BY [action],inserted_id;`)
    await record(`${name}: later write`, `INSERT dbo.${table}_prior(id,n) VALUES(2,200)`)
    await record(`${name}: state after later write`, 'SELECT XACT_STATE() AS xact_state, @@TRANCOUNT AS tran_count')
    if (finish === 'rollback') {
      await record(`${name}: rollback`, 'ROLLBACK TRANSACTION')
    } else {
      await record(`${name}: commit`, 'COMMIT TRANSACTION')
      const afterCommit = await record(`${name}: state after commit`, 'SELECT XACT_STATE() AS xact_state, @@TRANCOUNT AS tran_count')
      if (afterCommit.sets[0].rows[0][1] > 0) {
        await record(`${name}: rollback after failed commit`, 'ROLLBACK TRANSACTION')
      }
    }
    const finalState = await record(`${name}: final state`, 'SELECT XACT_STATE() AS xact_state, @@TRANCOUNT AS tran_count')
    assert.deepEqual(finalState.sets[0].rows, [[0, 0]], `${name}: transaction leaked`)
  }

  // A new connection observes only committed state, independently of the session
  // that executed each failed MERGE.
  const reader = await connect({ ...config, options: { ...config.options, database: databaseName } })
  try {
    for (const { name, table } of scenarios) {
      const result = canonical(await capture(reader, `SELECT id,n FROM dbo.${table} ORDER BY id; SELECT id,n FROM dbo.${table}_prior ORDER BY id; SELECT [action],inserted_id FROM dbo.${table}_sink ORDER BY [action],inserted_id;`))
      run.push({ name: `${name}: after reconnect`, sql: `SELECT target, prior and sink rows FROM dbo.${table}`, result })
      assert.deepEqual(result.errors, [], `${name}: reconnect`)
    }
  } finally {
    await new Promise(resolve => { reader.once('end', resolve); reader.close() })
  }
  try { validate(run) }
  catch (error) {
    await writeFile(resolve(output, `../failed-${repeat}.json`), JSON.stringify(run, null, 2) + '\n')
    throw error
  }
  return run
}

await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, connection => observe(connection, config, repeat)))
  }
  assert.deepEqual(bindGeneratedDatabaseNames(runs[0]), bindGeneratedDatabaseNames(runs[1]), 'fresh SQL Server databases differ after binding only generated database names')
  const actual = { image: container.image, runs }
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) {
    assert.equal(actual.image, retained.image)
    assert.deepEqual(actual.runs.map(bindGeneratedDatabaseNames), retained.runs.map(bindGeneratedDatabaseNames), 'fresh capture differs from retained fixture after binding only generated database names')
  }
  await writeFile(output, JSON.stringify(actual) + '\n')
  if (writeFixture) {
    assert.equal(retained, undefined, 'refusing to overwrite retained fixture')
    await writeFile(fixture, JSON.stringify(actual) + '\n')
  }
  console.log(`Captured ${runs[0].length} MERGE transaction observations in two fresh databases${retained ? ' and matched the retained fixture after binding generated database names' : ''}`)
})
