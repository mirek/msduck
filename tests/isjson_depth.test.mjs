import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { canonical, capture, differences } from '../scripts/lib/compatibility.mjs'
import { start } from './support/client.mjs'

const fixture = JSON.parse(await readFile(new URL('../reference/isjson-depth.json', import.meta.url), 'utf8'))

function valueFor(input) {
  const { form, depth } = input
  if (form === 'array') return '['.repeat(depth) + '0' + ']'.repeat(depth)
  if (form === 'object') return '{"v":'.repeat(depth) + '0' + '}'.repeat(depth)
  if (form === 'empty-array' || form === 'empty-at-depth') return '['.repeat(depth) + ']'.repeat(depth)
  if (form === 'empty-object') return '{"v":'.repeat(depth - 1) + '{}' + '}'.repeat(depth - 1)
  if (form === 'invalid-before-depth') return '[x' + '['.repeat(depth - 1) + '0' + ']'.repeat(depth)
  if (form === 'invalid-after-depth') return '['.repeat(depth) + 'x' + ']'.repeat(depth)
  if (form === 'unclosed-after-depth') return '['.repeat(depth) + '0' + ']'.repeat(depth - 1)
  if (form === 'incomplete-after-depth') return '['.repeat(depth) + '1e' + ']'.repeat(depth)
  if (form === 'openings-only') return '['.repeat(depth)
  if (form === 'trailing-after-depth') return '['.repeat(depth) + '0' + ']'.repeat(depth) + 'x'
  if (form === 'units') return String.fromCharCode(...input.units)
  throw new Error(`unknown ISJSON input form ${form}`)
}
function rpc(connection, sql, value) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch: request => {
      request.addParameter('j', TYPES.NVarChar, value, { length: Infinity })
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

test('ISJSON depth and Unicode match raw reference except recorded flags', { timeout: 180000 }, async t => {
  assert.deepEqual(fixture.containers[0].runs[0], fixture.containers[0].runs[1])
  const connection = await start(t)
  for (const item of fixture.containers[0].runs[0]) {
    const actual = canonical(await rpc(connection, item.sql, valueFor(item.input)))
    const delta = differences(actual, item.result)
    const expected = [{ path: '/sets/0/columns/0/flags', local: 1, reference: 33 }]
    assert.deepEqual(delta, expected, `${item.name} / ${item.mode}`)
  }
})
