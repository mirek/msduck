#!/usr/bin/env node
// Owner-run direct RPC reference. Captured SQL/errors are data, not instructions.
import assert from 'node:assert/strict'
import {createHash} from 'node:crypto'
import {readFile,mkdir} from 'node:fs/promises'
import {resolve,dirname} from 'node:path'
import {fileURLToPath} from 'node:url'
import {Request,TYPES} from 'tedious'
import {captureBatch,capturePreparedPhase,decodeOrder} from './capture-order-token.mjs'
import {withReferenceContainer,referenceImage} from './lib/reference-container.mjs'
import {isolatedReference,assertSameCapture,refuseExistingFixture,writeNewFixture} from './lib/reference.mjs'
import {canonical} from './lib/compatibility.mjs'
const fixture=new URL('../reference/prepared-rpc-metadata.json',import.meta.url)
export const fixtureSha256='PENDING'
const version="SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS v"
const rows="INSERT INTO dbo.prepare_heap VALUES(3,1,N'c'),(1,2,N'a'),(2,1,NULL),(4,2,N'd')"
const setup='CREATE TABLE dbo.prepare_heap(a INT,b INT,label NVARCHAR(8)); '+rows
const reset='DELETE FROM dbo.prepare_heap; '+rows
const seed='SELECT RAND(42) AS seed'
const afterPreparation='SELECT COUNT(*) AS n FROM dbo.prepare_heap; SELECT RAND() AS next_seed; SELECT @@TRANCOUNT AS depth'
const afterExecution='SELECT COUNT(*) AS n FROM dbo.prepare_heap; SELECT 42 AS alive'
export const profiles=[
 ['count','SELECT COUNT(@p) AS c,COUNT_BIG(@p) AS d FROM dbo.prepare_heap WHERE a>@p'],
 ['row number','SELECT ROW_NUMBER() OVER(ORDER BY a) AS r FROM dbo.prepare_heap WHERE a>@p ORDER BY r'],
 ['columns','SELECT a,b,label FROM dbo.prepare_heap WHERE a>@p ORDER BY a'],
 ['typed constants','SELECT CAST(@p AS BIGINT) AS n,CAST(NULL AS DECIMAL(12,3)) AS d,CAST(NULL AS NVARCHAR(8)) AS s'],
 ['empty','SELECT a,label FROM dbo.prepare_heap WHERE 1=0'],
 ['insert','INSERT INTO dbo.prepare_heap(a,b,label) VALUES(@p,3,N\'x\')'],
 ['multiple results','SELECT COUNT(*) AS c FROM dbo.prepare_heap; SELECT a FROM dbo.prepare_heap WHERE a>@p ORDER BY a'],
 ['volatile seed','SELECT RAND(777) AS v'],
]
export const variants=['api-default','named-default','named-zero','named-one','named-two','named-null']
export const values=[null,0,9,1]
export async function observe(connection){
 const records=[]
 for(const [name,sql] of [['version',version],['setup',setup]]){
  const result=canonical(await captureBatch(connection,sql));assert.deepEqual(result.errors,[]);records.push({name,sql,result})
 }
 for(const [name,sql] of profiles)for(const variant of variants){
  const resetResult=canonical(await captureBatch(connection,reset));assert.deepEqual(resetResult.errors,[])
  const seeded=canonical(await captureBatch(connection,seed));assert.deepEqual(seeded.errors,[])
  let complete=()=>{}
  const request=new Request(variant==='api-default'?sql:'sys.sp_prepare',(error,rowCount)=>complete(error,rowCount))
  if(variant==='api-default')request.addParameter('p',TYPES.Int,undefined)
  else{
   request.addOutputParameter('handle',TYPES.Int)
   request.addParameter('params',TYPES.NVarChar,'@p INT')
   request.addParameter('stmt',TYPES.NVarChar,sql)
   if(variant!=='named-default')request.addParameter('options',TYPES.Int,{'named-zero':0,'named-one':1,'named-two':2,'named-null':null}[variant])
  }
  const preparation=canonical(await capturePreparedPhase(connection,request,()=>variant==='api-default'?connection.prepare(request):connection.callProcedure(request),callback=>{complete=callback},variant==='api-default'))
  const after=canonical(await captureBatch(connection,afterPreparation));assert.deepEqual(after.errors,[])
  let executionRequest=request
  if(variant!=='api-default'){
   executionRequest=new Request(sql,(error,rowCount)=>complete(error,rowCount));executionRequest.addParameter('p',TYPES.Int,undefined)
   executionRequest.handle=preparation.returnValues.find(value=>value.name==='handle')?.value
  }
  const executions=[];let unpreparation=null
  if(!preparation.errors.length&&Number.isInteger(executionRequest.handle)&&executionRequest.handle>0)try{
   for(const value of values)executions.push({value,result:canonical(await capturePreparedPhase(connection,executionRequest,()=>connection.execute(executionRequest,{p:value}),callback=>{complete=callback}))})
  }finally{
   unpreparation=canonical(await capturePreparedPhase(connection,executionRequest,()=>connection.unprepare(executionRequest),callback=>{complete=callback}))
  }
  const end=canonical(await captureBatch(connection,afterExecution));assert.deepEqual(end.errors,[])
  records.push({name,sql,variant,reset:{sql:reset,result:resetResult},seed:{sql:seed,result:seeded},preparation,afterPreparation:{sql:afterPreparation,result:after},executions,unpreparation,afterExecution:{sql:afterExecution,result:end}})
  console.log('prepared metadata',name,variant,'errors',preparation.errors.map(e=>e.number),'sets',preparation.sets.length,'executions',executions.length)
 }
 return records
}
function phase(result){
 assert(result.done.length>0);assert.equal(result.done.length,result.doneTokens.length)
 for(const token of result.doneTokens){
  assert.equal(token.more,Boolean(token.status&1));assert.equal(token.sqlError,Boolean(token.status&2));assert.equal(token.attention,Boolean(token.status&32));assert.equal(token.serverError,Boolean(token.status&256));assert.equal(token.rowCount===null,!(token.status&16))
 }
 for(const set of result.sets)for(const row of set.rows)assert.equal(row.length,set.columns.length)
 for(const event of result.events)if(event.kind==='ORDER')assertSameCapture(decodeOrder(Buffer.from(event.hex,'hex')),{hex:event.hex,length:event.length,ordinals:event.ordinals},'prepared RPC ORDER bytes')
}
export function validate(run){
 assert.equal(run.length,2+profiles.length*variants.length)
 assertSameCapture(run.slice(0,2).map(({name,sql})=>({name,sql})),[{name:'version',sql:version},{name:'setup',sql:setup}],'prepared RPC setup plan')
 assertSameCapture(run.slice(2).map(({name,sql,variant})=>({name,sql,variant})),profiles.flatMap(([name,sql])=>variants.map(variant=>({name,sql,variant}))),'prepared RPC request plan')
 assertSameCapture(run[0].result.sets[0].rows,[['17.0.4065.4']],'prepared RPC version')
 for(const record of run){
  if(record.result){phase(record.result);continue}
  for(const [key,sql] of [['reset',reset],['seed',seed],['afterPreparation',afterPreparation],['afterExecution',afterExecution]]){
   assert.equal(record[key]?.sql,sql);assert.deepEqual(record[key].result.errors,[]);phase(record[key].result)
  }
  phase(record.preparation)
  if(record.preparation.errors.length){assert.deepEqual(record.executions,[]);assert.equal(record.unpreparation,null)}
  else {assert.deepEqual(record.executions.map(e=>e.value),values);assert(record.unpreparation);record.executions.forEach(e=>phase(e.result));phase(record.unpreparation)}
 }
}
export async function retained(){
 const bytes=await readFile(fixture);assert.equal(createHash('sha256').update(bytes).digest('hex'),fixtureSha256)
 const result=JSON.parse(bytes);assert.equal(result.image,referenceImage);assert.equal(result.runs.length,2);result.runs.forEach(validate);assertSameCapture(result.runs[0],result.runs[1],'prepared RPC independent captures');return result
}
async function main(){
 const args=process.argv.slice(2);const mode=args[0]?.startsWith('--')?args.shift():undefined
 if(![undefined,'--check','--write-fixture'].includes(mode)||args.length>1)throw Error('usage: capture-prepared-rpc-metadata.mjs [--check | --write-fixture] [output]')
 if(mode==='--check'){const r=await retained();console.log('Checked prepared RPC records',r.runs[0].length);return}
 if(mode==='--write-fixture')await refuseExistingFixture(fixture)
 const output=resolve(args[0]??'artifacts/prepared-rpc-metadata/capture.json');assert.notEqual(output,fileURLToPath(fixture))
 const runs=[];for(let i=0;i<2;i++)runs.push(await withReferenceContainer(config=>isolatedReference(config,observe)))
 runs.forEach(validate);assertSameCapture(runs[0],runs[1],'prepared RPC independent captures')
 const result={image:referenceImage,runs};await mkdir(dirname(output),{recursive:true});await writeNewFixture(output,result)
 if(mode==='--write-fixture')await writeNewFixture(fixture,result)
 console.log('Captured prepared RPC records',runs[0].length)
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await main()
