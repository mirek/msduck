// UPDATE and DELETE whose FROM tree joins the target through LEFT, RIGHT or
// FULL joins, OUTER APPLY or CROSS APPLY (issue #723, docs/gaps-outer_dml.md).
// Every case replays the SQL Server 2022 capture in reference/gaps-outer_dml.json
// and compares result rows, error numbers, DONE counts and the table readback.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'
import { start, query } from '../support/client.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/gaps-outer_dml.json', import.meta.url)))
const keep = result => canonical({
  sets: result.sets.map(set => ({ columns: set.columns.map(c => [c.name, c.type]), rows: set.rows })),
  errors: result.errors.map(e => ({ number: e.number, message: e.message })),
  done: result.done.filter(d => d.kind === 'done' || d.kind === 'doneInProc').map(d => d.rowCount),
})
// SQL Server does not order OUTPUT rows, so multi-row OUTPUT sets compare sorted.
const sorted = result => ({ ...result, dml: { ...result.dml, sets: result.dml.sets.map(set => ({ ...set, rows: [...set.rows].sort((a, b) => JSON.stringify(a) < JSON.stringify(b) ? -1 : 1) })) } })
const reset = 'DROP TABLE IF EXISTS items; DROP TABLE IF EXISTS foo; DROP TABLE IF EXISTS bar;'

test('outer-join UPDATE and DELETE match the SQL Server capture', async t => {
  const connection = await start(t)
  assert.equal(reference.cases.length, 41)
  // Every case runs, so one report lists all differences.
  const differences = []
  for (const entry of reference.cases) {
    const setup = await capture(connection, reset + entry.setup)
    assert.deepEqual(setup.errors, [], `${entry.name}: setup`)
    const dml = keep(await capture(connection, entry.dml))
    const readback = keep(await capture(connection, entry.readback))
    try { assert.deepEqual(sorted({ dml, readback }), sorted(entry.result)) }
    catch { differences.push({ name: entry.name, actual: { dml, readback }, expected: entry.result }) }
  }
  assert.deepEqual(differences, [])
})

test('the generic repros update and delete exactly the SQL Server rows', async t => {
  const connection = await start(t)
  const setup = `${reset}
    CREATE TABLE items(id INT PRIMARY KEY, value INT);
    CREATE TABLE foo(id INT, value INT);
    INSERT INTO items VALUES (1, 10), (2, 20), (3, 30);
    INSERT INTO foo VALUES (1, 100), (3, 300), (3, 300);`
  await query(connection, setup)
  const update = await query(connection, 'UPDATE target SET value=COALESCE(source.value,0) FROM items target LEFT JOIN foo source ON source.id=target.id; SELECT @@ROWCOUNT')
  assert.equal(update.rowCount, 4) // three updated rows plus the one-row SELECT
  assert.deepEqual(update.rows, [[3]])
  assert.deepEqual((await query(connection, 'SELECT id, value FROM items ORDER BY id')).rows, [[1, 100], [2, 0], [3, 300]])

  await query(connection, setup)
  const remove = await query(connection, 'DELETE target FROM items target LEFT JOIN foo source ON source.id=target.id WHERE source.id IS NULL; SELECT @@ROWCOUNT')
  assert.deepEqual(remove.rows, [[1]])
  assert.deepEqual((await query(connection, 'SELECT id, value FROM items ORDER BY id')).rows, [[1, 10], [3, 30]])

  // Parameters reach both the join tree and the assignment.
  await query(connection, setup)
  const { TYPES } = await import('tedious')
  const bound = await query(connection, 'UPDATE t SET value = ISNULL(s.value, @fallback) FROM items t LEFT JOIN foo s ON s.id = t.id AND s.value > @minimum; SELECT @@ROWCOUNT',
    [['fallback', TYPES.Int, -5], ['minimum', TYPES.Int, 150]])
  assert.deepEqual(bound.rows, [[3]])
  assert.deepEqual((await query(connection, 'SELECT id, value FROM items ORDER BY id')).rows, [[1, -5], [2, -5], [3, 300]])

  await assert.rejects(
    query(connection, 'UPDATE t SET value = value FROM items t LEFT JOIN foo s ON s.id = t.id'),
    error => error.number === 209
  )
})
