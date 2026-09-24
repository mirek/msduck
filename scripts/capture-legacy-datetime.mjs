// First-party SQL Server evidence: legacy temporal conversions are not merely
// backend TIMESTAMP casts followed by wire rounding.
import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {TYPES} from 'tedious'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

const cases = []
for (const type of ['DateTime', 'SmallDateTime']) {
  for (const value of [null, '2000-01-01T12:00:00.000Z']) {
    cases.push({id: `${type}-${value === null ? 'null' : 'value'}`, sql: 'SELECT @d AS value', parameter: {type, value}})
  }
}
for (const target of ['DATETIME', 'SMALLDATETIME']) {
  for (const fraction of ['29.998', '29.999', '29.9983333', '29.9983334', '30.000']) {
    const text = `2000-01-01T12:00:${fraction}`
    for (const source of ['string', 'datetime2']) {
      const input = source === 'string' ? `'${text}'` : `CAST('${text}' AS DATETIME2(7))`
      cases.push({id: `${target}-${source}-${fraction}`, sql: `SELECT CAST(${input} AS ${target}) AS value`})
    }
  }
  cases.push({id: `${target}-try-long-fraction`, sql: `SELECT TRY_CAST('2000-01-01T12:00:29.9983333' AS ${target}) AS value`})
  cases.push({id: `${target}-try-invalid`, sql: `SELECT TRY_CAST('invalid' AS ${target}) AS value`})
  for (const value of ['1899-12-31T23:59:59.999', '2079-06-06T23:59:29.998', '2079-06-06T23:59:29.999', '1752-12-31T23:59:59.999', '9999-12-31T23:59:59.999']) {
    cases.push({id: `${target}-range-${value}`, sql: `SELECT CAST('${value}' AS ${target}) AS value`})
  }
}
for (const target of ['DATETIME', 'SMALLDATETIME']) {
  for (const text of ['1752-12-31T23:59:59.9999999', '1899-12-31T23:59:59.9999999', '2079-06-06T23:59:29.9999999', '2079-06-06T23:59:30.0000000', '9999-12-31T23:59:59.9999999']) {
    cases.push({id: `${target}-typed-range-${text}`, sql: `SELECT CAST(CAST('${text}' AS DATETIME2(7)) AS ${target}) AS value`})
  }
  for (const text of ['2000-01-01T12:00:29.9983333', 'invalid']) {
    cases.push({id: `${target}-nvarchar-${text}`, sql: `SELECT CAST(N'${text}' AS ${target}) AS value`})
  }
  for (const source of ['string', 'datetime2']) {
    const text = '2000-01-01T12:00:29.999'
    const input = source === 'string' ? `'${text}'` : `CAST('${text}' AS DATETIME2(7))`
    cases.push({id: `${target}-${source}-rounded-expression`, sql: `SELECT CONVERT(VARCHAR(33),CAST(CAST(${input} AS ${target}) AS DATETIME2(7)),126) AS rounded,DATEPART(ns,CAST(${input} AS ${target})) AS fraction,CASE WHEN CAST(${input} AS ${target})=CAST('${text}' AS DATETIME2(7)) THEN 1 ELSE 0 END AS equal_original`})
  }
  cases.push({id: `${target}-assignment-rounding`, sql: `CREATE TABLE legacy_assignment(id INT,value ${target}); INSERT INTO legacy_assignment VALUES(1,'2000-01-01T12:00:29.999'),(2,CAST('2000-01-01T12:00:29.999' AS DATETIME2(7))); SELECT id,CONVERT(VARCHAR(33),CAST(value AS DATETIME2(7)),126) AS rounded,DATEPART(ns,value) AS fraction FROM legacy_assignment ORDER BY id; DROP TABLE legacy_assignment`})
  cases.push({id: `${target}-separate-assignment-rounding`, sql: `CREATE TABLE legacy_assignment(id INT,value ${target}); INSERT INTO legacy_assignment VALUES(1,'2000-01-01T12:00:29.999'); INSERT INTO legacy_assignment VALUES(2,CAST('2000-01-01T12:00:29.999' AS DATETIME2(7))); SELECT id,CONVERT(VARCHAR(33),CAST(value AS DATETIME2(7)),126) AS rounded,DATEPART(ns,value) AS fraction FROM legacy_assignment ORDER BY id; DROP TABLE legacy_assignment`})
}
for (const target of ['DATETIME', 'SMALLDATETIME']) {
  for (const text of ['0001-01-01T00:00:00', '1752-12-31T23:59:59.9983333', '1752-12-31T23:59:59.9983334', '9999-12-31T23:59:59.9983334']) {
    cases.push({id: `${target}-typed-edge-${text}`, sql: `SELECT CAST(CAST('${text}' AS DATETIME2(7)) AS ${target}) AS value`})
  }
}
const output = resolve(process.argv[2] ?? 'artifacts/compatibility/legacy-datetime-reference')
await mkdir(output, {recursive: true})
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(await isolatedReference(config, async connection => {
      const results = []
      let tokens = []
      const debugToken = connection.debug.token.bind(connection.debug)
      connection.debug.token = token => {
        if (token.name.startsWith('DONE')) tokens.push(JSON.parse(JSON.stringify(token)))
        debugToken(token)
      }
      for (const entry of cases) {
        connection.execSqlBatch = request => {
          if (entry.parameter) {
            const {type, value} = entry.parameter
            request.addParameter('d', TYPES[type], value === null ? null : new Date(value))
          }
          connection.execSql(request)
        }
        tokens = []
        const result = canonical(await capture(connection, entry.sql))
        const completion = [...tokens]
        connection.execSqlBatch = request => connection.execSql(request)
        const state = canonical(await capture(connection, 'SELECT @@ROWCOUNT AS r,@@ERROR AS e'))
        results.push({...entry, result, completion, state})
      }
      return results
    }))
    await writeFile(resolve(output, 'runs.json'), JSON.stringify(runs, null, 2) + '\n')
  }
  assert.deepEqual(runs[0], runs[1], 'Fresh legacy datetime captures differ')
  const actual = {image: container.image, identicalFreshCaptures: 2, mode: 'rpc', results: runs[0]}
  await writeFile(resolve(output, 'legacy-datetime.json'), JSON.stringify(actual, null, 2) + '\n')
  let retained
  try { retained = JSON.parse(await readFile(new URL('../reference/legacy-datetime.json', import.meta.url), 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assert.deepEqual(actual, retained, 'Retained legacy datetime reference differs')
  console.log(`Captured ${cases.length} legacy datetime cases twice identically${retained ? ' and matched retained fixture' : ''}`)
})
