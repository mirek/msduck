#!/usr/bin/env node
// First-party post-login TVP RPC request evidence. Never tap LOGIN7 or TLS records.
import assert from 'node:assert/strict'
import {mkdir,readFile,writeFile} from 'node:fs/promises'
import {dirname,resolve} from 'node:path'
import {isDeepStrictEqual} from 'node:util'
import {Request,TYPES} from 'tedious'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference,refuseExistingFixture,writeNewFixture,describeFirstDifference} from './lib/reference.mjs'
import {capture,canonical} from './lib/compatibility.mjs'

const fixture=new URL('../reference/tvp-wire.json',import.meta.url)
const args=process.argv.slice(2)
const writeFixture=args.includes('--write-fixture')
const positional=args.filter(arg=>arg!=='--write-fixture')
if(positional.length>1)throw Error('expected at most one output path')
const output=resolve(positional[0]??'artifacts/compatibility/tvp-wire/capture.json')
const MAX_CAPTURE=512*1024

class Reader {
 constructor(bytes){this.bytes=bytes;this.at=0}
 take(length){
  assert.ok(Number.isSafeInteger(length)&&length>=0&&length<=MAX_CAPTURE&&this.at+length<=this.bytes.length,'bounded TVP field')
  const part=this.bytes.subarray(this.at,this.at+length);this.at+=length;return part
 }
 u8(){return this.take(1)[0]}
 u16(){return this.take(2).readUInt16LE()}
 u32(){return this.take(4).readUInt32LE()}
 text(){return this.take(this.u8()*2).toString('utf16le')}
}
function traceRequests(connection){
 const packets=[]
 let total=0
 const outgoing=connection.messageIo.outgoingMessageStream
 const onData=packet=>{
  total+=packet.length
  assert.ok(total<=MAX_CAPTURE,'bounded post-login request capture')
  assert.ok(packet.length>=8&&packet.length<=32767,'bounded TDS packet')
  assert.equal(packet.readUInt16BE(2),packet.length,'complete TDS packet')
  assert.equal(packet[0],3,'only controlled RPC request packets')
  packets.push({type:packet[0],status:packet[1],packetId:packet[6],length:packet.length,rawHex:packet.toString('hex'),payloadHex:packet.subarray(8).toString('hex')})
 }
 outgoing.on('data',onData)
 return ()=>{
  outgoing.off('data',onData)
  assert.ok(packets.length>0&&packets.at(-1).status&1,'complete RPC request')
  for(const packet of packets.slice(0,-1))assert.equal(packet.status&1,0,'EOM only on final packet')
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
 const parameterName=r.text(), status=r.u8(), type=r.u8()
 assert.equal(type,0xf3,'TVPTYPE')
 const database=r.text(), schema=r.text(), typeName=r.text()
 const count=r.u16(),columns=[]
 if(count!==0xffff){
  assert.ok(count<=32,'bounded TVP column count')
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
 return {payloadHex:bytes.toString('hex'),headers,procedure,options,parameterName,status,
  tvp:{type,database,schema,typeName,columnCount:count,columns,rows}}
}

const columns=[
 {name:'id',type:TYPES.Int},
 {name:'label',type:TYPES.NVarChar,length:10},
 {name:'payload',type:TYPES.VarBinary,length:10}
]
const value=rows=>({schema:'dbo',name:'TvpProbe',columns,rows})
const cases=[
 {name:'null TVP',value:null,expectedError:8060},
 {name:'empty TVP',value:value([]),expectedCount:0},
 {name:'mixed nullable row',value:value([[1,null,Buffer.from([0,255])]]),expectedCount:1},
 {name:'multirow',value:value([[1,'alpha',Buffer.from([1,2])],[2,null,null],[3,'Z',Buffer.alloc(0)]]),expectedCount:3}
]
function runRpc(connection,entry){
 return new Promise(resolve=>{
  const rows=[],errors=[]
  const onError=error=>errors.push({number:error.number,state:error.state,class:error.class,message:error.message})
  connection.on('errorMessage',onError)
  const request=new Request('dbo.TvpProbeEcho',(error,rowCount)=>{
   connection.off('errorMessage',onError)
   resolve({rowCount:rowCount??null,rows,errors,error:error?{number:error.number??null,message:error.message}:null})
  })
  request.on('row',cells=>rows.push(cells.map(cell=>canonical(cell.value))))
  request.addParameter('rows',TYPES.TVP,entry.value)
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
  const stop=traceRequests(connection)
  let packets,callback
  try{callback=await runRpc(connection,entry)}finally{packets=stop()}
  const request=decodeRequest(packets)
  assert.equal(request.procedure,'dbo.TvpProbeEcho')
  assert.equal(request.parameterName,'@rows')
  assert.equal(request.tvp.columnCount,entry.value===null?0xffff:3)
  assert.equal(request.tvp.rows.length,entry.value?.rows.length??0)
  if(entry.expectedError){
   assert.equal(callback.error?.number,entry.expectedError)
   assert.equal(callback.errors[0]?.number,entry.expectedError)
  }else{
   assert.deepEqual(callback.errors,[])
   assert.equal(callback.error,null)
   assert.deepEqual(callback.rows,[[entry.expectedCount]])
  }
  observations.push({name:entry.name,packets,request,callback})
 }
 return {version,observations}
}
const runContainer=()=>withReferenceContainer(async(config,container)=>({
 image:container.image,
 runs:[await isolatedReference(config,observe),await isolatedReference(config,observe)]
}))
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
let retained
try{retained=JSON.parse(await readFile(fixture,'utf8'))}catch(error){if(error.code!=='ENOENT')throw error}
if(retained){
 assert.equal(actual.image,retained.image,'retained image')
 assert.ok(isDeepStrictEqual(actual,retained),`retained TVP wire difference: ${describeFirstDifference(actual,retained)}`)
}
if(writeFixture)await writeNewFixture(fixture,actual)
console.log(`Captured ${cases.length} TVP RPC request cases in ${runs.length} fresh databases across two containers${retained?' and matched retained fixture':''}`)
