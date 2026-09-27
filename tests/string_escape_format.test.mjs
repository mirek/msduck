import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { capture, canonical, differences } from '../scripts/lib/compatibility.mjs'
import { start } from './support/client.mjs'

const fixture = JSON.parse(await readFile(new URL('../reference/string-escape-format.json', import.meta.url), 'utf8'))

function rpc(connection, sql, type, value) {
  return capture({
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      request.addParameter('f', type, value)
      connection.execSql(request)
    },
  }, sql)
}

test('STRING_ESCAPE format cases preserve exact SQL Server comparison and computed-flag gap', { timeout: 120000 }, async t => {
  assert.deepEqual(fixture.runs[0], fixture.runs[1], 'reference captures must agree across fresh databases')
  const connection = await start(t)
  for (const item of fixture.runs[0]) {
    const type = item.parameter && TYPES[item.parameter.type]
    if (item.parameter) assert.ok(type, item.name)
    const actual = canonical(await (item.parameter
      ? rpc(connection, item.sql, type, item.parameter.value)
      : capture(connection, item.sql)))
    // The only remaining difference is the result_properties computed bit.
    // Keep it visible in the raw comparison until the disjoint successor owns
    // that shared SQL metadata rule; do not normalize the fixture or capture.
    const expectedDiff = item.result.sets.length
      ? [{ path: '/sets/0/columns/0/flags', local: 1, reference: 33 }]
      : []
    assert.deepEqual(differences(actual, item.result), expectedDiff, item.name)
  }
})
