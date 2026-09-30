import assert from 'node:assert/strict'
import {readFile} from 'node:fs/promises'
import {test} from 'node:test'
import {Connection,Request,TYPES} from 'tedious'
import {start,query} from './support/client.mjs'

const stream=[.7143559450345097,.041009986028273604,.6493841884705345,.40716252980396633]
const bits=value=>{const buffer=Buffer.alloc(8);buffer.writeDoubleBE(value);return buffer.toString('hex')}
function batch(connection,sql) {
 return new Promise((resolve,reject)=>{
  const rows=[]
  const request=new Request(sql,error=>error?reject(error):resolve(rows))
  request.on('row',cells=>rows.push(cells.map(cell=>cell.value)))
  connection.execSqlBatch(request)
 })
}
test('RAND captured streams retain all 777 exact FLOAT payloads',async t=>{
 const c=await start(t)
 const reference=JSON.parse(await readFile(new URL('../reference/rand-session.json',import.meta.url)))
 let checked=0
 for(const record of reference.runs[0].filter(record=>record.name.startsWith('stream '))) {
  const actual=await batch(c,record.sql)
  const expected=record.result.sets.flatMap(set=>set.rows)
  assert.equal(actual.length,expected.length,record.name)
  for(let row=0;row<expected.length;row++)for(let column=0;column<expected[row].length;column++) {
   const value=expected[row][column]
   if(value && typeof value==='object' && value.floatBits) {
    assert.equal(bits(actual[row][column]),value.floatBits,`${record.name} row ${row}`);checked++
   } else assert.equal(actual[row][column],value)
  }
 }
 assert.equal(checked,777)
})
test('RAND lazy contexts and percentile empty effects stay isolated between sessions',async t=>{
 const c=await start(t)
 const other=new Connection(c.config);t.after(()=>other.close());other.on('error',()=>{})
 await new Promise((resolve,reject)=>other.connect(error=>error?reject(error):resolve()))
 assert.deepEqual((await query(c,'SELECT RAND(42) AS r')).rows,[[stream[0]]])
 assert.deepEqual((await query(other,'SELECT RAND(1) AS r')).rows,[[.7135919932129235]])
 assert.deepEqual((await query(c,'SELECT CASE WHEN 1=1 THEN .5 ELSE RAND(1) END AS r')).rows,[[.5]])
 assert.deepEqual((await query(c,'SELECT RAND() AS r FROM(VALUES(1))s(n) WHERE 1=0')).rows,[])
 assert.deepEqual((await query(c,'SELECT RAND(NULL) AS r')).rows,[[null]])
 assert.deepEqual((await query(c,'SELECT RAND() AS a,RAND() AS b')).rows,[[stream[1],stream[2]]])
 for(const kind of ['CONT','DISC'])for(const empty of [false,true]) {
  await query(c,'SELECT RAND(42) AS seed')
  const sql=`SELECT PERCENTILE_${kind}(RAND()*0+.5) WITHIN GROUP(ORDER BY n) OVER() AS p FROM(VALUES(1),(2),(3),(4))s(n)${empty?' WHERE 1=0':''}`
  assert.deepEqual((await query(c,sql)).rows,empty?[]:Array.from({length:4},()=>[kind==='CONT'?2.5:2]))
  assert.deepEqual((await query(c,'SELECT RAND() AS after')).rows,[[stream[2]]])
 }
 await query(c,'SELECT RAND(42) AS seed')
 await batch(c,'BEGIN TRAN; SELECT RAND() AS r; ROLLBACK TRAN')
 assert.deepEqual((await query(c,'SELECT RAND() AS after')).rows,[[stream[2]]])
})
test('preparing RAND never draws and repeated INT bindings recover after NULL',async t=>{
 const c=await start(t)
 for(const sql of ['SELECT RAND(@seed) AS r','DECLARE @p FLOAT=RAND(@seed); SELECT @p AS r']) {
 await query(c,'SELECT RAND(42) AS seed')
 let callback=()=>{}
 const request=new Request(sql,(error)=>callback(error))
 request.addParameter('seed',TYPES.Int,undefined)
 await new Promise((resolve,reject)=>{request.once('prepared',resolve);request.once('error',reject);c.prepare(request)})
 assert.deepEqual((await query(c,'SELECT RAND() AS after')).rows,[[stream[1]]])
 for(const seed of [42,null,42]) {
  const rows=[];const onRow=cells=>rows.push(cells.map(cell=>cell.value));request.on('row',onRow)
  request.error=undefined
  await new Promise((resolve,reject)=>{callback=error=>error?reject(error):resolve();c.execute(request,{seed})})
  request.off('row',onRow)
  assert.deepEqual(rows,[[seed===null?null:stream[0]]])
 }
 request.error=undefined
 await new Promise((resolve,reject)=>{callback=error=>error?reject(error):resolve();c.unprepare(request)})
 assert.deepEqual((await query(c,'SELECT RAND() AS after')).rows,[[stream[1]]])
 }
})
test('prepared FLOAT seed conversion retains 232 state 3 and does not advance on failure',async t=>{
 const c=await start(t)
 await query(c,'SELECT RAND(42) AS seed')
 let callback=()=>{}
 const request=new Request('SELECT RAND(@seed) AS r',error=>callback(error))
 request.addParameter('seed',TYPES.Float,undefined)
 await new Promise((resolve,reject)=>{request.once('prepared',resolve);request.once('error',reject);c.prepare(request)})
 request.error=undefined
 await assert.rejects(new Promise((resolve,reject)=>{callback=error=>error?reject(error):resolve();c.execute(request,{seed:2147483648})}),
  error=>error.number===232 && error.state===3 && error.class===16 && error.message==='Arithmetic overflow error for type int, value = 2147483648.000000.')
 assert.deepEqual((await query(c,'SELECT RAND() AS after')).rows,[[stream[1]]])
 for(const seed of [1.9,null,42]) {
  const rows=[];const onRow=cells=>rows.push(cells.map(cell=>cell.value));request.on('row',onRow);request.error=undefined
  await new Promise((resolve,reject)=>{callback=error=>error?reject(error):resolve();c.execute(request,{seed})});request.off('row',onRow)
  assert.deepEqual(rows,[[seed===null?null:seed===42?stream[0]:.7135919932129235]])
 }
 request.error=undefined
 await new Promise((resolve,reject)=>{callback=error=>error?reject(error):resolve();c.unprepare(request)})
})
test('RAND source character and BIGINT diagnostics recover without advancing',async t=>{
 const c=await start(t)
 for(const [seed,number,state,message] of [["'abc'",245,1,"Conversion failed when converting the varchar value 'abc' to data type int."],["N'abc'",245,1,"Conversion failed when converting the nvarchar value 'abc' to data type int."],["CAST(2147483648 AS BIGINT)",8115,2,'Arithmetic overflow error converting expression to data type int.']]) {
  await query(c,'SELECT RAND(42) AS seed')
  await assert.rejects(query(c,`SELECT RAND(${seed}) AS r`),error=>error.number===number && error.state===state && error.message===message)
  assert.deepEqual((await query(c,'SELECT RAND() AS after')).rows,[[stream[1]]])
 }
 let callback=()=>{}
 const request=new Request('SELECT RAND(@seed) AS r',error=>callback(error))
 request.addParameter('seed',TYPES.NVarChar,undefined,{length:16})
 await new Promise((resolve,reject)=>{request.once('prepared',resolve);request.once('error',reject);c.prepare(request)})
 for(const seed of ['42','abc','1.9',null,'42']) {
  const rows=[];const onRow=cells=>rows.push(cells.map(cell=>cell.value));request.on('row',onRow);request.error=undefined
  const run=new Promise((resolve,reject)=>{callback=error=>error?reject(error):resolve();c.execute(request,{seed})})
  if(seed==='abc'||seed==='1.9') await assert.rejects(run,error=>error.number===245 && error.state===1 && error.message===`Conversion failed when converting the nvarchar value '${seed}' to data type int.`)
  else {await run;assert.deepEqual(rows,[[seed===null?null:stream[0]]])}
  request.off('row',onRow)
 }
 request.error=undefined
 await new Promise((resolve,reject)=>{callback=error=>error?reject(error):resolve();c.unprepare(request)})
})
test('RAND declarations preserve nullable computed FLOAT including nested and empty results',async t=>{
 const c=await start(t)
 for(const expression of ['RAND(42)','RAND(NULL)','COALESCE(RAND(),1)','COALESCE(RAND(NULL),1)','ISNULL(RAND(),1)','ISNULL(RAND(NULL),1)','CASE WHEN 1=1 THEN .5 ELSE RAND() END','RAND()']) {
  await query(c,'SELECT RAND(42) AS seed')
  const result=await query(c,`SELECT ${expression} AS r${expression==='RAND()'?' FROM(VALUES(1))s(n) WHERE 1=0':''}`)
  const nonnull=expression.startsWith('ISNULL(')
  const column=result.columns[0][0]
  assert.deepEqual({name:column.colName,type:column.type.name,length:column.dataLength??null,flags:column.flags},
   {name:'r',type:nonnull?'Float':'FloatN',length:nonnull?null:8,flags:nonnull?32:33},expression)
 }
 const combined=await query(c,"SELECT RAND() AS r,UPPER(CAST(N'x' AS NVARCHAR(7))) AS label")
 assert.equal(combined.columns[0][1].dataLength,14,'RAND enrichment must preserve another resolved declaration')
})
