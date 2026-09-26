import assert from 'node:assert/strict'
import {createRequire} from 'node:module'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {setTimeout as delay} from 'node:timers/promises'
import {Request} from 'tedious'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {connect, command, isolatedReference, assertSameCapture} from './lib/reference.mjs'
import {canonical} from './lib/compatibility.mjs'
const require = createRequire(import.meta.url)
const SqlBatchPayload = require('tedious/lib/sqlbatch-payload.js')
const StreamParser = require('tedious/lib/token/stream-parser.js')
const {RequestTokenHandler} = require('tedious/lib/token/handler.js')
const output = resolve(process.argv[2] ?? 'artifacts/compatibility/attention-boundaries-reference')
await mkdir(output, {recursive: true})

const cases = []
for (const boundary of ['idle-one','idle-two','active-one','active-two','ignore-empty','ignore-payload'])
 for (const transaction of ['none','off','on']) cases.push({boundary, transaction})
function packet(kind, status, id, payload = Buffer.alloc(0)) {
 const b = Buffer.alloc(8+payload.length)
 assert(b.length<=4096)
 b[0]=kind; b[1]=status; b.writeUInt16BE(b.length,2); b[6]=id
 payload.copy(b,8)
 return b
}
function observe(connection) {
 const transport = connection.messageIo.securePair?.cleartext ?? connection.messageIo.socket
 const packets = []
 let partial=Buffer.alloc(0), total=0
 const onData = chunk => {
  total+=chunk.length
  assert(total<=4*1024*1024, 'bounded post-login trace')
  partial=Buffer.concat([partial,chunk])
  while(partial.length>=8) {
   const length=partial.readUInt16BE(2)
   assert(length>=8)
   if(partial.length<length) break
   packets.push({direction:'in',hex:partial.subarray(0,length).toString('hex')})
   partial=partial.subarray(length)
  }
 }
 transport.on('data',onData)
 return {
  packets,
  send(parts) {
   for(const part of parts) packets.push({direction:'out',hex:part.toString('hex')})
   // One write establishes a client burst, not a server scheduling guarantee.
   transport.write(Buffer.concat(parts))
  },
  stop() {transport.off('data',onData); assert.equal(partial.length,0,'partial captured packet')},
 }
}
function columns(values) {
 return values.map(c=>({name:c.colName,type:c.type.name,length:c.dataLength??null,precision:c.precision??null,scale:c.scale??null,flags:c.flags,collation:canonical(c.collation??null)}))
}
async function response(connection) {
 const message=await connection.messageIo.readMessage()
 const parts=[]
 for await(const part of message) parts.push(part)
 const payload=Buffer.concat(parts), tokens=[]
 let boundHex=payload.toString('hex')
 const handler=new RequestTokenHandler(connection,new Request('',()=>{}))
 for await(const token of StreamParser.parseTokens([payload],connection.debug,connection.config.options)) {
  if(token.name==='COLMETADATA') tokens.push({name:token.name,columns:columns(token.columns)})
  else if(token.name==='ROW'||token.name==='NBCROW') tokens.push({name:token.name,values:canonical(token.columns.map(c=>c.value))})
  else if(['DONE','DONEINPROC','DONEPROC'].includes(token.name)) {
   tokens.push({name:token.name,more:token.more,sqlError:token.sqlError,attention:token.attention,serverError:token.serverError,rowCount:token.rowCount??null,command:token.curCmd})
  } else if(token.handlerName==='onRollbackTransaction') {
   assert.equal(token.oldValue.toString('hex'),connection.currentTransactionDescriptor().toString('hex'),'rollback identifies current transaction')
   const encoded=Buffer.concat([Buffer.from([0xe3,11,0,10,0,8]),token.oldValue])
   const offset=payload.indexOf(encoded)
   assert(offset>=0 && payload.indexOf(encoded,offset+1)===-1,'unique exact rollback ENVCHANGE encoding')
   assert.equal(boundHex,payload.toString('hex'),'at most one rollback binding per response')
   boundHex=boundHex.slice(0,(offset+6)*2)+'<active-transaction-descriptor>'+boundHex.slice((offset+14)*2)
   tokens.push({name:token.name,handler:token.handlerName,newValue:canonical(token.newValue),oldValue:'verified active transaction descriptor'})
   handler[token.handlerName](token)
  } else if(token.name==='ORDER') {
   tokens.push({name:token.name,columns:token.orderColumns})
  } else if(['ERROR','INFO'].includes(token.name)) {
   tokens.push({name:token.name,number:token.number,state:token.state,class:token.class,lineNumber:token.lineNumber,message:token.message})
  } else throw Error(`Unexpected token ${token.name}/${token.handlerName}; preserve raw capture and extend explicitly`)
 }
 return {hex:payload.toString('hex'),boundHex,tokens}
}
async function bounded(connection, work) {
 let timer
 try {
  return await Promise.race([work(),new Promise((_,reject)=>{
   timer=setTimeout(()=>{connection.close();reject(Error('response deadline exceeded; connection closed'))},10000)
  })])
 } finally {clearTimeout(timer)}
}
async function oneCase(config, entry, key) {
 return isolatedReference(config,async connection=>{
  const observer=await connect(config)
  let wire
  const rawResponses=[], responsePayloads=[], responses=[]
  try {
   await command(connection,`CREATE TABLE dbo.boundary_probe(n INT); SET DATEFIRST 2; SET XACT_ABORT ${entry.transaction==='on'?'ON':'OFF'}; ${entry.transaction==='none'?'':'BEGIN TRANSACTION;'} INSERT dbo.boundary_probe VALUES(1)`)
   const spid=(await command(connection,'SELECT @@SPID')).sets[0].rows[0][0]
   const transactionDescriptor=connection.currentTransactionDescriptor().toString('hex')
   wire=observe(connection)
   const sqlParts=sql=>Buffer.concat([...new SqlBatchPayload(sql,connection.currentTransactionDescriptor(),connection.config.options)])
   const sendSql=sql=>wire.send([packet(1,1,1,sqlParts(sql))])
   const receive=async()=>{
    assert(responses.length<16, 'bounded response message count')
    const r=await bounded(connection,()=>response(connection))
    rawResponses.push(r.hex);responsePayloads.push(r.boundHex);responses.push(r.tokens)
    return r.tokens
   }
   let active=null
   if(entry.boundary.startsWith('active')) {
    sendSql("INSERT dbo.boundary_probe VALUES(2); WAITFOR DELAY '00:00:45'; INSERT dbo.boundary_probe VALUES(3)")
    const deadline=Date.now()+15000
    while(!active) {
     const result=await command(observer,`SELECT command,wait_type FROM sys.dm_exec_requests WHERE session_id=${spid}`)
     active=result.sets[0].rows.find(row=>row[1]==='WAITFOR')??null
     assert(Date.now()<deadline,'observer did not establish active WAITFOR')
     if(!active) await delay(10)
    }
   }
   if(entry.boundary.startsWith('ignore')) {
    const prefix=sqlParts('INSERT dbo.boundary_probe VALUES(9)')
    const padding=Buffer.from(' '.repeat((4096-8-prefix.length)/2),'utf16le')
    wire.send([packet(1,0,1,Buffer.concat([prefix,padding])),packet(1,3,2,entry.boundary==='ignore-payload'?Buffer.from('; SELECT 99 AS ignored','utf16le'):Buffer.alloc(0))])
   } else {
    wire.send(Array.from({length:entry.boundary.endsWith('two')?2:1},()=>packet(6,1,1)))
   }
   if(active) {
    let acknowledged=false
    while(!acknowledged) acknowledged=(await receive()).some(t=>t.attention)
   }
   // The probe is an ordered wire barrier. Idle Attention need not send a reply;
   // do not infer absence from a short sleep or leave a timed-out read pending.
   for(const marker of ['edge_marker','edge_final']) {
    sendSql(`SELECT 4242 AS ${marker},@@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state,@@DATEFIRST AS first_day; SELECT n FROM dbo.boundary_probe ORDER BY n`)
    let seen=false
    while(!seen) {
     const tokens=await receive()
     seen=tokens.some(t=>t.name==='COLMETADATA'&&t.columns.some(c=>c.name===marker))
    }
   }
   wire.stop()
   const reassembled=[]
   let fragments=[]
   for(const packet of wire.packets.filter(p=>p.direction==='in')) {
    const bytes=Buffer.from(packet.hex,'hex')
    assert.equal(bytes[0],4,'tabular server response')
    fragments.push(bytes.subarray(8))
    if(bytes[1]&1) {reassembled.push(Buffer.concat(fragments).toString('hex'));fragments=[]}
   }
   assert.equal(fragments.length,0,'response EOM observed')
   assertSameCapture(rawResponses,reassembled,'every observed response consumed exactly once')
   const result={entry,active,responses,responsePayloads,transactionDescriptor,rawResponses,packets:wire.packets}
   await writeFile(resolve(output,`${key}.json`),JSON.stringify(result,null,2)+'\n')
   return result
  } catch(error) {
   await writeFile(resolve(output,`${key}-failed.json`),JSON.stringify({entry,message:error.message,responses,rawResponses,packets:wire?.packets??[]},null,2)+'\n')
   throw error
  } finally {observer.close()}
 })
}
await withReferenceContainer(async(config,container)=>{
 const runs=[]
 for(let repeat=0;repeat<2;repeat++) {
  const run=[]
  for(let i=0;i<cases.length;i++) {
   run.push(await oneCase(config,cases[i],`run-${repeat}-case-${i}`))
   console.log(`Captured ${repeat}/${i} ${JSON.stringify(cases[i])}`)
  }
  runs.push(run)
 }
 await writeFile(resolve(output,'runs.json'),JSON.stringify(runs,null,2)+'\n')
 const stable=run=>run.map(({entry,active,responses,responsePayloads})=>({entry,active,responses,responsePayloads}))
 const observations=runs.map(stable)
 if(JSON.stringify(observations[0])!==JSON.stringify(observations[1])) throw Error('Boundary observations differ across runs; raw traces retained')
 const actual={image:container.image,transport:'TLS',results:observations[0],rawCaptures:runs}
 await writeFile(resolve(output,'attention-boundaries.json'),JSON.stringify(actual,null,2)+'\n')
 let fixture
 try {fixture=JSON.parse(await readFile(new URL('../reference/attention-boundaries.json',import.meta.url),'utf8'))}
 catch(error) {if(error.code!=='ENOENT') throw error}
 if(fixture) {
  assert.equal(actual.image,fixture.image)
  assert.equal(actual.transport,fixture.transport)
  assertSameCapture(actual.results,fixture.results,'Retained boundary observations differ')
 }
 console.log(`Captured ${cases.length} Attention/IGNORE boundaries twice${fixture?' and matched retained fixture':''}`)
})
