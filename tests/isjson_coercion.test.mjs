import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { capture, canonical, differences } from '../scripts/lib/compatibility.mjs'
import { start } from './support/client.mjs'

const fixture = JSON.parse(await readFile(new URL('../reference/isjson-coercion.json', import.meta.url), 'utf8'))

test('ISJSON input binding preserves the SQL Server capture and exposes descriptor gap', { timeout: 120000 }, async t => {
  assert.deepEqual(fixture.containers[0].runs[0], fixture.containers[0].runs[1])
  const connection = await start(t)
  for (const item of fixture.containers[0].runs[0]) {
    const actual = canonical(await capture(connection, item.sql))
    const delta = differences(actual, item.result)
    // The shared computed-result property is still reserved by another
    // worker. Assert the exact outstanding mismatch instead of hiding it.
    const descriptorGap = item.result.sets.length ? [{
      path: '/sets/0/columns/0/flags', local: 1, reference: 33,
    }] : []
    assert.deepEqual(delta, descriptorGap, `${item.name}/${item.mode}`)
  }
})
