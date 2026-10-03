// ISNULL, COALESCE, IIF and CASE over OPENJSON key and value columns (issue
// #900, docs/openjson-isnull.md). Every case replays the SQL Server capture in
// reference/openjson-isnull.json: setup batches, then one query batch whose
// result sets (names, types, lengths, rows), errors and DONE counts must match.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { isDeepStrictEqual } from 'node:util'
import { test } from 'node:test'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'
import { describeFirstDifference } from '../../scripts/lib/reference.mjs'
import { start } from '../support/client.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/openjson-isnull.json', import.meta.url)))
const keep = result => canonical({
  sets: result.sets.map(set => ({ columns: set.columns.map(c => [c.name, c.type, c.length]), rows: set.rows })),
  errors: result.errors.map(e => ({ number: e.number, class: e.class ?? null, state: e.state ?? null, message: e.message })),
  done: result.done.filter(d => d.kind === 'done' || d.kind === 'doneInProc').map(d => d.rowCount),
})
const reset = 'DROP TRIGGER IF EXISTS docs_audit; DROP FUNCTION IF EXISTS dbo.foo; DROP TABLE IF EXISTS items; DROP TABLE IF EXISTS audit; DROP TABLE IF EXISTS docs;'

// Remaining differences are outside this lowering (docs/openjson-isnull.md).
// Each known case asserts msduck's complete current result, so any further
// change, fix or regression, fails here.
const knownResults = {}

test('ISNULL, COALESCE, IIF and CASE over OPENJSON match the SQL Server capture', async t => {
  const connection = await start(t)
  assert.equal(reference.cases.length, 17)
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
