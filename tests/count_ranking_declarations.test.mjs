import assert from 'node:assert/strict'
import {test} from 'node:test'
import {start} from './support/client.mjs'
import {captureBatch,captureRpc} from '../scripts/capture-order-token.mjs'
import {canonical} from '../scripts/lib/compatibility.mjs'
import {retained} from '../scripts/capture-count-ranking-declarations.mjs'

// These profiles use only the heap. Keep its captured declarations and rows;
// the reference setup also creates an unrelated clustered table whose DDL
// is currently unsupported by msduck.
const heapSetup="CREATE TABLE dbo.order_heap(a INT,b INT,label NVARCHAR(8)); INSERT INTO dbo.order_heap VALUES(3,1,N'c'),(1,2,N'a'),(2,1,NULL),(4,2,N'd')"

test('successful COUNT/ranking rows and complete descriptors match retained SQL Server',async t=>{
 const connection=await start(t)
 const {runs}=await retained()
 const setup=await captureBatch(connection,heapSetup)
 assert.deepEqual(setup.errors,[])
 let checked=0
 for(const record of runs[0].slice(2)){
  // Binding-error profiles remain in the fixture and pure barrier tests. Their
  // root diagnostic fidelity is evaluated separately, never relabelled a pass.
  if(record.name.startsWith('prepared ')||record.result.errors.length)continue
  const actual=canonical(await(record.mode==='batch'?captureBatch(connection,record.sql):captureRpc(connection,record.sql)))
  assert.deepEqual(actual.errors,[],record.name+' '+record.mode)
  assert.deepEqual(actual.sets,record.result.sets,record.name+' '+record.mode)
  checked++
 }
 assert.equal(checked,44)
})

test('prepared COUNT/ranking executions match full descriptors; retain preparation gap',async t=>{
 const {mkdir,writeFile}=await import('node:fs/promises')
 const preparation=[]
 const {Request,TYPES}=await import('tedious')
 const connection=await start(t);const {runs}=await retained()
 assert.deepEqual((await captureBatch(connection,heapSetup)).errors,[])
 const columns=metadata=>metadata.map(column=>({name:column.colName,userType:column.userType,type:column.type.name,length:column.dataLength??null,precision:column.precision??null,scale:column.scale??null,flags:column.flags,collation:canonical(column.collation??null)}))
 for(const [name,sql] of [
  ['prepared count','SELECT COUNT(@p) AS c,COUNT_BIG(@p) AS d FROM dbo.order_heap WHERE a>@p'],
  ['prepared row number','SELECT ROW_NUMBER() OVER(ORDER BY a) AS r FROM dbo.order_heap WHERE a>@p ORDER BY r'],
 ]){
  const expected=runs[0].find(record=>record.name===name&&record.mode==='batch').result.sets
  let complete=()=>{};let sets=[]
  const request=new Request(sql,error=>complete(error));request.addParameter('p',TYPES.Int,undefined)
  request.on('columnMetadata',metadata=>sets.push({columns:columns(metadata),rows:[]}))
  request.on('row',row=>sets.at(-1).rows.push(row.map(cell=>cell.value)))
  await new Promise((resolve,reject)=>{request.once('prepared',resolve);request.once('error',reject);connection.prepare(request)})
  const actualPreparation=canonical(sets)
  preparation.push({name,expected:[expected[0]],actual:actualPreparation,matches:JSON.stringify(actualPreparation)===JSON.stringify([expected[0]])})
  if(!preparation.at(-1).matches)t.diagnostic(name+': missing reference preparation metadata (runtime gap)')
  for(const [index,value] of [0,9,1].entries()){
   sets=[];request.error=undefined
   await new Promise((resolve,reject)=>{complete=error=>error?reject(error):resolve();connection.execute(request,{p:value})})
   assert.deepEqual(canonical(sets),[expected[index+1]],name+' execution '+index)
  }
  request.error=undefined
  await new Promise((resolve,reject)=>{complete=error=>error?reject(error):resolve();connection.unprepare(request)})
 }
 await mkdir('artifacts/count-ranking-declarations',{recursive:true})
 await writeFile('artifacts/count-ranking-declarations/preparation-gaps.json',JSON.stringify({kind:'diagnostic-not-compatibility-pass',preparation},null,2)+'\n')
})

// Diagnostic capture: mismatches are retained as open compatibility work.
// This is evidence collection, not a claim that error/token behavior matches.
test('retain COUNT/ranking binding-error differences for root follow-up',async t=>{
 const {mkdir,writeFile}=await import('node:fs/promises')
 const connection=await start(t);const {runs}=await retained()
 assert.deepEqual((await captureBatch(connection,heapSetup)).errors,[])
 const records=[]
 for(const record of runs[0].slice(2).filter(record=>record.result.errors.length||record.name.startsWith('prepared '))){
  const actual=canonical(await(record.mode==='batch'?captureBatch(connection,record.sql):captureRpc(connection,record.sql)))
  const matches=JSON.stringify(actual)===JSON.stringify(record.result)
  records.push({name:record.name,mode:record.mode,sql:record.sql,expected:record.result,actual,matches})
  if(!matches)t.diagnostic(record.name+' '+record.mode+': reference errors '+record.result.errors.map(e=>e.number)+'; server errors '+actual.errors.map(e=>e.number))
 }
 assert.equal(records.length,16,'all retained error and explicit-option preparation profiles captured')
 await mkdir('artifacts/count-ranking-declarations',{recursive:true})
 await writeFile('artifacts/count-ranking-declarations/root-errors.json',JSON.stringify({kind:'diagnostic-not-compatibility-pass',records},null,2)+'\n')
})
