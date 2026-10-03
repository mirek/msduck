#!/usr/bin/env node
// Retain complete ASCII SQL Server TRANSLATE relationships and original wire evidence.
// Usage: capture-translate-ascii-matching.mjs [output] [--write-fixture | --one-database | --check-fixture]
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { createHash } from 'node:crypto'
import { lstat, mkdir, mkdtemp, readFile, realpath, stat, writeFile, symlink, link, chmod, rm } from 'node:fs/promises'
import { basename, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { isDeepStrictEqual, promisify } from 'node:util'
import { execFile } from 'node:child_process'
import { resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { canonical as baseCanonical } from './lib/compatibility.mjs'
import { referenceImage, withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture } from './lib/reference.mjs'
// JSON cannot retain negative zero as a number. Preserve that observed value explicitly.
function canonical(value) {
  if (typeof value === 'number' && Object.is(value,-0)) return {kind:'number',value:'-0'}
  if (Array.isArray(value)) return value.map(canonical)
  if (value && typeof value === 'object' && !Buffer.isBuffer(value) && !(value instanceof Date)) {
    return Object.fromEntries(Object.entries(value).map(([key,item])=>[key,canonical(item)]))
  }
  return baseCanonical(value)
}
const phaseByteLimit=1024*1024
const captureByteLimit=128*1024*1024
const deadline=Date.now()+30*60*1000
function checkDeadline() { if (Date.now()>deadline) throw new Error('TRANSLATE ASCII matching capture watchdog expired') }


const fixture = new URL('../reference/translate-ascii-matching.json', import.meta.url)
const flags = ['--write-fixture', '--one-database', '--check-fixture', '--self-test']
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const checkFixture = args.includes('--check-fixture')
const selfTest = args.includes('--self-test')
const positional = args.filter(arg => !flags.includes(arg))
if (args.some(arg => arg.startsWith('--') && !flags.includes(arg))) throw new Error('unknown option')
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
if ((checkFixture || selfTest) && (writeFixture || oneDatabase || positional.length || (checkFixture && selfTest))) throw new Error('--check-fixture takes no other arguments')
const output = resolve(positional[0] ?? '.tmp/translate-ascii-matching/capture.json')

// Ordered token evidence. The shared capture() buckets metadata, messages and
// DONE events separately and keeps only DONE kind/count/more, so this script
// also records every token the request handler sees, in wire order, with the
// raw DONE status word, current command and count field. tedious drops the
// INXACT bit and invalid counts, so the DONE parsers are wrapped to retain the
// 12 raw bytes. Both hooks are local to this process.
const require = createRequire(import.meta.url)
const doneParsers = require('tedious/lib/token/done-token-parser.js')
const { RequestTokenHandler } = require('tedious/lib/token/handler.js')
for (const name of ['doneParser', 'doneInProcParser', 'doneProcParser']) {
  const original = doneParsers[name]
  assert.equal(typeof original, 'function', name)
  doneParsers[name] = (buffer, offset, options) => {
    const result = original(buffer, offset, options)
    assert(options.tdsVersion >= '7_2', 'expected an 8-byte DONE count')
    result.value.raw = {
      status: buffer.readUInt16LE(offset),
      command: buffer.readUInt16LE(offset + 2),
      count: buffer.readBigUInt64LE(offset + 4).toString(),
    }
    return result
  }
}
// Retain exactly the bytes consumed by RETURNVALUE, even when tedious discards
// consumed prefixes while awaiting another chunk. The bound is specific to
// this capture (the only output parameter is the IntN prepare handle).
const StreamParser = require('tedious/lib/token/stream-parser.js')
const originalReturnParser = StreamParser.prototype.readReturnValueToken
assert.equal(typeof originalReturnParser, 'function', 'returnvalue parser')
StreamParser.prototype.readReturnValueToken = async function () {
  const parser = this
  const originalWait = parser.waitForChunk
  const hadOwnWait = Object.hasOwn(parser, 'waitForChunk')
  const pieces = []
  const limit = 1024 * 1024
  let start = parser.position, length = 0, waits = 0
  function consumed() {
    const size = parser.position - start
    if (size < 0 || length + size > limit) throw new Error('RETURNVALUE capture exceeds byte bound')
    if (size) pieces.push(Buffer.from(parser.buffer.subarray(start, parser.position)))
    length += size
    start = parser.position
  }
  parser.waitForChunk = async function () {
    consumed()
    if (++waits > limit || length + parser.buffer.length - parser.position > limit) {
      throw new Error('RETURNVALUE capture exceeds fragment bound')
    }
    await originalWait.call(parser)
    start = parser.position
    if (length + parser.buffer.length - start > limit) throw new Error('RETURNVALUE capture exceeds byte bound')
  }
  try {
    const token = await originalReturnParser.call(parser)
    consumed()
    const bytes = Buffer.concat(pieces, length)
    if (bytes.length < 4) throw new Error('RETURNVALUE raw header is incomplete')
    const nameLength = bytes.readUInt8(2)
    const statusOffset = 3 + nameLength * 2
    if (bytes.length <= statusOffset) throw new Error('RETURNVALUE raw name is incomplete')
    token.raw = {
      ordinal: bytes.readUInt16LE(0),
      name: bytes.toString('utf16le', 3, statusOffset),
      status: bytes.readUInt8(statusOffset),
      hex: bytes.toString('hex'),
    }
    return token
  } finally {
    if (hadOwnWait) parser.waitForChunk = originalWait
    else delete parser.waitForChunk
  }
}
// Capture ROW/NBCROW bytes from that actual result evaluation. The separately
// projected CONVERT binary is another server evaluation, not a substitute.
for(const method of ['readRowToken','readNbcRowToken']) {
  const original=StreamParser.prototype[method]
  assert.equal(typeof original,'function',method)
  StreamParser.prototype[method]=async function() {
    const parser=this,originalWait=parser.waitForChunk,hadOwn=Object.hasOwn(parser,'waitForChunk')
    const pieces=[]
    let start=parser.position,length=0,waits=0
    const consumed=()=>{
      const count=parser.position-start
      if(count<0 || length+count>phaseByteLimit)throw new Error('raw row byte bound')
      if(count)pieces.push(Buffer.from(parser.buffer.subarray(start,parser.position)))
      length+=count;start=parser.position
    }
    parser.waitForChunk=async function() {
      consumed()
      if(++waits>phaseByteLimit || length+parser.buffer.length-parser.position>phaseByteLimit)throw new Error('raw row fragment bound')
      await originalWait.call(parser);start=parser.position
      if(length+parser.buffer.length-start>phaseByteLimit)throw new Error('raw row byte bound')
    }
    try {
      const token=await original.call(parser);consumed()
      token.raw={hex:Buffer.concat(pieces,length).toString('hex')}
      return token
    } finally {
      if(hadOwn)parser.waitForChunk=originalWait
      else delete parser.waitForChunk
    }
  }
}
function rowPayloads(token,columns) {
  assert(typeof token.raw?.hex==='string' && /^(?:[0-9a-f]{2})*$/.test(token.raw.hex),'raw row hex')
  const raw=Buffer.from(token.raw.hex,'hex')
  assert(raw.length<=phaseByteLimit,'raw row bound')
  let offset=0
  function take(count) {assert(Number.isSafeInteger(count)&&count>=0&&offset+count<=raw.length,'raw row bounds');const result=raw.subarray(offset,offset+count);offset+=count;return result}
  const bitmap=token.token==='NBCROW'?take(Math.ceil(columns.length/8)):null
  const cells=[]
  for(const [index,column] of columns.entries()) {
    if(bitmap && bitmap[index>>3]&(1<<(index%8))){cells.push(null);continue}
    if(column.type==='Int'){cells.push(take(4));continue}
    if(column.type==='IntN') {
      const length=take(1)[0]
      assert([0,1,2,4,8].includes(length),'IntN row length')
      cells.push(length===0?null:take(length));continue
    }
    assert(['VarBinary','VarChar','NVarChar'].includes(column.type),'unmeasured row type '+column.type)
    if(column.length===65535) {
      const total=take(8).readBigUInt64LE()
      if(total===0xffffffffffffffffn){cells.push(null);continue}
      const pieces=[];let size=0
      for(;;){const length=take(4).readUInt32LE();if(length===0)break;size+=length;assert(size<=phaseByteLimit,'raw PLP bound');pieces.push(take(length))}
      assert(total===0xfffffffffffffffen || total===BigInt(size),'raw PLP length')
      cells.push(Buffer.concat(pieces,size))
    } else {
      const length=take(2).readUInt16LE()
      cells.push(length===65535?null:take(length))
    }
  }
  assert.equal(offset,raw.length,'complete raw row consumed')
  return cells
}
function reconcileRow(token,columns,row) {
  const cells=rowPayloads(token,columns)
  const iconv=require('iconv-lite')
  for(const [index,payload]of cells.entries()) {
    const column=columns[index],value=row[index]
    if(payload===null){assert.equal(value,null);continue}
    if(column.type==='VarBinary')assertSameCapture(value,{kind:'binary',value:payload.toString('hex')})
    else if(column.type==='NVarChar') {assert.equal(payload.length%2,0,'UTF16 row alignment');assert.equal(value,payload.toString('utf16le'),'actual raw UTF16 row')}
    else if(column.type==='VarChar'){assert(column.collation?.codepage,'explicit captured ANSI codepage');assert.equal(value,iconv.decode(payload,column.collation.codepage),'actual raw ANSI row')}
    else if(['Int','IntN'].includes(column.type)) {
      if(payload.length===8)assertSameCapture(value,{kind:'bigint',value:payload.readBigInt64LE().toString()})
      else assert.equal(value,payload.length===1?payload[0]:payload.readIntLE(0,payload.length))
    }
  }
}

async function testRawRows() {
  const metadata=[{type:TYPES.NVarChar,dataLength:1024}]
  for(const [kind,raw,value]of [[0xd1,'04003dd80000','\ud83d\0'],[0xd1,'ffff',null],[0xd2,'0004003dd80000','\ud83d\0'],[0xd2,'01',null]]) {
    const bytes=Buffer.concat([Buffer.from([kind]),Buffer.from(raw,'hex'),Buffer.from('fd000000000000000000000000','hex')])
    async function parse(chunks) {
      const tokens=[]
      for await(const token of StreamParser.parseTokens(chunks,{token(){}},{tdsVersion:'7_4',useUTC:true},metadata))tokens.push(token)
      assert.equal(tokens.length,2)
      assert.equal(tokens[0].raw.hex,raw,'same consumed raw row bytes')
      assert.equal(tokens[0].columns[0].value,value,'lossless decoded row')
      reconcileRow({token:kind===0xd1?'ROW':'NBCROW',raw:tokens[0].raw},[{type:'NVarChar',length:1024}],[value])
      assert.equal(tokens[1].name,'DONE')
    }
    await parse([bytes])
    for(let split=1;split<bytes.length;split++)await parse([bytes.subarray(0,split),bytes.subarray(split)])
    await parse(Array.from(bytes,byte=>Buffer.from([byte])))
    if(value!==null)await assert.rejects(parse([bytes.subarray(0,3)]),/unexpected end of data/)
  }
  assert.throws(()=>rowPayloads({token:'ROW',raw:{hex:'00'.repeat(phaseByteLimit+1)}},[]),/raw row bound/)
  console.log('ROW/NBCROW all splits/bytewise/null/unpaired-unit fidelity and truncation proofs pass')
}

const metadata = value => ({
  type: value.type.name, userType: value.userType, flags: value.flags, dataLength: value.dataLength ?? null,
  precision: value.precision ?? null, scale: value.scale ?? null, collation: value.collation ?? null,
})
const taps = new WeakMap()
const message = token => ({ number: token.number, state: token.state, class: token.class, lineNumber: token.lineNumber, serverName: token.serverName, procName: token.procName, message: token.message })
const done = token => ({ status: token.raw.status, command: token.raw.command, count: token.raw.count })
for (const [method, describe] of Object.entries({
  onColMetadata: token => ({ columns: token.columns.map(column => column.colName), descriptors: token.columns.map(columnDescriptor) }),
  onRow: token => ({raw:token.raw}),
  onOrder: token => ({ columns: token.orderColumns }),
  onErrorMessage: message,
  onInfoMessage: message,
  onReturnStatus: token => ({ value: token.value }),
  onReturnValue: token => ({ paramOrdinal: token.paramOrdinal, paramName: token.paramName, raw: token.raw, metadata: metadata(token.metadata), value: token.value }),
  onDone: done,
  onDoneInProc: done,
  onDoneProc: done,
  onDatabaseChange: token => ({ newValue: token.newValue }),
  onLanguageChange: token => ({ newValue: token.newValue }),
  onCharsetChange: token => ({ newValue: token.newValue }),
  onSqlCollationChange: token => ({ newValue: token.newValue }),
  onPacketSizeChange: token => ({ newValue: token.newValue }),
  onBeginTransaction: token => ({ newValue: token.newValue }),
  onCommitTransaction: token => ({ oldValue: token.oldValue }),
  onRollbackTransaction: token => ({ oldValue: token.oldValue }),
  onResetConnection: () => ({}),
})) {
  const original = RequestTokenHandler.prototype[method]
  assert.equal(typeof original, 'function', method)
  RequestTokenHandler.prototype[method] = function (token) {
    taps.get(this.request)?.push(canonical({ token: token.name, ...describe(token) }))
    return original.call(this, token)
  }
}

// Resolve symlinks through the nearest existing ancestor so a not-yet-existing
// output path or a symlinked directory cannot alias the retained fixture.
async function canonicalPath(path) {
  try { return await realpath(path) }
  catch (error) {
    if (error.code !== 'ENOENT') throw error
    const parent = dirname(path)
    if (parent === path) return path
    return resolve(await canonicalPath(parent), basename(path))
  }
}

async function refuseFixtureOutput(outputPath, fixturePath) {
  if (await canonicalPath(outputPath) === await canonicalPath(fixturePath)) {
    throw new Error('refusing to write capture output over retained fixture ' + fixturePath)
  }
  let outputStat, fixtureStat
  try { [outputStat, fixtureStat] = await Promise.all([stat(outputPath), stat(fixturePath)]) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (outputStat && fixtureStat && outputStat.dev === fixtureStat.dev && outputStat.ino === fixtureStat.ino) {
    throw new Error('refusing to write capture output over retained fixture ' + fixturePath)
  }
  try {
    await lstat(outputPath)
    throw new Error('refusing to overwrite existing capture output ' + outputPath)
  } catch (error) { if (error.code !== 'ENOENT') throw error }
}


function wireEvidence(type,value,options={},collation) {
  const parameter={...options,collation,value:type.validate(value,collation)}
  return {typeInfo:type.generateTypeInfo(parameter).toString('hex'),length:type.generateParameterLength(parameter).toString('hex'),payload:Buffer.concat([...type.generateParameterData(parameter,{useUTC:true})]).toString('hex')}
}
const cases=[],provenance=new Map(),rpcPrograms=[],preparedPrograms=[]
const collations=['SQL_Latin1_General_CP1_CI_AS','Latin1_General_100_CI_AS','Latin1_General_100_CS_AS','Latin1_General_100_CI_AI','Latin1_General_100_CS_AI','Latin1_General_100_BIN2','Latin1_General_100_CI_AS_SC','Latin1_General_100_CI_AS_SC_UTF8']
const domains=['VARCHAR(512)','NVARCHAR(512)']
const units=text=>Array.from({length:text.length},(_,i)=>text.charCodeAt(i))
const unitHex=value=>{const bytes=Buffer.alloc(value.length*2);value.forEach((unit,i)=>bytes.writeUInt16LE(unit,i*2));return bytes.toString('hex')}
const alphabet=Array.from({length:128},(_,i)=>i)
const sentinels=[0xe9,0x20ac]
function operand(value,declaration,collation) {
  return `CONVERT(${declaration},CONVERT(NVARCHAR(512),0x${unitHex(value)}) COLLATE ${collation}) COLLATE ${collation}`
}
function projection(names) {
  const [s,m,t,u]=names
  return `CONVERT(VARBINARY(MAX),${s}) AS source_bytes,CONVERT(VARBINARY(MAX),${m}) AS mapping_bytes,CONVERT(VARBINARY(MAX),${t}) AS first_sentinel_bytes,CONVERT(VARBINARY(MAX),${u}) AS second_sentinel_bytes,TRANSLATE(${s},${m},${t}) AS translated_first,CONVERT(VARBINARY(MAX),TRANSLATE(${s},${m},${t})) AS first_binary_projection,TRANSLATE(${s},${m},${u}) AS translated_second,CONVERT(VARBINARY(MAX),TRANSLATE(${s},${m},${u})) AS second_binary_projection`
}
for(const collation of collations)for(const declaration of domains)for(let mapping=0;mapping<128;mapping++) {
  const source=declaration.startsWith('VARCHAR')?`CONVERT(VARCHAR(512),0x${Buffer.from(alphabet).toString('hex')}) COLLATE ${collation}`:operand(alphabet,declaration,collation)
  const sql=`;WITH v AS (SELECT ${source} AS s,${operand([mapping],declaration,collation)} AS m,${operand([sentinels[0]],declaration,collation)} AS t,${operand([sentinels[1]],declaration,collation)} AS u) SELECT ${projection(['s','m','t','u'])} FROM v;`
  const name=`ASCII grid ${declaration} ${collation} mapping ${mapping}`
  cases.push([name,sql]);provenance.set(name,{kind:'ASCII grid',declaration,collation,mapping,sourceUnits:declaration.startsWith('NVARCHAR')?alphabet:null,originalBinaryBytes:declaration.startsWith('VARCHAR')?alphabet:null,mappingUnits:[mapping],sentinelUnits:sentinels,construction:declaration.startsWith('VARCHAR')?'original binary ASCII to database VARCHAR, then explicit COLLATE':'original UTF16 ASCII to NVARCHAR, then explicit COLLATE',roles:['native operands','first TRANSLATE text and independent binary projection','second TRANSLATE text and independent binary projection']})
}
function contextSql() {return 'SELECT @@TRANCOUNT AS transaction_count,1 AS reusable'}
let outgoingCapture=null
for(const wireType of [TYPES.VarChar,TYPES.NVarChar]) {
  const generate=wireType.generateParameterData
  wireType.generateParameterData=function*(parameter,options) {
    const chunks=[]
    for(const bytes of generate.call(this,parameter,options)){chunks.push(Buffer.from(bytes));yield bytes}
    if(outgoingCapture)outgoingCapture.push({type:wireType.name,typeInfo:wireType.generateTypeInfo(parameter).toString('hex'),length:wireType.generateParameterLength(parameter).toString('hex'),payload:Buffer.concat(chunks).toString('hex')})
  }
}

for(const collation of collations)for(const declaration of domains) {
  const wireType=declaration.startsWith('NVARCHAR')?TYPES.NVarChar:TYPES.VarChar
  const name=`prepared ${declaration} ${collation}`
  const names=['s','m','t','u'].map(x=>`@${x} COLLATE ${collation}`)
  const sql=`SELECT ${projection(names)}`
  const bindingSets=[{s:String.fromCharCode(...alphabet),m:'A',t:'é',u:'€'},{s:'aAaA',m:'a',t:'é',u:'€'},{s:' -\0 ',m:' ',t:'é',u:'€'},{s:null,m:'A',t:'é',u:'€'},{s:'ab',m:'ab',t:'é',u:'€'},{s:String.fromCharCode(...alphabet),m:'A',t:'é',u:'€'}]
  preparedPrograms.push([name,sql,['s','m','t','u'].map(x=>[x,wireType,{length:512}]),bindingSets]);provenance.set(name,{kind:'prepared ASCII matching',declaration,collation,bindings:bindingSets,roles:['native emitted operands','first and second TRANSLATE']})
}
const programSql=new Map([...cases,...preparedPrograms].map(([name,sql])=>[name,sql]))
assert.equal(cases.length,2048)
assert(cases.length+preparedPrograms.length+2<=2500,'fixed program bound')
const exclusions={grid:'Complete128x128 ASCII function relationships under eight exact profiles and two declared domains. Original ANSI bytes recode through the captured database domain; no directly stored malformed UTF8 evidence.',matching:'Two independently evaluated nonASCII sentinels and same-evaluation raw text ROW bytes; no ordinary equality, name-based folding or presumed relation axioms. Unknown beyond ASCII and all841 malformed-variable outputs.',prepared:'Original VARCHAR/NVARCHAR512 declarations; full alphabet, varied case/control/space, typedNULL, genuine9828 length mismatch, original repeat; actual emitted typed payload bytes retained.'}

const errorFields = message

const columnDescriptor = x => {
  const { colName,type,userType,dataLength,precision,scale,flags,collation,...details }=x
  return canonical({ name:colName,type:type.name,userType,length:dataLength??null,precision:precision??null,scale:scale??null,flags,collation:collation??null,...details })
}
// SQL Server includes its hostname in diagnostics. Pin this explicit input,
// while the lifecycle helper retains independent random names/ports/databases.
const referenceHostname='msduck-translate-ascii-reference'
const exec=promisify(execFile)
async function ownedDocker(args,env) {
  // Cleanup remains available after the watchdog expires.
  if(args[0]!=='rm' && args[0]!=='ps') checkDeadline()
  const command=args[0]==='run'?[args[0],'--hostname',referenceHostname,'--memory','4g','--cpus','2','--pids-limit','512',...args.slice(1)]:args
  return (await exec('docker',command,{env:{...process.env,...env},maxBuffer:4*1024*1024,timeout:60000})).stdout.trim()
}
async function observed(connection, sql, parameters) {
  return new Promise(resolveDone => {
    const result={sets:[],done:[],errors:[],info:[],returnStatus:null,tokens:[]}
    const request=new Request(sql,(error,rowCount)=>{
      connection.off('errorMessage',onError)
      connection.off('infoMessage',onInfo)
      result.rowCount=rowCount
      if (error && !result.errors.length) result.errors.push({message:error.message,number:error.number??null})
      resolveDone(canonical(result))
    })
    const onError=error=>result.errors.push(errorFields(error))
    const onInfo=info=>result.info.push(errorFields(info))
    connection.on('errorMessage',onError)
    connection.on('infoMessage',onInfo)
    taps.set(request,result.tokens)
    request.on('columnMetadata',columns=>result.sets.push({columns:columns.map(columnDescriptor),rows:[]}))
    request.on('row',row=>result.sets.at(-1).rows.push(row.map(x=>x.value)))
    for (const kind of ['done','doneInProc','doneProc']) request.on(kind,(rowCount,more)=>result.done.push({kind,rowCount:rowCount??null,more}))
    request.on('doneProc',(_count,_more,status)=>{result.returnStatus=status})
    if (!parameters) connection.execSqlBatch(request)
    else {
      for (const [name,type,value,options] of parameters) request.addParameter(name,type,value,options)
      connection.execSql(request)
    }
  })
}

// Prepare once, execute each value set on the same handle, then unprepare.
async function prepared(connection, sql, declarations, valueSets) {
  let result
  let complete = () => {}
  const fresh = () => ({ sets: [], done: [], errors: [], info: [], returnStatus: null, tokens: [] })
  const request = new Request(sql, (...args) => complete(...args))
  for (const [name, type, options] of declarations) request.addParameter(name, type, undefined, options)
  const onError = e => result?.errors.push(errorFields(e))
  const onInfo = e => result?.info.push(errorFields(e))
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(columnDescriptor), rows: [] }))
  request.on('row', row => result.sets.at(-1).rows.push(row.map(x => x.value)))
  for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => result?.done.push({ kind, rowCount: rowCount ?? null, more }))
  request.on('doneProc', (_count, _more, status) => { if (result) result.returnStatus = status })
  const phase = start => new Promise(resolveDone => {
    checkDeadline()
    result = fresh()
    taps.set(request, result.tokens)
    complete = (error, rowCount) => {
      result.rowCount = rowCount
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      resolveDone(result)
    }
    start()
  })
  // Tedious reports sp_prepare completion through 'prepared'/'error', not the request callback.
  const preparePhase = () => new Promise(resolveDone => {
    checkDeadline()
    result = fresh()
    taps.set(request, result.tokens)
    const finish = error => {
      request.off('prepared', onPrepared)
      request.off('error', onFailed)
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      result.prepared = !error
      resolveDone(result)
    }
    const onPrepared = () => finish()
    const onFailed = error => finish(error)
    request.once('prepared', onPrepared)
    request.once('error', onFailed)
    connection.prepare(request)
  })
  try {
    const prepare = await preparePhase()
    const executions = []
    for (const values of valueSets) {
      request.error = undefined
      outgoingCapture=[]
      try {
        const result=await phase(() => connection.execute(request,values))
        executions.push({values,wire:outgoingCapture,result})
      } finally { outgoingCapture=null }
    }
    // Tedious keeps a failed execution's error on the request; do not report it for sp_unprepare.
    request.error = undefined
    const unprepare = await phase(() => connection.unprepare(request))
    return canonical({ prepare, executions, unprepare })
  } finally {
    result = undefined
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    checkDeadline()
    if (process.env.TRANSLATE_ASCII_MATCHING_TRACE) console.error(name)
    const result = canonical(await observed(connection, sql, parameters))
    records.push({
      name, sql, ...(provenance.has(name)?{input:provenance.get(name)}:{}),
      ...(parameters ? { parameters: parameters.map(([parameter, type, value, options]) => ({
        name: parameter, type: type.name, value: canonical(value), wire:wireEvidence(type,value,options), ...(options ? { options } : {}),
      })) } : {}),
      result,
    })
  }
  await record('server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version,CAST(SERVERPROPERTY('Collation') AS NVARCHAR(128)) AS server_collation,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
  for (const [name, sql] of cases) {
    await record(name,sql)
    checkDeadline()
    records.at(-1).reuse=canonical(await observed(connection,'SELECT @@TRANCOUNT AS transaction_count,1 AS reusable'))
  }
  for (const [name, sql, parameters] of rpcPrograms) await record(name, sql, parameters)
  for (const [name, sql, declarations, valueSets] of preparedPrograms) {
    const context=canonical(await observed(connection,contextSql(provenance.get(name).language)))
    checkDeadline()
    if (process.env.TRANSLATE_ASCII_MATCHING_TRACE) console.error(name)
    records.push({
      name, sql, input:provenance.get(name),contextSql:contextSql(provenance.get(name).language),context,
      bindingCollation:canonical(connection.databaseCollation),
      declarations: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, typeInfo:wireEvidence(type,null,options,connection.databaseCollation).typeInfo, ...(options ? { options } : {}) })),
      bindingWire:valueSets.map(values=>declarations.map(([name,type,options])=>({name,...wireEvidence(type,values[name],options,connection.databaseCollation)}))),
      prepared: await prepared(connection, sql, declarations, valueSets),
    })
    records.at(-1).reuse=canonical(await observed(connection,'SELECT @@TRANCOUNT AS transaction_count,1 AS reusable'))
  }
  await record('connection reusable', 'SELECT @@TRANCOUNT AS transaction_count,1 AS reusable')
  return records
}

function nativeUnits(bytes,column) {
  return units(column.type==='NVarChar'?bytes.toString('utf16le'):require('iconv-lite').decode(bytes,column.collation.codepage))
}
function measuredRow(phase) {
  assert.equal(phase.sets.length,1,'single complete result')
  const set=phase.sets[0];assert.equal(set.rows.length,1,'single measured row')
  const token=phase.tokens.find(x=>['ROW','NBCROW'].includes(x.token));assert(token,'actual raw row retained')
  return {set,row:set.rows[0],cells:rowPayloads(token,set.columns)}
}
function gridRelation(record) {
  const {set,row,cells}=measuredRow(record.result)
  assert.equal(record.result.errors.length,0,'ASCII program must succeed')
  assert.equal(set.columns.length,8,'native and independent text/binary controls')
  const unicode=record.input.declaration.startsWith('NVARCHAR')
  const expectedSource=Buffer.from(unicode?unitHex(alphabet):Buffer.from(alphabet).toString('hex'),'hex')
  assert.equal(cells[0].toString('hex'),expectedSource.toString('hex'),'original complete ASCII source')
  assert.equal(cells[1].toString('hex'),unicode?unitHex([record.input.mapping]):Buffer.from([record.input.mapping]).toString('hex'),'original mapping')
  const relations=[]
  for(const [markerIndex,textIndex,binaryIndex] of [[2,4,5],[3,6,7]]) {
    const descriptor=set.columns[textIndex]
    assert.equal(descriptor.type,unicode?'NVarChar':'VarChar','original declared text domain')
    const marker=nativeUnits(cells[markerIndex],descriptor)
    assertSameCapture(marker,[sentinels[markerIndex-2]],'representable single nonASCII sentinel')
    assert(marker[0]>=128,'sentinel outside source alphabet')
    const output=nativeUnits(cells[textIndex],descriptor)
    assert.equal(output.length,128,'complete128-character function output')
    assert.equal(cells[binaryIndex].toString('hex'),cells[textIndex].toString('hex'),'independent binary evaluation agrees on ASCII controls')
    const relation=output.map((unit,index)=>{assert(unit===index||unit===marker[0],'unexpected output remains unknown');return unit===marker[0]})
    relations.push(relation)
  }
  assertSameCapture(relations[0],relations[1],'independent sentinel relationship agreement')
  return relations[0]
}
function relationReport(run) {
  return collations.flatMap(collation=>domains.map(declaration=>{
    const records=run.filter(r=>r.input?.kind==='ASCII grid'&&r.input.collation===collation&&r.input.declaration===declaration)
    assert.equal(records.length,128,'complete mapping inventory')
    const matrix=records.map(gridRelation)
    const failures=Object.fromEntries(['reflexive','symmetric','transitive'].map(kind=>[kind,{count:0,witnesses:[]}]))
    const fail=(kind,witness)=>{const failure=failures[kind];failure.count++;if(failure.witnesses.length<16)failure.witnesses.push(witness)}
    for(let a=0;a<128;a++) {
      if(!matrix[a][a])fail('reflexive',a)
      for(let b=0;b<128;b++) {
        if(matrix[a][b]!==matrix[b][a])fail('symmetric',[a,b])
        if(matrix[a][b])for(let c=0;c<128;c++)if(matrix[b][c]&&!matrix[a][c])fail('transitive',[a,b,c])
      }
    }
    const rowsHex=matrix.map(row=>{const bytes=Buffer.alloc(16);row.forEach((matched,index)=>{if(matched)bytes[index>>3]|=1<<(index&7)});return bytes.toString('hex')})
    return {collation,declaration,alphabetSize:128,rowAxis:'mapping ASCII code',bitAxis:'source ASCII code; least significant bit first within each byte',rowsHex,failures,classification:Object.values(failures).every(x=>x.count===0)?'observed equivalence within ASCII only':'inconsistent observed relation; unknown'}
  }))
}
function decodeRelation(report) {
  assert.equal(report.alphabetSize,128,'relation alphabet size')
  assert.equal(report.rowAxis,'mapping ASCII code','explicit relation row orientation')
  assert.equal(report.bitAxis,'source ASCII code; least significant bit first within each byte','explicit relation bit orientation')
  assert.equal(report.rowsHex.length,128,'complete packed relation rows')
  return report.rowsHex.map(hex=>{assert(typeof hex==='string'&&/^[0-9a-f]{32}$/.test(hex),'exact128-bit relation row');const bytes=Buffer.from(hex,'hex');return alphabet.map(index=>Boolean(bytes[index>>3]&(1<<(index&7))))})
}
function validateRelationReports(reports,run) {
  assert.equal(reports.length,16,'complete profile relation reports')
  for(const [index,report]of reports.entries()) {
    const collation=collations[Math.floor(index/2)],declaration=domains[index%2]
    assert.equal(report.collation,collation);assert.equal(report.declaration,declaration)
    const records=run.filter(r=>r.input?.kind==='ASCII grid'&&r.input.collation===collation&&r.input.declaration===declaration)
    const decoded=decodeRelation(report)
    for(const [mapping,record]of records.entries())assertSameCapture(decoded[mapping],gridRelation(record),'independently decoded packed bits against raw function observations')
  }
}
function validate(run) {
  assert.equal(run.length,2+cases.length+preparedPrograms.length)
  assertSameCapture(run.map(x=>x.name),['server version',...cases.map(x=>x[0]),...preparedPrograms.map(x=>x[0]),'connection reusable'])
  for(const record of run)if(provenance.has(record.name)) {
    assert.equal(record.sql,programSql.get(record.name),record.name+': exact generated SQL input')
    assertSameCapture(record.input,canonical(provenance.get(record.name)),record.name+': exact input')
    assert(record.reuse,record.name+': reuse retained');assert.equal(record.reuse.errors.length,0);assertSameCapture(record.reuse.sets[0].rows,[[0,1]])
    if(!record.prepared)gridRelation(record)
    else {
      assert.equal(record.prepared.prepare.prepared,true)
      assert.equal(record.prepared.executions.length,record.input.bindings.length)
      assertSameCapture(record.declarations.map(d=>({name:d.name,type:d.type,length:d.options?.length})),['s','m','t','u'].map(name=>({name,type:record.input.declaration.startsWith('NVARCHAR')?'NVarChar':'VarChar',length:512})),'original prepared declarations')
      assert.equal(record.context.errors.length,0)
      for(const [index,execution] of record.prepared.executions.entries()) {
        assertSameCapture(execution.values,record.input.bindings[index],'original prepared binding')
        assertSameCapture(execution.result.sets.map(x=>x.columns),record.prepared.prepare.sets.map(x=>x.columns),record.name+': unchanged prepared descriptors')
        assertSameCapture(execution.wire,record.bindingWire[index].map(({name,...wire})=>({type:record.declarations.find(d=>d.name===name).type,...wire})),record.name+': actual emitted bytes')
        if(index===4){assert(execution.result.errors.length>0,'genuine mismatch diagnostic retained');assert(execution.result.errors.every(e=>e.number===9828&&e.state===(record.input.declaration.startsWith('NVARCHAR')?3:1)&&e.class===16),'exact mismatch diagnostics')}
        else {
          assert.equal(execution.result.errors.length,0);const measured=measuredRow(execution.result)
          if(index===3){assert.equal(measured.cells[0],null);for(const position of [4,5,6,7])assert.equal(measured.row[position],null)}
          else assertSameCapture(nativeUnits(measured.cells[0],measured.set.columns[4]),units(execution.values.s),'native prepared source preserves original ASCII including trailing space/NUL')
          for(const [position,name]of [[1,'m'],[2,'t'],[3,'u']])assertSameCapture(nativeUnits(measured.cells[position],measured.set.columns[4]),units(execution.values[name]),'original prepared mapping/sentinels after declared-domain conversion')
        }
      }
      assertSameCapture(record.prepared.executions[0].result,record.prepared.executions.at(-1).result,'unchanged prepared replay')
    }
  }
}
const retainedDigests=[{"id":"container0/database0","sha256":"a7bd99b345709a93ab0e8540c70347c5c93cef8949fbde8973e8794db817a7b3"},{"id":"container0/database1","sha256":"a7bd99b345709a93ab0e8540c70347c5c93cef8949fbde8973e8794db817a7b3"},{"id":"container1/database0","sha256":"a7bd99b345709a93ab0e8540c70347c5c93cef8949fbde8973e8794db817a7b3"},{"id":"container1/database1","sha256":"a7bd99b345709a93ab0e8540c70347c5c93cef8949fbde8973e8794db817a7b3"}]
function digestRun(run){return createHash('sha256').update(JSON.stringify(run)).digest('hex')}
function runDigests(actual){return actual.containers.flatMap((c,ci)=>c.runs.map((run,ri)=>({id:`container${ci}/database${ri}`,sha256:digestRun(run)})))}
function variablePath(record,segments) {
  if(record.input?.kind!=='ASCII grid'||segments[0]!=='result')return false
  const rest=segments.slice(1)
  if(rest[0]==='sets'&&rest[1]==='0'&&rest[2]==='rows'&&rest[3]==='0'&&[4,5,6,7].includes(Number(rest[4])))return rest.length===5||(rest.length===6&&rest[5]==='value')
  if(rest[0]==='tokens'&&rest[2]==='raw'&&rest[3]==='hex'&&rest.length===4)return ['ROW','NBCROW'].includes(record.result.tokens[Number(rest[1])]?.token)
  return false
}
function exactDifferences(left,right,path=[]) {
  if(Object.is(left,right))return []
  if(left&&right&&typeof left==='object'&&typeof right==='object'&&Array.isArray(left)===Array.isArray(right)) {
    return [...new Set([...Object.keys(left),...Object.keys(right)])].flatMap(key=>exactDifferences(left[key],right[key],[...path,key]))
  }
  return [{segments:path,left:canonical(left),right:canonical(right)}]
}
function compareRuns(left,right,leftId,rightId) {
  return exactDifferences(left,right).map(({segments,left:before,right:after})=>{
    const record=left[Number(segments[0])]
    const measured=variablePath(record,segments.slice(1))
    return {leftRun:leftId,rightRun:rightId,record:record.name,path:'/'+segments.join('/'),left:before,right:after,classification:measured?'measured ASCII matching/output variability; unknown':'unexpected stable-field difference'}
  })
}
function variationReport(actual) {
  const runs=actual.containers.flatMap(c=>c.runs),ids=runDigests(actual).map(x=>x.id)
  return runs.slice(1).flatMap((run,index)=>compareRuns(runs[0],run,ids[0],ids[index+1]))
}
function compareCaptures(retained,actual) {
  return retained.containers.flatMap((container,ci)=>container.runs.flatMap((run,ri)=>compareRuns(run,actual.containers[ci].runs[ri],`retained/container${ci}/database${ri}`,`reproduction/container${ci}/database${ri}`)))
}

function firstDifference(left, right) {
  if (left.length !== right.length) return 'record count ' + left.length + ' vs ' + right.length
  const index = left.findIndex((record, i) => !isDeepStrictEqual(record, right[i]))
  return index < 0 ? null : 'record ' + index + ' (' + left[index].name + ')'
}

function requireSame(left, right, label) {
  const difference = firstDifference(left, right)
  if (difference) throw new Error('CONCAT_WS/TRANSLATE observations differ ' + label + ' at ' + difference)
}

function validateTokens(run) {
  for (const record of run) {
    const phases = record.prepared
      ? [record.context,record.prepared.prepare, ...record.prepared.executions.map(x => x.result), record.prepared.unprepare,record.reuse]
      : [record.result,...(record.reuse?[record.reuse]:[])]
    if (record.prepared && (!Array.isArray(record.prepared.prepare.tokens) ||
        record.prepared.prepare.tokens.filter(token => token.token === 'RETURNVALUE').length !== 1)) {
      throw new Error(record.name + ': expected exactly one prepare handle token')
    }
    for (const phase of phases) {
      assert(phase.tokens.length<=1024,record.name+': token bound')
      assert(Buffer.byteLength(JSON.stringify(phase))<=phaseByteLimit,record.name+': phase byte bound')
      if((record.prepared && (phase===record.prepared.prepare || phase===record.prepared.unprepare)) || record.name==='server version' || record.name==='connection reusable' || phase===record.reuse) assert.equal(phase.errors.length,0,record.name+': successful control')
      if(record.parameters || (record.prepared && (phase===record.prepared.prepare || phase===record.prepared.unprepare))) assert.equal(phase.returnStatus,0,record.name+': control RPC status')
      if (record.parameters || (record.prepared && phase!==record.context && phase!==record.reuse)) {
        const statuses = phase.tokens?.filter(token => token.token === 'RETURNSTATUS')
        if (statuses?.length !== 1 || statuses[0].value !== phase.returnStatus) {
          throw new Error(record.name + ': missing or inconsistent raw RETURNSTATUS token')
        }
      }
      if (!Array.isArray(phase.tokens) || !phase.tokens.length || !/^DONE(PROC)?$/.test(phase.tokens.at(-1).token)) {
        throw new Error(record.name + ': missing complete ordered token evidence')
      }
      // Cross-check independent public events against the ordered handler tap.
      // Four matching copies alone cannot detect identically missing tokens.
      let setIndex = 0, doneIndex = 0, errorIndex = 0, infoIndex = 0
      let activeSet = null, rows = 0
      const finishSet = () => {
        if (activeSet !== null) {
          assert.equal(rows, activeSet.rows.length, record.name + ': ordered row count')
          activeSet = null
        }
      }
      for (const token of phase.tokens) {
        if (token.token === 'COLMETADATA') {
          finishSet()
          activeSet = phase.sets[setIndex++]
          assert(activeSet, record.name + ': unexpected COLMETADATA')
          assertSameCapture(token.columns, activeSet.columns.map(column => column.name), record.name + ': ordered column names')
          assertSameCapture(token.descriptors,activeSet.columns,record.name+': complete ordered descriptors')
          for (const column of activeSet.columns) {
            assert(Number.isInteger(column.userType) && column.userType>=0 && column.userType<=0xffffffff,record.name+': userType')
            for (const field of ['schema','udtInfo','tableName']) assert(Object.hasOwn(column,field),record.name+': missing descriptor '+field)
            if (['Text','NText','Image'].includes(column.type)) assert(Array.isArray(column.tableName) && column.tableName.every(x=>typeof x==='string'),record.name+': legacy LOB tableName')
          }
          rows = 0
        } else if (token.token === 'ROW' || token.token === 'NBCROW') {
          assert(activeSet, record.name + ': row outside result metadata')
          assert(rows<activeSet.rows.length,record.name+': extra ordered row')
          reconcileRow(token,activeSet.columns,activeSet.rows[rows])
          rows++
        } else if (token.token === 'ERROR' || token.token === 'INFO') {
          const captured = token.token === 'ERROR' ? phase.errors[errorIndex++] : phase.info[infoIndex++]
          assert(captured, record.name + ': unexpected ordered message')
          assert.equal(typeof captured.serverName,'string',record.name+': missing diagnostic serverName')
          assert.equal(typeof captured.procName,'string',record.name+': missing diagnostic procName')
          for (const key of Object.keys(captured)) assertSameCapture(token[key], captured[key], record.name + ': ordered message ' + key)
        } else if (/^DONE(INPROC|PROC)?$/.test(token.token)) {
          finishSet()
          const captured = phase.done[doneIndex++]
          assert(captured, record.name + ': unexpected ordered completion')
          const kind = { DONE: 'done', DONEINPROC: 'doneInProc', DONEPROC: 'doneProc' }[token.token]
          assert.equal(kind, captured.kind, record.name + ': completion kind')
          assert.equal(Boolean(token.status & 1), captured.more, record.name + ': completion MORE')
          assert.equal(token.status & 16 ? Number(token.count) : null, captured.rowCount, record.name + ': completion row count')
        }
      }
      finishSet()
      assert.equal(setIndex, phase.sets.length, record.name + ': missing COLMETADATA')
      assert.equal(doneIndex, phase.done.length, record.name + ': missing completion')
      assert.equal(errorIndex, phase.errors.length, record.name + ': missing ERROR')
      assert.equal(infoIndex, phase.info.length, record.name + ': missing INFO')
      for (const token of phase.tokens) {
        if (/^DONE(INPROC|PROC)?$/.test(token.token)) {
          if (!Number.isInteger(token.status) || token.status < 0 || token.status > 65535 ||
              !Number.isInteger(token.command) || token.command < 0 || token.command > 65535 ||
              typeof token.count !== 'string' || !/^[0-9]+$/.test(token.count) || BigInt(token.count) > 0xffffffffffffffffn) {
            throw new Error(record.name + ': invalid raw DONE fields')
          }
        }
        if (token.token === 'RETURNVALUE') {
          if (token.raw?.name !== '@handle' || token.raw?.status !== 1 || token.raw?.ordinal !== token.paramOrdinal ||
              token.metadata?.type !== 'IntN' || token.metadata.dataLength !== 4 || token.metadata.flags !== 0 ||
              typeof token.raw.hex !== 'string' || token.raw.hex.length > 2 * 1024 * 1024 || !/^(?:[0-9a-f]{2})+$/.test(token.raw.hex)) {
            throw new Error(record.name + ': incomplete prepare handle raw fields')
          }
          const bytes = Buffer.from(token.raw.hex, 'hex')
          const header = 4 + 2 * '@handle'.length
          if (bytes.length !== header + 13 || bytes.readUInt16LE(0) !== token.paramOrdinal ||
              bytes.readUInt8(2) !== '@handle'.length || bytes.toString('utf16le', 3, header - 1) !== '@handle' ||
              bytes.readUInt8(header - 1) !== 1 || bytes.readUInt32LE(header) !== token.metadata.userType ||
              bytes.readUInt16LE(header + 4) !== token.metadata.flags || bytes[header + 6] !== 0x26 ||
              bytes[header + 7] !== 4 || bytes[header + 8] !== 4 || bytes.readInt32LE(header + 9) !== token.value) {
            throw new Error(record.name + ': prepare handle bytes disagree with decoded fields')
          }
        }
      }
    }
  }
}

function validateFourCaptures(actual,retained=false) {
  assert.equal(actual.hostname,referenceHostname,'explicit reference hostname')
  assertSameCapture(actual.exclusions,exclusions,'explicit capture exclusions')
  const acquired={hostname:actual.hostname,exclusions:actual.exclusions,containers:actual.containers}
  assert.equal(actual.acquiredSha256,createHash('sha256').update(JSON.stringify(acquired)+'\n').digest('hex'),'complete acquired raw integrity')
  assert.equal(actual.containers.length,2,'two containers')
  for(const container of actual.containers) {
    assert.equal(container.image,referenceImage)
    assert.equal(container.runs.length,2,'two fresh databases')
    for(const [runIndex,run]of container.runs.entries()){validate(run);validateTokens(run);validateRelationReports(actual.relations[actual.containers.indexOf(container)][runIndex],run)}
  }
  assertSameCapture(actual.runDigests,runDigests(actual),'exact per-run integrity')
  assertSameCapture(actual.variationReport,variationReport(actual),'complete unnormalized variation report')
  assertSameCapture(actual.relations,actual.containers.map(c=>c.runs.map(relationReport)),'complete observed relation reports')
  assert(actual.variationReport.every(x=>x.classification==='measured ASCII matching/output variability; unknown'),'unexpected stable-field variability; raw capture retained')
  if(retained && retainedDigests.length)assertSameCapture(actual.runDigests,retainedDigests,'immutable retained run contracts')
}

async function persistAcquiredCapture(actual,path) {
  const raw=JSON.stringify(actual)+'\n'
  assert(Buffer.byteLength(raw)<=captureByteLimit,'acquired capture byte bound')
  await writeFile(path,raw,{flag:'wx'})
  return createHash('sha256').update(raw).digest('hex')
}
async function retainAndDerive(actual,output,oneDatabase) {
  actual.acquiredSha256=await persistAcquiredCapture(actual,output+'.raw.json')
  if(!oneDatabase){actual.runDigests=runDigests(actual);actual.variationReport=variationReport(actual);actual.relations=actual.containers.map(c=>c.runs.map(relationReport))}
}
async function testRawRetention(retained) {
  const root=resolve('.tmp');await mkdir(root,{recursive:true})
  const temporary=await mkdtemp(resolve(root,'translate-ascii-invalid-'))
  try {
    const run=structuredClone(retained.containers[0].runs[0]);const invalid=run[1]
    invalid.result.errors.push({number:529,state:1,class:16,message:'retained synthetic server failure'})
    const acquired={containers:[{runs:[run]}]}
    const output=resolve(temporary,'acquired.json'),path=output+'.raw.json'
    await assert.rejects(retainAndDerive(acquired,output,false),/ASCII program must succeed/)
    const hash=acquired.acquiredSha256
    assert.equal(createHash('sha256').update(await readFile(path)).digest('hex'),hash,'failed relation leaves raw bytes intact')
    assert.equal(JSON.parse(await readFile(path,'utf8')).containers[0].runs[0][1].result.errors.at(-1).number,529,'original invalid diagnostic retained')
    await assert.rejects(persistAcquiredCapture(acquired,path),error=>error.code==='EEXIST','raw evidence is exclusive')
    console.log('Invalid relation retains original acquired raw observations and exclusive destination')
  } finally {await rm(temporary,{recursive:true,force:true})}
}

async function readRetained() {
  try {
    assert((await stat(fixture)).size<=captureByteLimit,'retained fixture byte bound')
    return JSON.parse(await readFile(fixture, 'utf8'))
  }
  catch (error) { if (error.code !== 'ENOENT') throw error }
}

async function testObserver(retained) {
  const run=retained.containers[0].runs[0]
  const handle=run.find(record=>record.prepared).prepared.prepare.tokens.find(token=>token.token==='RETURNVALUE')
  const bytes=Buffer.concat([Buffer.from([0xac]),Buffer.from(handle.raw.hex,'hex'),Buffer.from('fd000000000000000000000000','hex')])
  async function parse(chunks) {
    const tokens=[]
    for await (const token of StreamParser.parseTokens(chunks,{token() {}},{tdsVersion:'7_4',useUTC:true})) tokens.push(token)
    assert.equal(tokens.length,2)
    assertSameCapture(tokens[0].raw,handle.raw)
    assert.equal(tokens[0].value,handle.value)
    assert.equal(tokens[1].name,'DONE')
  }
  await parse([bytes])
  for (let split=1;split<bytes.length;split++) await parse([bytes.subarray(0,split),bytes.subarray(split)])
  await parse(Array.from(bytes,byte=>Buffer.from([byte])))
  await assert.rejects(parse([bytes.subarray(0,10)]),/unexpected end of data/)
  const corruptions=[
    r=>r[0].result.sets[0].rows[0][0]='corrupted',
    r=>r[0].result.sets[0].columns[0].length=7,
    r=>delete r[0].result.sets[0].columns[0].userType,
    r=>r[0].result.tokens.splice(0,1),
    r=>r[0].result.tokens.find(t=>/^DONE/.test(t.token)).count='999',
    r=>r.find(x=>x.prepared).prepared.prepare.tokens.find(t=>t.token==='RETURNVALUE').raw.hex='00',
    r=>r.find(x=>x.prepared).prepared.executions[4].result.errors[0].message='corrupted',
    r=>r.find(x=>x.prepared).prepared.executions[4].result.errors[0].number=999,
    r=>r.find(x=>x.prepared).prepared.executions[4].result.errors[0].serverName='corrupted',
    r=>r.find(x=>x.input?.kind==='ASCII grid').input.declaration='VARCHAR(1)',
    r=>delete r[1].reuse,
    r=>r[1].reuse.sets[0].rows[0][0]=1,
    r=>r.find(x=>x.input?.kind==='ASCII grid').result.sets[0].rows[0][0]={kind:'binary',value:'00'},
    r=>r.find(x=>x.prepared).bindingWire[0][0].payload='00',
    r=>r.find(x=>x.prepared).prepared.executions[0].result.returnStatus=99,
    r=>r[1].input.mapping=127,
    r=>r[1].sql='SELECT 1',
    r=>r.find(x=>x.prepared).declarations[0].options.length=1,
    r=>r[1].result.sets[0].rows[0][2]={kind:'binary',value:'58'},
    r=>r[1].result.tokens.find(t=>t.token==='ROW').raw.hex='00',
  ]
  for (const mutate of corruptions) {
    const actual=structuredClone(retained)
    for (const container of actual.containers) for (const copy of container.runs) mutate(copy)
    actual.runDigests=runDigests(actual)
    actual.acquiredSha256=createHash('sha256').update(JSON.stringify({hostname:actual.hostname,exclusions:actual.exclusions,containers:actual.containers})+'\n').digest('hex')
    assert.throws(()=>validateFourCaptures(actual,false),'identical four-copy corruption must fail after refreshing integrity fields')
  }
  for(const mutate of [r=>r.rowsHex[0]='00'.repeat(16),r=>r.rowsHex[0]='00',r=>r.bitAxis='most significant bit first',r=>r.rowAxis='source ASCII code',r=>r.failures.reflexive.count=1]) {
    const actual=structuredClone(retained)
    for(const container of actual.relations)for(const reports of container)mutate(reports[0])
    assert.throws(()=>validateFourCaptures(actual,false),'packed relation corruption rejected independently of raw/run digest pins')
  }
  console.log('All RETURNVALUE fragments pass; twenty refreshed-integrity capture corruptions and five packed-relation corruptions rejected')
}
async function testCliSafety() {
  const root=resolve('.tmp')
  await mkdir(root,{recursive:true})
  const temporary=await mkdtemp(resolve(root,'translate-matching-cli-'))
  const marker=resolve(temporary,'docker-called')
  const fakeDocker=resolve(temporary,'docker')
  await writeFile(fakeDocker,'#!/bin/sh\nprintf called > "$MSDUCK_TRANSLATE_ASCII_MATCHING_CLI_MARKER"\nexit 99\n',{flag:'wx'})
  await chmod(fakeDocker,0o700)
  const retainedPath=fileURLToPath(fixture)
  const before=createHash('sha256').update(await readFile(retainedPath)).digest('hex')
  const existing=resolve(temporary,'existing.json')
  const alias=resolve(temporary,'hardlink.json')
  const symbolic=resolve(temporary,'symbolic.json')
  const parent=resolve(temporary,'reference-alias')
  await writeFile(existing,'owned pre-existing output\n',{flag:'wx'})
  await link(retainedPath,alias)
  await symlink(retainedPath,symbolic)
  await symlink(dirname(retainedPath),parent)
  const probes=[
    [retainedPath],[existing],[alias],[symbolic],[resolve(parent,basename(retainedPath))],
    [resolve(temporary,'new.json'),'--write-fixture'],['--unknown'],['--one-database','--write-fixture'],['--check-fixture','extra'],
  ]
  try {
    for(const args of probes) {
      let failed=false
      try { await exec(process.execPath,[fileURLToPath(import.meta.url),...args],{env:{...process.env,PATH:temporary+':'+process.env.PATH,MSDUCK_TRANSLATE_ASCII_MATCHING_CLI_MARKER:marker},maxBuffer:1024*1024,timeout:10000}) }
      catch(error) { failed=error.code===1 }
      assert(failed,'CLI negative must reject before Docker')
      assert(!existsSync(marker),'CLI refusal must precede Docker')
    }
    assert.equal(await readFile(existing,'utf8'),'owned pre-existing output\n','existing output untouched')
    assert.equal(createHash('sha256').update(await readFile(retainedPath)).digest('hex'),before,'retained fixture untouched')
    console.log('Nine destination/CLI negatives rejected before Docker; fixture unchanged')
  } finally { await rm(temporary,{recursive:true,force:true}) }
}
async function testLifecycleSafety() {
  for(const scenario of ['success','work failure','readiness failure','partial start failure','auto-remove race']) {
    const calls=[]
    let ownedName, closed=0
    const docker=async(args,environment)=>{
      calls.push([...args])
      if(args[0]==='run') {
        ownedName=args[args.indexOf('--name')+1]
        assert(/^msduck-reference-[a-f0-9-]+$/.test(ownedName),'fresh owned lifecycle name')
        assert(args.includes('msduck.reference=1'),'reference ownership label')
        assert(args.includes(`msduck.owner=${process.cwd()}:${process.pid}`),'worker ownership label')
        assert.equal(args.at(-1),referenceImage,'pinned lifecycle image')
        assert(!args.includes(environment.MSSQL_SA_PASSWORD),'credential must not enter arguments')
        if(scenario==='partial start failure') throw new Error('test partial start failure')
        return 'owned-test-container'
      }
      if(args[0]==='port') { assert.equal(args[1],ownedName);return '127.0.0.1:14333' }
      if(args[0]==='inspect') { assert.equal(args.at(-1),ownedName);return scenario==='readiness failure'?'false':'true' }
      if(args[0]==='rm') {
        assertSameCapture(args,['rm','--force',ownedName],'cleanup must target only owned name')
        if(scenario==='auto-remove race') throw new Error('already removed')
        return ''
      }
      if(args[0]==='ps') {
        assert(args.includes(`name=^/${ownedName}$`),'cleanup verification must name only owned container')
        return ''
      }
      throw new Error('unexpected lifecycle operation')
    }
    const action=()=>withReferenceContainer(async(config,container)=>{
      assert.equal(config.options.port,14333)
      assert.equal(container.name,ownedName)
      if(scenario==='work failure') throw new Error('test work failure')
      return 'complete'
    },{docker,image:referenceImage,connect:async()=>({close(){closed++}}),delay:async()=>{},timeout:10})
    if(scenario==='work failure'||scenario==='partial start failure') await assert.rejects(action,/test .* failure/)
    else if(scenario==='readiness failure') await assert.rejects(action,/exited before login readiness/)
    else assert.equal(await action(),'complete')
    assert(calls.some(args=>args[0]==='rm'&&args.at(-1)===ownedName),'all lifecycle paths clean up their owned name')
    assert.equal(closed,scenario==='partial start failure'||scenario==='readiness failure'?0:1,'readiness connection closed')
  }
  console.log('Five owned lifecycle success/failure/partial-start/auto-remove cases pass without Docker')
}
if (selfTest) {
  const retained=await readRetained()
  if (!retained) throw new Error('no retained fixture')
  validateFourCaptures(retained,true)
  await testRawRows()
  await testRawRetention(retained)
  await testObserver(retained)
  await testCliSafety()
  await testLifecycleSafety()
} else if (checkFixture) {
  const retained = await readRetained()
  if (!retained) throw new Error('no retained fixture')
  validateFourCaptures(retained,true)
  console.log('Retained fixture holds ' + retained.containers[0].runs[0].length + ' complete observations per run; '+retained.variationReport.length+' exact ASCII output differences; no exhaustive collation support claim')
} else {
  if (writeFixture && existsSync(fixture)) throw new Error('refusing to overwrite retained fixture')
  await refuseFixtureOutput(output, fileURLToPath(fixture))
  await refuseFixtureOutput(output+'.comparison.json',fileURLToPath(fixture))
  await refuseFixtureOutput(output+'.raw.json',fileURLToPath(fixture))
  await mkdir(resolve(output, '..'), { recursive: true })
  const containers = []
  for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
    await withReferenceContainer(async (config, container) => {
      const runs = []
      for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
        const run = await isolatedReference({
          ...config, options: { ...config.options, requestTimeout: 120000,useUTC:true },
        }, observe)
        runs.push(run)
      }
      containers.push({ image: container.image, runs })
    },{docker:ownedDocker,image:referenceImage})
  }
  const actual = { hostname:referenceHostname,exclusions,containers }
  // Preserve the complete acquired observations before any semantic relation
  // derivation can reject errors, sentinel behavior or independent evaluations.
  await retainAndDerive(actual,output,oneDatabase)
  // Retain the raw artifact before validation so a failing capture can be inspected.
  const serialized=JSON.stringify(actual)+'\n'
  assert(Buffer.byteLength(serialized)<=captureByteLimit,'capture byte bound')
  await writeFile(output, serialized, { flag: 'wx' })
  if (oneDatabase) { validate(containers[0].runs[0]); validateTokens(containers[0].runs[0]) }
  else validateFourCaptures(actual)
  const retained = writeFixture ? undefined : await readRetained()
  if(retained && !oneDatabase) {
    validateFourCaptures(retained,true)
    const differences=compareCaptures(retained,actual)
    await writeFile(output+'.comparison.json',JSON.stringify({retainedDigests:retained.runDigests,reproductionDigests:actual.runDigests,differences})+'\n',{flag:'wx'})
    assert(differences.every(x=>x.classification==='measured ASCII matching/output variability; unknown'),'unexpected stable-field reproduction difference; raw comparison retained')
    console.log('Reproduction stable fields unchanged; '+differences.length+' exact ASCII output differences retained without normalization')
  }
  if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
  console.log('Captured ' + containers[0].runs[0].length + ' TRANSLATE ASCII matching observations' +
    (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
    (retained && !oneDatabase ? '; complete ASCII observations compared without normalization' : '') +
    (writeFixture ? '; wrote new retained fixture' : ''))
}
