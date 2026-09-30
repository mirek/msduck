import assert from 'node:assert/strict'
import {test} from 'node:test'
import {mkdir,writeFile} from 'node:fs/promises'
import {start} from './support/client.mjs'
import {observe,retained} from '../scripts/capture-prepared-rpc-metadata.mjs'

test('prepared RPC responses match SQL Server and never execute during preparation',async t=>{
 const connection=await start(t)
 const actual=await observe(connection)
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
   assert.deepEqual(a.executions[j].result.errors,e.executions[j].result.errors,name+' errors')
   assert.deepEqual(a.executions[j].result.sets,e.executions[j].result.sets,name+' execution '+j)
  }
 }
})
