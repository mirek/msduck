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

// Owner-run SQL Server 2025 capture in a fresh database produced this exact
// response for each batch below: 13606 ends the batch, without 3621, a DML
// DONE command, or results from the later SELECT statements.
test('ISJSON depth errors end DML batches at the captured boundary', { timeout: 30000 }, async t => {
  const connection = await start(t)
  const deep = '['.repeat(129) + '0' + ']'.repeat(129)
  const quoted = `N'${deep}'`
  const create = canonical(await capture(connection, 'CREATE TABLE dbo.depth_probe(v INT,j NVARCHAR(MAX))'))
  assert.deepEqual(create.errors, [])
  const seed = canonical(await capture(connection, `INSERT dbo.depth_probe(v,j) VALUES(5,${quoted})`))
  assert.deepEqual(seed.errors, [])
  const expected = {
    sets: [],
    done: [{ kind: 'done', rowCount: null, more: false }],
    errors: [{
      number: 13606, state: 1, class: 16, lineNumber: 1,
      message: 'JSON text/path that has more than 128 nesting levels cannot be parsed.',
    }],
    info: [], returnStatus: null, rowCount: 0,
  }
  const cases = [
    ['literal insert', `INSERT dbo.depth_probe(v) SELECT 1 WHERE ISJSON(${quoted})=1; SELECT 99 AS after_error; SELECT COUNT(*) AS n FROM dbo.depth_probe`],
    ['column insert', 'INSERT dbo.depth_probe(v) SELECT 1 FROM dbo.depth_probe WHERE ISJSON(j)=1; SELECT 99 AS after_error; SELECT COUNT(*) AS n FROM dbo.depth_probe'],
    ['column update', 'UPDATE dbo.depth_probe SET v=2 WHERE ISJSON(j)=1; SELECT 99 AS after_error; SELECT v FROM dbo.depth_probe'],
    ['column delete', 'DELETE dbo.depth_probe WHERE ISJSON(j)=1; SELECT 99 AS after_error; SELECT COUNT(*) AS n FROM dbo.depth_probe'],
  ]
  for (const [name, sql] of cases) {
    assert.deepEqual(canonical(await capture(connection, sql)), expected, name)
  }
  const remaining = canonical(await capture(connection, 'SELECT v FROM dbo.depth_probe'))
  assert.deepEqual(remaining.sets[0].rows, [[5]])
})
