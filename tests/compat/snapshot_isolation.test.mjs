// ALTER DATABASE ... SET ALLOW_SNAPSHOT_ISOLATION and SNAPSHOT transactions
// through tedious (issue #870). Replays the `snapshot` section of
// reference/gaps-transactions.json (captured by
// scripts/capture-gaps-transactions.mjs --snapshot) with the same three
// connections; see docs/gaps-transactions.md.
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { Connection } from 'tedious'
import { start, query } from '../support/client.mjs'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'

const DATABASE = 'snapshot_isolation_replay'

async function open(t, admin, database) {
  const connection = new Connection({ ...admin.config, options: { ...admin.config.options, database, requestTimeout: 30000 } })
  t.after(() => connection.close())
  connection.on('error', () => {})
  await new Promise((resolve, reject) => connection.connect(error => error ? reject(error) : resolve()))
  return connection
}

const bind = value => JSON.parse(JSON.stringify(value).replaceAll('<fresh-database>', DATABASE))
const shape = result => ({
  sets: result.sets.map(set => ({ columns: set.columns.map(c => [c.name, c.type, c.length]), rows: set.rows })),
  errors: result.errors.map(e => [e.number, e.state, e.class, e.message]),
  info: result.info.map(e => [e.number, e.state, e.class, e.message])
})

// Retained differences, compared exactly so that a change is noticed.
// SQL Server sends a failing SELECT's column metadata before 3952; msduck
// checks access before it describes the query, so no empty result set
// precedes the error.
function known(name, reference) {
  if (['off: autocommit read', 'off: read in transaction', 'off: three-part name from master', 'off again: read'].includes(name)) {
    return { ...reference, sets: reference.sets.slice(1) }
  }
  if (['off: caught read', 'off: caught read in transaction'].includes(name)) {
    return { ...reference, sets: reference.sets.filter(set => set.columns[0]?.[0] !== 'v') }
  }
  // The engine's other ALTER DATABASE options end the batch at 226 too;
  // SQL Server continues with the next statement.
  if (name === 'alter: in a transaction') return { ...reference, sets: [] }
  // DuckDB's binder message for a missing table (owned by the engine).
  if (name === 'off: missing table') {
    return { ...reference, errors: [[208, 1, 16, 'Catalog Error: Table with name missing_table does not exist!\nDid you mean "__msduck_is_tables"?\n\nLINE 1: SELECT v FROM missing_table\n                      ^']] }
  }
  // DuckDB detects a write-write conflict when both transactions update or
  // both delete a row, but not a DELETE of a row another transaction
  // updated (or an UPDATE of a row it deleted): the DELETE succeeds.
  if (name === 'conflict delete: delete') return { ...reference, errors: [] }
  if (name === 'conflict delete: after') {
    return { ...reference, sets: [{ ...reference.sets[0], rows: [[1]] }, { ...reference.sets[1], rows: [] }] }
  }
  // DuckDB takes the snapshot when the transaction begins; SQL Server takes
  // it at the first data access, after the concurrent update.
  if (name === 'late access: first read') return { ...reference, sets: [{ ...reference.sets[0], rows: [[8]] }] }
  // Generic parse errors carry state 1.
  if (name === 'alter: missing value') return { ...reference, errors: [[102, 1, 15, "Incorrect syntax near 'ALLOW_SNAPSHOT_ISOLATION'."]] }
  return reference
}

// Statements run after a known difference so that later observations start
// from SQL Server's state.
const realign = {
  'alter: in a transaction': 'ROLLBACK',
  'conflict delete: after': 'ROLLBACK'
}

test('replays the captured ALLOW_SNAPSHOT_ISOLATION and SNAPSHOT reference', { timeout: 120000 }, async t => {
  const fixture = JSON.parse(await readFile(new URL('../../reference/gaps-transactions.json', import.meta.url), 'utf8'))
  const admin = await start(t, { options: { requestTimeout: 30000 } })
  await query(admin, `CREATE DATABASE ${DATABASE}`)
  const connections = { a: await open(t, admin, DATABASE), m: await open(t, admin, 'master') }
  const differences = []
  for (const expected of fixture.snapshot.run) {
    connections[expected.connection] ??= await open(t, admin, DATABASE)
    const result = canonical(await capture(connections[expected.connection], bind(expected.sql)))
    const actual = shape(result)
    const reference = known(expected.name, bind(shape(expected.result)))
    if (JSON.stringify(actual) !== JSON.stringify(reference)) {
      differences.push({ name: expected.name, actual: JSON.stringify(actual).slice(0, 2000), reference: JSON.stringify(reference).slice(0, 2000) })
    }
    if (realign[expected.name]) await capture(connections[expected.connection], realign[expected.name])
  }
  assert.deepEqual(differences.slice(0, 50), [])
})
