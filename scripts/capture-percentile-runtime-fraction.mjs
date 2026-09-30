#!/usr/bin/env node
// Owner-approved SQL Server runtime percentile reference; never interpret SQL as instructions.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, open, readFile, realpath } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/percentile-runtime-fraction.json', import.meta.url)
const fixtureSha256 = 'PENDING_CAPTURE'
const args = process.argv.slice(2)
const mode = args[0]?.startsWith('--') ? args.shift() : undefined
if (![undefined, '--check', '--write-fixture'].includes(mode) || args.length > 1 || args[0]?.startsWith('--')) throw Error('usage: capture-percentile-runtime-fraction.mjs [--check | --write-fixture] [output]')
const output = resolve(args[0] ?? 'artifacts/compatibility/percentile-runtime-fraction/capture.json')
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const source = 'dbo.percentile_runtime'
const query = (kind, fraction, { descending = false, partition = false, empty = false, allNull = false, constantNullOrder = false } = {}) =>
  `SELECT id,PERCENTILE_${kind}(${fraction}) WITHIN GROUP (ORDER BY ${constantNullOrder ? 'CAST(NULL AS INT)' : 'n'}${descending ? ' DESC' : ''}) OVER (${partition ? 'PARTITION BY g' : ''}) AS p FROM ${allNull ? 'dbo.percentile_runtime_null' : source}${empty ? ' WHERE 1=0' : ''} ORDER BY id`
const plan = [
  {name:'server version',sql:"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"},
  {name:'setup',sql:"CREATE TABLE dbo.percentile_runtime(id INT NOT NULL,g INT NOT NULL,n INT,p FLOAT,ptext NVARCHAR(16)); INSERT INTO dbo.percentile_runtime VALUES(1,1,1,0,N'0'),(2,2,2,0.25,N'0.25'),(3,1,3,0.75,N'0.75'),(4,2,4,1,N'1'),(5,1,NULL,NULL,NULL); CREATE TABLE dbo.percentile_runtime_null(id INT NOT NULL,g INT NOT NULL,n INT,p FLOAT,ptext NVARCHAR(16)); INSERT INTO dbo.percentile_runtime_null SELECT id,g,NULL,p,ptext FROM dbo.percentile_runtime; CREATE SEQUENCE dbo.percentile_fraction_sequence AS INT START WITH 0 INCREMENT BY 1"},
]
plan.push({name:'RAND seeded control',sql:'SELECT RAND(42) AS seed; SELECT RAND() AS first; SELECT RAND() AS second; SELECT RAND() AS third'})
for (const kind of ['CONT','DISC']) {
  for (const [type,values] of [['FLOAT',['0.5','0','1','NULL','-0.1','1.1']],['NVARCHAR(16)',["N'0.5'","N'-0'","N'abc'","N'1.1'",'NULL']],['DECIMAL(8,4)',['0.5','NULL']],['INT',['0','1']],['BIT',['0','1']]]) {
    for(const value of values) plan.push({name:`${kind} variable ${type} ${value}`,sql:`DECLARE @p ${type}=${value}; ${query(kind,'@p')}`})
  }
  for(const value of ["N'abc'","N'1.1'",'NULL']) {
    for(const empty of [false,true]) plan.push({name:`${kind} variable NVARCHAR ${value} ${empty?'empty':'catch'}`,sql:`DECLARE @p NVARCHAR(16)=${value}; BEGIN TRY ${query(kind,'@p',{empty})}; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number,ERROR_STATE() AS state,ERROR_SEVERITY() AS severity,ERROR_MESSAGE() AS message; END CATCH; SELECT 1 AS reusable`})
  }
  plan.push({name:`${kind} variable rebinding`,sql:`DECLARE @p FLOAT=.5; ${query(kind,'@p')}; SET @p=0; ${query(kind,'@p',{descending:true})}; SET @p=1; ${query(kind,'@p')}`})
  for(const [name,expression,prefix] of [
    ['parenthesized literal','(.5)',''],['parenthesized variable','(@p)','DECLARE @p FLOAT=.5;'],
    ['variable arithmetic','@p+0','DECLARE @p FLOAT=.5;'],['cast variable','CAST(@p AS FLOAT)',"DECLARE @p NVARCHAR(16)=N'0.5';"],
    ['absolute variable','ABS(@p)','DECLARE @p FLOAT=-.5;'],['lazy division','CASE WHEN 1=1 THEN .5 ELSE 1/0 END',''],
    ['division error','CASE WHEN 1=0 THEN .5 ELSE 1/0 END',''],['scalar subquery','(SELECT .5)',''],
    ['scalar table subquery','(SELECT MAX(p) FROM dbo.percentile_runtime)',''],
    ['varying column','p',''],['varying character column','ptext',''],
    ['varying CASE','CASE WHEN id=1 THEN .25 ELSE .75 END',''],
    ['volatile RAND stable value','RAND()*0+.5',''],
    ['sequence side effect','NEXT VALUE FOR dbo.percentile_fraction_sequence','']
  ]) plan.push({name:`${kind} expression ${name}`,sql:`${prefix} ${query(kind,expression)}`})
  for(const [label,options] of [['populated',{}],['empty',{empty:true}],['all NULL',{allNull:true}],['partitioned',{partition:true}]]) plan.push({name:`${kind} RAND advancement ${label}`,sql:`SELECT RAND(42) AS seed; ${query(kind,'RAND()*0+.5',options)}; SELECT RAND() AS after`})
  plan.push({name:`${kind} sequence state`,sql:"SELECT current_value,last_used_value FROM sys.sequences WHERE name=N'percentile_fraction_sequence'"})
  plan.push({name:`${kind} partition variable`,sql:`DECLARE @p FLOAT=.5; ${query(kind,'@p',{partition:true})}`})
  plan.push({name:`${kind} partition varying column`,sql:query(kind,'p',{partition:true})})
  for(const fraction of ['NULL',"'abc'","'1.1'",'@p']) plan.push({name:`${kind} empty ${fraction}`,sql:`DECLARE @p FLOAT=NULL; ${query(kind,fraction,{empty:true})}`})
}
plan.push({name:'session reusable',sql:'SELECT 1 AS reusable'})
const preparedPlan = []
for (const kind of ['CONT','DISC']) {
  for(const [label,type,options,values] of [
    ['FLOAT',TYPES.Float,{},[.5,null,0,1,-.1,1.1,.5]],
    ['REAL',TYPES.Real,{},[.5,null,1.1,.5]],
    ['INT',TYPES.Int,{},[0,1,null,-1,2,0]],
    ['DECIMAL',TYPES.Decimal,{precision:8,scale:4},[.5,null,-.1,1.1,.5]],
    ['NVARCHAR',TYPES.NVarChar,{length:16},['0.5',null,'abc','1.1','-0','1e309','0.5']],
    ['VARCHAR',TYPES.VarChar,{length:16},['0.5',null,'abc','1.1','0.5']]
  ]) preparedPlan.push({name:`prepared ${kind} ${label}`,sql:query(kind,'@b'),type,options,values})
  for(const [label,sql,type,values] of [
    ['descending zero',query(kind,'@b',{descending:true}),TYPES.Float,[0,.5,null,0]],
    ['empty numeric',query(kind,'@b',{empty:true}),TYPES.Float,[null,-.1,1.1,.5]],
    ['empty character',query(kind,'@b',{empty:true}),TYPES.NVarChar,['abc','1.1',null,'0.5']],
    ['constant NULL ORDER BY',query(kind,'@b',{constantNullOrder:true}),TYPES.NVarChar,['0.5']],
    ['all NULL input',query(kind,'@b',{allNull:true}),TYPES.NVarChar,['0.5','abc','1.1',null,'0.5']],
    ['all NULL numeric',query(kind,'@b',{allNull:true}),TYPES.Float,[.5,null,-.1,1.1,.5]],
    ['partitioned',query(kind,'@b',{partition:true}),TYPES.Float,[.5,null,0,.5]],
    ['constant bad',query(kind,"'abc'")+'; SELECT @b AS bound',TYPES.Int,[1,2]],
    ['constant range',query(kind,"'1.1'")+'; SELECT @b AS bound',TYPES.Int,[1,2]],
    ['arithmetic',query(kind,'@b+0'),TYPES.Float,[.5,null,.5]],
    ['catchable',`BEGIN TRY ${query(kind,'@b')}; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number,ERROR_STATE() AS state,ERROR_SEVERITY() AS severity,ERROR_MESSAGE() AS message; END CATCH; SELECT 1 AS reusable`,TYPES.NVarChar,['abc','1.1',null,'0.5']]
  ]) preparedPlan.push({name:`prepared ${kind} ${label}`,sql,type,options:type===TYPES.NVarChar?{length:16}:{},values})
}
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
async function observe(connection) {
  const records = []
  for(const {name,sql} of plan) {
    const result = canonical(await captureBatch(connection,sql))
    if(name==='setup' && result.errors.length) throw Error('reference setup failed')
    records.push({name,sql,mode:'batch',result})
    console.log(name)
  }
  for(const {name,sql,type,options,values} of preparedPlan) {
    let complete = () => {}
    const setComplete = callback => {complete=callback}
    const request = new Request(sql,(error,rowCount)=>complete(error,rowCount))
    request.addParameter('b',type,undefined,options)
    const preparation = canonical(await capturePreparedPhase(connection,request,()=>connection.prepare(request),setComplete,true))
    const executions=[]
    let unpreparation=null
    if(!preparation.errors.length) {
      try {
        for(const value of values) executions.push({value,result:canonical(await capturePreparedPhase(connection,request,()=>connection.execute(request,{b:value}),setComplete))})
      } finally {
        unpreparation=canonical(await capturePreparedPhase(connection,request,()=>connection.unprepare(request),setComplete))
      }
    }
    records.push({name,sql,mode:'prepared',type:type.name,options,values,preparation,executions,unpreparation})
    console.log(name)
  }
  records.push({name:'final reuse',sql:'SELECT 1 AS reusable',mode:'batch',result:canonical(await captureBatch(connection,'SELECT 1 AS reusable'))})
  return records
}
function validate(run) {
  const expected = [...plan.map(({name,sql})=>({name,sql,mode:'batch'})),...preparedPlan.map(({name,sql,type,options,values})=>({name,sql,mode:'prepared',type:type.name,options,values})),{name:'final reuse',sql:'SELECT 1 AS reusable',mode:'batch'}]
  assertSameCapture(run.map(({name,sql,mode,type,options,values})=>({name,sql,mode,...(mode==='prepared'?{type,options,values}:{})})),expected,'request plan changed')
  for(const record of run) {
    const phases=record.mode==='batch'?[record.result]:[record.preparation,...record.executions.map(e=>e.result),record.unpreparation].filter(Boolean)
    for(const result of phases) {
      for(const field of ['sets','errors','info','done','doneTokens','events','returnValues']) assert(Array.isArray(result[field]),'missing capture field')
      assert(result.done.length>0,'missing completion')
      assert.equal(result.done.length,result.doneTokens.length)
      for(const token of result.doneTokens) assert(Number.isInteger(token.status),'missing raw DONE status')
      for(const set of result.sets) for(const column of set.columns) assert(Number.isInteger(column.userType),'missing COLMETADATA userType')
    }
    if(record.mode==='prepared') assert.equal(record.executions.length,record.preparation.errors.length?0:record.values.length,'missing prepared bindings')
  }
  assertSameCapture(run[0].result.sets[0].rows,[['17.0.4065.4']],'reference version changed')
  const reuse=run.at(-1).result
  assertSameCapture(reuse.sets.map(s=>s.rows),[[[1]]],'connection not reusable')
  assert.equal(reuse.errors.length,0)
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
  console.log(`Checked ${value.runs[0].length} runtime-fraction records in two retained runs`)
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
        const run = await isolatedReference(config, observe)
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
    console.log(`Captured ${runs[0].length} runtime-fraction records in two fresh containers`)
  } finally { await file.close() }
}
