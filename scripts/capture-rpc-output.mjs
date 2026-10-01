#!/usr/bin/env node
// First-party SQL Server evidence for application RPC OUTPUT parameters.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { canonical, capture } from './lib/compatibility.mjs'

const fixture = new URL('../reference/rpc-output.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/rpc-output/rpc-output.json')

function diagnostic(error) {
  return { number: error.number ?? null, state: error.state ?? null,
    class: error.class ?? null, lineNumber: error.lineNumber ?? null, message: error.message }
}
function metadata(column) {
  return { name: column.colName ?? null, type: column.type?.name ?? null,
    length: column.dataLength ?? null, precision: column.precision ?? null,
    scale: column.scale ?? null, flags: column.flags ?? null,
    userType: column.userType ?? null, collation: canonical(column.collation ?? null) }
}
function outputMetadata(value) {
  return { type: value?.type?.name ?? null, length: value?.dataLength ?? null,
    precision: value?.precision ?? null, scale: value?.scale ?? null,
    flags: value?.flags ?? null, userType: value?.userType ?? null,
    collation: canonical(value?.collation ?? null) }
}
function add(request, parameter) {
  const {name, type, value, output, options} = parameter
  if (output) request.addOutputParameter(name, type, value, options)
  else request.addParameter(name, type, value, options)
}
function record(connection, entry) {
  return new Promise(resolve => {
    const events = []
    const request = new Request(entry.sql, (error, rowCount) => {
      events.push({kind: 'callback', rowCount: rowCount ?? null, error: error ? diagnostic(error) : null})
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      resolve({name: entry.name, mode: entry.mode ?? 'execSql', sql: entry.sql,
        parameters: entry.parameters.map(p => ({name:p.name, type:p.type.name, output:!!p.output,
          options:p.options ?? null, input:canonical(p.value)})), events:canonical(events)})
    })
    const onError = error => events.push({kind:'errorMessage', ...diagnostic(error)})
    const onInfo = info => events.push({kind:'infoMessage', ...diagnostic(info)})
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    request.on('columnMetadata', columns => events.push({kind:'columnMetadata', columns:columns.map(metadata)}))
    request.on('row', columns => events.push({kind:'row', values:canonical(columns.map(c => c.value))}))
    request.on('returnValue', (name, value, type) => events.push({kind:'returnValue', name,
      value:canonical(value), metadata:outputMetadata(type)}))
    for (const kind of ['done','doneInProc','doneProc']) {
      request.on(kind, (rowCount, more, returnStatus) => events.push({kind,
        rowCount:rowCount ?? null, more:!!more,
        ...(kind === 'doneProc' ? {returnStatus:returnStatus ?? null} : {})}))
    }
    for (const parameter of entry.parameters) add(request, parameter)
    if (entry.mode === 'callProcedure') connection.callProcedure(request)
    else connection.execSql(request)
  })
}

async function recordPrepared(connection) {
  const events = []
  let complete = () => {}
  const request = new Request('SET @p=@p+1; SELECT @p AS value', (error, rowCount) => complete(error, rowCount))
  request.addOutputParameter('p', TYPES.Int)
  const onError = error => events.push({kind:'errorMessage', ...diagnostic(error)})
  const onInfo = info => events.push({kind:'infoMessage', ...diagnostic(info)})
  connection.on('errorMessage',onError)
  connection.on('infoMessage',onInfo)
  request.on('columnMetadata', columns => events.push({kind:'columnMetadata', columns:columns.map(metadata)}))
  request.on('row', columns => events.push({kind:'row', values:canonical(columns.map(c=>c.value))}))
  request.on('returnValue', (name,value,type) => events.push({kind:'returnValue',name,
    value:canonical(value),metadata:outputMetadata(type)}))
  for(const kind of ['done','doneInProc','doneProc']) request.on(kind,(rowCount,more,returnStatus)=>
    events.push({kind,rowCount:rowCount??null,more:!!more,
      ...(kind==='doneProc'?{returnStatus:returnStatus??null}:{})}))
  let prepared=false
  try {
    await new Promise((resolve,reject)=>{
      request.once('prepared',resolve)
      request.once('error',reject)
      connection.prepare(request)
    })
    prepared=true
    events.push({kind:'prepared'})
    for(const value of [3,9]) {
      await new Promise(resolve=>{
        complete=(error,rowCount)=>{
          events.push({kind:'callback',input:value,rowCount:rowCount??null,error:error?diagnostic(error):null})
          resolve()
        }
        request.error=undefined
        connection.execute(request,{p:value})
      })
    }
  } finally {
    if(prepared) await new Promise((resolve,reject)=>{
      complete=error=>error?reject(error):resolve()
      request.error=undefined
      connection.unprepare(request)
    })
    connection.off('errorMessage',onError)
    connection.off('infoMessage',onInfo)
  }
  events.push({kind:'unprepared'})
  return {name:'prepared output repeated execution',mode:'prepare/execute',
    sql:request.sqlTextOrProcedure,events:canonical(events)}
}

const out = (name, type, value, options) => ({name,type,value,output:true,options})
const input = (name, type, value, options) => ({name,type,value,output:false,options})
const largeText = 'A'.repeat(8190) + '\uD800' + 'Z'
const largeBytes = Buffer.alloc(8193, 0x5a)
const cases = [
  {name:'int null to 42 with rows', sql:'SELECT @p AS before_value; SET @p=42; SELECT @p AS after_value', parameters:[out('p',TYPES.Int,null)]},
  {name:'int initialized unchanged', sql:'SELECT @p AS value', parameters:[out('p',TYPES.Int,7)]},
  {name:'int assigned null', sql:'SET @p=NULL; SELECT @p AS value', parameters:[out('p',TYPES.Int,7)]},
  {name:'two outputs and ordinary input', sql:'SELECT @z=@n+1, @a=@n+2; SELECT @a AS first_selected, @z AS second_selected',
    parameters:[out('z',TYPES.Int,1), input('n',TYPES.Int,5), out('a',TYPES.Int,2)]},
  {name:'bigint output', sql:'SET @p=9223372036854775807', parameters:[out('p',TYPES.BigInt,'0')]},
  {name:'bit output', sql:'SET @p=1', parameters:[out('p',TYPES.Bit,false)]},
  {name:'nvarchar bounded empty', sql:"SET @p=N''", parameters:[out('p',TYPES.NVarChar,null,{length:4})]},
  {name:'nvarchar bounded null', sql:'SET @p=NULL', parameters:[out('p',TYPES.NVarChar,'ab',{length:4})]},
  {name:'nvarchar bounded raw surrogate', sql:"SET @p=NCHAR(0xD800)+N'Z'", parameters:[out('p',TYPES.NVarChar,'',{length:4})]},
  {name:'varchar bounded', sql:"SET @p='abcd'", parameters:[out('p',TYPES.VarChar,'',{length:4})]},
  {name:'varbinary bounded empty', sql:'SET @p=0x', parameters:[out('p',TYPES.VarBinary,null,{length:4})]},
  {name:'varbinary bounded null', sql:'SET @p=NULL', parameters:[out('p',TYPES.VarBinary,Buffer.from([1]),{length:4})]},
  {name:'decimal exact', sql:'SET @p=1234567.89', parameters:[out('p',TYPES.Decimal,'0',{precision:9,scale:2})]},
  {name:'decimal null', sql:'SET @p=NULL', parameters:[out('p',TYPES.Decimal,'1.00',{precision:9,scale:2})]},
  {name:'nvarchar max plp', sql:'SET @p=@source', parameters:[out('p',TYPES.NVarChar,null,{length:0xFFFF}),input('source',TYPES.NVarChar,largeText,{length:0xFFFF})]},
  {name:'varbinary max plp', sql:'SET @p=@source', parameters:[out('p',TYPES.VarBinary,null,{length:0xFFFF}),input('source',TYPES.VarBinary,largeBytes,{length:0xFFFF})]},
  {name:'nocount off with print', sql:"SET NOCOUNT OFF; SELECT @p=9; PRINT N'output marker'; SELECT @p AS value", parameters:[out('p',TYPES.Int,1)]},
  {name:'nocount on with print', sql:"SET NOCOUNT ON; SELECT @p=9; PRINT N'output marker'; SELECT @p AS value", parameters:[out('p',TYPES.Int,1)]},
  {name:'throw after assignment', sql:"SET @p=9; THROW 51001,N'output failure',1", parameters:[out('p',TYPES.Int,1)]},
  {name:'throw before assignment', sql:"THROW 51002,N'output failure',1; SET @p=9", parameters:[out('p',TYPES.Int,1)]},
  {name:'caught throw after assignment', sql:"SET @p=9; BEGIN TRY THROW 51003,N'caught output failure',1; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS caught; END CATCH", parameters:[out('p',TYPES.Int,1)]},
  {name:'procedure output and return status', mode:'callProcedure', sql:'dbo.rpc_output_probe', parameters:[out('p',TYPES.Int,7)]},
]

async function observe(connection) {
  const server = canonical(await capture(connection,
    "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version, CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS collation"))
  assert.deepEqual(server.errors, [])
  const setup = canonical(await capture(connection,
    'CREATE PROCEDURE dbo.rpc_output_probe @p INT OUTPUT AS BEGIN SET NOCOUNT ON; SET @p=@p+1; SELECT @p AS value; RETURN 7; END'))
  assert.deepEqual(setup.errors, [])
  const results = []
  for (const entry of cases) {
    results.push(await record(connection, {...entry, mode:entry.mode ?? 'execSql'}))
    const reuse = canonical(await capture(connection, 'SELECT 1 AS reusable'))
    assert.deepEqual(reuse.errors, [], `${entry.name}: connection reuse`)
  }
  results.push(await recordPrepared(connection))
  assert.equal(results.length, 23)
  const find = name => {
    const result = results.find(item => item.name === name)
    assert(result, `missing ${name}`)
    return result.events
  }
  for (const entry of results.slice(0,-1)) assert.equal(entry.events.at(-1).kind,'callback',entry.name)
  const ordered = find('two outputs and ordinary input').filter(event => event.kind==='returnValue')
  assert.deepEqual(ordered.map(event => event.name),['z','a'])
  assert.deepEqual(ordered.map(event => event.value),[6,7])
  assert.equal(find('nvarchar bounded null').find(event => event.kind==='returnValue').metadata.length,8)
  assert.equal(find('decimal null').find(event => event.kind==='returnValue').metadata.precision,9)
  assert.equal(find('nvarchar bounded raw surrogate').find(event => event.kind==='returnValue').value,'\uD800Z')
  assert.equal(find('nvarchar max plp').find(event => event.kind==='returnValue').value.length,largeText.length)
  for (const name of ['throw after assignment','throw before assignment']) {
    assert.equal(find(name).filter(event => event.kind==='returnValue').length,0,name)
  }
  const prepared = find('prepared output repeated execution')
  assert.deepEqual(prepared.filter(event => event.kind==='returnValue').map(event => event.value),[1,4,10])
  return {server, setup, results}
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(dirname(output), {recursive:true})
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let i=0; i<2; i++) runs.push(await isolatedReference({
    ...config, options:{...config.options, requestTimeout:120000}
  }, observe))
  assertSameCapture(runs[0],runs[1],'fresh RPC OUTPUT captures differ')
  const actual = {image:container.image, identicalFreshCaptures:2, ...runs[0]}
  await writeFile(output, JSON.stringify(actual) + '\n')
  let retained
  try { retained=JSON.parse(await readFile(fixture,'utf8')) }
  catch (error) { if(error.code!=='ENOENT') throw error }
  if(retained) assertSameCapture(actual,retained,'retained RPC OUTPUT capture differs')
  if(writeFixture) await writeNewFixture(fixture,actual)
  console.log(`Captured ${actual.results.length} RPC OUTPUT cases twice identically${retained?' and matched retained fixture':''}`)
})
