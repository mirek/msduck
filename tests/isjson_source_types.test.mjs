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

test('ISJSON source types retain captured diagnostics and report remaining differences', { timeout: 120000 }, async t => {
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
    const missingColumnType = item.name.endsWith(' missing column')
      ? item.name.slice(0, -' missing column'.length)
      : null
    let nameResolutionGap = []
    if (missingColumnType) {
      assert.equal(actual.errors.length, 1, item.name)
      assert.equal(actual.errors[0].number, 50000, item.name)
      assert.ok(
        actual.errors[0].message.startsWith(
          'Binder Error: Referenced column "missing_column" was not found because the FROM clause is missing\n\nLINE 1:',
        ) && actual.errors[0].message.includes('missing_column'),
        item.name,
      )
      nameResolutionGap = [
        { path: '/errors/0/number', local: 50000, reference: 207 },
        { path: '/errors/0/message', local: actual.errors[0].message, reference: "Invalid column name 'missing_column'." },
      ]
    }
    const expected = descriptorGap.concat(sourceTypeGap, nameResolutionGap)
    if (JSON.stringify(delta) !== JSON.stringify(expected)) {
      failures.push({ name: item.name, mode: item.mode, differences: delta.slice(0, 4) })
    }
  }
  assert.equal(failures.length, 0, JSON.stringify(failures))
})
