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
