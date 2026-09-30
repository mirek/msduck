import assert from 'node:assert/strict'
import {test} from 'node:test'
import {retained,validate,retainedCteDelete,retainedTemporalSets,validateTemporalSets} from '../scripts/capture-prepared-rpc-metadata.mjs'

test('preparation preserves declarations, option errors and handle allocation',async()=>{
 const {runs}=await retained()
 for(const run of runs){
  let handle=0
  for(const record of run.slice(2)){
   const p=record.preparation
   if(['named-zero','named-two','named-null'].includes(record.variant)){
    assert.deepEqual(p.errors.map(e=>[e.number,e.state,e.class,e.message]),[[214,3,16,"Procedure expects parameter '@options' of type 'int'."]])
    assert.equal(p.returnStatus,214);assert.equal(p.returnValues[0].value,null)
    assert.deepEqual(p.events.map(e=>e.kind),['ERROR','RETURNSTATUS','RETURNVALUE','DONEPROC'])
   }else{
    assert.deepEqual(p.errors,[]);assert.equal(p.returnValues[0].value,++handle)
    assert.equal(p.returnValues[0].flags,0)
    assert.equal(p.returnStatus,record.name==='multiple results'?8182:0)
    assert(p.sets.every(s=>s.rows.length===0))
    assert.deepEqual(p.doneTokens.map(t=>[t.kind,t.status,t.command,t.rowCount]),record.name==='multiple results'?[['DONEPROC',0,224,null]]:[['DONEINPROC',17,record.name==='insert'?195:193,0],['DONEPROC',0,224,null]])
    if(p.sets.length)for(const execution of record.executions)assert.deepEqual(execution.result.sets[0].columns,p.sets[0].columns,record.name)
   }
  }
  assert.equal(handle,24)
 }
})

test('preparation does not insert rows or consume seeded random state',async()=>{
 const {runs}=await retained()
 for(const run of runs)for(const record of run.slice(2)){
  assert.deepEqual(record.afterPreparation.result.sets.map(s=>s.rows),[[[4]],[[0.041009986028273604]],[[0]]])
  const inserted=record.name==='insert'&&!record.preparation.errors.length
  assert.deepEqual(record.afterExecution.result.sets.map(s=>s.rows),[[[inserted?8:4]],[[42]]])
 }
})

test('raw capture validation rejects missing phases and damaged ORDER or DONE',async()=>{
 const {runs}=await retained()
 assert.throws(()=>validate(runs[0].slice(0,-1)))
 const missing=structuredClone(runs[0]);delete missing[2].afterPreparation
 assert.throws(()=>validate(missing))
 const done=structuredClone(runs[0]);done[2].preparation.doneTokens[0].status^=16
 assert.throws(()=>validate(done))
 const order=structuredClone(runs[0]);order.find(r=>r.name==='row number').preparation.events.find(e=>e.kind==='ORDER').ordinals=[0]
 assert.throws(()=>validate(order))
})


test('CTE DELETE captures preserve reuse counts, exact preparation and rollback',async()=>{
 const {runs}=await retainedCteDelete()
 for(const run of runs){
  assert.equal(run.length,10)
  let handle=0
  for(const record of run.slice(2)){
   assert.deepEqual(record.preparation.errors,[])
   assert.deepEqual(record.preparation.sets,[])
   assert.equal(record.preparation.returnValues[0].value,++handle)
   assert.deepEqual(record.preparation.doneTokens.map(t=>[t.kind,t.status,t.command,t.rowCount]),[['DONEINPROC',17,196,0],['DONEPROC',0,224,null]])
   assert.deepEqual(record.afterPreparation.result.sets.map(s=>s.rows),[[[4]],[[0.041009986028273604]],[[0]]])
   assert.deepEqual(record.executions.map(e=>e.value),[null,9,1,0])
   const rollback=record.name.endsWith('rollback')
   assert.deepEqual(record.executions.map(e=>e.result.doneTokens[0].rowCount),rollback?[0,0,3,4]:[0,0,3,1])
   for(const execution of record.executions){
    assert.deepEqual(execution.result.errors,[])
    if(rollback){
     assert.deepEqual(execution.beforeRollback.sets[1].rows,[[1]])
     assert.deepEqual(execution.afterRollback.sets.map(s=>s.rows),[[[1,2,'a'],[2,1,null],[3,1,'c'],[4,2,'d']],[[0]]])
    }
   }
   assert.deepEqual(record.afterExecution.result.sets.map(s=>s.rows),[[[rollback?4:0]],[[42]]])
  }
 }
})


test('temporal set captures preserve both runs, complete plan and raw descriptors',async()=>{
 const {runs}=await retainedTemporalSets()
 for(const run of runs)for(const record of run.slice(2)){
  const column=record.preparation.sets[0].columns[0]
  assert.equal(column.type,record.name.startsWith('DATETIME2')?'DateTime2':'DateTimeOffset')
  assert.equal(column.scale,7)
  assert.equal(column.flags,/INTERSECT|EXCEPT/.test(record.name)?33:1)
  assert.deepEqual(record.preparation.events.map(e=>e.kind),['COLMETADATA','DONEINPROC','RETURNSTATUS','RETURNVALUE','DONEPROC'])
 }
 assert.throws(()=>validateTemporalSets(runs[0].slice(0,-1)))
 const reordered=structuredClone(runs[0]);[reordered[2],reordered[4]]=[reordered[4],reordered[2]]
 assert.throws(()=>validateTemporalSets(reordered))
 const damaged=structuredClone(runs[0]);damaged[2].preparation.doneTokens[0].status^=16
 assert.throws(()=>validateTemporalSets(damaged))
})
