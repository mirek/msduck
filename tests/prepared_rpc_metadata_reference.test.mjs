import assert from 'node:assert/strict'
import {test} from 'node:test'
import {retained,validate} from '../scripts/capture-prepared-rpc-metadata.mjs'

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
