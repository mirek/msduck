#!/usr/bin/env node
// First-party post-login SQL Server RETURNVALUE wire evidence.
import assert from 'node:assert/strict'
import {mkdir,readFile,writeFile} from 'node:fs/promises'
import {dirname,resolve} from 'node:path'
import {Request,TYPES} from 'tedious'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference,assertSameCapture,refuseExistingFixture,writeNewFixture} from './lib/reference.mjs'
import {capture,canonical} from './lib/compatibility.mjs'

const fixture=new URL('../reference/rpc-output-wire.json',import.meta.url)
const args=process.argv.slice(2)
const writeFixture=args.includes('--write-fixture')
const positional=args.filter(arg=>arg!=='--write-fixture')
if(positional.length>1)throw Error('expected at most one output path')
const output=resolve(positional[0]??'artifacts/compatibility/rpc-output-wire/capture.json')
const MAX_CAPTURE=2*1024*1024

class Reader{
  constructor(bytes){this.bytes=bytes;this.at=0}
  take(length){
    assert.ok(Number.isSafeInteger(length)&&length>=0&&length<=MAX_CAPTURE&&this.at+length<=this.bytes.length,'bounded TDS field')
    const value=this.bytes.subarray(this.at,this.at+length);this.at+=length;return value
  }
  u8(){return this.take(1)[0]}
  u16(){return this.take(2).readUInt16LE()}
  u32(){return this.take(4).readUInt32LE()}
  u64(){return this.take(8).readBigUInt64LE()}
  utf16(units){return this.take(units*2).toString('utf16le')}
}
function tracePackets(connection){
  const packets=[]
  let pending=Buffer.alloc(0),total=0
  const incoming=connection.messageIo.securePair?.cleartext??connection.messageIo.socket
  const onData=chunk=>{
    total+=chunk.length
    assert.ok(total<=MAX_CAPTURE,'bounded post-login capture')
    pending=Buffer.concat([pending,chunk])
    while(pending.length>=8){
      const length=pending.readUInt16BE(2)
      assert.ok(length>=8&&length<=32767,'valid TDS packet length')
      if(pending.length<length)break
      const packet=pending.subarray(0,length)
      packets.push({type:packet[0],status:packet[1],length,payloadHex:packet.subarray(8).toString('hex')})
      pending=pending.subarray(length)
    }
  }
  incoming.on('data',onData)
  return ()=>{
    incoming.off('data',onData)
    assert.equal(pending.length,0,'complete post-login TDS packets')
    return packets
  }
}
function response(packets){
  const messages=[]
  let parts=[]
  for(const packet of packets){
    assert.equal(packet.type,4,'tabular response packet')
    parts.push(Buffer.from(packet.payloadHex,'hex'))
    if(packet.status&1){messages.push(Buffer.concat(parts));parts=[]}
  }
  assert.equal(parts.length,0,'complete tabular messages')
  assert.equal(messages.length,1,'one RPC response message')
  return messages[0]
}
function readValue(reader,type,max){
  if(type===0x26||type===0x68||type===0x6a||type===0x6c){
    const length=reader.u8()
    assert.ok(length===0||length<=max,'bounded BYTELEN value')
    return {length,valueHex:reader.take(length).toString('hex')}
  }
  if(max!==0xffff){
    const length=reader.u16()
    if(length===0xffff)return {length:null,valueHex:null}
    assert.ok(length<=max,'bounded variable value')
    return {length,valueHex:reader.take(length).toString('hex')}
  }
  const total=reader.u64()
  if(total===0xffffffffffffffffn)return {totalLength:null,chunks:[],valueHex:null}
  assert.ok(total===0xfffffffffffffffen||total<=BigInt(MAX_CAPTURE),'bounded PLP total')
  const chunks=[]
  let count=0
  for(;;){
    const length=reader.u32()
    if(length===0)break
    count+=length
    assert.ok(count<=MAX_CAPTURE,'bounded PLP chunks')
    chunks.push({length,hex:reader.take(length).toString('hex')})
  }
  if(total!==0xfffffffffffffffen)assert.equal(BigInt(count),total,'PLP total matches chunks')
  return {totalLength:total.toString(),chunks,valueHex:chunks.map(c=>c.hex).join('')}
}
function decodeReturnValue(reader,bytes,start){
  const ordinal=reader.u16()
  const nameUnits=reader.u8()
  const nameHex=reader.take(nameUnits*2).toString('hex')
  const name=Buffer.from(nameHex,'hex').toString('utf16le')
  const status=reader.u8()
  const userType=reader.u32()
  const flags=reader.u16()
  assert.equal(flags&0x0800,0,'no encrypted output metadata in fixture')
  const typeStart=reader.at
  const type=reader.u8()
  let max,precision=null,scale=null,collationHex=null
  if(type===0x26||type===0x68)max=reader.u8()
  else if(type===0x6a||type===0x6c){max=reader.u8();precision=reader.u8();scale=reader.u8()}
  else if([0xa5,0xa7,0xad,0xaf,0xe7,0xef].includes(type)){
    max=reader.u16()
    if([0xa7,0xaf,0xe7,0xef].includes(type))collationHex=reader.take(5).toString('hex')
  }else throw Error(`unexpected RETURNVALUE type 0x${type.toString(16)}`)
  const typeInfoHex=bytes.subarray(typeStart,reader.at).toString('hex')
  const value=readValue(reader,type,max)
  return {token:'RETURNVALUE',offset:start,end:reader.at,rawHex:bytes.subarray(start,reader.at).toString('hex'),
    ordinal,nameUnits,nameHex,name,status,userType,flags,type,typeInfoHex,max,precision,scale,collationHex,value}
}
function decodeTokens(bytes){
  const reader=new Reader(bytes),tokens=[]
  while(reader.at<bytes.length){
    const start=reader.at,kind=reader.u8()
    if(kind===0xac){tokens.push(decodeReturnValue(reader,bytes,start));continue}
    if(kind===0x79){const value=reader.take(4).readInt32LE();tokens.push({token:'RETURNSTATUS',offset:start,end:reader.at,value,rawHex:bytes.subarray(start,reader.at).toString('hex')});continue}
    if([0xfd,0xfe,0xff].includes(kind)){
      const status=reader.u16(),command=reader.u16(),count=reader.u64().toString()
      tokens.push({token:{253:'DONE',254:'DONEPROC',255:'DONEINPROC'}[kind],offset:start,end:reader.at,status,command,count,rawHex:bytes.subarray(start,reader.at).toString('hex')});continue
    }
    if(kind===0xaa||kind===0xab){
      const length=reader.u16(),body=new Reader(reader.take(length))
      const number=body.u32(),state=body.u8(),severity=body.u8(),message=body.utf16(body.u16())
      const server=body.utf16(body.u8()),procedure=body.utf16(body.u8()),line=body.u32()
      assert.equal(body.at,body.bytes.length,'complete diagnostic token')
      tokens.push({token:kind===0xaa?'ERROR':'INFO',offset:start,end:reader.at,
        rawHex:bytes.subarray(start,reader.at).toString('hex'),number,state,severity,message,server,procedure,line});continue
    }
    throw Error(`unexpected TDS token 0x${kind.toString(16)} at ${start}`)
  }
  assert.equal(reader.at,bytes.length,'complete response token stream')
  return tokens
}
const longText='A'.repeat(4100)+'\uD800'
const cases=[
  {name:'int output',sql:'SET @p=42',parameters:[['p',TYPES.Int,null,undefined]]},
  {name:'nvarchar four null',sql:'SET @p=NULL',parameters:[['p',TYPES.NVarChar,'x',{length:4}]]},
  {name:'nvarchar raw surrogate',sql:"SET @p=NCHAR(0xD800)+N'Z'",parameters:[['p',TYPES.NVarChar,'',{length:4}]]},
  {name:'decimal nine two',sql:'SET @p=1234567.89',parameters:[['p',TYPES.Decimal,'0',{precision:9,scale:2}]]},
  {name:'nvarchar max plp',sql:'SET @p=@source',parameters:[['p',TYPES.NVarChar,null,{length:0xffff}],['source',TYPES.NVarChar,longText,{length:0xffff},false]]},
  {name:'small before mixed max outputs',sql:"SELECT @first=N'x', @middle=7, @last=0x12",
    parameters:[['first',TYPES.NVarChar,null,{length:0xffff}],['middle',TYPES.Int,0,undefined],['last',TYPES.VarBinary,null,{length:0xffff}]]},
  {name:'throw after assignment',sql:"SET @p=9; THROW 51001,N'output failure',1",parameters:[['p',TYPES.Int,1,undefined]]},
  {name:'procedure nonzero status',mode:'procedure',sql:'dbo.rpc_output_wire_probe',parameters:[['p',TYPES.Int,7,undefined]]}
]
function runRpc(connection,entry){
  return new Promise(resolve=>{
    const request=new Request(entry.sql,(error,rowCount)=>resolve({rowCount:rowCount??null,
      error:error?{number:error.number??null,message:error.message}:null}))
    for(const [name,type,value,options,isOutput=true] of entry.parameters){
      if(isOutput)request.addOutputParameter(name,type,value,options)
      else request.addParameter(name,type,value,options)
    }
    if(entry.mode==='procedure')connection.callProcedure(request)
    else connection.execSql(request)
  })
}
async function runPrepare(connection){
  let complete=()=>{}
  const request=new Request('SET @p=@p+1',error=>complete(error))
  request.addParameter('p',TYPES.Int)
  const stop=tracePackets(connection)
  let packets
  try{
    await new Promise((resolve,reject)=>{
      request.once('prepared',resolve)
      request.once('error',reject)
      connection.prepare(request)
    })
  }finally{packets=stop()}
  const payload=response(packets)
  const tokens=decodeTokens(payload)
  await new Promise((resolve,reject)=>{
    complete=error=>error?reject(error):resolve()
    request.error=undefined
    connection.unprepare(request)
  })
  return {name:'prepared handle',mode:'sp_prepare',sql:'SET @p=@p+1',
    parameters:[{name:'p',type:'Int',output:false}],packets,
    payloadHex:payload.toString('hex'),tokens}
}
function validate(observations){
  const find=name=>{
    const found=observations.find(item=>item.name===name)
    assert(found,`missing ${name}`)
    return found.tokens
  }
  const ordinary=find('int output')
  assert.deepEqual(ordinary.map(token=>token.token),['DONEINPROC','RETURNSTATUS','RETURNVALUE','DONEPROC'])
  const value=ordinary.find(token=>token.token==='RETURNVALUE')
  assert.equal(value.ordinal,2)
  assert.equal(value.name,'@p')
  assert.equal(value.status,1)
  assert.equal(value.flags,0)
  assert.equal(value.typeInfoHex,'2604')
  assert.equal(value.value.valueHex,'2a000000')
  assert.equal(find('nvarchar four null').find(token=>token.token==='RETURNVALUE').value.valueHex,null)
  assert.equal(find('nvarchar raw surrogate').find(token=>token.token==='RETURNVALUE').value.valueHex,'00d85a00')
  const decimal=find('decimal nine two').find(token=>token.token==='RETURNVALUE')
  assert.equal(decimal.typeInfoHex,'6a110902')
  const max=find('nvarchar max plp').find(token=>token.token==='RETURNVALUE')
  assert.equal(max.max,0xffff)
  assert.equal(max.value.valueHex.length/2,Buffer.byteLength(longText,'utf16le'))
  assert.equal(max.value.valueHex,Buffer.from(longText,'utf16le').toString('hex'))
  const mixed=find('small before mixed max outputs').filter(token=>token.token==='RETURNVALUE')
  assert.deepEqual(mixed.map(token=>[token.ordinal,token.name]),
    [[3,'@middle'],[2,'@first'],[4,'@last']])
  assert.equal(find('throw after assignment').filter(token=>token.token==='RETURNVALUE').length,0)
  assert.equal(find('procedure nonzero status').find(token=>token.token==='RETURNSTATUS').value,7)
  const handle=find('prepared handle')
  assert.deepEqual(handle.map(token=>token.token),['DONEINPROC','RETURNSTATUS','RETURNVALUE','DONEPROC'])
  const returnedHandle=handle.find(token=>token.token==='RETURNVALUE')
  assert.equal(returnedHandle.ordinal,0)
  assert.equal(returnedHandle.name,'@handle')
  assert.equal(returnedHandle.status,1)
  assert.equal(returnedHandle.flags,0)
  assert.equal(returnedHandle.typeInfoHex,'2604')
}
async function observe(connection){
  const server=canonical(await capture(connection,"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version"))
  assert.deepEqual(server.errors,[])
  const setup=canonical(await capture(connection,
    'CREATE PROCEDURE dbo.rpc_output_wire_probe @p INT OUTPUT AS BEGIN SET NOCOUNT ON; SET @p=@p+1; RETURN 7; END'))
  assert.deepEqual(setup.errors,[])
  const observations=[]
  for(const entry of cases){
    const stop=tracePackets(connection)
    let packets
    let callback
    try{callback=await runRpc(connection,entry)}finally{packets=stop()}
    const payload=response(packets)
    const tokens=decodeTokens(payload)
    assert.equal(tokens.at(-1)?.token,'DONEPROC',entry.name)
    observations.push({name:entry.name,mode:entry.mode??'sp_executesql',sql:entry.sql,
      parameters:entry.parameters.map(([name,type,value,options,isOutput=true])=>
        ({name,type:type.name,input:canonical(value),options:options??null,output:isOutput})),
      callback,packets,payloadHex:payload.toString('hex'),tokens})
  }
  observations.push(await runPrepare(connection))
  validate(observations)
  return {server,setup,observations}
}
function stable(run){
  return {server:run.server,setup:run.setup,observations:run.observations.map(item=>{
    if(item.name!=='throw after assignment')return item
    // SQL Server's container hostname is embedded in the ERROR token. Keep
    // both raw captures; compare its parsed semantics across containers.
    const {payloadHex,packets,...rest}=item
    return {...rest,packets:packets.map(({payloadHex:ignored,...packet})=>packet),
      tokens:item.tokens.map(token=>{
        const {offset,end,...fields}=token
        if(token.token!=='ERROR')return fields
        const {server,rawHex,...diagnostic}=fields
        return diagnostic
      })}
  })}
}
if(writeFixture)await refuseExistingFixture(fixture)
await mkdir(dirname(output),{recursive:true})
const captureContainer=()=>withReferenceContainer(async(config,container)=>{
  const runs=[]
  for(let i=0;i<2;i++)runs.push(await isolatedReference({...config,options:{...config.options,requestTimeout:120000}},observe))
  assertSameCapture(runs[0],runs[1],'fresh databases in one container differ')
  return {image:container.image,runs}
})
const first=await captureContainer()
const second=await captureContainer()
assert.equal(first.image,second.image,'same pinned image')
assertSameCapture(stable(first.runs[0]),stable(second.runs[0]),'independent containers differ beyond host identity')
const actual={image:first.image,identicalFreshCapturesPerContainer:2,
  runs:first.runs,independentRuns:second.runs}
await writeFile(output,JSON.stringify(actual)+'\n')
let retained
try{retained=JSON.parse(await readFile(fixture,'utf8'))}catch(error){if(error.code!=='ENOENT')throw error}
if(retained){
  assert.equal(actual.image,retained.image,'retained pinned image')
  assertSameCapture(stable(actual.runs[0]),stable(retained.runs[0]),'retained RPC OUTPUT wire observation differs')
  assertSameCapture(stable(actual.independentRuns[0]),stable(retained.independentRuns[0]),'retained independent observation differs')
}
if(writeFixture)await writeNewFixture(fixture,actual)
console.log(`Captured ${actual.runs[0].observations.length} post-login RPC OUTPUT wire cases twice in each of two independent containers${retained?' and matched retained stable observations':''}`)
