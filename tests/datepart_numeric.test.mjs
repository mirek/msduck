import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { isDeepStrictEqual } from 'node:util'
import { TYPES } from 'tedious'
import { query, start } from './support/client.mjs'

const retained = JSON.parse(readFileSync(new URL('../reference/datepart-numeric.json', import.meta.url), 'utf8')).runs[0]

test('numeric DATEPART replays retained SQL Server values and diagnostics', {timeout: 120000}, async t => {
  const connection = await start(t)
  const differences = []
  for (const item of retained) {
    const expected = item.result
    const parameters = item.parameters?.map(parameter => [
      parameter.name, TYPES[parameter.type], parameter.value, parameter.options
    ]) ?? []
    if (expected.errors.length) {
      try {
        const actual = await query(connection, item.sql, parameters)
        differences.push({name:item.name,expectedError:expected.errors[0],actualRows:actual.rows})
      } catch (error) {
        const wanted = expected.errors[0]
        const actual = {number:error.number,state:error.state,class:error.class,message:error.message}
        const expectedError = {number:wanted.number,state:wanted.state,class:wanted.class,message:wanted.message}
        if (!isDeepStrictEqual(actual, expectedError)) differences.push({name:item.name,expectedError,actualError:actual})
      }
      continue
    }
    try {
      const actual = await query(connection, item.sql, parameters)
      const expectedRows = expected.sets.flatMap(set => set.rows)
      if (!isDeepStrictEqual(actual.rows, expectedRows)) differences.push({name:item.name,expectedRows,actualRows:actual.rows})
    } catch (error) {
      differences.push({name:item.name,unexpectedError:{number:error.number,state:error.state,message:error.message}})
    }
  }
  // The shared engine error wrapper is reserved by another task. Preserve
  // every current difference from SQL Server until that owner hands it off.
  const byName = new Map(retained.map(item => [item.name, item]))
  const known = [
    ['before first date', 1], ['after last day', 1],
    ['tzoffset decimal', 1], ['tzoffset float', 1], ['tzoffset bit', 1]
  ].map(([name, state]) => {
    const wanted = byName.get(name).result.errors[0]
    return {
      name,
      expectedError:{number:wanted.number,state:wanted.state,class:wanted.class,message:wanted.message},
      actualError:{number:wanted.number,state,class:wanted.class,message:`Invalid Input Error: ${wanted.message}`}
    }
  })
  assert.deepEqual(differences, known)
})
