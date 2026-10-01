// #temp tables and table variables (gaps-temp-tables-v1): the probes of
// scripts/capture-gaps-temp_tables.mjs replayed against msduck, compared with
// the SQL Server capture in reference/gaps-temp_tables.json: every row value,
// every error number, and state and class of the errors this feature raises.
// See docs/gaps-temp_tables.md.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { Connection, Request } from 'tedious'
import { start, query } from '../support/client.mjs'
import { observe } from '../../scripts/capture-gaps-temp_tables.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/gaps-temp_tables.json', import.meta.url), 'utf8')).results

// Errors raised by the feature itself carry SQL Server's state and class.
const exact = new Set([102, 208, 1087, 2714, 3701])

function open(connection, database) {
  const other = new Connection({ ...connection.config, options: { ...connection.config.options, database } })
  other.on('error', () => {})
  return new Promise((resolve, reject) => other.connect(error => error ? reject(error) : resolve(other)))
}

test('temporary objects match the SQL Server capture', { timeout: 120000 }, async t => {
  const connection = await start(t)
  await query(connection, 'CREATE DATABASE temp_tables_probe')
  await query(connection, 'USE temp_tables_probe')
  const run = await observe(connection, database => open(connection, database))
  assert.equal(run.length, reference.length)
  for (const [index, expected] of reference.entries()) {
    const actual = run[index]
    assert.equal(actual.name, expected.name)
    const label = `${expected.name}: ${expected.sql}`
    assert.deepEqual(actual.result.sets.map(set => set.rows), expected.result.sets.map(set => set.rows), `${label}: rows`)
    assert.deepEqual(actual.result.errors.map(error => error.number), expected.result.errors.map(error => error.number), `${label}: errors ${JSON.stringify(actual.result.errors)}`)
    for (const [position, error] of expected.result.errors.entries()) {
      if (!exact.has(error.number)) continue
      const { number, state, class: severity, message } = actual.result.errors[position]
      assert.deepEqual({ number, state, class: severity, message }, { number, state: error.state, class: error.class, message: error.message }, label)
    }
  }
})
