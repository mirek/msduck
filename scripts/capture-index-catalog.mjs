// First-party catalog snapshots; all query projections are explicit and retained.
import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

const indexSql = `SELECT s.name AS schema_name,o.name AS table_name,i.name,i.index_id,i.type,i.type_desc,i.is_unique,i.data_space_id,i.ignore_dup_key,i.is_primary_key,i.is_unique_constraint,i.fill_factor,i.is_padded,i.is_disabled,i.is_hypothetical,i.allow_row_locks,i.allow_page_locks,i.has_filter,i.filter_definition,i.auto_created,i.optimize_for_sequential_key FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id JOIN sys.schemas s ON s.schema_id=o.schema_id WHERE o.type='U' AND s.name IN ('dbo','alt') ORDER BY s.name,o.name,i.index_id`
const columnSql = `SELECT s.name AS schema_name,o.name AS table_name,i.name AS index_name,ic.index_id,ic.index_column_id,ic.column_id,c.name AS column_name,ic.key_ordinal,ic.partition_ordinal,ic.is_descending_key,ic.is_included_column FROM sys.index_columns ic JOIN sys.indexes i ON i.object_id=ic.object_id AND i.index_id=ic.index_id JOIN sys.objects o ON o.object_id=ic.object_id JOIN sys.schemas s ON s.schema_id=o.schema_id LEFT JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id WHERE o.type='U' AND s.name IN ('dbo','alt') ORDER BY s.name,o.name,ic.index_id,ic.index_column_id`
const programs = [
 ['schema', 'CREATE SCHEMA alt'],
 ['heaps', 'CREATE TABLE dbo.a(id INT,value INT,payload VARCHAR(10)); CREATE TABLE dbo.b(id INT,value INT)'],
 ['quoted-table', 'CREATE TABLE alt.[odd.table]([odd.column] INT,other INT)'],
 ['ordinary', 'CREATE INDEX ix ON dbo.a(id)'],
 ['same-name-other-table', 'CREATE UNIQUE INDEX ix ON dbo.b(id)'],
 ['unique-null', 'INSERT INTO dbo.b(id,value) VALUES(NULL,10)'],
 ['unique-null-duplicate', 'INSERT INTO dbo.b(id,value) VALUES(NULL,20)'],
 ['unique-null-cleanup', 'DELETE FROM dbo.b WHERE id IS NULL'],
 ['quoted-index', 'CREATE INDEX [odd.index] ON alt.[odd.table]([odd.column])'],
 ['descending-include', 'CREATE INDEX mixed ON dbo.a(value DESC,id ASC) INCLUDE(payload)'],
 ['filtered', 'CREATE INDEX filtered ON dbo.a(value) WHERE value IS NOT NULL'],
 ['duplicate-name-error', 'CREATE INDEX ix ON dbo.a(value)'],
 ['begin-create', 'BEGIN TRANSACTION; CREATE INDEX transient ON dbo.a(payload)'],
 ['rollback-create', 'ROLLBACK TRANSACTION'],
 ['begin-drop', 'BEGIN TRANSACTION; DROP INDEX ix ON dbo.a'],
 ['rollback-drop', 'ROLLBACK TRANSACTION'],
 ['drop-one', 'DROP INDEX ix ON dbo.a'],
 ['reuse-free-index-id', 'CREATE INDEX replacement ON dbo.a(id)'],
 ['disable', 'ALTER INDEX mixed ON dbo.a DISABLE'],
 ['rebuild', 'ALTER INDEX mixed ON dbo.a REBUILD'],
 ['explicit-constraints', 'CREATE TABLE dbo.keys_table(id INT NOT NULL CONSTRAINT pk_keys PRIMARY KEY,other INT CONSTRAINT uq_keys UNIQUE)'],
 ['constraint-drop-error', 'DROP INDEX pk_keys ON dbo.keys_table'],
 ['drop-recreate-table', 'DECLARE @old INT=OBJECT_ID(\'dbo.a\'); DROP TABLE dbo.a; CREATE TABLE dbo.a(id INT); SELECT CAST(CASE WHEN OBJECT_ID(\'dbo.a\')<>@old THEN 1 ELSE 0 END AS BIT) AS new_object_identity'],
 ['recreated-index', 'CREATE INDEX ix ON dbo.a(id)'],
 ['begin-drop-table', 'BEGIN TRANSACTION; DROP TABLE dbo.b'],
 ['rollback-drop-table', 'ROLLBACK TRANSACTION'],
 ['committed-drop', 'BEGIN TRANSACTION; DROP INDEX ix ON dbo.b; COMMIT TRANSACTION'],
]
const output=resolve(process.argv[2] ?? 'artifacts/compatibility/index-catalog-reference')
await mkdir(output,{recursive:true})
await withReferenceContainer(async(config,container)=>{
 const runs=[]
 for(let repeat=0;repeat<2;repeat++){
  const results=await isolatedReference(config,async connection=>{
   let tokens=[]
   const debugToken=connection.debug.token.bind(connection.debug)
   connection.debug.token=token=>{if(token.name.startsWith('DONE'))tokens.push(JSON.parse(JSON.stringify(token)));debugToken(token)}
   const results=[]
   for(const [id,sql] of programs){
    tokens=[]
    const result=canonical(await capture(connection,sql))
    const completion=[...tokens]
    if(!['duplicate-name-error','constraint-drop-error','unique-null-duplicate'].includes(id)) assert.equal(result.errors.length,0,`Unexpected reference failure in ${id}: ${JSON.stringify(result.errors)}`)
    const state=canonical(await capture(connection,'SELECT @@ROWCOUNT AS r,@@ERROR AS e,@@TRANCOUNT AS t'))
    const indexes=canonical(await capture(connection,indexSql))
    const columns=canonical(await capture(connection,columnSql))
    assert.equal(indexes.errors.length,0,`Index projection failed after ${id}`)
    assert.equal(columns.errors.length,0,`Index-column projection failed after ${id}`)
    results.push({id,sql,result,completion,state,indexes,columns})
   }
   return results
  })
  runs.push(results)
  await writeFile(resolve(output,'runs.json'),JSON.stringify(runs,null,2)+'\n')
 }
 assert.deepEqual(runs[0],runs[1],'Fresh catalog captures differ')
 const actual={image:container.image,identicalFreshCaptures:2,indexSql,columnSql,results:runs[0]}
 await writeFile(resolve(output,'index-catalog.json'),JSON.stringify(actual,null,2)+'\n')
 let fixture
 try{fixture=JSON.parse(await readFile(new URL('../reference/index-catalog.json',import.meta.url),'utf8'))}catch(error){if(error.code!=='ENOENT')throw error}
 if(fixture)assert.deepEqual(actual,fixture,'Retained index catalog reference differs')
 console.log(`Captured ${programs.length} index catalog snapshots twice identically${fixture?' and matched retained fixture':''}`)
})
