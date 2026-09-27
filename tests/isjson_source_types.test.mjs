import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { capture, canonical, differences } from '../scripts/lib/compatibility.mjs'
import { start } from './support/client.mjs'

const fixture = JSON.parse(await readFile(new URL('../reference/isjson-source-types.json', import.meta.url), 'utf8'))
const erasedColumnTypes = new Map([
  ['money', 'decimal'],
  ['smallmoney', 'decimal'],
  ['datetime', 'datetime2'],
  ['smalldatetime', 'datetime2'],
])

test('ISJSON source-type binding matches retained SQL Server diagnostics and reports descriptor gap', { timeout: 120000 }, async t => {
  assert.deepEqual(fixture.containers[0].runs[0], fixture.containers[0].runs[1])
  const connection = await start(t)
  const failures = []
  for (const item of fixture.containers[0].runs[0]) {
    const actual = canonical(await capture(connection, item.sql))
    const delta = differences(actual, item.result)
    const descriptorGap = item.result.sets.length ? [{
      path: '/sets/0/columns/0/flags', local: 1, reference: 33,
    }] : []
    const columnType = item.name.endsWith(' source column')
      ? item.name.slice(0, -' source column'.length)
      : null
    const lowered = erasedColumnTypes.get(columnType)
    const sourceTypeGap = lowered ? [{
      path: '/errors/0/message',
      local: `Argument data type ${lowered} is invalid for argument 1 of isjson function.`,
      reference: `Argument data type ${columnType} is invalid for argument 1 of isjson function.`,
    }] : []
    const expected = descriptorGap.concat(sourceTypeGap)
    if (JSON.stringify(delta) !== JSON.stringify(expected)) {
      failures.push({ name: item.name, mode: item.mode, differences: delta.slice(0, 4) })
    }
  }
  assert.equal(failures.length, 0, JSON.stringify(failures))
})
