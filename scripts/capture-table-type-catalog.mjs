#!/usr/bin/env node
// Capture owner-controlled SQL Server table-type catalog relationships. Generated
// IDs remain raw in each run, and this pinned image must replay them exactly.
import assert from 'node:assert/strict'
import {lstat,mkdir,readFile,realpath,stat,writeFile} from 'node:fs/promises'
import {basename,dirname,resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {isDeepStrictEqual} from 'node:util'
import {capture,canonical} from './lib/compatibility.mjs'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference,refuseExistingFixture,writeNewFixture,describeFirstDifference} from './lib/reference.mjs'

const fixture=new URL('../reference/table-type-catalog.json',import.meta.url)
const args=process.argv.slice(2)
const writeFixture=args.includes('--write-fixture')
const positional=args.filter(arg=>arg!=='--write-fixture')
if(positional.length>1)throw Error('expected at most one output path')
const output=resolve(positional[0]??'artifacts/compatibility/table-type-catalog/capture.json')
async function canonicalTarget(path){
 let current=path,missing=[]
 while(true){
  try{return resolve(await realpath(current),...missing.reverse())}
  catch(error){if(error.code!=='ENOENT')throw error}
  const parent=dirname(current)
  if(parent===current)throw Error('cannot resolve capture output path')
  missing.push(basename(current));current=parent
 }
}
async function assertSeparateOutput(){
 for(let part=output;;part=dirname(part)){
  let info
  try{info=await lstat(part)}catch(error){if(error.code!=='ENOENT')throw error}
  assert.ok(!info?.isSymbolicLink(),'capture output path contains a symlink')
  if(dirname(part)===part)break
 }
 const retained=fileURLToPath(fixture)
 assert.notEqual(await canonicalTarget(output),await canonicalTarget(retained),'capture output aliases retained fixture')
 let outputFile,retainedFile
 try{outputFile=await stat(output)}catch(error){if(error.code!=='ENOENT')throw error}
 try{retainedFile=await stat(retained)}catch(error){if(error.code!=='ENOENT')throw error}
 assert.ok(!outputFile||!retainedFile||outputFile.dev!==retainedFile.dev||outputFile.ino!==retainedFile.ino,
  'capture output hard-links retained fixture')
}
await assertSeparateOutput()
if(writeFixture)await refuseExistingFixture(fixture)

const queries=[
 ['version',"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version"],
 ['types',"SELECT name,system_type_id,user_type_id,schema_id,principal_id,max_length,precision,scale,collation_name,is_nullable,is_user_defined,is_assembly_type,default_object_id,rule_object_id,is_table_type FROM sys.types WHERE name='CatalogProbe' ORDER BY schema_id"],
 ['table_types',"SELECT name,system_type_id,user_type_id,schema_id,principal_id,max_length,precision,scale,collation_name,is_nullable,is_user_defined,is_assembly_type,default_object_id,rule_object_id,is_table_type,type_table_object_id,is_memory_optimized FROM sys.table_types WHERE name='CatalogProbe' ORDER BY schema_id"],
 ['objects',"SELECT o.name,o.object_id,o.schema_id,o.parent_object_id,o.type,o.type_desc,o.is_ms_shipped FROM sys.objects o JOIN sys.table_types t ON t.type_table_object_id=o.object_id WHERE t.name='CatalogProbe' ORDER BY t.schema_id"],
 ['columns',"SELECT t.schema_id,c.object_id,c.name,c.column_id,c.system_type_id,c.user_type_id,c.max_length,c.precision,c.scale,c.collation_name,c.is_nullable,c.is_ansi_padded,c.is_identity,c.is_computed,c.default_object_id FROM sys.columns c JOIN sys.table_types t ON t.type_table_object_id=c.object_id WHERE t.name='CatalogProbe' ORDER BY t.schema_id,c.column_id"],
 ['lookups',"SELECT TYPE_ID('dbo.CatalogProbe') AS dbo_type_id,TYPE_ID('app.CatalogProbe') AS app_type_id,TYPE_NAME(TYPE_ID('app.CatalogProbe')) AS app_type_name,OBJECT_ID('dbo.CatalogProbe','TT') AS dbo_name_object_id,OBJECT_ID('app.CatalogProbe','TT') AS app_name_object_id,OBJECT_NAME((SELECT type_table_object_id FROM sys.table_types WHERE name='CatalogProbe' AND schema_id=SCHEMA_ID('app'))) AS app_object_name,COL_NAME((SELECT type_table_object_id FROM sys.table_types WHERE name='CatalogProbe' AND schema_id=SCHEMA_ID('app')),2) AS app_column_name"]
]

function rows(run,name){return run.find(entry=>entry.name===name)?.result.sets[0]?.rows??[]}
function check(run){
 const types=rows(run,'types'),tableTypes=rows(run,'table_types'),objects=rows(run,'objects'),columns=rows(run,'columns'),lookups=rows(run,'lookups')
 assert.equal(types.length,2)
 assert.equal(tableTypes.length,2)
 assert.equal(objects.length,2)
 assert.equal(columns.length,6)
 assert.equal(lookups.length,1)
 for(let i=0;i<2;i++){
  const type=types[i],table=tableTypes[i],object=objects[i]
  assert.deepEqual(table.slice(0,15),type,'sys.table_types inherits sys.types')
  assert.equal(type[0],'CatalogProbe')
  assert.equal(type[1],243)
  assert.equal(type[2],table[2])
  assert.equal(type[14],true)
  assert.equal(table[15],object[1],'type_table_object_id links sys.objects')
  assert.equal(table[16],false)
  assert.equal(object[2],4,'backing TT object is in sys schema')
  assert.equal(object[0],`TT_CatalogProbe_${object[1].toString(16).toUpperCase()}`)
  assert.equal(object[4].trim(),'TT')
  assert.equal(object[5],'TYPE_TABLE')
  assert.equal(object[6],true)
  assert.deepEqual(columns.slice(i*3,i*3+3).map(c=>c[0]),[type[3],type[3],type[3]])
  assert.deepEqual(columns.slice(i*3,i*3+3).map(c=>c[1]),[object[1],object[1],object[1]])
  assert.deepEqual(columns.slice(i*3,i*3+3).map(c=>c[3]),[1,2,3])
 }
 const bySchema=new Map(types.map((type,i)=>[type[3],i]))
 const dbo=bySchema.get(1),app=bySchema.get(types.find(type=>type[3]!==1)?.[3])
 assert.notEqual(dbo,undefined);assert.notEqual(app,undefined)
 assert.equal(lookups[0][0],types[dbo][2]);assert.equal(lookups[0][1],types[app][2])
 assert.equal(lookups[0][2],'CatalogProbe')
 assert.equal(lookups[0][3],null);assert.equal(lookups[0][4],null)
 assert.equal(lookups[0][5],objects[app][0]);assert.equal(lookups[0][6],'label')
}

async function observe(connection){
 const setup=[]
 for(const sql of [
  'CREATE SCHEMA app',
  'CREATE TYPE dbo.CatalogProbe AS TABLE (id INT NOT NULL, label NVARCHAR(12) NULL, payload VARBINARY(5) NULL)',
  'CREATE TYPE app.CatalogProbe AS TABLE (id INT NOT NULL, label NVARCHAR(12) NULL, payload VARBINARY(5) NULL)'
 ]){
  const result=canonical(await capture(connection,sql))
  assert.equal(result.errors.length,0,`reference setup: ${sql}`)
  setup.push({sql,result})
 }
 const rejected=[]
 for(const sql of [
  'CREATE TYPE dbo.[int] AS TABLE (x INT NOT NULL)',
  'CREATE TYPE dbo.BadDecimal AS TABLE (bad DECIMAL(2,3))'
 ]){
  const result=canonical(await capture(connection,sql))
  assert.ok(result.errors.length>0,`reference should reject: ${sql}`)
  rejected.push({sql,result})
 }
 assert.equal(rejected[0].result.errors[0]?.number,219,'built-in type name collision')
 assert.equal(rejected[1].result.errors[0]?.number,183,'decimal scale above precision')
 const observations=[]
 for(const [name,sql] of queries){
  const result=canonical(await capture(connection,sql))
  assert.equal(result.errors.length,0,`reference query: ${name}`)
  observations.push({name,sql,result})
 }
 check(observations)
 return {setup,rejected,observations}
}

const runContainer=()=>withReferenceContainer(async(config,container)=>({
 image:container.image,
 runs:[await isolatedReference(config,observe),await isolatedReference(config,observe)]
}))
await mkdir(dirname(output),{recursive:true})
const first=await runContainer(),second=await runContainer()
assert.equal(first.image,second.image)
const runs=[...first.runs,...second.runs]
const actual={image:first.image,runs}
await writeFile(output,JSON.stringify(actual)+'\n')
const differences=runs.slice(1).map((run,i)=>{
 const exactMatch=isDeepStrictEqual(run,runs[0])
 return {run:i+1,exactMatch,firstDifference:exactMatch?null:describeFirstDifference(run,runs[0])}
})
assert.ok(differences.every(d=>d.exactMatch),`reference catalog replays differ: ${differences.find(d=>!d.exactMatch)?.firstDifference}`)
if(writeFixture)await writeNewFixture(fixture,{...actual,differences})
else{
 let retained
 try{retained=JSON.parse(await readFile(fixture,'utf8'))}catch(error){if(error.code!=='ENOENT')throw error}
 if(retained){
  assert.equal(retained.image,actual.image)
  assert.ok(isDeepStrictEqual({...actual,differences},retained),
   `retained catalog fixture changed: ${describeFirstDifference({...actual,differences},retained)}`)
 }
}
console.log(`Captured catalog relationships in ${runs.length} fresh databases across two containers; ${differences.filter(d=>d.exactMatch).length} raw replays matched first run`)
