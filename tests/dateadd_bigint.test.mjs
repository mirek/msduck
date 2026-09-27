import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { test } from 'node:test'
import { isDeepStrictEqual } from 'node:util'
import { TYPES } from 'tedious'
import { capture, canonical, differences } from '../scripts/lib/compatibility.mjs'
import { describeFirstDifference } from '../scripts/lib/reference.mjs'
import { start } from './support/client.mjs'

const fixture = JSON.parse(await readFile(new URL('../reference/dateadd-bigint.json', import.meta.url), 'utf8'))

function rpc(connection, sql, parameters) {
  return capture({
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      for (const parameter of parameters) {
        assert.equal(parameter.type, 'BigInt')
        request.addParameter(parameter.name, TYPES.BigInt, parameter.value)
      }
      connection.execSql(request)
    },
  }, sql)
}

test('BIGINT DATEADD replays retained SQL Server 2025 behavior', { timeout: 180000 }, async t => {
  assert.equal(fixture.freshDatabases, 4)
  assert.equal(fixture.independentContainers, 2)
  const connection = await start(t)
  const evidence = []
  const semanticDifferences = []
  for (const item of fixture.results) {
    if (item.name === 'server version') continue
    const actual = canonical(await (item.parameters
      ? rpc(connection, item.sql, item.parameters)
      : capture(connection, item.sql)))
    const rawDifferences = differences(actual, item.result)
    evidence.push({ name: item.name, sql: item.sql, actual, reference: item.result, rawDifferences })
    const actualValues = { rows: actual.sets.flatMap(set => set.rows), errorNumbers: actual.errors.map(error => error.number) }
    const referenceValues = { rows: item.result.sets.flatMap(set => set.rows), errorNumbers: item.result.errors.map(error => error.number) }
    if (!isDeepStrictEqual(actualValues, referenceValues)) {
      semanticDifferences.push(`${item.name}: ${describeFirstDifference(actualValues, referenceValues)}`)
    }
    for (let index = 0; index < Math.min(actual.sets.length, item.result.sets.length); index++) {
      const shape = set => set.columns.map(column => [column.type, column.length, column.scale])
      if (!isDeepStrictEqual(shape(actual.sets[index]), shape(item.result.sets[index]))) {
        semanticDifferences.push(`${item.name}: ${describeFirstDifference(shape(actual.sets[index]), shape(item.result.sets[index]))}`)
      }
    }
  }
  await mkdir('artifacts/compatibility', { recursive: true })
  await writeFile('artifacts/compatibility/dateadd-bigint-replay.json', JSON.stringify(evidence) + '\n')
  assert.equal(semanticDifferences.length, 0, `DATEADD value/error-number differences:\n${semanticDifferences.join('\n')}`)
  console.log(`BIGINT DATEADD raw replay: ${evidence.filter(item => !item.rawDifferences.length).length}/${evidence.length} complete; all differences retained`)
})
