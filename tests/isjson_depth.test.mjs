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

test('ISJSON depth and Unicode retain raw reference differences', { timeout: 180000 }, async t => {
  assert.deepEqual(fixture.containers[0].runs[0], fixture.containers[0].runs[1])
  const connection = await start(t)
  for (const item of fixture.containers[0].runs[0]) {
    const actual = canonical(await rpc(connection, item.sql, valueFor(item.input)))
    const delta = differences(actual, item.result)
    const overDepth = item.result.errors[0]?.number === 13606
    const descriptorGap = { path: '/sets/0/columns/0/flags', local: 1, reference: 33 }
    let expected = [descriptorGap]
    if (overDepth) {
      const localValue = ['unclosed-after-depth', 'trailing-after-depth'].includes(item.input.form) ? 0 : 1
      const missing = { kind: 'missing' }
      const depthError = {
        number: 13606, state: 1, class: 16, lineNumber: 1,
        message: 'JSON text/path that has more than 128 nesting levels cannot be parsed.',
      }
      expected = [
        descriptorGap,
        { path: '/sets/0/rows/0', local: [localValue], reference: missing },
        { path: '/done/0/kind', local: 'doneInProc', reference: 'doneProc' },
        { path: '/done/0/rowCount', local: 1, reference: null },
        { path: '/done/0/more', local: true, reference: false },
        { path: '/done/1', local: { kind: 'doneProc', rowCount: null, more: false }, reference: missing },
        { path: '/errors/0', local: missing, reference: depthError },
        { path: '/returnStatus', local: 0, reference: missing },
        { path: '/rowCount', local: 1, reference: 0 },
      ]
    }
    assert.deepEqual(delta, expected, `${item.name} / ${item.mode}`)
  }
})
