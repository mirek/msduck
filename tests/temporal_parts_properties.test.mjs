import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { query, start } from './support/client.mjs'

const retained = JSON.parse(readFileSync(new URL('../reference/temporal-parts.json', import.meta.url), 'utf8')).containers[0].runs[0]
const cases = []
for (const family of ['time', 'datetime2', 'datetimeoffset']) {
  for (const suffix of [
    'scale 3 maximum fraction', 'scale 3 empty metadata',
    'precision arithmetic', 'precision division', 'precision bitwise',
    'precision parentheses', 'bound first', 'prepared first',
  ]) {
    const name = `${family} ${suffix}`
    const item = retained.find(item => item.name === name)
    assert.ok(item, `missing reference probe: ${name}`)
    cases.push(item)
  }
}

test('FROMPARTS public descriptors retain SQL Server computed and nullable flags', { timeout: 120000 }, async t => {
  const connection = await start(t)
  for (const item of cases) {
    const parameters = item.parameters?.map(parameter => [
      parameter.name, TYPES[parameter.type], parameter.value,
    ]) ?? []
    const actual = await query(connection, item.sql, parameters)
    const expected = item.result.sets[0]
    assert.deepEqual(actual.columns[0].map(column => [
      column.type.name, column.scale ?? null, column.flags,
    ]), expected.columns.map(column => [
      column.type, column.scale, column.flags,
    ]), item.name)
    assert.equal(actual.rows.length, expected.rows.length, item.name)
  }
})
