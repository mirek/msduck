#!/usr/bin/env node
// Retain SQL Server CONCAT_WS and TRANSLATE rows, descriptors, diagnostics and completions.
// Usage: capture-temporal-guid-format.mjs [output] [--write-fixture | --one-database | --check-fixture]
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
function checkDeadline() { if (Date.now()>deadline) throw new Error('temporal/GUID capture watchdog expired') }


const fixture = new URL('../reference/temporal-guid-format.json', import.meta.url)
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
const output = resolve(positional[0] ?? 'artifacts/compatibility/temporal-guid-format/capture.json')

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


function wireEvidence(type,value,options={}) {
  const parameter={...options,value:type.validate(value)}
  return {typeInfo:type.generateTypeInfo(parameter).toString('hex'),length:type.generateParameterLength(parameter).toString('hex'),payload:Buffer.concat([...type.generateParameterData(parameter,{useUTC:true})]).toString('hex')}
}
const cases=[],provenance=new Map(),rpcPrograms=[],preparedPrograms=[]
const languages=['us_english','French','German']
const modern=[]
for(let scale=0;scale<=7;scale++) for(const [family,base] of [['TIME','23:59:59.9999999'],['DATETIME2','2024-02-29T23:59:59.9999999'],['DATETIMEOFFSET','2024-02-29T23:59:59.9999999+01:30']]) {
  modern.push([`${family}(${scale})`,base],[`${family}(${scale})`,family==='TIME'?'00:00:00.1200000':family==='DATETIME2'?'2024-01-02T00:00:00.1200000':'2024-01-02T00:00:00.1200000-00:30'],[`${family}(${scale})`,null])
}
for(let month=1;month<=12;month++) modern.push(['DATE',`2024-${String(month).padStart(2,'0')}-${month%2?'02':'28'}`])
modern.push(['DATE','0001-01-01'],['DATE','9999-12-31'],['DATE','2024-02-29'],['DATE',null],['DATETIME2(7)','0001-01-01T00:00:00.0000000'],['DATETIME2(7)','9999-12-31T23:59:59.9999999'])
for(const input of ['2024-02-29T00:00:00.1234567+00:00','2024-02-29T00:00:00.1234567+14:00','2024-02-29T00:00:00.1234567-14:00','0001-01-01T14:00:00.0000000+14:00','9999-12-31T09:59:59.9999999-14:00','0001-01-01T00:00:00.0000000+14:00','9999-12-31T23:59:59.9999999-14:00']) modern.push(['DATETIMEOFFSET(7)',input])
for(const input of ['00000000-0000-0000-0000-000000000000','FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF','00112233-4455-6677-8899-AABBCCDDEEFF','6f9619ff-8b86-d011-b42d-00c04fc964ff',null]) modern.push(['UNIQUEIDENTIFIER',input])
const legacy=[]
for(const type of ['DATETIME','SMALLDATETIME']) {
  for(let month=1;month<=12;month++) for(const hour of ['00','12','23']) legacy.push([type,`2024-${String(month).padStart(2,'0')}-${month%2?'02':'28'}T${hour}:04:05.123`])
  legacy.push([type,null])
}
for(const input of ['1753-01-01T00:00:00.000','9999-12-31T23:59:59.997','2024-02-29T23:59:59.998','2024-02-29T23:59:59.999','2024-01-02T03:04:05.001','2024-01-02T03:04:05.002','2024-01-02T03:04:05.004','2024-01-02T03:04:05.005']) legacy.push(['DATETIME',input])
for(const input of ['1900-01-01T00:00:00','2079-06-06T23:59:00','2024-02-29T23:59:29.998','2024-02-29T23:59:29.999','2024-02-29T23:59:30.000','2079-06-06T23:59:59']) legacy.push(['SMALLDATETIME',input])
function nativeSql(type,expr) {
  const names=type==='UNIQUEIDENTIFIER'?[]:type==='DATE'?['YEAR','MONTH','DAY']:type.startsWith('TIME(')?['HOUR','MINUTE','SECOND','NANOSECOND']:['YEAR','MONTH','DAY','HOUR','MINUTE','SECOND','NANOSECOND',...(type.startsWith('DATETIMEOFFSET')?['TZOFFSET']:[])]
  return `SELECT ${expr} AS source,CONVERT(VARBINARY(MAX),${expr}) AS stored_bytes${names.map(name=>`,DATEPART(${name},${expr}) AS ${name.toLowerCase()}`).join('')}`
}
function roles(type,expr) {
  const result=[['native stored parts',nativeSql(type,expr)]]
  for(const unicode of [false,true]) {
    const domain=unicode?'unicode':'ansi',target=unicode?'NVARCHAR(MAX)':'VARCHAR(MAX)',empty=unicode?"N''":"''",a=unicode?"N'a'":"'a'",b=unicode?"N'b'":"'b'"
    result.push([`default cast ${domain}`,`SELECT CAST(${expr} AS ${target}) AS value`],[`explicit style0 ${domain}`,`SELECT CONVERT(${target},${expr},0) AS value`],[`explicit style121 ${domain}`,`SELECT CONVERT(${target},${expr},121) AS value`],[`translate ${domain}`,`SELECT TRANSLATE(${expr},${empty},${empty}) AS value`],[`concat first ${domain}`,`SELECT CONCAT_WS(${empty},${expr},${empty}) AS value`],[`concat separator ${domain}`,`SELECT CONCAT_WS(${expr},${a},${b}) AS value`],[`concat last ${domain}`,`SELECT CONCAT_WS(${empty},${empty},${expr}) AS value`])
  }
  return result
}
function contextSql(language) { return `SET LANGUAGE ${language}; SET DATEFORMAT ymd; SET DATEFIRST 7; SELECT @@LANGUAGE AS language,@@DATEFIRST AS datefirst` }
for(const language of languages) {
  const name=`session ${language}`,sql=contextSql(language)
  cases.push([name,sql]);provenance.set(name,{kind:'session',language,dateformat:'ymd',datefirst:7})
  const inputs=language==='us_english'?[...modern,...legacy]:legacy
  inputs.forEach(([type,value],index)=>{
    const expr=`CAST(${value===null?'NULL':"'"+value+"'"} AS ${type})`
    for(const [role,sql] of roles(type,expr)) {
      const name=`${language} ${index} ${type} ${value===null?'NULL':value} ${role}`
      cases.push([name,sql]);provenance.set(name,{kind:'typed SQL source',language,dateformat:'ymd',datefirst:7,declaration:type,originalText:value,role})
    }
  })
}
assert(cases.length<=6000,'fixed capture matrix bound')
function date(value,delta=0) { const result=new Date(value); assert.equal(result.getTimezoneOffset(),0,'Non-UTC driver timezone is not admitted; run with TZ=UTC'); result.nanosecondDelta=delta; return result }
let outgoingCapture=null
const first=date('2024-02-29T03:04:05.123Z',0.0004567),last=date('2024-12-31T23:59:59.997Z')
const preparedTypes=[['DATE',TYPES.Date,{}],['TIME(7)',TYPES.Time,{scale:7}],['DATETIME2(7)',TYPES.DateTime2,{scale:7}],['DATETIMEOFFSET(7)',TYPES.DateTimeOffset,{scale:7}],['DATETIME',TYPES.DateTime,{}],['SMALLDATETIME',TYPES.SmallDateTime,{}],['UNIQUEIDENTIFIER',TYPES.UniqueIdentifier,{}]]
for(const wireType of new Set(preparedTypes.map(([,wireType])=>wireType))) {
  const generate=wireType.generateParameterData
  wireType.generateParameterData=function*(parameter,options) {
    const chunks=[]
    for(const bytes of generate.call(this,parameter,options)) { chunks.push(Buffer.from(bytes)); yield bytes }
    if(outgoingCapture) outgoingCapture.push({type:wireType.name,typeInfo:wireType.generateTypeInfo(parameter).toString('hex'),length:wireType.generateParameterLength(parameter).toString('hex'),payload:Buffer.concat(chunks).toString('hex')})
  }
}
for(const [type,wireType,options] of preparedTypes) for(const language of (['DATETIME','SMALLDATETIME'].includes(type)?languages:['us_english'])) {
  const bindings=type==='UNIQUEIDENTIFIER'?['00112233-4455-6677-8899-AABBCCDDEEFF',null,'6f9619ff-8b86-d011-b42d-00c04fc964ff','00112233-4455-6677-8899-AABBCCDDEEFF']:[first,null,last,first]
  for(const [role,sql] of roles(type,'@p').filter(([role])=>['native stored parts','default cast ansi','translate unicode','concat first ansi','concat separator unicode'].includes(role))) {
    const name=`prepared ${language} ${type} ${role}`
    preparedPrograms.push([name,sql,[['p',wireType,options]],bindings.map(p=>({p}))]);provenance.set(name,{kind:'prepared typed source',language,dateformat:'ymd',datefirst:7,declaration:type,role,bindings:bindings.map(value=>value instanceof Date?{utcISO:value.toISOString(),driverNanosecondDelta:value.nanosecondDelta}:value),driverUseUTC:true,offsetAdmission:type.startsWith('DATETIMEOFFSET')?'Driver UTC offset only; signed offsets are separate SQL-source observations.':null})
  }
}
const exclusions={matrix:'Bounded orthogonal default-collation matrix: modern numeric formats/scales in us_english; legacy month/clock/rounding inputs in three languages. No full language×allvalues×scale×offset crossproduct.',prepared:'Original driver temporal/GUID declarations, actual generated bytes; UTC DateTimeOffset prepared bindings only. No ticks/offsets inferred from decoded Date values.',styles:'defaultCAST, explicit0/121, TRANSLATE and CONCAT_WS roles remain independent, including admitted errors.'}
const errorFields = message

const columnDescriptor = x => {
  const { colName,type,userType,dataLength,precision,scale,flags,collation,...details }=x
  return canonical({ name:colName,type:type.name,userType,length:dataLength??null,precision:precision??null,scale:scale??null,flags,collation:collation??null,...details })
}
// SQL Server includes its hostname in diagnostics. Pin this explicit input,
// while the lifecycle helper retains independent random names/ports/databases.
const referenceHostname='msduck-temporal-reference'
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
    if (process.env.TEMPORAL_GUID_TRACE) console.error(name)
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
    if (process.env.TEMPORAL_GUID_TRACE) console.error(name)
    records.push({
      name, sql, input:provenance.get(name),contextSql:contextSql(provenance.get(name).language),context,
      declarations: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, typeInfo:wireEvidence(type,null,options).typeInfo, ...(options ? { options } : {}) })),
      bindingWire:valueSets.map(values=>declarations.map(([name,type,options])=>({name,...wireEvidence(type,values[name],options)}))),
      prepared: await prepared(connection, sql, declarations, valueSets),
    })
    records.at(-1).reuse=canonical(await observed(connection,'SELECT @@TRANCOUNT AS transaction_count,1 AS reusable'))
  }
  await record('connection reusable', 'SELECT @@TRANCOUNT AS transaction_count,1 AS reusable')
  return records
}

function validate(run) {
  assert.equal(run.length,2+cases.length+preparedPrograms.length)
  assertSameCapture(run.map(x=>x.name),['server version',...cases.map(x=>x[0]),...preparedPrograms.map(x=>x[0]),'connection reusable'])
  for(const record of run) {
    if(provenance.has(record.name) && !record.prepared) assert(record.reuse,record.name+': missing reuse control')
    if(provenance.has(record.name)) assertSameCapture(record.input,canonical(provenance.get(record.name)),record.name+': exact typed input')
    if(record.reuse) {
      assert.equal(record.reuse.errors.length,0,record.name+': connection reuse')
      assertSameCapture(record.reuse.sets[0].rows,[[0,1]],record.name+': connection reuse rows')
    }
    if(record.prepared) {
      assertSameCapture(record.contextSql,contextSql(record.input.language),record.name+': explicit session SQL')
      assert.equal(record.context.errors.length,0,record.name+': successful session setup')
      assert.equal(record.prepared.prepare.prepared,true)
      for(const [index,execution] of record.prepared.executions.entries()) {
        assertSameCapture(execution.result.sets.map(x=>x.columns),record.prepared.prepare.sets.map(x=>x.columns),record.name+': unchanged prepared descriptors')
        assertSameCapture(execution.wire,record.bindingWire[index].map(({name,...wire})=>({type:record.declarations.find(d=>d.name===name).type,...wire})),record.name+': actual emitted parameter bytes')
        if(record.input.role==='native stored parts') {
          const wire=execution.wire[0],bytes=Buffer.from(wire.payload,'hex')
          let stored
          if(execution.values.p===null) stored=null
          else {
            if(wire.type==='DateTime') { bytes.subarray(0,4).reverse();bytes.subarray(4,8).reverse() }
            if(wire.type==='SmallDateTime') { bytes.subarray(0,2).reverse();bytes.subarray(2,4).reverse() }
            const prefix=['Time','DateTime2','DateTimeOffset'].includes(wire.type)?Buffer.from([7]):Buffer.alloc(0)
            stored={kind:'binary',value:Buffer.concat([prefix,bytes]).toString('hex')}
          }
          assertSameCapture(execution.result.sets[0].rows[0][1],stored,record.name+': independent SQL stored bytes versus emitted TDS payload')
        }
      }
    }
  }
}
function validateContract(run) {
  const expected='6d7b02260531d0bbd7d326ff8a5cd52d95b80d45e1553b0b7e92df98a61604d5'
  assert.equal(createHash('sha256').update(JSON.stringify(run)).digest('hex'),expected,'complete observation contract')
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
      if(record.parameters || (record.prepared && phase!==record.context && phase!==record.reuse)) assert.equal(phase.returnStatus,0,record.name+': RPC status')
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
  assertSameCapture(actual.exclusions,exclusions,'explicit capture exclusions')
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
    r=>r[0].result.sets[0].rows[0][0]='corrupted',
    r=>r[0].result.sets[0].columns[0].length=7,
    r=>delete r[0].result.sets[0].columns[0].userType,
    r=>r[0].result.tokens.splice(0,1),
    r=>r[0].result.tokens.find(t=>/^DONE/.test(t.token)).count='999',
    r=>r.find(x=>x.prepared).prepared.prepare.tokens.find(t=>t.token==='RETURNVALUE').raw.hex='00',
    r=>r.find(x=>x.result?.errors.length).result.errors[0].message='corrupted',
    r=>r.find(x=>x.result?.errors.length).result.errors[0].number=999,
    r=>r.find(x=>x.result?.errors.length).result.errors[0].serverName='corrupted',
    r=>r.find(x=>x.input?.kind==='typed SQL source').input.declaration='VARCHAR(1)',
    r=>delete r[1].reuse,
    r=>r[1].reuse.sets[0].rows[0][0]=1,
    r=>r.find(x=>x.input?.role==='native stored parts').result.sets[0].rows[0][1]={kind:'binary',value:'00'},
    r=>r.find(x=>x.prepared).bindingWire[0][0].payload='00',
    r=>r.find(x=>x.prepared).prepared.executions[0].result.returnStatus=99,
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
  const temporary=await mkdtemp(resolve(root,'temporal-guid-cli-'))
  const marker=resolve(temporary,'docker-called')
  const fakeDocker=resolve(temporary,'docker')
  await writeFile(fakeDocker,'#!/bin/sh\nprintf called > "$MSDUCK_TEMPORAL_GUID_CLI_MARKER"\nexit 99\n',{flag:'wx'})
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
      try { await exec(process.execPath,[fileURLToPath(import.meta.url),...args],{env:{...process.env,PATH:temporary+':'+process.env.PATH,MSDUCK_TEMPORAL_GUID_CLI_MARKER:marker},maxBuffer:1024*1024,timeout:10000}) }
      catch(error) { failed=error.code===1 }
      assert(failed,'CLI negative must reject before Docker')
      assert(!existsSync(marker),'CLI refusal must precede Docker')
    }
    await assert.rejects(exec(process.execPath,[fileURLToPath(import.meta.url),resolve(temporary,'timezone.json')],{env:{...process.env,TZ:'Europe/Zurich',PATH:temporary+':'+process.env.PATH,MSDUCK_TEMPORAL_GUID_CLI_MARKER:marker},maxBuffer:1024*1024,timeout:10000}),/Non-UTC driver timezone/,'unadmitted timezone must fail before Docker')
    assert(!existsSync(marker),'timezone refusal must precede Docker')
    assert.equal(await readFile(existing,'utf8'),'owned pre-existing output\n','existing output untouched')
    assert.equal(createHash('sha256').update(await readFile(retainedPath)).digest('hex'),before,'retained fixture untouched')
    console.log('Ten destination/CLI/timezone negatives rejected before Docker; fixture unchanged')
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
  validateFourCaptures(retained)
  await testObserver(retained)
  await testCliSafety()
  await testLifecycleSafety()
} else if (checkFixture) {
  const retained = await readRetained()
  if (!retained) throw new Error('no retained fixture')
  validateFourCaptures(retained)
  console.log('Retained fixture holds ' + retained.containers[0].runs[0].length + ' matching temporal/GUID observations in four fresh databases across two containers')
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
          ...config, options: { ...config.options, requestTimeout: 120000,useUTC:true },
        }, observe)
        runs.push(run)
      }
      containers.push({ image: container.image, runs })
    },{docker:ownedDocker,image:referenceImage})
  }
  const actual = { hostname:referenceHostname,exclusions,containers }
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
    if (!isDeepStrictEqual(actual, retained)) throw new Error('temporal/GUID capture envelope differs from retained fixture')
  }
  if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
  console.log('Captured ' + containers[0].runs[0].length + ' temporal/GUID observations' +
    (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
    (retained && !oneDatabase ? ' and matched retained fixture' : '') +
    (writeFixture ? '; wrote new retained fixture' : ''))
}
