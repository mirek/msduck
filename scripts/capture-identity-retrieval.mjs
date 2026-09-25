#!/usr/bin/env node
// Retain exact identity-retrieval rows, metadata, errors and completion tokens.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, isolatedReference } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/identity-retrieval.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/identity-retrieval/capture.json')
const probe = "SELECT SCOPE_IDENTITY() AS scoped, @@IDENTITY AS session_last, IDENT_CURRENT('dbo.parent_id') AS parent_last, IDENT_CURRENT('dbo.audit_id') AS audit_last"

function rpc(connection, sql, parameters) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch: request => {
      for (const [name, value] of Object.entries(parameters)) request.addParameter(name, TYPES.Int, value)
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

async function observe(connection, config) {
  const records = []
  async function record(name, sql, session = 'primary', parameters) {
    const target = session === 'primary' ? connection : secondary
    const result = canonical(await (parameters ? rpc(target, sql, parameters) : capture(target, sql)))
    records.push({ name, session, sql, ...(parameters ? { parameters } : {}), result })
    return result
  }
  let secondary
  try {
    await record('initial session values', 'SELECT SCOPE_IDENTITY() AS scoped, @@IDENTITY AS session_last')
    await record('create parent', 'CREATE TABLE dbo.parent_id(id INT IDENTITY(10,2) PRIMARY KEY, value INT CONSTRAINT uq_parent_value UNIQUE)')
    await record('create audit', 'CREATE TABLE dbo.audit_id(id INT IDENTITY(100,3) PRIMARY KEY, parent_id INT)')
    await record('before allocation', probe)
    await record('first batch insert', `INSERT dbo.parent_id(value) VALUES(1); ${probe}`)
    await record('after first batch', probe)

    const db = (await capture(connection, 'SELECT DB_NAME()')).sets[0].rows[0][0]
    secondary = await connect({ ...config, options: { ...config.options, database: db } })
    await record('second session before allocation', probe, 'secondary')
    await record('second session insert', `INSERT dbo.parent_id(value) VALUES(2); ${probe}`, 'secondary')
    await record('first session after second', probe)

    await record('rpc insert 3', `INSERT dbo.parent_id(value) VALUES(@value); ${probe}`, 'primary', { value: 3 })
    await record('after rpc insert', probe)
    await record('rpc insert 4', `INSERT dbo.parent_id(value) VALUES(@value); ${probe}`, 'primary', { value: 4 })
    await record('second session after rpc', probe, 'secondary')

    await record('duplicate insert failure', 'INSERT dbo.parent_id(value) VALUES(3)')
    await record('after duplicate failure', probe)
    await record('rollback in same batch', `BEGIN TRANSACTION; INSERT dbo.parent_id(value) VALUES(5); ${probe}; ROLLBACK TRANSACTION; ${probe}`)
    await record('after rollback', `SELECT id,value FROM dbo.parent_id ORDER BY id; ${probe}`)

    await record('create trigger', `CREATE TRIGGER dbo.parent_id_audit ON dbo.parent_id AFTER INSERT AS BEGIN SET NOCOUNT ON; INSERT dbo.audit_id(parent_id) SELECT id FROM inserted; END`)
    await record('triggered insert', `INSERT dbo.parent_id(value) VALUES(6); ${probe}`)
    await record('after triggered batch', probe)
    await record('second session after trigger', probe, 'secondary')

    await record('create procedure', `CREATE PROCEDURE dbo.insert_parent_id AS BEGIN INSERT dbo.parent_id(value) VALUES(7); SELECT SCOPE_IDENTITY() AS proc_scope, @@IDENTITY AS proc_session; END`)
    await record('procedure scope', `EXEC dbo.insert_parent_id; ${probe}`)
    await record('after procedure batch', probe)

    await record('explicit identity insert', `SET IDENTITY_INSERT dbo.parent_id ON; INSERT dbo.parent_id(id,value) VALUES(50,8); ${probe}; SET IDENTITY_INSERT dbo.parent_id OFF`)
    await record('after explicit identity', probe)

    await record('create big identity', 'CREATE TABLE dbo.big_id(id BIGINT IDENTITY(9223372036854775800,1) PRIMARY KEY, value INT)')
    const bigProbe = "SELECT SCOPE_IDENTITY() AS scoped, @@IDENTITY AS session_last, IDENT_CURRENT('dbo.big_id') AS table_last, CONVERT(VARCHAR(40),SCOPE_IDENTITY()) AS scoped_text, CONVERT(VARCHAR(40),@@IDENTITY) AS session_text, CONVERT(VARCHAR(40),IDENT_CURRENT('dbo.big_id')) AS table_text"
    await record('big identity insert', `INSERT dbo.big_id(value) VALUES(1); SELECT id FROM dbo.big_id; ${bigProbe}`)
    await record('after big identity batch', bigProbe)

    await record('truncate parent', `TRUNCATE TABLE dbo.parent_id; ${probe}`)
    await record('after truncate', probe)
    assert.equal(records.length, 31)
    return records
  } finally {
    if (secondary && !secondary.closed) await new Promise(resolve => { secondary.once('end', resolve); secondary.close() })
  }
}

function validate(run) {
  const get = name => {
    const item = run.find(item => item.name === name)
    assert(item, `missing ${name}`)
    return item.result
  }
  const initial = get('initial session values')
  assert.deepEqual(initial.sets[0].rows, [[null, null]])
  const first = get('first batch insert')
  assert.equal(first.errors.length, 0)
  assert.equal(first.sets.length, 1)
  assert.equal(first.sets[0].columns[0].type, 'NumericN')
  assert.equal(first.sets[0].columns[0].precision, 38)
  assert.equal(first.sets[0].columns[0].scale, 0)
  const row = name => get(name).sets.at(-1).rows[0]
  assert.deepEqual(row('before allocation'), [null, null, 10, 100])
  assert.deepEqual(row('first batch insert'), [10, 10, 10, 100])
  assert.deepEqual(row('after first batch'), [10, 10, 10, 100])
  assert.deepEqual(row('second session insert'), [12, 12, 12, 100])
  assert.deepEqual(row('first session after second'), [10, 10, 12, 100])
  assert.deepEqual(row('rpc insert 3'), [14, 14, 14, 100])
  assert.deepEqual(row('after rpc insert'), [10, 14, 14, 100])
  assert.deepEqual(row('after duplicate failure'), [10, 16, 18, 100])
  assert.equal(get('duplicate insert failure').errors[0].number, 2627)
  assert.equal(get('after duplicate failure').errors.length, 0)
  assert.deepEqual(row('after rollback'), [20, 20, 20, 100])
  assert.deepEqual(row('triggered insert'), [22, 100, 22, 100])
  assert.deepEqual(row('procedure scope'), [22, 103, 24, 103])
  assert.deepEqual(row('explicit identity insert'), [50, 106, 50, 106])
  assert.equal(row('after truncate')[2], 10)
  const big = get('big identity insert')
  assert.equal(big.sets.length, 2)
  assert.equal(big.sets[0].rows[0][0], '9223372036854775800')
  assert.deepEqual(big.sets[1].rows[0].slice(3), Array(3).fill('9223372036854775800'))
  for (const item of run) assert(item.result.done.length > 0, `${item.name}: missing completion`)
}

await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, connection => observe(connection, config))
    validate(run)
    runs.push(run)
  }
  assert.deepEqual(runs[0], runs[1], 'identity retrieval differs across fresh databases')
  const actual = { image: container.image, runs }
  await writeFile(output, JSON.stringify(actual) + '\n')
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assert.deepEqual(actual, retained, 'identity retrieval differs from retained fixture')
  if (writeFixture) {
    assert.equal(retained, undefined, 'refusing to overwrite retained fixture')
    await writeFile(fixture, JSON.stringify(actual) + '\n')
  }
  console.log(`Captured ${runs[0].length} identity observations in two fresh databases${retained ? ' and matched retained raw fixture' : ''}`)
})
