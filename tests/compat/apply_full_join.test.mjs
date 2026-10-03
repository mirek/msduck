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
// ISNULL over an OPENJSON value with an N'' fallback fails to convert. Each
// known case asserts msduck's complete current result, so any further change,
// fix or regression, fails here.
const lengths = changes => reference => {
  const result = structuredClone(reference)
  for (const [index, length] of changes) result.sets[0].columns[index][2] = length
  return result
}
const max = 65535
const knownResults = {
  'report repro cross apply': lengths([[1, max]]),
  'report repro outer apply': lengths([[1, max]]),
  'function star metadata': lengths([[0, max]]),
  'multirow cross apply': lengths([[1, max]]),
  'multirow outer apply': lengths([[1, max]]),
  'derived cross apply': lengths([[1, max], [4, 4], [5, 4]]),
  'derived outer apply': lengths([[1, max], [4, 4], [5, 4]]),
  'star of both sides': () => ({"sets": [{"columns": [["id", "Int", null], ["key", "NVarChar", 8000], ["value", "NVarChar", 65535], ["type", "IntN", 4], ["key", "NVarChar", 8000], ["value", "NVarChar", 65535], ["type", "IntN", 4]], "rows": [[1, "a", "1", 2, "a", "2", 2]]}], "errors": [], "done": [1]}),
  'qualified star of one side': lengths([[3, 4]]),
  'top and order in body': lengths([[1, max]]),
  'ambiguous column': () => ({"sets": [], "errors": [{"number": 50000, "class": 16, "state": 1, "message": "Binder Error: Ambiguous reference to column name \"key\" (use: \"l.key\" or \"r.key\")\n\nLINE 1: ...LECT i.id, x.\"key\" FROM items i CROSS JOIN LATERAL (SELECT \"key\" FROM (SELECT 1 AS __msduck_full_join_side WHERE EXISTS...\n                                                                      ^"}], "done": [null]}),
  'trigger diff of json snapshots': () => ({"sets": [], "errors": [{"number": 245, "class": 16, "state": 1, "message": "Conversion Error: Type VARCHAR with value '' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: ...() || '.dbo.audit', 'key') AS \"__store_col_1\", __msduck_check_store_nvarchar(__msduck_carrier_input(__value2), -1, __msdu...\n                                                                         ^"}], "done": [null]}),
}

test('correlated FULL JOIN APPLY bodies match the SQL Server capture', async t => {
  const connection = await start(t)
  assert.equal(reference.cases.length, 24)
  // Every case runs, so one report lists all differences.
  const differences = []
  const differingFromReference = []
  for (const entry of reference.cases) {
    for (const batch of [reset, ...entry.setup]) {
      const setup = await capture(connection, batch)
      assert.equal(setup.errors.length, 0, `${entry.name}: setup failed with ${setup.errors[0]?.number}`)
    }
    const actual = keep(await capture(connection, entry.query))
    const expected = knownResults[entry.name]?.(entry.result) ?? entry.result
    if (!isDeepStrictEqual(actual, expected)) differences.push(`${entry.name}: ${describeFirstDifference(actual, expected)}`)
    if (!isDeepStrictEqual(actual, entry.result)) differingFromReference.push(entry.name)
  }
  assert.deepEqual(differences, [])
  assert.deepEqual(differingFromReference, Object.keys(knownResults))
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
