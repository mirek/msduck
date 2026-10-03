// Correlated APPLY bodies that FULL OUTER JOIN two row sources (issue #867,
// docs/apply-full-join.md). Every case replays the SQL Server capture in
// reference/apply-full-join.json: setup batches, then one query batch whose
// result sets (names, types, lengths, rows), errors and DONE counts must match.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { isDeepStrictEqual } from 'node:util'
import { test } from 'node:test'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'
import { describeFirstDifference } from '../../scripts/lib/reference.mjs'
import { start, query } from '../support/client.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/apply-full-join.json', import.meta.url)))
const keep = result => canonical({
  sets: result.sets.map(set => ({ columns: set.columns.map(c => [c.name, c.type, c.length]), rows: set.rows })),
  errors: result.errors.map(e => ({ number: e.number, class: e.class ?? null, state: e.state ?? null, message: e.message })),
  done: result.done.filter(d => d.kind === 'done' || d.kind === 'doneInProc').map(d => d.rowCount),
})
const reset = 'DROP TRIGGER IF EXISTS docs_audit; DROP FUNCTION IF EXISTS dbo.foo; DROP TABLE IF EXISTS items; DROP TABLE IF EXISTS audit; DROP TABLE IF EXISTS docs;'

// Remaining differences are outside the APPLY lowering (docs/apply-full-join.md):
// COALESCE over the OPENJSON key column widens to nvarchar(max), OPENJSON's
// type column is int, duplicate derived-table column names are not rejected
// (8156), the ambiguous column is DuckDB's binder error rather than 209, and
// ISNULL over an OPENJSON value with an N'' fallback fails to convert. They are
// kept exact so any change, fix or regression, shows up here.
const keyLength = name => `${name}: first difference at .sets[0].columns[1][2]: actual 65535, expected 8000`
const knownDifferences = [
  keyLength('report repro cross apply'),
  keyLength('report repro outer apply'),
  'function star metadata: first difference at .sets[0].columns[0][2]: actual 65535, expected 8000',
  keyLength('multirow cross apply'),
  keyLength('multirow outer apply'),
  keyLength('derived cross apply'),
  keyLength('derived outer apply'),
  'star of both sides: first difference at .sets[0] (array length 1 vs 0): actual object(2 keys), expected undefined',
  'qualified star of one side: first difference at .sets[0].columns[3][2]: actual 4, expected 1',
  keyLength('top and order in body'),
  'ambiguous column: first difference at .errors[0].number: actual 50000, expected 209',
  'trigger diff of json snapshots: first difference at .sets[0] (array length 0 vs 1): actual undefined, expected object(2 keys)',
]
// Cases whose rows and DONE counts differ, not only column metadata.
const knownRowDifferences = ['star of both sides', 'trigger diff of json snapshots']
const rowsOf = result => ({ rows: result.sets.map(set => set.rows), done: result.done })

test('correlated FULL JOIN APPLY bodies match the SQL Server capture', async t => {
  const connection = await start(t)
  assert.equal(reference.cases.length, 24)
  // Every case runs, so one report lists all differences.
  const differences = []
  const rowDifferences = []
  for (const entry of reference.cases) {
    for (const batch of [reset, ...entry.setup]) {
      const setup = await capture(connection, batch)
      assert.equal(setup.errors.length, 0, `${entry.name}: setup failed with ${setup.errors[0]?.number}`)
    }
    const actual = keep(await capture(connection, entry.query))
    if (!isDeepStrictEqual(actual, entry.result)) differences.push(`${entry.name}: ${describeFirstDifference(actual, entry.result)}`)
    if (!isDeepStrictEqual(rowsOf(actual), rowsOf(entry.result))) rowDifferences.push(entry.name)
  }
  assert.deepEqual(rowDifferences, knownRowDifferences)
  assert.deepEqual(differences, knownDifferences)
})

test('the report repro returns the changed key through CROSS and OUTER APPLY', async t => {
  const connection = await start(t)
  await query(connection, reset)
  await query(connection, `CREATE TABLE items(id INT PRIMARY KEY, lhs NVARCHAR(MAX), rhs NVARCHAR(MAX));
    INSERT INTO items VALUES (1, N'{"a":1}', N'{"a":2}'), (2, NULL, N'{"b":true}'), (3, N'{"c":null}', NULL);`)
  await query(connection, "CREATE FUNCTION dbo.foo(@lhs NVARCHAR(MAX), @rhs NVARCHAR(MAX)) RETURNS TABLE AS RETURN (SELECT COALESCE(l.[key], r.[key]) AS [key], l.[value] AS old_value, r.[value] AS new_value FROM OPENJSON(@lhs) l FULL OUTER JOIN OPENJSON(@rhs) r ON l.[key] = r.[key])")
  const cross = await query(connection, 'SELECT i.id, x.[key], x.old_value, x.new_value FROM items i CROSS APPLY dbo.foo(i.lhs, i.rhs) x ORDER BY i.id')
  assert.deepEqual(cross.rows, [[1, 'a', '1', '2'], [2, 'b', null, 'true'], [3, 'c', null, null]])
  // A parameter reaches the correlated body too.
  const { TYPES } = await import('tedious')
  const outer = await query(connection, 'SELECT i.id, x.[key], x.new_value FROM items i OUTER APPLY dbo.foo(@doc, i.rhs) x ORDER BY i.id, x.[key]',
    [['doc', TYPES.NVarChar, '{"a":5,"z":0}']])
  assert.deepEqual(outer.rows, [[1, 'a', '2'], [1, 'z', null], [2, 'a', null], [2, 'b', 'true'], [2, 'z', null], [3, 'a', null], [3, 'z', null]])
})
