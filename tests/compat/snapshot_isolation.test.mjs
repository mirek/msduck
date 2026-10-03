// ALTER DATABASE ... SET ALLOW_SNAPSHOT_ISOLATION and SNAPSHOT transactions
// through tedious (issue #870). Replays the `snapshot` section of
// reference/gaps-transactions.json (captured by
// scripts/capture-gaps-transactions.mjs --snapshot) with the same four
// connections and compares SQL Server's full result: rows, column
// descriptors, errors, informational messages, DONE tokens and row counts.
// See docs/gaps-transactions.md.
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { Connection } from 'tedious'
import { start, query } from '../support/client.mjs'
import { capture, canonical, differences as compare } from '../../scripts/lib/compatibility.mjs'

const DATABASE = 'snapshot_isolation_replay'

async function open(t, admin, database) {
  const connection = new Connection({ ...admin.config, options: { ...admin.config.options, database, requestTimeout: 30000 } })
  t.after(() => connection.close())
  connection.on('error', () => {})
  await new Promise((resolve, reject) => connection.connect(error => error ? reject(error) : resolve()))
  return connection
}

const bind = value => JSON.parse(JSON.stringify(value).replaceAll('<fresh-database>', DATABASE))
// Retained differences, compared exactly so that a change is noticed. Each
// edits a copy of SQL Server's full result into msduck's.
const edit = (reference, change) => {
  const copy = structuredClone(reference)
  change(copy)
  return copy
}
const metadataFirst = [
  'off: autocommit read', 'off: read in transaction', 'off: three-part name from master', 'off again: read',
  'to on: snapshot transaction', 'to off: new snapshot transaction', 'to off: idle snapshot transaction reads'
]
// Inside TRY, msduck sends no DONE for the statement that failed; the value
// is the index of that DONE in SQL Server's result.
const caughtDone = {
  'off: caught read': 1,
  'off: caught read in transaction': 2,
  'alter: caught': 2,
  'caught conflict: update': 1
}
// SQL Server marks these sys.databases option columns with flags 9 in this
// projection; msduck declares them as it does for other projections (33,
// as in reference/alter-database-sessions.json).
const optionFlags = ['initial state', 'state after off', 'state after on', 'state with both options', 'off again: state']
function known(name, reference) {
  // SQL Server sends a failing SELECT's column metadata before 3952, 3954
  // and 3956; msduck checks access before it describes the query.
  if (metadataFirst.includes(name)) return edit(reference, r => { r.sets.shift() })
  if (name in caughtDone) {
    return edit(reference, r => {
      r.sets = r.sets.filter(set => set.columns[0]?.name !== 'v')
      r.done.splice(caughtDone[name], 1)
    })
  }
  if (optionFlags.includes(name)) {
    return edit(reference, r => { for (const column of r.sets[0].columns.slice(1)) column.flags = 33 })
  }
  // The engine's other ALTER DATABASE options end the batch at 226 too;
  // SQL Server continues with the next statement.
  if (name === 'alter: in a transaction') {
    return edit(reference, r => {
      r.sets = []
      r.done = [r.done[0], { kind: 'done', rowCount: null, more: false }]
      r.rowCount = 0
    })
  }
  // DuckDB's binder message for a missing table (owned by the engine).
  if (name === 'off: missing table') {
    return edit(reference, r => {
      r.errors[0].message = 'Catalog Error: Table with name missing_table does not exist!\nDid you mean "__msduck_is_tables"?\n\nLINE 1: SELECT v FROM missing_table\n                      ^'
    })
  }
  // Generic parse errors carry state 1.
  if (name === 'alter: missing value') return edit(reference, r => { r.errors[0].state = 1 })
  // DuckDB detects a write-write conflict when both transactions update or
  // both delete a row, but not a DELETE of a row another transaction
  // updated (or an UPDATE of a row it deleted): the DELETE succeeds.
  if (name === 'conflict delete: delete') {
    return edit(reference, r => {
      r.errors = []
      r.done[0].rowCount = 1
      r.rowCount = 1
    })
  }
  if (name === 'conflict delete: after') {
    return edit(reference, r => {
      r.sets[0].rows = [[1]]
      r.sets[1].rows = []
      r.done[1].rowCount = 0
      r.rowCount = 1
    })
  }
  // DuckDB takes the snapshot when the transaction begins; SQL Server takes
  // it at the first data access, after the concurrent update.
  if (name === 'late access: first read') return edit(reference, r => { r.sets[0].rows = [[8]] })
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
  // An ALTER that waits runs in the background between its `start` and
  // `finish` entries, as in the capture.
  const pending = {}
  const pause = 1500
  for (const expected of fixture.snapshot.run) {
    connections[expected.connection] ??= await open(t, admin, DATABASE)
    if (expected.start) {
      const started = Date.now()
      pending[expected.connection] = capture(connections[expected.connection], bind(expected.sql)).then(result => ({ result, elapsed: Date.now() - started }))
      await new Promise(resolve => setTimeout(resolve, pause))
      continue
    }
    let result, waited
    if (expected.finish) {
      ({ result } = await pending[expected.connection])
      waited = (await pending[expected.connection]).elapsed >= pause
      delete pending[expected.connection]
    } else {
      result = await capture(connections[expected.connection], bind(expected.sql))
    }
    const actual = { ...canonical(result), ...(expected.finish ? { waited } : {}) }
    const reference = { ...known(expected.name, bind(expected.result)), ...(expected.finish ? { waited: expected.waited } : {}) }
    const found = compare(actual, reference)
    if (found.length) differences.push({ name: expected.name, differences: JSON.stringify(found.slice(0, 8)).slice(0, 3000) })
    if (realign[expected.name]) await capture(connections[expected.connection], realign[expected.name])
  }
  assert.deepEqual(differences.slice(0, 50), [])
})
