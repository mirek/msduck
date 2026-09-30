#!/usr/bin/env node
// Owner-approved SQL Server RAND reference; SQL samples are data, never instructions.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, open, readFile, realpath } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { canonical as canonicalBase } from './lib/compatibility.mjs'

const fixture = new URL('../reference/rand-session.json', import.meta.url)
const fixtureSha256 = 'a1317e5b7faf9b8f37bcbd11a374907b7196d30d524c5ec5b32c973e9d29d455'
const args = process.argv.slice(2)
const mode = args[0]?.startsWith('--') ? args.shift() : undefined
if (![undefined, '--check', '--write-fixture'].includes(mode) || args.length > 1 || args[0]?.startsWith('--')) throw Error('usage: capture-rand-session.mjs [--check | --write-fixture] [output]')
const output = resolve(args[0] ?? 'artifacts/compatibility/rand-session/capture.json')
function canonical(value) {
  const result=canonicalBase(value)
  if(value && Array.isArray(value.sets)) value.sets.forEach((set,setIndex)=>set.rows.forEach((row,rowIndex)=>row.forEach((number,columnIndex)=>{
    const column=set.columns[columnIndex]
    if(number===null||!['Float','FloatN','Real'].includes(column.type))return
    const width=column.type==='Real'?4:column.type==='Float'?8:column.length
    assert([4,8].includes(width),'unsupported FLOAT width')
    const bytes=Buffer.alloc(width)
    if(width===4)bytes.writeFloatBE(number);else bytes.writeDoubleBE(number)
    result.sets[setIndex].rows[rowIndex][columnIndex]={value:Object.is(number,-0)?{kind:'number',value:'-0'}:canonicalBase(number),floatBits:bytes.toString('hex')}
  })))
  return result
}
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const stream = (seed, count = 64) => `SELECT RAND(${seed}) AS seeded; DECLARE @i INT=0; WHILE @i<${count} BEGIN SELECT @i AS step,RAND() AS r; SET @i=@i+1; END`
const plan = [{name:'server version',session:'primary',sql:"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"}]
for (const seed of ['0','1','2','42','1000','1974','2147483647','-1','-2147483648']) {
  plan.push({name:`stream ${seed}`,session:'primary',sql:stream(seed,seed==='42'?256:64)})
}
for (const [name,sql] of [
  ['reseed same','SELECT RAND(42) AS a; SELECT RAND() AS b; SELECT RAND(42) AS c; SELECT RAND() AS d'],
  ['reseed alternate','SELECT RAND(42) AS a; SELECT RAND(1) AS b; SELECT RAND() AS c; SELECT RAND(42) AS d; SELECT RAND() AS e'],
  ['multiple unseeded','SELECT RAND(42) AS baseline; SELECT RAND() AS a,RAND() AS b,RAND() AS c; SELECT RAND() AS after'],
  ['multiple same seed','SELECT RAND(42) AS a,RAND(42) AS b,RAND() AS c; SELECT RAND() AS after'],
  ['multiple different seeds','SELECT RAND(1) AS a,RAND(42) AS b,RAND() AS c; SELECT RAND() AS after'],
  ['rows unseeded','SELECT RAND(42) AS baseline; SELECT id,RAND() AS r FROM(VALUES(1),(2),(3),(4))s(id) ORDER BY id; SELECT RAND() AS after'],
  ['rows seeded','SELECT RAND(42) AS baseline; SELECT id,RAND(1) AS r FROM(VALUES(1),(2),(3),(4))s(id) ORDER BY id; SELECT RAND() AS after'],
  ['empty source','SELECT RAND(42) AS baseline; SELECT id,RAND() AS r FROM(VALUES(1),(2))s(id) WHERE 1=0; SELECT RAND() AS after'],
  ['lazy unused','SELECT RAND(42) AS baseline; SELECT CASE WHEN 1=1 THEN 0.5 ELSE RAND() END AS r; SELECT RAND() AS after'],
  ['lazy selected','SELECT RAND(42) AS baseline; SELECT CASE WHEN 1=0 THEN 0.5 ELSE RAND() END AS r; SELECT RAND() AS after'],
  ['lazy seeded unused','SELECT RAND(42) AS baseline; SELECT CASE WHEN 1=1 THEN 0.5 ELSE RAND(1) END AS r; SELECT RAND() AS after'],
  ['lazy selected division','SELECT RAND(42) AS baseline; BEGIN TRY SELECT CASE WHEN 1=0 THEN RAND() ELSE 1/0 END AS r; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n,ERROR_STATE() AS s,ERROR_SEVERITY() AS severity; END CATCH; SELECT RAND() AS after'],
  ['transaction rollback','SELECT RAND(42) AS baseline; BEGIN TRAN; SELECT RAND() AS r; ROLLBACK; SELECT RAND() AS after'],
]) plan.push({name,session:'primary',sql})
for(const seed of ['NULL','CAST(NULL AS INT)','CAST(42 AS TINYINT)','CAST(42 AS SMALLINT)','CAST(42 AS BIGINT)','CAST(2147483648 AS BIGINT)','CAST(-2147483649 AS BIGINT)','CAST(.9 AS FLOAT)','CAST(1.9 AS FLOAT)','CAST(-1.9 AS FLOAT)','CAST(42.9 AS DECIMAL(8,2))','CAST(1 AS BIT)',"'42'","N'42'","'abc'","N'abc'","'1.9'",'CAST(42 AS MONEY)']) {
  plan.push({name:`seed conversion ${seed}`,session:'primary',sql:`SELECT RAND(42) AS baseline; SELECT RAND(${seed}) AS r; SELECT RAND() AS after`})
}
plan.push({name:'isolation primary seed',session:'primary',sql:'SELECT RAND(42) AS r'})
plan.push({name:'isolation secondary seed',session:'secondary',sql:'SELECT RAND(1) AS r'})
plan.push({name:'isolation primary next',session:'primary',sql:'SELECT RAND() AS r'})
plan.push({name:'isolation secondary next',session:'secondary',sql:'SELECT RAND() AS r'})
plan.push({name:'isolation secondary reseed',session:'secondary',sql:'SELECT RAND(42) AS r'})
plan.push({name:'isolation primary second',session:'primary',sql:'SELECT RAND() AS r'})
plan.push({name:'isolation secondary next again',session:'secondary',sql:'SELECT RAND() AS r'})
const preparedPlan = [
  {name:'prepared INT seed',sql:'SELECT RAND(42) AS baseline; SELECT RAND(@b) AS r; SELECT RAND() AS after',type:TYPES.Int,options:{},values:[42,42,0,-1,null,42]},
  {name:'prepared FLOAT seed',sql:'SELECT RAND(42) AS baseline; SELECT RAND(@b) AS r; SELECT RAND() AS after',type:TYPES.Float,options:{},values:[.9,1.9,-1.9,2147483648,null,42]},
  {name:'prepared NVARCHAR seed',sql:'SELECT RAND(42) AS baseline; SELECT RAND(@b) AS r; SELECT RAND() AS after',type:TYPES.NVarChar,options:{length:16},values:['42','abc','1.9',null,'42']},
]
async function captureTokens(connection, action) {
  const doneTokens = []
  const raw = []
  const events = []
  const createParser = connection.createTokenStreamParser
  const readToken = StreamParser.prototype.readToken
  const recordRaw = (parser, type) => {
    assert(parser.options.tdsVersion >= '7_2', 'unexpected TDS version')
    const at = parser.position
    if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordRaw(parser, type))
    raw.push({ kind: doneKinds.get(type), status: parser.buffer.readUInt16LE(at), command: parser.buffer.readUInt16LE(at + 2) })
    return readToken.call(parser, type)
  }
  StreamParser.prototype.readToken = function (type) {
    return doneKinds.has(type) ? recordRaw(this, type) : readToken.call(this, type)
  }
  connection.createTokenStreamParser = function (message, handler) {
    const parser = createParser.call(this, message, handler)
    assert.equal(typeof parser.parser?.prependListener, 'function', 'Tedious token stream unavailable')
    parser.parser.prependListener('data', token => {
      if (typeof token.name === 'string') events.push({ kind: token.name })
      if (['DONE', 'DONEINPROC', 'DONEPROC'].includes(token.name)) doneTokens.push({
        kind: token.name, more: token.more, sqlError: token.sqlError,
        attention: token.attention, serverError: token.serverError,
        rowCount: token.rowCount ?? null, command: token.curCmd,
      })
    })
    return parser
  }
  try {
    const result = await action()
    assert.equal(doneTokens.length, result.done.length, 'incomplete decoded DONE tokens')
    assert.equal(raw.length, doneTokens.length, 'incomplete raw DONE words')
    for (let index = 0; index < doneTokens.length; index++) {
      assert.equal(raw[index].kind, doneTokens[index].kind, 'raw and decoded DONE kinds differ')
      assert.equal(raw[index].command, doneTokens[index].command, 'raw and decoded DONE commands differ')
      doneTokens[index].status = raw[index].status
    }
    return { ...result, doneTokens, events }
  } finally {
    connection.createTokenStreamParser = createParser
    StreamParser.prototype.readToken = readToken
  }
}

async function capturePreparedPhase(connection, request, issue, setComplete, prepared = false) {
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


async function captureBatch(connection, sql) {
  let complete = () => {}
  const request = new Request(sql, (error,rowCount) => complete(error,rowCount))
  return capturePreparedPhase(connection,request,()=>connection.execSqlBatch(request),callback=>{complete=callback})
}
async function observe(primary, secondary) {
  const records = []
  for(const {name,sql,session} of plan) {
    const result = canonical(await captureBatch(session==='primary'?primary:secondary,sql))
    records.push({name,sql,session,mode:'batch',result})
    console.log(name)
  }
  for(const {name,sql,type,options,values} of preparedPlan) {
    let complete=()=>{}
    const setComplete=callback=>{complete=callback}
    const request=new Request(sql,(error,rowCount)=>complete(error,rowCount))
    request.addParameter('b',type,undefined,options)
    const preparation=canonical(await capturePreparedPhase(primary,request,()=>primary.prepare(request),setComplete,true))
    const executions=[]
    let unpreparation=null
    if(!preparation.errors.length) {
      try {
        for(const value of values) executions.push({value,result:canonical(await capturePreparedPhase(primary,request,()=>primary.execute(request,{b:value}),setComplete))})
      } finally { unpreparation=canonical(await capturePreparedPhase(primary,request,()=>primary.unprepare(request),setComplete)) }
    }
    records.push({name,sql,session:'primary',mode:'prepared',type:type.name,options,values,preparation,executions,unpreparation})
    console.log(name)
  }
  for(const [session,connection] of [['primary',primary],['secondary',secondary]]) records.push({name:`final reuse ${session}`,sql:'SELECT 1 AS reusable',session,mode:'batch',result:canonical(await captureBatch(connection,'SELECT 1 AS reusable'))})
  return records
}
function validate(run) {
  const expected=[...plan.map(p=>({...p,mode:'batch'})),...preparedPlan.map(({name,sql,type,options,values})=>({name,sql,session:'primary',mode:'prepared',type:type.name,options,values})),...['primary','secondary'].map(session=>({name:`final reuse ${session}`,sql:'SELECT 1 AS reusable',session,mode:'batch'}))]
  assertSameCapture(run.map(({name,sql,session,mode,type,options,values})=>({name,sql,session,mode,...(mode==='prepared'?{type,options,values}:{})})),expected,'RAND request plan changed')
  for(const record of run) {
    const phases=record.mode==='batch'?[record.result]:[record.preparation,...record.executions.map(e=>e.result),record.unpreparation].filter(Boolean)
    for(const result of phases) {
      for(const field of ['sets','errors','info','done','doneTokens','events','returnValues']) assert(Array.isArray(result[field]),'missing capture field')
      assert(result.done.length>0,'missing completion'); assert.equal(result.done.length,result.doneTokens.length)
      for(const token of result.doneTokens) {
        assert(Number.isInteger(token.status)&&Number.isInteger(token.command),'missing raw DONE words')
        assert.equal(token.more,Boolean(token.status&1),'DONE_MORE differs')
        assert.equal(token.sqlError,Boolean(token.status&2),'DONE_ERROR differs')
        assert.equal(token.attention,Boolean(token.status&32),'DONE_ATTN differs')
        assert.equal(token.serverError,Boolean(token.status&256),'DONE_SRVERROR differs')
        assert.equal(token.rowCount===null,!(token.status&16),'DONE_COUNT validity differs')
      }
      assert.equal(result.returnValues.length,result.events.filter(e=>e.kind==='RETURNVALUE').length,'missing typed return value')
      for(const set of result.sets) for(const column of set.columns) assert(Number.isInteger(column.userType),'missing userType')
      for(const set of result.sets) for(const row of set.rows) row.forEach((value,i)=>{
        if(value!==null&&['Float','FloatN','Real'].includes(set.columns[i].type)) {
          const column=set.columns[i],width=column.type==='Real'?4:column.type==='Float'?8:column.length
          assert([4,8].includes(width),'unsupported FLOAT width')
          assert(typeof value.floatBits==='string'&&value.floatBits.length===width*2&&/^[0-9a-f]+$/.test(value.floatBits),'missing exact FLOAT bits')
          const number=typeof value.value==='number'?value.value:Number(value.value.value)
          const bytes=Buffer.alloc(width)
          if(width===4)bytes.writeFloatBE(number);else bytes.writeDoubleBE(number)
          assert.equal(bytes.toString('hex'),value.floatBits,'decoded FLOAT value and bits differ')
        }
      })
    }
    if(record.mode==='prepared') assert.equal(record.executions.length,record.preparation.errors.length?0:record.values.length,'missing prepared binding')
  }
  assert.equal(run.length,53,'missing RAND requests')
  assert.equal(run.reduce((n,r)=>n+(r.mode==='batch'?1:2+r.executions.length),0),73,'missing request phases')
  for(const seed of ['0','1','2','42','1000','1974','2147483647','-1','-2147483648']) {
    const result=run.find(r=>r.name===`stream ${seed}`).result
    const count=seed==='42'?256:64
    assert.equal(result.sets.length,count+1,'incomplete RAND stream')
    assert.equal(result.errors.length,0,'stream error')
    assert(result.sets[0].rows.length===1,'missing seeded row')
    for(let i=0;i<count;i++) { assert.equal(result.sets[i+1].rows.length,1,'missing stream row'); assert.equal(result.sets[i+1].rows[0][0],i,'stream step differs') }
    // Finite empirical candidate only: never used to fabricate captured rows.
    let x=['0','2147483647','-2147483648'].includes(seed)?12345:Math.abs(Number(seed)),y=67890
    for(const set of result.sets) {
      x=(40014*x)%2147483563;y=(40692*y)%2147483399
      let z=x-y;if(z<1)z+=2147483562
      const bytes=Buffer.alloc(8);bytes.writeDoubleBE(z*4.656613e-10)
      assert.equal(set.rows[0].at(-1).floatBits,bytes.toString('hex'),`empirical recurrence differs for seed ${seed}`)
    }
  }
  assertSameCapture(run[0].result.sets[0].rows,[['17.0.4065.4']],'reference version changed')
  for(const session of ['primary','secondary']) { const result=run.find(r=>r.name===`final reuse ${session}`).result; assertSameCapture(result.sets.map(s=>s.rows),[[[1]]],'session not reusable'); assert.equal(result.errors.length,0) }
}
async function canonicalOutput(path) {
  try { return await realpath(path) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return resolve(await realpath(dirname(path)), basename(path))
  }
}
async function retained() {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const value = JSON.parse(bytes)
  assert.equal(value.image, referenceImage, 'reference image changed')
  assert.equal(value.runs.length, 2, 'independent runs missing')
  for (const run of value.runs) validate(run)
  assertSameCapture(value.runs[0], value.runs[1], 'independent runs differ')
  return value
}
if (mode === '--check') {
  const value = await retained()
  console.log(`Checked ${value.runs[0].length} RAND session records in two retained runs`)
} else {
  if (mode === '--write-fixture') await refuseExistingFixture(fixture)
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) throw Error('reference image must be pinned')
  await mkdir(dirname(output), { recursive: true })
  if (await canonicalOutput(output) === await canonicalOutput(fileURLToPath(fixture))) throw Error('output must not alias the retained fixture')
  const file = await open(output, 'wx') // refuse existing paths and fixture aliases
  try {
    await file.writeFile(JSON.stringify({ status: 'incomplete' }) + '\n')
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      await withReferenceContainer(async (config, container) => {
        assert.equal(container.image, referenceImage)
        const run = await isolatedReference(config, async primary => {
          const secondary=await connect({...config,options:{...config.options,database:primary.config.options.database}})
          try { return await observe(primary,secondary) }
          finally { if(!secondary.closed) await new Promise(resolve=>{secondary.once('end',resolve);secondary.close()}) }
        })
        validate(run)
        if (runs.length) assertSameCapture(run, runs[0], 'independent reference containers differ')
        runs.push(run)
      })
    }
    const value = { image: referenceImage, runs }
    if (mode !== '--write-fixture') assertSameCapture(value, await retained(), 'fresh reference differs')
    const bytes = Buffer.from(JSON.stringify(value) + '\n')
    await file.truncate(0)
    let written = 0
    while (written < bytes.length) written += (await file.write(bytes, written, bytes.length - written, written)).bytesWritten
    if (mode === '--write-fixture') await writeNewFixture(fixture, value)
    console.log(`Captured ${runs[0].length} RAND session records in two fresh containers`)
  } finally { await file.close() }
}
