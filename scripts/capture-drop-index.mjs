// First-party DROP INDEX binding evidence. Keep raw diagnostics and DONE tokens.
import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference, assertSameCapture} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

const cases = [
  ['qualified', 'DROP INDEX ix ON dbo.a'],
  ['unqualified', 'DROP INDEX ix ON a'],
  ['quoted', 'DROP INDEX [odd.index] ON [alt].[odd.table]'],
  ['other-schema', 'DROP INDEX ix ON alt.a'],
  ['same-name-other-table', 'DROP INDEX ix ON dbo.b'],
  ['missing-index', 'DROP INDEX absent ON dbo.a'],
  ['wrong-target', 'DROP INDEX only_a ON dbo.b'],
  ['missing-table', 'DROP INDEX ix ON dbo.absent'],
  ['missing-schema', 'DROP INDEX ix ON absent.a'],
  ['if-exists-present', 'DROP INDEX IF EXISTS ix ON dbo.a'],
  ['if-exists-missing-index', 'DROP INDEX IF EXISTS absent ON dbo.a'],
  ['if-exists-wrong-target', 'DROP INDEX IF EXISTS only_a ON dbo.b'],
  ['if-exists-missing-table', 'DROP INDEX IF EXISTS ix ON dbo.absent'],
  ['if-exists-missing-schema', 'DROP INDEX IF EXISTS ix ON absent.a'],
  ['multiple', 'DROP INDEX ix ON dbo.a, ix ON dbo.b'],
  ['multiple-later-missing', 'DROP INDEX ix ON dbo.a, absent ON dbo.b'],
  ['multiple-first-missing', 'DROP INDEX absent ON dbo.a, ix ON dbo.b'],
  ['multiple-if-exists', 'DROP INDEX IF EXISTS absent ON dbo.a, ix ON dbo.b'],
  ['repeated-target', 'DROP INDEX ix ON dbo.a, ix ON dbo.a'],
  ['legacy', 'DROP INDEX dbo.a.ix'],
  ['missing-on', 'DROP INDEX ix'],
  ['qualified-index', 'DROP INDEX dbo.ix ON dbo.a'],
  ['maxdop-option', 'DROP INDEX ix ON dbo.a WITH (MAXDOP = 1)'],
  ['online-option', 'DROP INDEX ix ON dbo.a WITH (ONLINE = OFF)'],
]
const setup = ['CREATE SCHEMA alt',
  'CREATE TABLE dbo.a (id INT, value INT)', 'CREATE TABLE dbo.b (id INT, value INT)',
  'CREATE TABLE alt.a (id INT)', 'CREATE TABLE alt.[odd.table] (id INT)',
  'CREATE INDEX ix ON dbo.a(id)', 'CREATE INDEX only_a ON dbo.a(value)',
  'CREATE INDEX ix ON dbo.b(id)', 'CREATE INDEX ix ON alt.a(id)',
  'CREATE INDEX [odd.index] ON alt.[odd.table](id)']
const inventory = "SELECT s.name AS schema_name,o.name AS table_name,i.name AS index_name FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id JOIN sys.schemas s ON s.schema_id=o.schema_id WHERE i.name IS NOT NULL AND o.type='U' AND s.name IN ('dbo','alt') ORDER BY s.name,o.name,i.name"
const output=resolve(process.argv[2] ?? 'artifacts/compatibility/drop-index-reference')
await mkdir(output,{recursive:true})
await withReferenceContainer(async(config,container)=>{
  const runs=[]
  for(let repeat=0;repeat<2;repeat++){
    const results=[]
    for(const [id,sql] of cases){
      results.push(await isolatedReference(config,async connection=>{
        for(const statement of setup){
          const result=canonical(await capture(connection,statement))
          assert.equal(result.errors.length,0,`Reference setup failed: ${statement}`)
        }
        const tokens=[]
        const debugToken=connection.debug.token.bind(connection.debug)
        connection.debug.token=token=>{if(token.name.startsWith('DONE'))tokens.push(JSON.parse(JSON.stringify(token)));debugToken(token)}
        const result=canonical(await capture(connection,sql))
        const completion=[...tokens]
        const state=canonical(await capture(connection,'SELECT @@ROWCOUNT AS r,@@ERROR AS e'))
        const remaining=canonical(await capture(connection,inventory))
        return {id,sql,result,completion,state,remaining}
      }))
    }
    runs.push(results)
    await writeFile(resolve(output,'runs.json'),JSON.stringify(runs,null,2)+'\n')
  }
  assertSameCapture(runs[0],runs[1],'Fresh DROP INDEX captures differ')
  const actual={image:container.image,identicalFreshCaptures:2,setup,inventory,results:runs[0]}
  await writeFile(resolve(output,'drop-index.json'),JSON.stringify(actual,null,2)+'\n')
  const path=new URL('../reference/drop-index.json',import.meta.url)
  let fixture
  try{fixture=JSON.parse(await readFile(path,'utf8'))}catch(error){if(error.code!=='ENOENT')throw error}
  if(fixture)assertSameCapture(actual,fixture,'Retained DROP INDEX reference differs')
  console.log(`Captured ${cases.length} DROP INDEX observations twice identically${fixture?' and matched retained fixture':''}`)
})
