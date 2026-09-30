import assert from 'node:assert/strict'
import { test } from 'node:test'
import { createHash, randomUUID } from 'node:crypto'
import { createRequire } from 'node:module'
import { execFileSync } from 'node:child_process'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { Request, TYPES } from 'tedious'
import { start, query } from './support/client.mjs'
import { capture, canonical, differences } from '../scripts/lib/compatibility.mjs'
import { assertSameCapture } from '../scripts/lib/reference.mjs'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
let plan, preparedPlan
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
  const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
  const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
  const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
  const onMetadata = metadata => result.sets.push({ columns: metadata.map(c => ({ name: c.colName, type: c.type.name,
    length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null,
    flags: c.flags, collation: canonical(c.collation ?? null) })), rows: [] })
  const onRow = row => result.sets.at(-1).rows.push(row.map(c => c.value))
  const done = Object.fromEntries(['done', 'doneInProc', 'doneProc'].map(kind => [kind, (rowCount, more, status) => {
    result.done.push({ kind, rowCount: rowCount ?? null, more })
    if (kind === 'doneProc') result.returnStatus = status
  }]))
  connection.on('errorMessage', onError); connection.on('infoMessage', onInfo)
  request.on('columnMetadata', onMetadata); request.on('row', onRow)
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
    request.off('columnMetadata', onMetadata); request.off('row', onRow)
    for (const [kind, listener] of Object.entries(done)) request.off(kind, listener)
  }
}

async function observe(connection) {
  const records = []
  for (const { name, sql } of plan) {
    const result = canonical(await captureTokens(connection, () => capture(connection, sql)))
    records.push({ name, sql, mode: 'batch', result })
    console.log(name)
  }
  for (const { name, sql, type, options, values } of preparedPlan) {
    let complete = () => {}
    const setComplete = callback => { complete = callback }
    const request = new Request(sql, (error, rowCount) => complete(error, rowCount))
    request.addParameter('b', type, undefined, options)
    const preparation = canonical(await capturePreparedPhase(connection, request, () => connection.prepare(request), setComplete, true))
    const executions = []
    let unpreparation = null
    if (!preparation.errors.length) {
      try {
        for (const value of values) {
          const result = canonical(await capturePreparedPhase(connection, request,
            () => connection.execute(request, { b: value }), setComplete))
          executions.push({ value, result })
        }
      } finally {
        unpreparation = canonical(await capturePreparedPhase(connection, request,
          () => connection.unprepare(request), setComplete))
      }
    }
    records.push({ name, sql, mode: 'prepared', type: type.name, options: options ?? null,
      values, preparation, executions, unpreparation })
    console.log(name)
  }
  return records
}



function rows(result) {
  return result.sets.map(set => set.rows.map(row => row.map((value, i) => {
    const column = set.columns[i]
    if (value === null || !['FloatN', 'Float', 'Real'].includes(column.type)) return value
    const width = column.type === 'Real' ? 4 : column.type === 'Float' ? 8 : column.length
    assert([4, 8].includes(width))
    const bytes = Buffer.alloc(width)
    const number = typeof value === 'number' ? value : Number(value.value)
    if (width === 4) bytes.writeFloatBE(number); else bytes.writeDoubleBE(number)
    return { floatBits: bytes.toString('hex') }
  })))
}
const diagnostics = result => result.errors.map(({number,state,class:severity,message})=>[number,state,severity,message])

test('percentile character fraction replay preserves raw gaps and captured values errors and bindings', async t => {
  const bytes = await readFile(new URL('../reference/percentile-character-fraction.json', import.meta.url))
  const fixtureSha256 = createHash('sha256').update(bytes).digest('hex')
  assert.equal(fixtureSha256, '2d345861c98702b60c1ba69de35e65436b4c760bc7785706b6f9cdee60bf938e')
  const fixture = JSON.parse(bytes)
  assertSameCapture(fixture.runs[0], fixture.runs[1], 'independent reference runs differ')
  const reference = fixture.runs[0].filter(entry=>entry.name!=='server version')
  plan = reference.filter(entry=>entry.mode==='batch')
  preparedPlan = reference.filter(entry=>entry.mode==='prepared').map(entry=>({...entry,type:TYPES[entry.type]}))
  const connection = await start(t)
  const local = await observe(connection)
  assert.equal(local.length,108)
  const cases=local.map(entry=>{
    const expected=reference.find(record=>record.name===entry.name)
    return { name:entry.name,local:entry,reference:expected,differences:differences(entry,expected) }
  })
  const sourceRevision = execFileSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).trim()
  const path = `artifacts/compatibility/percentile-character-fraction/execution-${sourceRevision}-${randomUUID()}.json`
  await mkdir('artifacts/compatibility/percentile-character-fraction',{recursive:true})
  await writeFile(path,JSON.stringify({sourceRevision,fixtureSha256,referenceImage:fixture.image,
    limitations:['two numeric exact-range endpoint controls','prepared invalid/range error timing','dynamic fraction binding','metadata and wire event differences'],cases},null,2)+'\n',{flag:'wx'})
  let checked=0
  for(const record of cases){
    const {name,local,reference}=record
    if(local.mode==='batch'){
      if(name.endsWith('expression 1.00000000000000000001'))continue
      assertSameCapture(diagnostics(local.result),diagnostics(reference.result),`${name}: diagnostic identity`)
      if(!reference.result.errors.length){
        assertSameCapture(rows(local.result),rows(reference.result),`${name}: exact values / FLOAT bits`)
        if(name.startsWith('CONT'))assertSameCapture(local.result.sets[0].columns.at(-1).type,'FloatN',`${name}: FLOAT result`)
      }
      checked++
    }else if(name==='prepared CONT character fraction'||name==='prepared DISC Unicode fraction'){
      assertSameCapture(diagnostics(local.preparation),[],`${name}: prepare failed`)
      assert.equal(local.executions.length,reference.executions.length)
      for(const [i,entry]of local.executions.entries()){
        assertSameCapture(diagnostics(entry.result),[],`${name}: execution failed`)
        assertSameCapture(rows(entry.result),rows(reference.executions[i].result),`${name}: rebound values / bits`)
      }
      assertSameCapture(diagnostics(local.unpreparation),[],`${name}: unprepare failed`)
    }
  }
  assert.equal(checked,101) // 100 percentile batches and final reuse; no hidden missing request
})

test('percentile constant cast widths descending zero and supplementary lexical probes agree', async t=>{
  const connection=await start(t)
  for(const fraction of ["CAST('0.55' AS VARCHAR(2))","CAST(N'0.55' AS NVARCHAR(2))","'-1e-400'"]){
    const result=await query(connection,`SELECT PERCENTILE_CONT(${fraction}) WITHIN GROUP(ORDER BY n DESC) OVER() AS p FROM (VALUES(1),(2),(3),(4))s(n)`)
    assert.deepEqual(result.rows,[[4],[4],[4],[4]])
  }
  for(const fraction of ["N'\u180e0.5'","N'0.5\0junk'","'1D-1'"]){
    const result=await query(connection,`SELECT PERCENTILE_CONT(${fraction}) WITHIN GROUP(ORDER BY n) OVER() AS p FROM(VALUES(1),(2),(3),(4))s(n)`)
    const expected=fraction==="'1D-1'"?1.3:2.5
    assertSameCapture(result.rows,[[expected],[expected],[expected],[expected]],'supplementary value')
  }
})

test('percentile conversion diagnostics remain catchable and explicit application identity is preserved',async t=>{
  const connection=await start(t)
  const result=await query(connection,"BEGIN TRY SELECT PERCENTILE_CONT('abc') WITHIN GROUP(ORDER BY n) OVER() AS p FROM(VALUES(1),(2))s(n); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n,ERROR_STATE() AS s,ERROR_MESSAGE() AS m; END CATCH")
  assertSameCapture(result.rows,[[8114,5,'Error converting data type varchar to float.']],'catchable conversion')
  const thrown=await capture(connection,"THROW 50001,'Error converting data type varchar to float.',7")
  assertSameCapture(diagnostics(thrown),[[50001,7,16,'Error converting data type varchar to float.']],'application identity')
  assertSameCapture((await query(connection,'SELECT 1 AS reusable')).rows,[[1]],'reuse')
})
