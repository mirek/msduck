import assert from 'node:assert/strict'
import {execFileSync} from 'node:child_process'
import {mkdtempSync, readFileSync, rmSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
import {start} from './support/client.mjs'
import {capture} from '../scripts/lib/compatibility.mjs'

const observations=JSON.parse(readFileSync(new URL('../reference/attention-boundaries.json',import.meta.url),'utf8')).results
function frame(kind,status,id,body=Buffer.alloc(0)) {
 const bytes=Buffer.alloc(8+body.length)
 bytes[0]=kind;bytes[1]=status;bytes.writeUInt16BE(bytes.length,2);bytes[6]=id
 body.copy(bytes,8)
 return bytes
}
function sqlBody(connection,sql) {
 const header=Buffer.alloc(22)
 header.writeUInt32LE(22,0);header.writeUInt32LE(18,4);header.writeUInt16LE(2,8)
 connection.currentTransactionDescriptor().copy(header,10)
 header.writeUInt32LE(1,18)
 return Buffer.concat([header,Buffer.from(sql,'utf16le')])
}
async function reply(connection) {
 let timeout
 try {
  return await Promise.race([(async()=>{
   const message=await connection.messageIo.readMessage(), parts=[]
   let length=0
   for await(const part of message) {
    length+=part.length;assert(length<=64*1024,'bounded control response')
    parts.push(part)
   }
   return Buffer.concat(parts).toString('hex')
  })(),new Promise((_,reject)=>{timeout=setTimeout(()=>{connection.close();reject(Error('control response deadline exceeded'))},5000)})])
 } finally {clearTimeout(timeout)}
}
function tlsOptions(t) {
 const directory=mkdtempSync(join(tmpdir(),'msduck-attention-tls-'))
 t.after(()=>rmSync(directory,{recursive:true,force:true}))
 const certificate=join(directory,'cert.pem'), key=join(directory,'key.pem')
 execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-days','1','-subj','/CN=localhost','-keyout',key,'-out',certificate],{stdio:'ignore'})
 return {serverArgs:['--tls-cert',certificate,'--tls-key',key],options:{encrypt:true,trustServerCertificate:true,serverName:'localhost'}}
}

// Registered from the existing tedious suite so normal CI discovery/sharding
// executes these cases without a second independent test-file entry point.
export function registerAttentionBoundaries(register) {
 for(const transport of ['tcp','tls']) for(const expected of observations.filter(x=>!x.entry.boundary.startsWith('active'))) {
  const {boundary,transaction}=expected.entry
  register(`wire Attention boundary: ${transport} ${boundary} ${transaction}`,{timeout:30000},async t=>{
   const c=await start(t,transport==='tls'?tlsOptions(t):{})
   const setup=await capture(c,`CREATE TABLE dbo.boundary_probe(n INT); SET DATEFIRST 2; SET XACT_ABORT ${transaction==='on'?'ON':'OFF'}; ${transaction==='none'?'':'BEGIN TRANSACTION;'} INSERT dbo.boundary_probe VALUES(1)`)
   assert.deepEqual(setup.errors,[])
   const socket=c.messageIo.securePair?.cleartext??c.messageIo.socket
   if(boundary.startsWith('idle')) {
    socket.write(Buffer.concat(Array.from({length:boundary==='idle-two'?2:1},()=>frame(6,1,1))))
   } else {
    const body=sqlBody(c,'INSERT dbo.boundary_probe VALUES(9)')
    const padding=Buffer.from(' '.repeat((4096-8-body.length)/2),'utf16le')
    socket.write(Buffer.concat([frame(1,0,1,Buffer.concat([body,padding])),frame(1,3,2,boundary==='ignore-payload'?Buffer.from('; SELECT 99 AS ignored','utf16le'):Buffer.alloc(0))]))
   }
   const completionCount=boundary==='idle-two'?2:1
   for(let i=0;i<completionCount;i++) assert.equal(await reply(c),expected.responsePayloads[i],'exact completion payload and separate EOM')
   // This regression targets boundary completion, transaction lifetime and prior
   // writes. The reference's combined XACT_STATE expression remains a separate gap.
   const state=await capture(c,'SELECT @@TRANCOUNT AS transaction_count,@@DATEFIRST AS first_day; SELECT n FROM dbo.boundary_probe ORDER BY n')
   assert.deepEqual(state.errors,[])
   const referenceRows=expected.responses.at(-1).filter(t=>t.name==='ROW'||t.name==='NBCROW').map(t=>t.values)
   assert.deepEqual(state.sets.map(s=>s.rows),[[[referenceRows[0][1],referenceRows[0][3]]],referenceRows.slice(1)])
   if(transaction!=='none') {
    assert.deepEqual((await capture(c,'ROLLBACK TRANSACTION')).errors,[])
    const after=await capture(c,'SELECT COUNT(*) AS n FROM dbo.boundary_probe')
    assert.deepEqual(after.errors,[]);assert.deepEqual(after.sets[0].rows,[[0]])
   }
  })
 }
}
