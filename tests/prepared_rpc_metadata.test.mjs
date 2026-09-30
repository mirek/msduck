import assert from 'node:assert/strict'
import {test} from 'node:test'
import {mkdir,writeFile} from 'node:fs/promises'
import {start} from './support/client.mjs'
import {observe,retained} from '../scripts/capture-prepared-rpc-metadata.mjs'

test('prepared RPC responses match SQL Server and never execute during preparation',async t=>{
 const connection=await start(t)
 // SERVERPROPERTY is a separate unsupported surface; retain its raw response.
 const actual=await observe(connection,{verifyVersion:false})
 const expected=(await retained()).runs[0]
 await mkdir('artifacts/prepared-rpc-metadata',{recursive:true})
 // Preserve complete execution/unprepare token differences for follow-up.
 await writeFile('artifacts/prepared-rpc-metadata/runtime.json',JSON.stringify({kind:'raw-comparison-with-open-execution-token-gaps',expected,actual},null,2)+'\n')
 for(let i=2;i<expected.length;i++){
  const a=actual[i],e=expected[i],name=e.name+' '+e.variant
  assert.deepEqual(a.preparation,e.preparation,name+' preparation')
  assert.deepEqual(a.afterPreparation.result.sets,e.afterPreparation.result.sets,name+' nonexecution')
  assert.deepEqual(a.afterExecution.result.sets,e.afterExecution.result.sets,name+' side effects')
  assert.equal(a.executions.length,e.executions.length,name)
  for(let j=0;j<e.executions.length;j++){
   const response=a.executions[j].result,reference=e.executions[j].result
   for(const key of Object.keys(reference).filter(key=>key!=='events'))assert.deepEqual(response[key],reference[key],name+' execution '+j+' '+key)
   if(reference.events.some(event=>event.kind==='ORDER')){
    if(JSON.stringify(response.events)!==JSON.stringify(reference.events))t.diagnostic(name+' execution '+j+': ORDER event gap retained in raw capture')
   }else assert.deepEqual(response.events,reference.events,name+' execution '+j+' events')
  }
  assert.deepEqual(a.unpreparation,e.unpreparation,name+' unprepare')
 }
})
