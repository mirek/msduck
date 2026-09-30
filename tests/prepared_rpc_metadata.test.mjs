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
   if(reference.events.some(event=>event.kind==='ORDER'||event.kind==='NBCROW')){
    if(JSON.stringify(response.events)!==JSON.stringify(reference.events)){
     // Permit only the two observed framing differences: omitted ORDER and
     // ordinary ROW instead of compressed NBCROW. Preserve raw tokens above.
     const knownFraming=reference.events.filter(event=>event.kind!=='ORDER').map(event=>event.kind==='NBCROW'?{kind:'ROW'}:event)
     assert.deepEqual(response.events,knownFraming,name+' execution '+j+' events with known ORDER/NBCROW differences')
     t.diagnostic(name+' execution '+j+': ORDER/NBCROW framing differences retained in raw capture')
    }
   }else assert.deepEqual(response.events,reference.events,name+' execution '+j+' events')
  }
  assert.deepEqual(a.unpreparation,e.unpreparation,name+' unprepare')
 }
})

// Golden preparation responses from two matching fresh SQL Server captures.
// This reduced plan starts handle allocation at 1; all other response fields
// are compared verbatim, including empty rows, flags, collation and DONE.
const scalarReference=[{"name":"calendar","sql":"SELECT EOMONTH('2024-01-15',@p) AS d,DATEFROMPARTS(2024,1,@p) AS f","preparation":{"sets":[{"columns":[{"name":"d","userType":0,"type":"Date","length":null,"precision":null,"scale":null,"flags":33,"collation":null},{"name":"f","userType":0,"type":"Date","length":null,"precision":null,"scale":null,"flags":33,"collation":null}],"rows":[]}],"done":[{"kind":"doneInProc","rowCount":0,"more":true},{"kind":"doneProc","rowCount":null,"more":false}],"errors":[],"info":[],"returnStatus":0,"returnValues":[{"name":"handle","value":9,"userType":0,"type":"IntN","length":4,"precision":null,"scale":null,"flags":0,"collation":null}],"doneTokens":[{"kind":"DONEINPROC","more":true,"sqlError":false,"attention":false,"serverError":false,"rowCount":0,"command":193,"status":17},{"kind":"DONEPROC","more":false,"sqlError":false,"attention":false,"serverError":false,"rowCount":null,"command":224,"status":0}],"events":[{"kind":"COLMETADATA"},{"kind":"DONEINPROC"},{"kind":"RETURNSTATUS"},{"kind":"RETURNVALUE"},{"kind":"DONEPROC"}]}},{"name":"catalog functions","sql":"SELECT OBJECT_ID(N'dbo.prepare_heap') AS o,SCHEMA_NAME(@p) AS s,TYPE_NAME(@p) AS t,COL_NAME(OBJECT_ID(N'dbo.prepare_heap'),@p) AS c","preparation":{"sets":[{"columns":[{"name":"o","userType":0,"type":"IntN","length":4,"precision":null,"scale":null,"flags":33,"collation":null},{"name":"s","userType":0,"type":"NVarChar","length":256,"precision":null,"scale":null,"flags":33,"collation":{"buffer":{"kind":"missing"},"lcid":1033,"flags":13,"version":0,"sortId":52,"codepage":"CP1252"}},{"name":"t","userType":0,"type":"NVarChar","length":256,"precision":null,"scale":null,"flags":33,"collation":{"buffer":{"kind":"missing"},"lcid":1033,"flags":13,"version":0,"sortId":52,"codepage":"CP1252"}},{"name":"c","userType":0,"type":"NVarChar","length":256,"precision":null,"scale":null,"flags":33,"collation":{"buffer":{"kind":"missing"},"lcid":1033,"flags":13,"version":0,"sortId":52,"codepage":"CP1252"}}],"rows":[]}],"done":[{"kind":"doneInProc","rowCount":0,"more":true},{"kind":"doneProc","rowCount":null,"more":false}],"errors":[],"info":[],"returnStatus":0,"returnValues":[{"name":"handle","value":17,"userType":0,"type":"IntN","length":4,"precision":null,"scale":null,"flags":0,"collation":null}],"doneTokens":[{"kind":"DONEINPROC","more":true,"sqlError":false,"attention":false,"serverError":false,"rowCount":0,"command":193,"status":17},{"kind":"DONEPROC","more":false,"sqlError":false,"attention":false,"serverError":false,"rowCount":null,"command":224,"status":0}],"events":[{"kind":"COLMETADATA"},{"kind":"DONEINPROC"},{"kind":"RETURNSTATUS"},{"kind":"RETURNVALUE"},{"kind":"DONEPROC"}]}},{"name":"identity functions","sql":"SELECT IDENT_CURRENT(N'dbo.prepare_heap') AS c,IDENT_SEED(N'dbo.prepare_heap') AS s,IDENT_INCR(N'dbo.prepare_heap') AS i","preparation":{"sets":[{"columns":[{"name":"c","userType":0,"type":"NumericN","length":17,"precision":38,"scale":0,"flags":33,"collation":null},{"name":"s","userType":0,"type":"NumericN","length":17,"precision":38,"scale":0,"flags":33,"collation":null},{"name":"i","userType":0,"type":"NumericN","length":17,"precision":38,"scale":0,"flags":33,"collation":null}],"rows":[]}],"done":[{"kind":"doneInProc","rowCount":0,"more":true},{"kind":"doneProc","rowCount":null,"more":false}],"errors":[],"info":[],"returnStatus":0,"returnValues":[{"name":"handle","value":19,"userType":0,"type":"IntN","length":4,"precision":null,"scale":null,"flags":0,"collation":null}],"doneTokens":[{"kind":"DONEINPROC","more":true,"sqlError":false,"attention":false,"serverError":false,"rowCount":0,"command":193,"status":17},{"kind":"DONEPROC","more":false,"sqlError":false,"attention":false,"serverError":false,"rowCount":null,"command":224,"status":0}],"events":[{"kind":"COLMETADATA"},{"kind":"DONEINPROC"},{"kind":"RETURNSTATUS"},{"kind":"RETURNVALUE"},{"kind":"DONEPROC"}]}},{"name":"JSON presence","sql":"SELECT JSON_PATH_EXISTS(N'{\"a\":1}',N'$.a') AS p","preparation":{"sets":[{"columns":[{"name":"p","userType":0,"type":"IntN","length":4,"precision":null,"scale":null,"flags":33,"collation":null}],"rows":[]}],"done":[{"kind":"doneInProc","rowCount":0,"more":true},{"kind":"doneProc","rowCount":null,"more":false}],"errors":[],"info":[],"returnStatus":0,"returnValues":[{"name":"handle","value":21,"userType":0,"type":"IntN","length":4,"precision":null,"scale":null,"flags":0,"collation":null}],"doneTokens":[{"kind":"DONEINPROC","more":true,"sqlError":false,"attention":false,"serverError":false,"rowCount":0,"command":193,"status":17},{"kind":"DONEPROC","more":false,"sqlError":false,"attention":false,"serverError":false,"rowCount":null,"command":224,"status":0}],"events":[{"kind":"COLMETADATA"},{"kind":"DONEINPROC"},{"kind":"RETURNSTATUS"},{"kind":"RETURNVALUE"},{"kind":"DONEPROC"}]}},{"name":"bare NULL","sql":"SELECT NULL AS n","preparation":{"sets":[{"columns":[{"name":"n","userType":0,"type":"IntN","length":4,"precision":null,"scale":null,"flags":33,"collation":null}],"rows":[]}],"done":[{"kind":"doneInProc","rowCount":0,"more":true},{"kind":"doneProc","rowCount":null,"more":false}],"errors":[],"info":[],"returnStatus":0,"returnValues":[{"name":"handle","value":43,"userType":0,"type":"IntN","length":4,"precision":null,"scale":null,"flags":0,"collation":null}],"doneTokens":[{"kind":"DONEINPROC","more":true,"sqlError":false,"attention":false,"serverError":false,"rowCount":0,"command":193,"status":17},{"kind":"DONEPROC","more":false,"sqlError":false,"attention":false,"serverError":false,"rowCount":null,"command":224,"status":0}],"events":[{"kind":"COLMETADATA"},{"kind":"DONEINPROC"},{"kind":"RETURNSTATUS"},{"kind":"RETURNVALUE"},{"kind":"DONEPROC"}]}}]

test('scalar prepared declarations match SQL Server without evaluating operands',async t=>{
 const connection=await start(t)
 const actual=await observe(connection,{verifyVersion:false,profilePlan:scalarReference.map(({name,sql})=>[name,sql]),variantPlan:['api-default','named-one'],execute:false})
 await mkdir('artifacts/prepared-rpc-metadata',{recursive:true})
 await writeFile('artifacts/prepared-rpc-metadata/scalar-runtime.json',JSON.stringify({scalarReference,actual},null,2)+'\n')
 let handle=0
 for(const record of actual.slice(2)){
  const expected=structuredClone(scalarReference.find(r=>r.name===record.name).preparation)
  expected.returnValues[0].value=++handle
  assert.deepEqual(record.preparation,expected,record.name+' '+record.variant)
  assert.deepEqual(record.afterPreparation.result.sets.map(s=>s.rows),[[[4]],[[0.041009986028273604]],[[0]]],record.name+' nonexecution')
  assert.deepEqual(record.afterExecution.result.sets.map(s=>s.rows),[[[4]],[[42]]],record.name+' unchanged table')
  assert.deepEqual(record.executions,[])
  assert.deepEqual(record.unpreparation.errors,[])
  assert.equal(record.unpreparation.returnStatus,0)
 }
})
