import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { test } from 'node:test'
import { isDeepStrictEqual } from 'node:util'
import { TYPES } from 'tedious'
import { capture, canonical, differences } from '../scripts/lib/compatibility.mjs'
import { describeFirstDifference } from '../scripts/lib/reference.mjs'
import { start } from './support/client.mjs'

const fixture = JSON.parse(await readFile(new URL('../reference/datediff-numeric.json', import.meta.url), 'utf8'))

function rpc(connection, sql, parameters) {
  return capture({
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      for (const parameter of parameters) {
        const type = TYPES[parameter.type]
        assert.ok(type, parameter.type)
        request.addParameter(parameter.name, type, parameter.value, parameter.options)
      }
      connection.execSql(request)
    },
  }, sql)
}

test('numeric DATEDIFF replays raw SQL Server observations', { timeout: 120000 }, async t => {
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
    const actualValues = { rows: actual.sets.map(set => set.rows), errorNumbers: actual.errors.map(error => error.number) }
    const referenceValues = { rows: item.result.sets.map(set => set.rows), errorNumbers: item.result.errors.map(error => error.number) }
    if (!isDeepStrictEqual(actualValues, referenceValues)) semanticDifferences.push(`${item.name}: ${describeFirstDifference(actualValues, referenceValues)}`)
  }
  await mkdir('artifacts/compatibility', { recursive: true })
  await writeFile('artifacts/compatibility/datediff-numeric-replay.json', JSON.stringify(evidence) + '\n')
  assert.equal(semanticDifferences.length, 0, `numeric DATEDIFF value/error-number differences:\n${semanticDifferences.join('\n')}`)
  const knownDifference = ({ path, local, reference }) =>
    (/^\/sets\/\d+\/columns\/\d+\/flags$/.test(path) && local === 1 && reference === 33)
    || (path === '/errors/0/state' && local === 1 && [0, 2].includes(reference))
    || (path === '/errors/0/message' && local === `Invalid Input Error: ${reference}`)
  const unexpected = evidence.flatMap(item => item.rawDifferences
    .filter(difference => !knownDifference(difference))
    .map(difference => `${item.name}: ${difference.path}`))
  assert.equal(unexpected.length, 0, `new raw differences beyond documented shared metadata/diagnostics: ${unexpected.join(', ')}`)
  const complete = evidence.filter(item => item.rawDifferences.length === 0).length
  console.log(`numeric DATEDIFF: ${complete}/${evidence.length} complete raw captures; all values and error numbers match; remaining raw differences retained in artifacts/compatibility/datediff-numeric-replay.json`)
})
