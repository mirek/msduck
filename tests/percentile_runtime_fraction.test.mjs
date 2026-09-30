import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Request,TYPES } from 'tedious'
import { start,query } from './support/client.mjs'
import { capture } from '../scripts/lib/compatibility.mjs'

const sql=(kind,fraction,where='')=>`SELECT PERCENTILE_${kind}(${fraction}) WITHIN GROUP(ORDER BY n) OVER() AS p FROM(VALUES(1),(2),(3),(4))s(n) ${where}`
const diagnostics=r=>r.errors.map(({number,state,class:severity,message})=>[number,state,severity,message])

test('runtime percentile local variables scalar expressions and catchable failures',async t=>{
 const c=await start(t)
 for(const kind of ['CONT','DISC']) {
  for(const [prefix,fraction] of [['DECLARE @p FLOAT=.5;','@p'],['DECLARE @p FLOAT=.5;','@p+0'],["DECLARE @p NVARCHAR(16)=N'0.5';",'CAST(@p AS FLOAT)'],['DECLARE @p FLOAT=-.5;','ABS(@p)'],['','CASE WHEN 1=1 THEN .5 ELSE 1/0 END'],['','(SELECT .5)']]) {
   assert.deepEqual((await query(c,prefix+sql(kind,fraction))).rows,Array.from({length:4},()=>[kind==='CONT'?2.5:2]))
  }
  for(const [fraction,number,state,message] of [["'abc'",8114,5,'Error converting data type varchar to float.'],['NULL',8727,1,'Input parameter of percentile function is outside of range [0, 1].'],['CASE WHEN 1=0 THEN .5 ELSE 1/0 END',8134,1,'Divide by zero error encountered.']]) {
   const failed=await capture(c,sql(kind,fraction))
   assert.deepEqual(diagnostics(failed),[[number,state,16,message]])
   assert.equal(failed.sets.length,1,'runtime failure must retain result metadata')
   assert.deepEqual(failed.sets[0].rows,[])
   assert.deepEqual((await query(c,sql(kind,fraction,'WHERE 1=0'))).rows,[])
  }
  const allNull=await capture(c,`SELECT PERCENTILE_${kind}(N'abc') WITHIN GROUP(ORDER BY n) OVER() AS p FROM(VALUES(CAST(NULL AS INT)))s(n)`)
  assert.deepEqual(diagnostics(allNull),[[8114,5,16,'Error converting data type nvarchar to float.']])
 }
 assert.deepEqual((await query(c,'SELECT 1 AS reusable')).rows,[[1]])
})

test('prepared percentile bindings recover without inspecting values during preparation',async t=>{
 const c=await start(t)
 for(const kind of ['CONT','DISC'])for(const empty of [false,true]) {
  let callback=()=>{}
  const request=new Request(sql(kind,'@b',empty?'WHERE 1=0':''),(e,n)=>callback(e,n))
  request.addParameter('b',TYPES.NVarChar,undefined,{length:16})
  await new Promise((resolve,reject)=>{request.once('prepared',resolve);request.once('error',reject);c.prepare(request)})
  try {
   for(const value of ['0.5','abc',null,'1.1','0.5']) {
    const rows=[],columns=[];const row=cells=>rows.push(cells.map(x=>x.value));const metadata=x=>columns.push(x)
    request.on('row',row);request.on('columnMetadata',metadata)
    let error
    try {error=await new Promise(resolve=>{callback=e=>resolve(e);request.error=undefined;c.execute(request,{b:value})})}
    finally {request.off('row',row);request.off('columnMetadata',metadata)}
    assert.equal(columns.length,1,'execution retains typed metadata')
    assert.equal(columns[0][0].type.name,kind==='CONT'?'FloatN':'IntN')
    if(!empty && ['abc',null,'1.1'].includes(value)) {assert.equal(error?.number,value==='abc'?8114:8727);assert.deepEqual(rows,[])}
    else {assert.ifError(error);assert.deepEqual(rows,empty?[]:Array.from({length:4},()=>[kind==='CONT'?2.5:2]))}
   }
  } finally {await new Promise((resolve,reject)=>{callback=e=>e?reject(e):resolve();c.unprepare(request)})}
 }
})
