import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

const setup = ['CREATE TABLE dbo.width_user(id INT)', 'CREATE VIEW dbo.width_view AS SELECT id FROM dbo.width_user']
const queries = [
  "SELECT name,type,type_desc,LEN(type) AS logical_length,DATALENGTH(type) AS bytes,CAST(type AS VARBINARY(2)) AS payload FROM sys.objects WHERE name IN ('width_user','width_view') ORDER BY name",
  "SELECT name,type FROM sys.objects WHERE name IN ('width_user','width_view') AND type='U'",
  "SELECT name,type FROM sys.objects WHERE name IN ('width_user','width_view') AND type='V'",
  "SELECT name,type FROM sys.tables WHERE name='width_user'",
  "SELECT name,type FROM sys.views WHERE name='width_view'",
  "SELECT name,type FROM sys.objects WHERE name IN ('width_user','width_view') AND type IN ('U','X') ORDER BY name",
  "SELECT name,type FROM sys.objects WHERE name IN ('width_user','width_view') AND type NOT IN ('U') ORDER BY name",
  "SELECT name,type FROM sys.objects WHERE name IN ('width_user','width_view') AND type IN ('U',NULL) ORDER BY name",
  "SELECT name,type FROM sys.objects WHERE name IN ('width_user','width_view') AND type NOT IN ('U',NULL) ORDER BY name",
  "SELECT * FROM sys.tables WHERE 1=0",
  "SELECT * FROM sys.views WHERE 1=0",
]
const output = resolve(process.argv[2] ?? 'artifacts/compatibility/object-catalog-width-reference')
await mkdir(output, {recursive: true})
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(await isolatedReference(config, async connection => {
      for (const sql of setup) assert.deepEqual(canonical(await capture(connection, sql)).errors, [])
      const results = []
      for (const sql of queries) results.push({sql, result: canonical(await capture(connection, sql))})
      const declarations = []
      for (const view of ['tables', 'views']) {
        const sql = `SELECT name,column_id,system_type_id,user_type_id,max_length,precision,scale,collation_name,is_nullable FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.${view}') ORDER BY column_id`
        const result = canonical(await capture(connection, sql))
        assert.deepEqual(result.errors, [])
        declarations.push({view, sql, result})
        const columns = result.sets[0].rows.slice(12).map(row => `[${row[0].replaceAll(']', ']]')}]`)
        const values = `SELECT name,${columns.join(',')} FROM sys.${view} WHERE name='${view === 'tables' ? 'width_user' : 'width_view'}'`
        results.push({sql: values, result: canonical(await capture(connection, values))})
      }
      return {results, declarations}
    }))
  }
  assert.deepEqual(runs[0], runs[1], 'Fresh object catalog captures differ')
  const actual = {image: container.image, identicalFreshCaptures: 2, setup, ...runs[0]}
  await writeFile(resolve(output, 'runs.json'), JSON.stringify(runs, null, 2) + '\n')
  await writeFile(resolve(output, 'object-catalog-width.json'), JSON.stringify(actual, null, 2) + '\n')
  let fixture
  try { fixture = JSON.parse(await readFile(new URL('../reference/object-catalog-width.json', import.meta.url), 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (fixture) assert.deepEqual(actual, fixture, 'Retained object catalog capture differs')
  console.log(`Captured ${actual.results.length} catalog observations twice identically${fixture ? ' and matched retained fixture' : ''}`)
})
