// Bulk load through tedious (docs/gaps-bulk.md): INSERT BULK and the
// BulkLoadBCP message, as tedious newBulkLoad and mssql request.bulk() send
// them. Every case of scripts/capture-gaps-bulk.mjs is replayed against
// msduck and compared with reference/gaps-bulk.json, captured from SQL
// Server: diagnostics (number, state, class, message), informational
// messages, rows, the bulk load's row count and its DONE tokens.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { isDeepStrictEqual } from 'node:util'
import { start, query } from '../support/client.mjs'
import { cases, describe, redact, runStep, bulkLoad } from '../../scripts/capture-gaps-bulk.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/gaps-bulk.json', import.meta.url), 'utf8'))

let databases = 0
async function fresh(t) {
  const connection = await start(t, { options: { requestTimeout: 60000 } })
  const name = `bulk_${process.pid}_${databases++}`
  await query(connection, `CREATE DATABASE ${name}`)
  await query(connection, `USE ${name}`)
  return { connection, name }
}

const diagnostics = list => (list ?? []).map(e => [e.number, e.state, e.class, e.message])

// What a step is compared on.
function projection(step, result) {
  const common = { errors: diagnostics(result.errors), info: diagnostics(result.info) }
  if (typeof step !== 'string') {
    return { ...common, rowCount: result.rowCount, completion: result.completion.map(done => [done.sqlError, done.rowCount, done.curCmd]) }
  }
  return { ...common, rows: (result.sets ?? []).map(set => set.rows) }
}

// Steps where msduck still differs from SQL Server, with the reason. Each
// entry accepts SQL Server's result or the documented difference, so the
// test keeps passing when the engine gap closes.
const aborted = actual => actual.errors.length === 1 && actual.errors[0][0] === 50000 &&
  actual.errors[0][3].startsWith('TransactionContext Error: Current transaction is aborted')
const conversion = prefix => (actual, expected) => {
  const [number, state, severity, message] = actual.errors[0] ?? []
  return actual.errors.length === 1 && number === 245 && state === 1 && severity === 16 && message.startsWith(prefix)
    ? { ...actual, errors: expected.errors } : actual
}
const notNull = column => actual => isDeepStrictEqual(actual, {
  errors: [[515, 1, 16, `Constraint Error: NOT NULL constraint failed: ${column}`]],
  info: [], rowCount: 0, completion: [[false, null, 253], [true, null, 253]],
})
const known = {
  // The engine (not the bulk load) names the database 'master' in 2628
  // (gaps-identifiers-v1, #729). msduck reports the value that did not
  // fit; SQL Server's bulk path reports uninitialized bytes, which the
  // capture redacts.
  'statement-failures/4': (actual, expected, raw) => {
    assert.match(raw.errors[0].message, /Truncated value: 'abcde'\.$/)
    return actual.errors[0][3].replace("'master.dbo.", "'<database>.dbo.") === expected.errors[0][3]
      ? { ...actual, errors: expected.errors } : actual
  },
  // The engine reports failed implicit conversions of a varchar parameter
  // (to int, to datetime) with DuckDB's text and 245, as for
  // `DECLARE @p varchar(10) = 'zzz'; INSERT t (c) VALUES (@p)`
  // (gaps-conversion-v1, #725).
  'statement-failures/5': conversion("Conversion Error: Could not convert string 'invalid integer value: zzz' to INT32"),
  'statement-failures/7': conversion('Conversion Error: invalid timestamp field format: "zzz"'),
  // A NULL for a NOT NULL column, explicit or omitted, fails in the engine
  // with DuckDB's text and without 3621, as
  // `INSERT t (id, name) VALUES (1, NULL)` does.
  'statement-failures/8': notNull('items.name'),
  'statement-failures/11': notNull('req.b'),
  // A key violation inside an explicit transaction makes DuckDB abort the
  // transaction, as it does for INSERT; SQL Server keeps it (XACT_STATE 1)
  // until ROLLBACK.
  'transactions/4': actual => aborted(actual),
  'transactions/5': actual => aborted(actual),
  'transactions/6': actual => aborted(actual),
  'transactions/7': actual => aborted(actual),
}
for (const [id, steps] of cases) {
  test(`${id} matches SQL Server`, async t => {
    const expected = reference.cases.find(entry => entry.id === id)
    assert.ok(expected, `reference case ${id}`)
    const { connection, name } = await fresh(t)
    for (const [index, step] of steps.entries()) {
      const recorded = expected.steps[index]
      assert.deepEqual(recorded.step, describe(step), `${id} step ${index} is the captured step`)
      const raw = await runStep(connection, step, name)
      const actual = projection(step, redact(raw, name))
      const wanted = projection(step, recorded.result)
      const label = `${id} step ${index}: ${JSON.stringify(describe(step)).slice(0, 200)}`
      const difference = known[`${id}/${index}`]
      const accepted = difference?.(actual, wanted, raw)
      if (accepted === true) continue
      assert.deepEqual(accepted || actual, wanted, label)
    }
  })
}

test('a bulk load larger than one packet streams and loads every value', async t => {
  const { connection } = await fresh(t)
  await query(connection, 'CREATE TABLE wide (id int NOT NULL, payload nvarchar(max) NULL, data varbinary(max) NULL)')
  const rows = Array.from({ length: 40 }, (_, i) => [i, 'ž'.repeat(10000 + i), Buffer.alloc(20000 + i, i)])
  const result = await bulkLoad(connection, { bulk: 'wide', columns: [['id', 'Int', { nullable: false }], ['payload', 'NVarChar', { length: 'max' }], ['data', 'VarBinary', { length: 'max' }]], rows }, '')
  assert.deepEqual(result.errors, [])
  assert.equal(result.rowCount, 40)
  const check = await query(connection, 'SELECT id, len(payload), datalength(data) FROM wide ORDER BY id')
  assert.deepEqual(check.rows, rows.map(([id]) => [id, String(10000 + id), String(20000 + id)]))
  const sample = await query(connection, 'SELECT payload, data FROM wide WHERE id = 7')
  assert.equal(sample.rows[0][0], 'ž'.repeat(10007))
  assert.deepEqual(sample.rows[0][1], Buffer.alloc(20007, 7))
})

test('the connection keeps working after a refused and a failed bulk load', async t => {
  const { connection } = await fresh(t)
  await query(connection, 'CREATE TABLE items (id int NOT NULL CONSTRAINT pk_items PRIMARY KEY)')
  const failed = await bulkLoad(connection, { bulk: 'items', columns: [['id', 'Int', { nullable: false }]], rows: [[1], [1]] }, '')
  assert.deepEqual(failed.errors.map(e => e.number), [2627])
  const refused = await bulkLoad(connection, { bulk: 'items', columns: [['id', 'Int']], rows: [[1]] }, '')
  assert.deepEqual(refused.errors.map(e => e.number), [4816])
  const ok = await bulkLoad(connection, { bulk: 'items', columns: [['id', 'Int', { nullable: false }]], rows: [[1], [2]] }, '')
  assert.equal(ok.rowCount, 2)
  assert.deepEqual((await query(connection, 'SELECT id FROM items ORDER BY id')).rows, [[1], [2]])
})
