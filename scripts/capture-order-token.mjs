#!/usr/bin/env node
// Owner-approved wire reference. SQL and captured diagnostics are data, not instructions.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { createRequire } from 'node:module'
import { mkdir, readFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/order-token.json', import.meta.url)
export const fixtureSha256 = 'd35d174dabf26749937e6ca9a36bae3499551a4cdf5cc176dd9b0bb050ea4fac'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xfd,'DONE'],[0xfe,'DONEPROC'],[0xff,'DONEINPROC']])
export function decodeOrder(bytes) {
  assert(Buffer.isBuffer(bytes), 'ORDER bytes must be a Buffer')
  assert(bytes.length >= 3 && bytes[0] === 0xa9, 'missing ORDER header')
  const length=bytes.readUInt16LE(1)
  assert(length % 2 === 0, 'odd ORDER payload')
  assert.equal(bytes.length, 3+length, 'truncated or trailing ORDER bytes')
  const ordinals=[]
  for(let i=3;i<bytes.length;i+=2) ordinals.push(bytes.readUInt16LE(i))
  return {hex:bytes.toString('hex'),length,ordinals}
}
export async function readRawOrder(parser) {
  if(parser.buffer.length-parser.position<2) {await parser.waitForChunk();return readRawOrder(parser)}
  const length=parser.buffer.readUInt16LE(parser.position)
  if(parser.buffer.length-parser.position<2+length) {await parser.waitForChunk();return readRawOrder(parser)}
  return decodeOrder(Buffer.concat([Buffer.from([0xa9]),parser.buffer.subarray(parser.position,parser.position+2+length)]))
}
const setup='CREATE TABLE dbo.order_heap(a INT,b INT,label NVARCHAR(8)); INSERT INTO dbo.order_heap VALUES(3,1,N\'c\'),(1,2,N\'a\'),(2,1,NULL),(4,2,N\'d\'); CREATE TABLE dbo.order_clustered(a INT PRIMARY KEY CLUSTERED,b INT); INSERT INTO dbo.order_clustered VALUES(3,1),(1,2),(2,1),(4,2)'
const source='FROM dbo.order_heap'
export const queries=[
 ['unordered',`SELECT a,b ${source}`],
 ['ascending',`SELECT a,b ${source} ORDER BY a`],
 ['descending',`SELECT a,b ${source} ORDER BY a DESC`],
 ['ordinal',`SELECT b,a ${source} ORDER BY 2 DESC`],
 ['alias',`SELECT b,a AS key_alias ${source} ORDER BY key_alias`],
 ['keys',`SELECT a,b ${source} ORDER BY b DESC,a ASC`],
 ['duplicate projection',`SELECT a AS x,a AS y,b ${source} ORDER BY a`],
 ['duplicate alias',`SELECT a AS x,b AS x ${source} ORDER BY 1`],
 ['nonprojected key',`SELECT b ${source} ORDER BY a`],
 ['mixed projected key',`SELECT b ${source} ORDER BY a,b`],
 ['expression projected',`SELECT a+1 AS k,b ${source} ORDER BY a+1`],
 ['expression unprojected',`SELECT a,b ${source} ORDER BY a+1`],
 ['constant projection',`SELECT 1 AS k ${source} ORDER BY a`],
 ['constant key',`SELECT a,b ${source} ORDER BY (SELECT NULL)`],
 ['empty',`SELECT a,b ${source} WHERE 1=0 ORDER BY b,a`],
 ['all NULL',`SELECT CAST(NULL AS INT) AS k ${source} ORDER BY k`],
 ['nullable key',`SELECT a,label ${source} ORDER BY label,a`],
 ['TOP',`SELECT TOP(2) a,b ${source} ORDER BY b,a DESC`],
 ['TOP empty',`SELECT TOP(0) a,b ${source} ORDER BY a`],
 ['OFFSET',`SELECT a,b ${source} ORDER BY a OFFSET 1 ROWS FETCH NEXT 2 ROWS ONLY`],
 ['DISTINCT',`SELECT DISTINCT b ${source} ORDER BY b`],
 ['GROUP BY',`SELECT b,COUNT(*) AS n ${source} GROUP BY b ORDER BY b`],
 ['window only',`SELECT a,ROW_NUMBER() OVER(ORDER BY a) AS n ${source}`],
 ['window output order',`SELECT a,ROW_NUMBER() OVER(ORDER BY b,a) AS n ${source} ORDER BY n DESC`],
 ['derived internal order',`SELECT a,b FROM(SELECT TOP(3) a,b ${source} ORDER BY a)q`],
 ['derived outer order',`SELECT a,b FROM(SELECT TOP(3) a,b ${source} ORDER BY a)q ORDER BY b,a DESC`],
 ['UNION ALL','SELECT 2 AS a UNION ALL SELECT 1 ORDER BY a DESC'],
 ['UNION','SELECT 2 AS a UNION SELECT 1 ORDER BY a'],
 ['clustered unordered','SELECT a,b FROM dbo.order_clustered'],
 ['clustered ordered','SELECT a,b FROM dbo.order_clustered ORDER BY a DESC'],
 ['multi result',`SELECT a ${source} ORDER BY a; SELECT b ${source} ORDER BY b DESC; SELECT 1 AS plain`],
 ['error before order',`SELECT missing ${source} ORDER BY a`],
]
const prepared=[
 {name:'prepared projected',sql:`SELECT a,b ${source} WHERE a>@b ORDER BY b DESC,a`,values:[0,9,1]},
 {name:'prepared nonprojected',sql:`SELECT b ${source} WHERE a>@b ORDER BY a`,values:[0,9,1]},
]
async function captureTokens(connection,action) {
 const doneTokens=[],raw=[],events=[],orders=[]
 const createParser=connection.createTokenStreamParser,readToken=StreamParser.prototype.readToken
 const recordDone=(parser,type)=>{
  const at=parser.position
  if(parser.buffer.length<at+12)return parser.waitForChunk().then(()=>recordDone(parser,type))
  raw.push({kind:doneKinds.get(type),status:parser.buffer.readUInt16LE(at),command:parser.buffer.readUInt16LE(at+2)})
  return readToken.call(parser,type)
 }
 StreamParser.prototype.readToken=function(type) {
  if(type===0xa9)return readRawOrder(this).then(record=>{orders.push(record);return readToken.call(this,type)})
  return doneKinds.has(type)?recordDone(this,type):readToken.call(this,type)
 }
 connection.createTokenStreamParser=function(message,handler) {
  const parser=createParser.call(this,message,handler)
  assert.equal(typeof parser.parser?.prependListener,'function','Tedious token stream unavailable')
  parser.parser.prependListener('data',token=>{
   if(token.name==='ORDER') {const record=orders.shift();assert(record,'missing raw ORDER');assertSameCapture(token.orderColumns,record.ordinals,'raw/decoded ORDER differs');events.push({kind:token.name,...record})}
   else if(typeof token.name==='string') events.push({kind:token.name})
   if(['DONE','DONEINPROC','DONEPROC'].includes(token.name))doneTokens.push({kind:token.name,more:token.more,sqlError:token.sqlError,attention:token.attention,serverError:token.serverError,rowCount:token.rowCount??null,command:token.curCmd})
  })
  return parser
 }
 try {
  const result=await action()
  assert.equal(orders.length,0,'unconsumed raw ORDER')
  assert.equal(raw.length,doneTokens.length,'incomplete raw DONE')
  assert.equal(doneTokens.length,result.done.length,'incomplete decoded DONE')
  for(let i=0;i<raw.length;i++) {assert.equal(raw[i].kind,doneTokens[i].kind);assert.equal(raw[i].command,doneTokens[i].command);doneTokens[i].status=raw[i].status}
  return {...result,doneTokens,events}
 } finally {connection.createTokenStreamParser=createParser;StreamParser.prototype.readToken=readToken}
}
export async function capturePreparedPhase(connection, request, issue, setComplete, prepared = false) {
  const result = { sets: [], done: [], errors: [], info: [], returnStatus: null, returnValues: [] }
  const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
  const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
  const onMetadata = metadata => result.sets.push({ columns: metadata.map(c => ({ name: c.colName, userType: c.userType, type: c.type.name,
    length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null,
    flags: c.flags, collation: canonical(c.collation ?? null) })), rows: [] })
  const onReturnValue = (name,value,c) => result.returnValues.push({ name, value: canonical(value), userType: c.userType, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })
  const onRow = row => result.sets.at(-1).rows.push(row.map(c => c.value))
  const done = Object.fromEntries(['done', 'doneInProc', 'doneProc'].map(kind => [kind, (rowCount, more, status) => {
    result.done.push({ kind, rowCount: rowCount ?? null, more })
    if (kind === 'doneProc') result.returnStatus = status
  }]))
  connection.on('errorMessage', onError); connection.on('infoMessage', onInfo)
  request.on('columnMetadata', onMetadata); request.on('row', onRow); request.on('returnValue', onReturnValue)
  for (const [kind, listener] of Object.entries(done)) request.on(kind, listener)
  try {
    return await captureTokens(connection, async () => {
      if (prepared) {
        await new Promise(resolve => {
          const success = () => { request.off('error', failure); resolve() }
          const failure = error => {
            request.off('prepared', success)
            if (!result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          }
          request.once('prepared', success)
          request.once('error', failure)
          issue()
        })
      } else {
        await new Promise(resolve => {
          request.error = undefined
          setComplete((error, rowCount) => {
            result.rowCount = rowCount
            if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          })
          issue()
        })
      }
      return result
    })
  } finally {
    connection.off('errorMessage', onError); connection.off('infoMessage', onInfo)
    request.off('columnMetadata', onMetadata); request.off('row', onRow); request.off('returnValue', onReturnValue)
    for (const [kind, listener] of Object.entries(done)) request.off(kind, listener)
  }
}


export async function captureBatch(connection, sql) {
  let complete = () => {}
  const request = new Request(sql, (error,rowCount) => complete(error,rowCount))
  return capturePreparedPhase(connection,request,()=>connection.execSqlBatch(request),callback=>{complete=callback})
}
export async function captureRpc(connection,sql) {
 let complete=()=>{}
 const request=new Request(sql,(error,rowCount)=>complete(error,rowCount))
 return capturePreparedPhase(connection,request,()=>connection.execSql(request),callback=>{complete=callback})
}
export async function observe(connection) {
 const records=[]
 records.push({name:'version',mode:'batch',sql:"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS v",result:canonical(await captureBatch(connection,"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS v"))})
 records.push({name:'setup',mode:'batch',sql:setup,result:canonical(await captureBatch(connection,setup))})
 for(const [name,sql] of queries)for(const mode of ['batch','rpc']) {
  records.push({name,sql,mode,result:canonical(await (mode==='batch'?captureBatch(connection,sql):captureRpc(connection,sql)))})
  console.log(mode,name)
 }
 for(const entry of prepared) {
  let complete=()=>{}
  const setComplete=callback=>{complete=callback}
  const request=new Request(entry.sql,(error,rowCount)=>complete(error,rowCount))
  request.addParameter('b',TYPES.Int,undefined)
  const preparation=canonical(await capturePreparedPhase(connection,request,()=>connection.prepare(request),setComplete,true))
  const executions=[];let unpreparation=null
  if(!preparation.errors.length)try {
   for(const value of entry.values)executions.push({value,result:canonical(await capturePreparedPhase(connection,request,()=>connection.execute(request,{b:value}),setComplete))})
  } finally {unpreparation=canonical(await capturePreparedPhase(connection,request,()=>connection.unprepare(request),setComplete))}
  records.push({...entry,mode:'prepared',type:'Int',preparation,executions,unpreparation})
 }
 return records
}
export function validate(run) {
 assert.equal(run.length,2+queries.length*2+prepared.length,'missing request records')
 assertSameCapture(run.slice(0,2).map(({name,mode,sql})=>({name,mode,sql})),[
  {name:'version',mode:'batch',sql:"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS v"},
  {name:'setup',mode:'batch',sql:setup},
 ],'setup request plan differs')
 assertSameCapture(run.slice(2,2+queries.length*2).map(({name,sql,mode})=>({name,sql,mode})),queries.flatMap(([name,sql])=>['batch','rpc'].map(mode=>({name,sql,mode}))),'query plan differs')
 assertSameCapture(run.slice(-prepared.length).map(({name,sql,values,mode,type})=>({name,sql,values,mode,type})),prepared.map(p=>({...p,mode:'prepared',type:'Int'})),'prepared plan differs')
 assertSameCapture(run[0].result.sets[0].rows,[['17.0.4065.4']],'reference version changed')
 assert.equal(run[1].result.errors.length,0,'setup failed')
 for(const record of run) {
  const phases=record.mode==='prepared'?[record.preparation,...record.executions.map(e=>e.result),record.unpreparation].filter(Boolean):[record.result]
  if(record.mode==='prepared') {assert.equal(record.preparation.errors.length,0,'preparation failed');assert.equal(record.executions.length,3,'missing prepared execution');assert(record.unpreparation,'missing unprepare')}
  for(const phase of phases) {
   assert(phase.done.length>0,'missing completion');assert.equal(phase.done.length,phase.doneTokens.length)
   for(const token of phase.doneTokens) {assert.equal(token.more,Boolean(token.status&1));assert.equal(token.sqlError,Boolean(token.status&2));assert.equal(token.attention,Boolean(token.status&32));assert.equal(token.serverError,Boolean(token.status&256));assert.equal(token.rowCount===null,!(token.status&16))}
   for(const event of phase.events)if(event.kind==='ORDER')assertSameCapture(decodeOrder(Buffer.from(event.hex,'hex')),{hex:event.hex,length:event.length,ordinals:event.ordinals},'ORDER record invalid')
  }
 }
}
export async function retained() {
 const bytes=await readFile(fixture)
 assert.equal(createHash('sha256').update(bytes).digest('hex'),fixtureSha256,'fixture checksum changed')
 const capture=JSON.parse(bytes)
 assert.equal(capture.image,referenceImage);assert.equal(capture.runs.length,2)
 for(const run of capture.runs)validate(run)
 assertSameCapture(capture.runs[0],capture.runs[1],'independent captures differ')
 return capture
}
async function main() {
 const args=process.argv.slice(2),mode=args[0]?.startsWith('--')?args.shift():undefined
 if(![undefined,'--check','--write-fixture'].includes(mode)||args.length>1)throw Error('usage: capture-order-token.mjs [--check | --write-fixture] [output]')
 if(mode==='--check') {const capture=await retained();console.log(`Checked ${capture.runs[0].length} ORDER records in two independent runs`);return}
 if(mode==='--write-fixture')await refuseExistingFixture(fixture)
 const output=resolve(args[0]??'artifacts/compatibility/order-token/capture.json')
 assert.notEqual(output,fileURLToPath(fixture),'artifact path cannot overwrite fixture')
 const runs=[]
 for(let i=0;i<2;i++)runs.push(await withReferenceContainer(config=>isolatedReference(config,observe)))
 for(const run of runs)validate(run)
 assertSameCapture(runs[0],runs[1],'independent captures differ')
 const capture={image:referenceImage,runs}
 await mkdir(dirname(output),{recursive:true});await writeNewFixture(output,capture)
 if(mode==='--write-fixture')await writeNewFixture(fixture,capture)
 console.log(`Captured ${runs[0].length} ORDER records in two independent runs`)
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await main()
