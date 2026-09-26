import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { start } from './support/client.mjs'
import { capture, canonical, differences } from '../scripts/lib/compatibility.mjs'

const fixture = JSON.parse(await readFile(new URL('../reference/temporal-parts.json', import.meta.url), 'utf8'))
const names = ['time precision cast', 'datetime2 precision cast', 'datetimeoffset precision cast']
const samples = fixture.containers[0].runs[0].filter(sample => names.includes(sample.name))
assert.deepEqual(samples.map(sample => sample.name), names)
for (const container of fixture.containers) {
  for (const run of container.runs) {
    assert.deepEqual(run.filter(sample => names.includes(sample.name)), samples,
      'the retained SQL Server captures must agree before replay')
  }
}

test('constant INT casts select the captured temporal FROMPARTS scale and descriptors', async t => {
  const connection = await start(t)
  for (const sample of samples) {
    const actual = canonical(await capture(connection, sample.sql))
    assert.deepEqual(differences(actual, sample.result), [{
      path: '/sets/0/columns/0/flags', local: 1, reference: 33,
    }], `${sample.name} retains the known computed-column flag gap`)

    const empty = canonical(await capture(connection, `${sample.sql} WHERE 1=0`))
    assert.deepEqual(empty.errors, [], `${sample.name} empty result errors`)
    assert.deepEqual(empty.sets[0].rows, [], `${sample.name} empty result rows`)
    assert.deepEqual(empty.sets[0].columns[0], {
      ...sample.result.sets[0].columns[0], flags: 1,
    }, `${sample.name} retains the known empty-result computed-column flag gap`)
    assert.deepEqual(empty.done, [{ kind: 'done', rowCount: 0, more: false }],
      `${sample.name} empty result completion`)
  }
})

test('nonconstant and non-INT casts cannot determine temporal metadata', async t => {
  const connection = await start(t)
  for (const scale of ['CAST(3 AS BIGINT)', 'CAST(3 AS SMALLINT)', 'CAST(NULL AS INT)', 'CAST(@p AS INT)']) {
    const sql = `DECLARE @p INT=3; SELECT TIMEFROMPARTS(1,2,3,0,${scale}) AS value`
    const actual = canonical(await capture(connection, sql))
    assert.equal(actual.errors[0]?.number, 10760, scale)
    assert.equal(actual.errors[0]?.state, 1, scale)
  }
})

test('constant INT conversion forms keep compile-time scale selection', async t => {
  const connection = await start(t)
  for (const [expression, scale] of [
    ['CAST(3.7 AS INT)', 3],
    ["CAST('3' AS INT)", 3],
    ['TRY_CAST(3 AS INT)', 3],
    ['CONVERT(INT,3)', 3],
    ['CAST(3 AS INT)+1', 4],
  ]) {
    const actual = canonical(await capture(connection,
      `SELECT TIMEFROMPARTS(1,2,3,0,${expression}) AS value`))
    assert.deepEqual(actual.errors, [], expression)
    assert.equal(actual.sets[0].columns[0].scale, scale, expression)
    assert.equal(actual.sets[0].rows.length, 1, expression)
  }
})
