import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { start, query } from './support/client.mjs'
import { capture, canonical } from '../scripts/lib/compatibility.mjs'

const cases = JSON.parse(await readFile(new URL('../reference/replicate-float.json', import.meta.url), 'utf8')).runs[0]

function parameterCapture(connection, sql, type, value) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch: request => {
      request.addParameter('x', type, value)
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

test('REAL and FLOAT REPLICATE values, descriptors and completions match SQL Server', { timeout: 120000 }, async t => {
  const connection = await start(t, { options: { requestTimeout: 15000 } })
  for (const item of cases) {
    if (item.name === 'r column') {
      await query(connection, 'CREATE TABLE dbo.replicate_float_source(id INT,r REAL,f FLOAT)')
      await query(connection, 'INSERT dbo.replicate_float_source VALUES (1,1,1),(2,-0.0,-0.0),(3,1.23456789,1.23456789),(4,0.00001,0.00001),(5,1000000,1000000),(6,NULL,NULL)')
    }
    const parameter = item.name.endsWith(' parameter')
    const type = item.name.startsWith('REAL ') ? TYPES.Real : TYPES.Float
    const value = item.name.includes('one') ? 1
      : item.name.includes('rounded') ? 1.23456789
        : item.name.includes('tiny') ? 0.00001 : null
    const actual = canonical(await (parameter
      ? parameterCapture(connection, item.sql, type, value)
      : capture(connection, item.sql)))
    assert.deepEqual(actual.errors, item.result.errors, item.name)
    assert.deepEqual(actual.sets[0].rows.map(row => row[0]), item.result.sets[0].rows.map(row => row[0]), item.name)
    assert.deepEqual(actual.sets[0].columns[0], item.result.sets[0].columns[0], item.name)
    assert.deepEqual(actual.done, item.result.done, item.name)
  }
})
