#!/usr/bin/env node
// Retain SQL Server CONCAT_WS and TRANSLATE rows, descriptors, diagnostics and completions.
// Usage: capture-concat-numeric-format.mjs [output] [--write-fixture | --one-database | --check-fixture]
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
function checkDeadline() { if (Date.now()>deadline) throw new Error('numeric capture watchdog expired') }


const fixture = new URL('../reference/concat-numeric-format.json', import.meta.url)
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
const output = resolve(positional[0] ?? 'artifacts/compatibility/concat-numeric-format/capture.json')

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
const metadata = value => ({
  type: value.type.name, userType: value.userType, flags: value.flags, dataLength: value.dataLength ?? null,
  precision: value.precision ?? null, scale: value.scale ?? null, collation: value.collation ?? null,
})
const taps = new WeakMap()
const message = token => ({ number: token.number, state: token.state, class: token.class, lineNumber: token.lineNumber, serverName: token.serverName, procName: token.procName, message: token.message })
const done = token => ({ status: token.raw.status, command: token.raw.command, count: token.raw.count })
for (const [method, describe] of Object.entries({
  onColMetadata: token => ({ columns: token.columns.map(column => column.colName), descriptors: token.columns.map(columnDescriptor) }),
  onRow: () => ({}),
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


// Original scalar text is retained in SQL and in input provenance; no JS Number
// stands in for DECIMAL/MONEY/BIGINT inputs. IEEE RPC values are constructed from
// retained little-endian bytes and checked against the tedious type encoder.
const scalarInputs=[]
function scalar(type,values) {
  for (const text of [...values,null]) scalarInputs.push({name:`${type} ${text ?? 'NULL'}`,type,text})
}
scalar('BIT',['0','1'])
scalar('TINYINT',['0','1','254','255'])
scalar('SMALLINT',['-32768','-1','0','1','32767'])
scalar('INT',['-2147483648','-1','0','1','2147483647'])
scalar('BIGINT',['-9223372036854775808','-1','0','1','9223372036854775807'])
for (const [type,values] of [
  ['DECIMAL(1,0)',['-9','0','9']],
  ['DECIMAL(8,2)',['-999999.99','-1.25','-0.00','0.00','1.20','999999.99']],
  ['DECIMAL(18,4)',['-99999999999999.9999','-1.2500','0.0000','1.2000','99999999999999.9999']],
  ['DECIMAL(38,0)',['-99999999999999999999999999999999999999','0','99999999999999999999999999999999999999']],
  ['DECIMAL(38,18)',['-99999999999999999999.999999999999999999','-0.000000000000000001','0.000000000000000000','1.200000000000000000','99999999999999999999.999999999999999999']],
  ['DECIMAL(38,38)',['-0.99999999999999999999999999999999999999','-0.00000000000000000000000000000000000001','0.00000000000000000000000000000000000000','0.99999999999999999999999999999999999999']],
]) scalar(type,values)
const moneyNeighbors=['-100.0051','-100.0050','-100.0049','-99.9951','-99.9950','-99.9949','-1.2351','-1.2350','-1.2349','-0.0051','-0.0050','-0.0049','0.0000','0.0049','0.0050','0.0051','1.2349','1.2350','1.2351','99.9949','99.9950','99.9951','100.0049','100.0050','100.0051']
scalar('MONEY',['-922337203685477.5808',...moneyNeighbors,'922337203685477.5807'])
scalar('SMALLMONEY',['-214748.3648',...moneyNeighbors,'214748.3647'])
// tedious 20.0.0 passes numbers through parseFloat, which loses -0. Preserve
// that single admitted numeric case in this process so signed-zero probes send
// the declared IEEE bytes rather than silently becoming positive zero.
for (const [type,width] of [[TYPES.Real,4],[TYPES.Float,8]]) {
  const validate=type.validate, generate=type.generateParameterData
  type.validate=function(value,...args) { return Object.is(value,-0)?-0:validate.call(this,value,...args) }
  type.generateParameterData=function*(parameter,...args) {
    if(!Object.is(parameter.value,-0)) { yield* generate.call(this,parameter,...args); return }
    const bytes=Buffer.alloc(width)
    if(width===4) bytes.writeFloatLE(-0); else bytes.writeDoubleLE(-0)
    yield bytes
  }
}
const floatInputs=[]
function ieee(kind,hex,label) {
  const bytes=Buffer.from(hex,'hex'), value=kind==='REAL'?bytes.readFloatLE():bytes.readDoubleLE()
  assert(Number.isFinite(value),'only finite IEEE input is admitted')
  const encoded=Buffer.alloc(bytes.length)
  if (kind==='REAL') encoded.writeFloatLE(value); else encoded.writeDoubleLE(value)
  assert.equal(encoded.toString('hex'),hex,'IEEE input must round-trip through JavaScript')
  const type=kind==='REAL'?TYPES.Real:TYPES.Float
  const validated=type.validate(value)
  const data=Buffer.concat([...type.generateParameterData({value:validated})]).toString('hex')
  assert.equal(data,hex,'actual tedious IEEE parameter payload')
  floatInputs.push({name:`${kind} ${label}`,kind,hex,value})
}
for (const kind of ['REAL','FLOAT']) {
  const width=kind==='REAL'?4:8
  const mask=(1n<<BigInt(width*8))-1n
  function bits(value) { const b=Buffer.alloc(width); if(width===4)b.writeFloatLE(value);else b.writeDoubleLE(value);return width===4?BigInt(b.readUInt32LE()):b.readBigUInt64LE() }
  function fromBits(value,label) { const b=Buffer.alloc(width); if(width===4)b.writeUInt32LE(Number(value));else b.writeBigUInt64LE(value);ieee(kind,b.toString('hex'),label) }
  for (const [label,raw] of width===4?[
    ['+zero',0n],['-zero',0x80000000n],['min subnormal',1n],['max subnormal',0x7fffffn],['min normal',0x800000n],['max finite',0x7f7fffffn],['negative min subnormal',0x80000001n],['negative max finite',0xff7fffffn],
  ]:[
    ['+zero',0n],['-zero',0x8000000000000000n],['min subnormal',1n],['max subnormal',0xfffffffffffffn],['min normal',0x10000000000000n],['max finite',0x7fefffffffffffffn],['negative min subnormal',0x8000000000000001n],['negative max finite',0xffefffffffffffffn],
  ]) fromBits(raw,label)
  for (const center of [1.234565,1.234575,9.999995,999999.5,0.00009999995,0.0001,0.00001,1000000]) {
    const raw=bits(center)
    for(const offset of [-1n,0n,1n]) fromBits((raw+offset)&mask,`${center} bits${offset>=0n?'+':''}${offset}`)
  }
  for(const value of [-1.234565,-999999.5,-0.0001,1,-1,0.1,1e20]) fromBits(bits(value),String(value))
  floatInputs.push({name:`${kind} NULL`,kind,hex:null,value:null})
}
const cases=[]
const rpcPrograms=[]
const provenance=new Map()
function expressions(input) {
  return [
    ['native',`${input} AS source,CONVERT(VARBINARY(MAX),${input}) AS source_storage`],
    ['explicit style0',`CONVERT(VARCHAR(MAX),${input},0) AS ansi,CONVERT(NVARCHAR(MAX),${input},0) AS unicode`],
    ['tr ansi',`TRANSLATE(${input},'','') AS value`],
    ['tr unicode',`TRANSLATE(${input},N'',N'') AS value`],
    ['cws first',`CONCAT_WS('',${input},'') AS value`],
    ['cws last',`CONCAT_WS('','',${input}) AS value`],
    ['cws separator',`CONCAT_WS(${input},'a','b') AS value`],
    ['cws unicode',`CONCAT_WS(N'',${input},N'') AS value`],
    ['cws unicode last',`CONCAT_WS(N'',N'',${input}) AS value`],
    ['cws unicode separator',`CONCAT_WS(${input},N'a',N'b') AS value`],
    ['cws null separator',`CONCAT_WS(NULL,${input},NULL) AS value`],
  ]
}
for (const source of scalarInputs) {
  const input=`CAST(${source.text===null?'NULL':`'${source.text}'`} AS ${source.type})`
  for (const [role,projection] of expressions(input)) {
    const name=`${source.name} ${role}`
    cases.push([name,`SELECT ${projection}`]); provenance.set(name,{kind:'typed scalar',declaration:source.type,scalarText:source.text,role})
  }
}
for (const source of floatInputs) for (const [role,projection] of expressions('@p')) {
  const name=`${source.name} ${role}`
  rpcPrograms.push([name,`SELECT ${projection}`,[['p',source.kind==='REAL'?TYPES.Real:TYPES.Float,source.value]]])
  provenance.set(name,{kind:'IEEE RPC',declaration:source.kind,ieeeLittleEndian:source.hex,wireBytes:source.kind==='REAL'?4:8,typeInfo:(source.kind==='REAL'?TYPES.Real:TYPES.Float).generateTypeInfo().toString('hex'),parameterLength:(source.kind==='REAL'?TYPES.Real:TYPES.Float).generateParameterLength({value:source.value}).toString('hex'),payloadHex:source.hex??'',role})
}
const preparedPrograms=[]
const additionalPreparedPrograms=[]
for(const kind of ['REAL','FLOAT']) {
  const selected=[floatInputs.find(x=>x.kind===kind&&x.name.endsWith('+zero')),floatInputs.find(x=>x.kind===kind&&x.name.endsWith('-zero')),floatInputs.find(x=>x.kind===kind&&x.name.endsWith('min subnormal')),floatInputs.find(x=>x.kind===kind&&x.name.endsWith('max finite')),floatInputs.find(x=>x.kind===kind&&x.value===null)]
  for(const [role,projection] of expressions('@p')) {
    const name=`prepared ${kind} ${role}`
    const destination=role==='cws unicode last'||role==='cws unicode separator'?additionalPreparedPrograms:preparedPrograms
    destination.push([name,`SELECT ${projection}`,[['p',kind==='REAL'?TYPES.Real:TYPES.Float]],[...selected,selected[0]].map(x=>({p:x.value}))])
    provenance.set(name,{kind:'prepared IEEE RPC',declaration:kind,role,bindings:[...selected,selected[0]].map(x=>({ieeeLittleEndian:x.hex,wireBytes:kind==='REAL'?4:8,parameterLength:(kind==='REAL'?TYPES.Real:TYPES.Float).generateParameterLength({value:x.value}).toString('hex'),payloadHex:x.hex??''}))})
  }
}
preparedPrograms.push(['prepared decimal fixed declaration',"SELECT TRANSLATE(@p,'','') AS tr,CONCAT_WS('',@p,'') AS cws",[['p',TYPES.Decimal,{precision:18,scale:4}]],[{p:1.25},{p:null},{p:-12345.5},{p:1.25}]],
 ['prepared money fixed declaration',"SELECT TRANSLATE(@p,'','') AS tr,CONCAT_WS('',@p,'') AS cws",[['p',TYPES.Money]],[{p:1.235},{p:null},{p:-1.235},{p:1.235}]],
 ['prepared bigint fixed declaration',"SELECT TRANSLATE(@p,'','') AS tr,CONCAT_WS('',@p,'') AS cws",[['p',TYPES.BigInt]],[{p:'9223372036854775807'},{p:null},{p:'-9223372036854775808'},{p:'9223372036854775807'}]])
// Append expanded contexts after the original prepared sequence, preserving all prior raw handles.
preparedPrograms.push(...additionalPreparedPrograms)
for(const program of preparedPrograms) if(!provenance.has(program[0])) provenance.set(program[0],{kind:'prepared scalar RPC',declaration:program[2].map(([name,type,options])=>({name,type:type.name,...options})),values:canonical(program[3]),note:'Money/Decimal driver values are JS numbers; exact decimal boundaries are separately retained typed scalar probes'})
function wireEvidence(type,value,options={}) {
  const parameter={...options,value:type.validate(value)}
  return {typeInfo:type.generateTypeInfo(parameter).toString('hex'),length:type.generateParameterLength(parameter).toString('hex'),payload:Buffer.concat([...type.generateParameterData(parameter)]).toString('hex')}
}
const rejectedScalars=[['TINYINT','256'],['SMALLINT','-32769'],['INT','2147483648'],['BIGINT','9223372036854775808'],['DECIMAL(1,0)','10'],['MONEY','922337203685477.5808'],['SMALLMONEY','214748.3648']]
for(const [type,text] of rejectedScalars) for(const [role,projection] of expressions(`CAST('${text}' AS ${type})`)) {
  const name=`rejected ${type} ${text} ${role}`
  cases.push([name,`SELECT ${projection}`])
  provenance.set(name,{kind:'rejected typed scalar',declaration:type,scalarText:text,role})
}
assert(cases.length+rpcPrograms.length+preparedPrograms.length<2500,'program count bound')
const errorFields = message

const columnDescriptor = x => {
  const { colName,type,userType,dataLength,precision,scale,flags,collation,...details }=x
  return canonical({ name:colName,type:type.name,userType,length:dataLength??null,precision:precision??null,scale:scale??null,flags,collation:collation??null,...details })
}
// SQL Server includes its hostname in diagnostics. Pin this explicit input,
// while the lifecycle helper retains independent random names/ports/databases.
const referenceHostname='msduck-numeric-reference'
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
      executions.push({ values, result: await phase(() => connection.execute(request, values)) })
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
    if (process.env.NUMERIC_TRACE) console.error(name)
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
  for (const [name, sql] of cases) await record(name, sql)
  for (const [name, sql, parameters] of rpcPrograms) await record(name, sql, parameters)
  for (const [name, sql, declarations, valueSets] of preparedPrograms) {
    checkDeadline()
    if (process.env.NUMERIC_TRACE) console.error(name)
    records.push({
      name, sql, input:provenance.get(name),
      declarations: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, typeInfo:wireEvidence(type,null,options).typeInfo, ...(options ? { options } : {}) })),
      bindingWire:valueSets.map(values=>declarations.map(([name,type,options])=>({name,...wireEvidence(type,values[name],options)}))),
      prepared: await prepared(connection, sql, declarations, valueSets),
    })
  }
  await record('connection reusable', 'SELECT @@TRANCOUNT AS transaction_count,1 AS reusable')
  return records
}

function validate(run) {
  assert.equal(run.length, 2 + cases.length + rpcPrograms.length + preparedPrograms.length)
  assertSameCapture(run.map(x=>x.name), ['server version',...cases.map(x=>x[0]),...rpcPrograms.map(x=>x[0]),...preparedPrograms.map(x=>x[0]),'connection reusable'])
  // Native source storage independently proves that the requested IEEE input
  // reached SQL Server. SQL VARBINARY float storage is big-endian here.
  for(const record of run) {
    if(record.input?.role!=='native') continue
    if(record.input.kind==='IEEE RPC') {
      const expected=record.input.ieeeLittleEndian===null?null:{kind:'binary',value:Buffer.from(record.input.ieeeLittleEndian,'hex').reverse().toString('hex')}
      assertSameCapture(record.result.sets[0].rows[0][1],expected,record.name+': native IEEE storage')
    } else if(record.input.kind==='prepared IEEE RPC') {
      for(let i=0;i<record.input.bindings.length;i++) {
        const hex=record.input.bindings[i].ieeeLittleEndian
        const expected=hex===null?null:{kind:'binary',value:Buffer.from(hex,'hex').reverse().toString('hex')}
        assertSameCapture(record.prepared.executions[i].result.sets[0].rows[0][1],expected,record.name+': native prepared IEEE storage')
      }
    }
  }
  for (const record of run) {
    if (provenance.has(record.name)) assertSameCapture(record.input,canonical(provenance.get(record.name)),record.name+': exact input provenance')
    if (record.prepared) {
      assert.equal(record.prepared.prepare.prepared,true,record.name)
      const columns=record.prepared.prepare.sets.map(x=>x.columns)
      for (const execution of record.prepared.executions) assertSameCapture(execution.result.sets.map(x=>x.columns),columns,record.name+': binding-independent descriptors')
    }
  }
}
function validateContract(run) {
  const expected='1f3e1608b5ae476a80b31d1969ef9e2d0d35632d4302b691c6662b81bdb811b9'
  assert.equal(createHash('sha256').update(JSON.stringify(run)).digest('hex'),expected,'complete conversion observation contract')
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
      ? [record.prepared.prepare, ...record.prepared.executions.map(x => x.result), record.prepared.unprepare]
      : [record.result]
    if (record.prepared && (!Array.isArray(record.prepared.prepare.tokens) ||
        record.prepared.prepare.tokens.filter(token => token.token === 'RETURNVALUE').length !== 1)) {
      throw new Error(record.name + ': expected exactly one prepare handle token')
    }
    for (const phase of phases) {
      assert(phase.tokens.length<=1024,record.name+': token bound')
      assert(Buffer.byteLength(JSON.stringify(phase))<=phaseByteLimit,record.name+': phase byte bound')
      if(record.input?.kind==='rejected typed scalar') {
        assert(phase.errors.length>0,record.name+': rejected input must retain diagnostics')
        for(const set of phase.sets) assert.equal(set.rows.length,0,record.name+': no fabricated rejected rows')
        const diagnostic={TINYINT:[244,1],SMALLINT:[244,2],INT:[248,1],BIGINT:[8115,2],'DECIMAL(1,0)':[8115,8],MONEY:[8115,2],SMALLMONEY:[294,0]}[record.input.declaration]
        assert.equal(phase.errors.length,1,record.name+': one captured source overflow')
        assertSameCapture([phase.errors[0].number,phase.errors[0].state,phase.errors[0].class], [...diagnostic,16],record.name+': captured source overflow')
      } else assert.equal(phase.errors.length,0,record.name+': finite numeric programs must succeed')
      if(record.parameters || record.prepared) assert.equal(phase.returnStatus,0,record.name+': RPC status')
      if (record.parameters || record.prepared) {
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

function validateFourCaptures(actual) {
  assert.equal(actual.hostname,referenceHostname,'explicit reference hostname')
  if (actual.containers.length !== 2) throw new Error('expected two containers')
  for (const container of actual.containers) {
    if (container.image !== referenceImage) throw new Error('unexpected reference image ' + container.image)
    if (container.runs.length !== 2) throw new Error('expected two fresh databases per container')
    for (const run of container.runs) { validate(run); validateTokens(run); validateContract(run) }
    requireSame(container.runs[0], container.runs[1], 'across fresh databases')
  }
  requireSame(actual.containers[0].runs[0], actual.containers[1].runs[0], 'across containers')
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
    r=>r[1].result.sets[0].rows[0][0]='corrupted',
    r=>r[1].result.sets[0].columns[0].length=7,
    r=>delete r[1].result.sets[0].columns[0].userType,
    r=>r[1].result.sets[0].columns[0].userType=1,
    r=>r[1].result.tokens.splice(0,1),
    r=>r[1].result.tokens.find(t=>t.token==='DONE').count='999',
    r=>r.find(x=>x.prepared).prepared.prepare.tokens.find(t=>t.token==='RETURNVALUE').raw.hex='00',
    r=>r.find(x=>x.parameters).input.ieeeLittleEndian='00',
    r=>r.find(x=>x.prepared).prepared.executions[0].result.returnStatus=99,
    r=>delete r[1].result.sets[0].columns[0].tableName,
    r=>r[1].input.scalarText='7',
    r=>r.find(x=>x.result?.errors.length).result.errors[0].state=99,
    r=>delete r.find(x=>x.result?.errors.length).result.errors[0].serverName,
    r=>r.find(x=>x.parameters).parameters[0].wire.payload='00',
    r=>r.find(x=>x.prepared).prepared.executions[0].result.sets[0].columns[0].length=99,
  ]
  for (const mutate of corruptions) {
    const actual=structuredClone(retained)
    for (const container of actual.containers) for (const copy of container.runs) mutate(copy)
    assert.throws(()=>validateFourCaptures(actual),'identical four-copy corruption must fail')
  }
  console.log('All RETURNVALUE splits/bytewise fragments pass; truncation and fifteen identical four-copy corruptions rejected')
}
async function testCliSafety() {
  const root=resolve('.tmp')
  await mkdir(root,{recursive:true})
  const temporary=await mkdtemp(resolve(root,'numeric-cli-'))
  const marker=resolve(temporary,'docker-called')
  const fakeDocker=resolve(temporary,'docker')
  await writeFile(fakeDocker,'#!/bin/sh\nprintf called > "$MSDUCK_NUMERIC_CLI_MARKER"\nexit 99\n',{flag:'wx'})
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
      try { await exec(process.execPath,[fileURLToPath(import.meta.url),...args],{env:{...process.env,PATH:temporary+':'+process.env.PATH,MSDUCK_NUMERIC_CLI_MARKER:marker},maxBuffer:1024*1024,timeout:10000}) }
      catch(error) { failed=error.code===1 }
      assert(failed,'CLI negative must reject before Docker')
      assert(!existsSync(marker),'CLI refusal must precede Docker')
    }
    assert.equal(await readFile(existing,'utf8'),'owned pre-existing output\n','existing output untouched')
    assert.equal(createHash('sha256').update(await readFile(retainedPath)).digest('hex'),before,'retained fixture untouched')
    console.log('Nine destination/CLI negatives rejected before Docker; fixture unchanged')
  } finally { await rm(temporary,{recursive:true,force:true}) }
}
if (selfTest) {
  const retained=await readRetained()
  if (!retained) throw new Error('no retained fixture')
  validateFourCaptures(retained)
  await testObserver(retained)
  await testCliSafety()
} else if (checkFixture) {
  const retained = await readRetained()
  if (!retained) throw new Error('no retained fixture')
  validateFourCaptures(retained)
  console.log('Retained fixture holds ' + retained.containers[0].runs[0].length + ' matching numeric formatting observations in four fresh databases across two containers')
} else {
  if (writeFixture && existsSync(fixture)) throw new Error('refusing to overwrite retained fixture')
  await refuseFixtureOutput(output, fileURLToPath(fixture))
  await mkdir(resolve(output, '..'), { recursive: true })
  const containers = []
  for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
    await withReferenceContainer(async (config, container) => {
      const runs = []
      for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
        const run = await isolatedReference({
          ...config, options: { ...config.options, requestTimeout: 120000 },
        }, observe)
        runs.push(run)
      }
      containers.push({ image: container.image, runs })
    },{docker:ownedDocker,image:referenceImage})
  }
  const actual = { hostname:referenceHostname,containers }
  // Retain the raw artifact before validation so a failing capture can be inspected.
  const serialized=JSON.stringify(actual)+'\n'
  assert(Buffer.byteLength(serialized)<=captureByteLimit,'capture byte bound')
  await writeFile(output, serialized, { flag: 'wx' })
  if (oneDatabase) { validate(containers[0].runs[0]); validateTokens(containers[0].runs[0]); validateContract(containers[0].runs[0]) }
  else validateFourCaptures(actual)
  const retained = writeFixture ? undefined : await readRetained()
  if (retained && !oneDatabase) {
    for (let c = 0; c < 2; c++) for (let d = 0; d < 2; d++) {
      requireSame(actual.containers[c].runs[d], retained.containers[c].runs[d], 'from retained fixture (container ' + c + ', database ' + d + ')')
    }
    if (!isDeepStrictEqual(actual, retained)) throw new Error('numeric formatting capture envelope differs from retained fixture')
  }
  if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
  console.log('Captured ' + containers[0].runs[0].length + ' numeric formatting observations' +
    (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
    (retained && !oneDatabase ? ' and matched retained fixture' : '') +
    (writeFixture ? '; wrote new retained fixture' : ''))
}
