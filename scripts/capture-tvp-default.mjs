#!/usr/bin/env node
// Owner-controlled, post-login TVP RPC default-status capture. The only wire
// mutation is one parameter StatusFlags byte, applied before TLS encryption.
import assert from 'node:assert/strict'
import {lstat,mkdir,readFile,realpath,stat,writeFile} from 'node:fs/promises'
import {basename,dirname,resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {isDeepStrictEqual} from 'node:util'
import {Request,TYPES} from 'tedious'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference,refuseExistingFixture,writeNewFixture,describeFirstDifference} from './lib/reference.mjs'
import {capture,canonical} from './lib/compatibility.mjs'

const fixture=new URL('../reference/tvp-default.json',import.meta.url)
const args=process.argv.slice(2)
const writeFixture=args.includes('--write-fixture')
const positional=args.filter(arg=>arg!=='--write-fixture')
if(positional.length>1)throw Error('expected at most one output path')
const output=resolve(positional[0]??'artifacts/compatibility/tvp-default/capture.json')
const MAX_CAPTURE=512*1024

async function canonicalTarget(path){
 let current=path,missing=[]
 while(true){
  try{return resolve(await realpath(current),...missing.reverse())}
  catch(error){if(error.code!=='ENOENT')throw error}
  const parent=dirname(current)
  if(parent===current)throw Error('cannot resolve capture output path')
  missing.push(basename(current));current=parent
 }
}
async function assertSeparateOutput(){
 for(let part=output;;part=dirname(part)){
  let info
  try{info=await lstat(part)}catch(error){if(error.code!=='ENOENT')throw error}
  assert.ok(!info?.isSymbolicLink(),'capture output path contains a symlink')
  if(dirname(part)===part)break
 }
 const retained=fileURLToPath(fixture)
 assert.notEqual(await canonicalTarget(output),await canonicalTarget(retained),'capture output aliases retained TVP fixture')
 let outputFile,retainedFile
 try{outputFile=await stat(output)}catch(error){if(error.code!=='ENOENT')throw error}
 try{retainedFile=await stat(retained)}catch(error){if(error.code!=='ENOENT')throw error}
 assert.ok(!outputFile||!retainedFile||outputFile.dev!==retainedFile.dev||outputFile.ino!==retainedFile.ino,
  'capture output hard-links retained TVP fixture')
}

class Reader {
 constructor(bytes){this.bytes=bytes;this.at=0}
 take(length){
  assert.ok(Number.isSafeInteger(length)&&length>=0&&length<=MAX_CAPTURE&&this.at+length<=this.bytes.length,'bounded RPC field')
  const part=this.bytes.subarray(this.at,this.at+length);this.at+=length;return part
 }
 u8(){return this.take(1)[0]}
 u16(){return this.take(2).readUInt16LE()}
 u32(){return this.take(4).readUInt32LE()}
 text(){return this.take(this.u8()*2).toString('utf16le')}
}

function statusOffset(packet){
 assert.ok(packet.length>=8&&packet[0]===3&&packet[1]&1,'single final RPC packet for default-bit probe')
 const r=new Reader(packet.subarray(8))
 const headerLength=r.u32()
 assert.ok(headerLength>=4&&headerLength<=r.bytes.length,'bounded ALL_HEADERS')
 r.take(headerLength-4)
 assert.equal(r.take(r.u16()*2).toString('utf16le'),'dbo.TvpProbeEcho')
 assert.equal(r.u16(),0,'RPC options')
 assert.equal(r.text(),'@rows')
 const at=8+r.at
 assert.equal(packet[at],0,'ordinary input StatusFlags before default-bit probe')
 assert.equal(packet[at+1],0xf3,'TVPTYPE after StatusFlags')
 return at
}

function traceRpc(connection,patchDefault){
 const outgoing=connection.messageIo.outgoingMessageStream
 const originalPush=outgoing.push
 const packets=[]
 let total=0,patched=0
 outgoing.push=function(chunk,...rest){
  if(chunk===null)return originalPush.call(this,chunk,...rest)
  assert.ok(Buffer.isBuffer(chunk)&&chunk.length>=8&&chunk.length<=32767,'bounded complete outgoing TDS packet')
  const packet=Buffer.from(chunk)
  assert.equal(packet.readUInt16BE(2),packet.length,'complete TDS packet length')
  assert.equal(packet[0],3,'only controlled RPC request packets')
  total+=packet.length
  assert.ok(total<=MAX_CAPTURE,'bounded post-login request capture')
  if(patchDefault){
   assert.equal(patched,0,'exactly one default-bit packet')
   packet[statusOffset(packet)]=2
   patched++
  }
  packets.push({type:packet[0],status:packet[1],packetId:packet[6],length:packet.length,
   rawHex:packet.toString('hex'),payloadHex:packet.subarray(8).toString('hex')})
  return originalPush.call(this,packet,...rest)
 }
 return ()=>{
  outgoing.push=originalPush
  assert.ok(packets.length>0&&packets.at(-1).status&1,'complete RPC request')
  for(const packet of packets.slice(0,-1))assert.equal(packet.status&1,0,'EOM only on final packet')
  if(patchDefault)assert.equal(patched,1,'one default-status mutation')
  return packets
 }
}

function decodeRequest(packets){
 const bytes=Buffer.concat(packets.map(packet=>Buffer.from(packet.payloadHex,'hex')))
 assert.ok(bytes.length<=MAX_CAPTURE,'bounded RPC payload')
 const r=new Reader(bytes)
 const headerLength=r.u32()
 assert.ok(headerLength>=4&&headerLength<=bytes.length,'bounded ALL_HEADERS')
 const headers=r.take(headerLength-4).toString('hex')
 const procedure=r.take(r.u16()*2).toString('utf16le')
 const options=r.u16()
 if(r.at===bytes.length)return {payloadHex:bytes.toString('hex'),headers,procedure,options,parameter:null}
 const name=r.text(),status=r.u8(),type=r.u8()
 assert.equal(type,0xf3,'TVPTYPE')
 const database=r.text(),schema=r.text(),typeName=r.text()
 const count=r.u16(),columns=[]
 if(count!==0xffff){
  assert.ok(count>=1&&count<=32,'bounded TVP column count')
  for(let i=0;i<count;i++){
   const userType=r.u32(),flags=r.u16(),kind=r.u8()
   let typeInfo
   if(kind===0x26)typeInfo={kind,width:r.u8()}
   else if(kind===0xe7)typeInfo={kind,maxBytes:r.u16(),collationHex:r.take(5).toString('hex')}
   else if(kind===0xa5)typeInfo={kind,maxBytes:r.u16()}
   else throw Error(`unexpected TVP column type 0x${kind.toString(16)}`)
   columns.push({userType,flags,typeInfo,name:r.text()})
  }
 }
 assert.equal(r.u8(),0,'TVP metadata terminator')
 const rows=[]
 while(r.bytes[r.at]===1){
  assert.ok(rows.length<4096,'bounded TVP row count')
  r.u8()
  const values=[]
  for(const column of columns){
   const {kind,width,maxBytes}=column.typeInfo
   const length=kind===0x26?r.u8():r.u16()
   const isNull=length===(kind===0x26?0:0xffff)
   assert.ok(isNull||length<=(kind===0x26?width:maxBytes),'bounded TVP cell')
   values.push({length,null:isNull,valueHex:isNull?null:r.take(length).toString('hex')})
  }
  rows.push(values)
 }
 assert.equal(r.u8(),0,'TVP row terminator')
 assert.equal(r.at,bytes.length,'complete controlled RPC payload')
 return {payloadHex:bytes.toString('hex'),headers,procedure,options,parameter:{name,status,
  tvp:{type,database,schema,typeName,columnCount:count,columns,rows}}}
}

const columns=[
 {name:'id',type:TYPES.Int},
 {name:'label',type:TYPES.NVarChar,length:10},
 {name:'payload',type:TYPES.VarBinary,length:10}
]
const empty={schema:'dbo',name:'TvpProbe',columns,rows:[]}
const cases=[
 {name:'omitted TVP',kind:'omitted',expectedCount:0},
 {name:'explicit null TVP',kind:'null',expectedError:8060},
 {name:'default-bit null TVP',kind:'default',expectedCount:0},
 {name:'empty TVP control',kind:'empty',expectedCount:0}
]

function runRpc(connection,entry){
 return new Promise(resolve=>{
  const result={sets:[],done:[],errors:[],info:[],returnStatus:null,rowCount:null,callbackError:null}
  const onError=error=>result.errors.push({number:error.number,state:error.state,class:error.class,lineNumber:error.lineNumber,message:error.message})
  const onInfo=info=>result.info.push({number:info.number,state:info.state,class:info.class,lineNumber:info.lineNumber,message:info.message})
  connection.on('errorMessage',onError)
  connection.on('infoMessage',onInfo)
  const request=new Request('dbo.TvpProbeEcho',(error,rowCount)=>{
   connection.off('errorMessage',onError)
   connection.off('infoMessage',onInfo)
   result.rowCount=rowCount??null
   result.callbackError=error?{number:error.number??null,message:error.message}:null
   resolve(canonical(result))
  })
  request.on('columnMetadata',metadata=>result.sets.push({
   columns:metadata.map(c=>({name:c.colName,type:c.type.name,length:c.dataLength??null,precision:c.precision??null,scale:c.scale??null,flags:c.flags,collation:canonical(c.collation??null)})),rows:[]
  }))
  request.on('row',cells=>result.sets.at(-1).rows.push(cells.map(cell=>canonical(cell.value))))
  for(const kind of ['done','doneInProc','doneProc'])request.on(kind,(rowCount,more)=>result.done.push({kind,rowCount:rowCount??null,more}))
  request.on('doneProc',(_count,_more,status)=>{result.returnStatus=status??null})
  if(entry.kind!=='omitted')request.addParameter('rows',TYPES.TVP,entry.kind==='empty'?empty:null)
  connection.callProcedure(request)
 })
}

async function observe(connection){
 const version=canonical(await capture(connection,"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version"))
 assert.deepEqual(version.errors,[])
 for(const sql of [
  'CREATE TYPE dbo.TvpProbe AS TABLE (id INT NULL, label NVARCHAR(10) NULL, payload VARBINARY(10) NULL)',
  'CREATE PROCEDURE dbo.TvpProbeEcho @rows dbo.TvpProbe READONLY AS SELECT COUNT(*) AS n FROM @rows'
 ])assert.deepEqual((await capture(connection,sql)).errors,[])
 const observations=[]
 for(const entry of cases){
  const stop=traceRpc(connection,entry.kind==='default')
  let packets,response
  try{response=await runRpc(connection,entry)}finally{packets=stop()}
  const request=decodeRequest(packets)
  assert.equal(request.procedure,'dbo.TvpProbeEcho')
  assert.equal(request.options,0)
  if(entry.kind==='omitted')assert.equal(request.parameter,null)
  else{
   assert.equal(request.parameter.name,'@rows')
   assert.equal(request.parameter.status,entry.kind==='default'?2:0)
   assert.equal(request.parameter.tvp.columnCount,entry.kind==='empty'?3:0xffff)
  }
  if(entry.expectedError){
   assert.equal(response.callbackError?.number,entry.expectedError)
   assert.equal(response.errors[0]?.number,entry.expectedError)
  }
  if(entry.expectedCount!==undefined){
   assert.deepEqual(response.errors,[])
   assert.equal(response.callbackError,null)
   assert.deepEqual(response.sets[0]?.rows,[[entry.expectedCount]])
  }
  observations.push({name:entry.name,packets,request,response})
 }
 const ordinary=Buffer.from(observations[1].request.payloadHex,'hex')
 const defaulted=Buffer.from(observations[2].request.payloadHex,'hex')
 assert.equal(ordinary.length,defaulted.length,'only the default status differs in null TVP requests')
 const differences=[]
 for(let i=0;i<ordinary.length;i++)if(ordinary[i]!==defaulted[i])differences.push([i,ordinary[i],defaulted[i]])
 const statusAt=statusOffset(Buffer.from(observations[1].packets[0].rawHex,'hex'))-8
 assert.deepEqual(differences,[[statusAt,0,2]],'one status byte is the entire default probe mutation')
 return {version,observations}
}

const runContainer=()=>withReferenceContainer(async(config,container)=>({
 image:container.image,
 runs:[await isolatedReference(config,observe),await isolatedReference(config,observe)]
}))
await assertSeparateOutput()
if(writeFixture)await refuseExistingFixture(fixture)
await mkdir(dirname(output),{recursive:true})
const first=await runContainer(),second=await runContainer()
assert.equal(first.image,second.image,'same pinned SQL Server image')
const runs=[...first.runs,...second.runs]
const comparisons=[]
for(let i=1;i<runs.length;i++){
 const equal=isDeepStrictEqual(runs[0],runs[i])
 comparisons.push({run:i,exactMatch:equal,firstDifference:equal?null:describeFirstDifference(runs[i],runs[0])})
}
const actual={image:first.image,runs,comparisons}
await writeFile(output,JSON.stringify(actual)+'\n')
assert.ok(comparisons.every(c=>c.exactMatch),`independent TVP default runs differ: ${comparisons.find(c=>!c.exactMatch)?.firstDifference}`)
let retained
try{retained=JSON.parse(await readFile(fixture,'utf8'))}catch(error){if(error.code!=='ENOENT')throw error}
if(retained){
 assert.equal(actual.image,retained.image,'retained image')
 assert.ok(isDeepStrictEqual(actual,retained),`retained TVP default difference: ${describeFirstDifference(actual,retained)}`)
}
if(writeFixture)await writeNewFixture(fixture,actual)
console.log(`Captured ${cases.length} TVP RPC cases in ${runs.length} fresh databases across two containers${retained?' and matched retained fixture':''}`)
