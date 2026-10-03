#!/usr/bin/env node
// Retain SQL Server's implicit REAL/FLOAT-to-VARCHAR conversion as seen by REPLICATE.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/replicate-float.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/replicate-float/capture.json')

const values = [
  ['zero', '0'], ['negative zero', '-0.0'], ['one', '1'], ['negative one', '-1'],
  ['fraction', '1.23456789'], ['negative fraction', '-1.23456789'],
  ['half', '0.5'], ['just below million', '999999'], ['million', '1000000'],
  ['round into million', '999999.5'], ['round below million', '999999.4'],
  ['large', '123456789'], ['small', '0.0001'], ['smaller', '0.00001'],
  ['round into fixed', '0.00009999995'], ['round below fixed', '0.00009999994'],
  ['sixth digit lower', '1.234564'], ['sixth digit midpoint', '1.234565'],
  ['sixth digit upper', '1.234566'], ['negative smaller', '-0.00001'],
  ['very small', '0.0000000123456789'], ['very large', '1234567890123456789'],
]
const types = ['REAL', 'FLOAT']
const query = value => `SELECT REPLICATE(${value},2) AS repeated,CONVERT(VARCHAR(23),${value}) AS converted,DATALENGTH(REPLICATE(${value},2)) AS repeated_bytes`

function rpc(connection, sql, type, value) {
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

async function observe(connection) {
  const results = []
  const record = async (name, sql, run) => {
    const result = canonical(await run())
    assert.equal(result.errors.length, 0, name)
    assert.equal(result.sets.length, 1, name)
    assert.equal(result.sets[0].rows.length > 0, true, name)
    assert.equal(result.done.length > 0, true, name)
    results.push({ name, sql, result })
  }
  for (const type of types) {
    for (const [label, value] of values) {
      const sql = query(`CAST(${value} AS ${type})`)
      await record(`${type} literal ${label}`, sql, () => capture(connection, sql))
    }
    const nullSql = query(`CAST(NULL AS ${type})`)
    await record(`${type} literal NULL`, nullSql, () => capture(connection, nullSql))
  }
  const create = 'CREATE TABLE dbo.replicate_float_source(id INT,r REAL,f FLOAT)'
  await capture(connection, create)
  const insert = `INSERT dbo.replicate_float_source VALUES (1,1,1),(2,-0.0,-0.0),(3,1.23456789,1.23456789),(4,0.00001,0.00001),(5,1000000,1000000),(6,NULL,NULL)`
  await capture(connection, insert)
  for (const column of ['r','f']) {
    const sql = `SELECT id,REPLICATE(${column},2) AS repeated,CONVERT(VARCHAR(23),${column}) AS converted,DATALENGTH(REPLICATE(${column},2)) AS repeated_bytes FROM dbo.replicate_float_source ORDER BY id`
    await record(`${column} column`, sql, () => capture(connection, sql))
  }
  for (const [name, type, value] of [
    ['REAL one', TYPES.Real, 1], ['FLOAT one', TYPES.Float, 1],
    ['REAL rounded', TYPES.Real, 1.23456789], ['FLOAT rounded', TYPES.Float, 1.23456789],
    ['REAL tiny', TYPES.Real, 0.00001], ['FLOAT tiny', TYPES.Float, 0.00001],
    ['REAL NULL', TYPES.Real, null], ['FLOAT NULL', TYPES.Float, null],
  ]) {
    const sql = query('@x')
    await record(`${name} parameter`, sql, () => rpc(connection, sql, type, value))
  }
  assert.deepEqual(results.find(item => item.name === 'REAL literal one').result.sets[0].rows, [['11','1',2]])
  assert.deepEqual(results.find(item => item.name === 'FLOAT literal one').result.sets[0].rows, [['11','1',2]])
  return results
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) runs.push(await isolatedReference(config, observe))
  assertSameCapture(runs[0], runs[1], 'REAL/FLOAT REPLICATE captures differ across fresh databases')
  const actual = { image: container.image, runs }
  await writeFile(output, JSON.stringify(actual) + '\n')
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'REAL/FLOAT REPLICATE capture differs from retained fixture')
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} REAL/FLOAT REPLICATE cases twice${retained ? ' and matched retained fixture' : ''}`)
})
