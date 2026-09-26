#!/usr/bin/env node
// First-party SQL Server FLOAT/REAL character-conversion observations.
// Keep raw rows, descriptors, errors and completion events from fresh containers.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/float-character.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/float-character/capture.json')

function rpc(connection, sql, parameters) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch: request => {
      for (const [name, type, value] of parameters) request.addParameter(name, type, value)
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    const result = canonical(await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql)))
    records.push({ name, sql, ...(parameters ? {
      // JSON has no -0 literal. Record the requested input explicitly so a
      // loaded fixture can be compared with a fresh in-memory capture.
      parameters: parameters.map(([parameter, type, value]) => ({
        name: parameter, type: type.name,
        value: Object.is(value, -0) ? { kind: 'negative-zero' } : value,
      })),
    } : {}), result })
  }

  // Compare explicit CAST and default CONVERT, including cases where the
  // physical REAL value differs from the decimal token or FLOAT value.
  const values = [
    ['zero', '0'], ['negative zero', '-0.0'], ['one', '1'], ['negative one', '-1'],
    ['fraction', '1.23456789'], ['round six digits', '1.234565'],
    ['below million', '999999.4'], ['round into million', '999999.5'],
    ['million', '1000000'], ['fixed threshold', '0.0001'],
    ['below fixed threshold', '0.00009999995'], ['scientific', '0.00001'],
    ['very large', '1234567890123456789'], ['null', 'NULL'],
  ]
  for (const kind of ['REAL', 'FLOAT']) {
    for (const [label, value] of values) {
      const source = `CAST(${value} AS ${kind})`
      await record(`${kind} ${label} CAST/default`, `SELECT CAST(${source} AS VARCHAR(23)) AS cast_v,CONVERT(VARCHAR(23),${source}) AS convert_v,CAST(${source} AS NVARCHAR(23)) AS cast_n,CONVERT(NVARCHAR(23),${source}) AS convert_n`)
    }
  }

  const source = 'CAST(1.23456789 AS FLOAT)'
  const nullSource = 'CAST(NULL AS FLOAT)'
  for (const family of ['VARCHAR', 'CHAR', 'NVARCHAR', 'NCHAR']) {
    for (const width of ['', '(1)', '(4)', '(23)', ...(family.endsWith('VARCHAR') ? ['(MAX)'] : [])]) {
      const target = `${family}${width}`
      const forms = [
        ['CAST', `CAST(${source} AS ${target})`],
        ['CONVERT', `CONVERT(${target},${source})`],
        ['TRY_CAST', `TRY_CAST(${source} AS ${target})`],
        ['TRY_CONVERT', `TRY_CONVERT(${target},${source})`],
      ]
      if (width === '(1)' || width === '(4)') {
        // A failing first projection hides later TRY results in one batch.
        for (const [form, expression] of forms) {
          await record(`${target} ${form}`, `SELECT ${expression} AS converted`)
        }
      } else {
        await record(`${target} forms`, `SELECT ${forms.map(([form, expression]) => `${expression} AS ${form.toLowerCase()}_value`).join(',')}`)
      }
      await record(`${target} null`, `SELECT CAST(${nullSource} AS ${target}) AS cast_value,TRY_CONVERT(${target},${nullSource}) AS try_value`)
    }
  }

  // The style argument is legal syntactically only on CONVERT/TRY_CONVERT.
  // Retain exact errors for unsupported styles and targets.
  for (const kind of ['REAL', 'FLOAT']) {
    const value = `CAST(1.23456789 AS ${kind})`
    for (const target of ['VARCHAR(32)', 'NVARCHAR(32)', 'CHAR(32)', 'NCHAR(32)']) {
      for (const style of [0, 1, 2, 3, -1, 4, 99]) {
        await record(`${kind} ${target} style ${style}`, `SELECT CONVERT(${target},${value},${style}) AS converted,TRY_CONVERT(${target},${value},${style}) AS tried`)
      }
    }
    for (const [label, number] of [
      ['negative zero', '-0.0'], ['one', '1'], ['negative one', '-1'],
      ['small', '0.00001'], ['million', '1000000'], ['null', 'NULL'],
    ]) {
      const operand = `CAST(${number} AS ${kind})`
      await record(`${kind} styled ${label}`, `SELECT CONVERT(VARCHAR(32),${operand},1) AS style_1,CONVERT(VARCHAR(32),${operand},2) AS style_2,CONVERT(VARCHAR(32),${operand},3) AS style_3`)
    }
    for (const family of ['VARCHAR', 'NVARCHAR']) {
      for (const width of [4, 16]) {
        for (const style of [1, 2, 3]) {
          const target = `${family}(${width})`
          await record(`${kind} ${target} style ${style} narrow CONVERT`, `SELECT CONVERT(${target},${value},${style}) AS converted`)
          await record(`${kind} ${target} style ${style} narrow TRY_CONVERT`, `SELECT TRY_CONVERT(${target},${value},${style}) AS converted`)
        }
      }
    }
  }

  await record('create columns', 'CREATE TABLE dbo.float_character_source(id INT PRIMARY KEY,r REAL,f FLOAT)')
  await record('insert columns', `INSERT dbo.float_character_source VALUES
    (1,1,1),(2,-0.0,-0.0),(3,1.23456789,1.23456789),
    (4,0.00009999995,0.00009999995),(5,1000000,1000000),(6,NULL,NULL)`)
  for (const [name, column] of [['REAL', 'r'], ['FLOAT', 'f']]) {
    await record(`${name} column casts`, `SELECT id,CAST(${column} AS VARCHAR(23)) AS v,CONVERT(VARCHAR(23),${column}) AS cv,CAST(${column} AS NVARCHAR(23)) AS n,CONVERT(CHAR(23),${column}) AS c FROM dbo.float_character_source ORDER BY id`)
  }

  const rpcSql = 'SELECT CAST(@n AS VARCHAR(23)) AS cast_v,CONVERT(NVARCHAR(23),@n) AS convert_n,TRY_CONVERT(CHAR(4),@n) AS try_c'
  for (const [kind, type] of [['REAL', TYPES.Real], ['FLOAT', TYPES.Float]]) {
    for (const [label, value] of [['one', 1], ['fraction', 1.23456789], ['negative zero', -0], ['null', null]]) {
      await record(`${kind} RPC ${label}`, rpcSql, [['n', type, value]])
    }
  }
  return records
}

function validate(run) {
  const names = new Set(run.map(item => item.name))
  assert.equal(names.size, run.length, 'duplicate probe names')
  assert.equal(run.length, 28 + 60 + 56 + 12 + 48 + 4 + 8)
  for (const item of run) assert(item.result.done.length > 0, `${item.name}: no completion`)
  const one = run.find(item => item.name === 'FLOAT one CAST/default')
  assert.deepEqual(one.result.sets[0].rows, [['1', '1', '1', '1']])
  const nullCase = run.find(item => item.name === 'FLOAT null CAST/default')
  assert.deepEqual(nullCase.result.sets[0].rows, [[null, null, null, null]])
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
const runs = []
let image
for (let repeat = 0; repeat < 2; repeat++) {
  await withReferenceContainer(async (config, container) => {
    image ??= container.image
    assert.equal(image, container.image)
    const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, observe)
    validate(run)
    runs.push(run)
  })
}
assertSameCapture(runs[0], runs[1], 'FLOAT/REAL character conversions differ across fresh containers')
const actual = { image, runs }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (retained) assertSameCapture(actual, retained, 'FLOAT/REAL character conversions differ from retained fixture')
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${runs[0].length} FLOAT/REAL character conversions twice in fresh containers${retained ? ' and matched retained fixture' : ''}`)
