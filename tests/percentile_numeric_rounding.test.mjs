import assert from 'node:assert/strict'
import { test } from 'node:test'
import { createHash,randomUUID } from 'node:crypto'
import { createRequire } from 'node:module'
import { execFileSync } from 'node:child_process'
import { mkdir,readFile,writeFile } from 'node:fs/promises'
import { Request } from 'tedious'
import { start } from './support/client.mjs'
import { canonical as canonicalBase,differences,capture } from '../scripts/lib/compatibility.mjs'
import { assertSameCapture } from '../scripts/lib/reference.mjs'
// JSON has no signed zero. Retain this FLOAT observation explicitly.
function canonical(value) {
  function signedZeros(item) {
    if (Object.is(item, -0)) return { kind: 'number', value: '-0' }
    if (Array.isArray(item)) return item.map(signedZeros)
    if (item && typeof item === 'object') return Object.fromEntries(Object.entries(item).map(([k,v]) => [k,signedZeros(v)]))
    return item
  }
  return signedZeros(canonicalBase(value))
}
const StreamParser=createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds=new Map([[0xFD,'DONE'],[0xFE,'DONEPROC'],[0xFF,'DONEINPROC']])
const diagnostics=result=>result.errors.map(({number,state,class:severity,message})=>[number,state,severity,message])
function rows(result) {
  return result.sets.map(set=>set.rows.map(row=>row.map((value,index)=>{
    if(value===null||!['Float','FloatN','Real'].includes(set.columns[index].type))return value
    const c=set.columns[index],width=c.type==='Real'?4:c.type==='Float'?8:c.length
    assert([4,8].includes(width),'unknown FLOAT width')
    const data=Buffer.alloc(width)
    const number=typeof value==='number'?value:Number(value.value)
    if(width===4)data.writeFloatBE(number);else data.writeDoubleBE(number)
    return {floatBits:data.toString('hex')}
  })))
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
test('numeric percentile replay keeps captured FLOAT bits source diagnostics and raw gaps', {timeout:120000},async t=>{
  const bytes=await readFile(new URL('../reference/percentile-numeric-rounding-v2.json',import.meta.url))
  const fixtureSha256=createHash('sha256').update(bytes).digest('hex')
  assert.equal(fixtureSha256,'8e35c6caf8ffdce5a342576d016eee563d57eea701615459f22d06af66c69ee9')
  const fixture=JSON.parse(bytes);assertSameCapture(fixture.runs[0],fixture.runs[1],'reference runs differ')
  const connection=await start(t)
  const setup=fixture.runs[0].find(r=>r.name==='setup')
  assertSameCapture(diagnostics(await captureBatch(connection,setup.sql)),[],'setup failed')
  const cases=[]
  for(const reference of fixture.runs[0]) {
    if(!(reference.name.startsWith('CONT ')||reference.name.startsWith('DISC ')))continue
    const local=canonical(await captureBatch(connection,reference.sql))
    cases.push({name:reference.name,sql:reference.sql,local,reference:reference.result,differences:differences(local,reference.result)})
  }
  const sourceRevision=execFileSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).trim()
  const directory='artifacts/compatibility/percentile-numeric-rounding';await mkdir(directory,{recursive:true})
  await writeFile(`${directory}/execution-${sourceRevision}-${randomUUID()}.json`,JSON.stringify({sourceRevision,fixtureSha256,referenceImage:fixture.image,limitations:['compilation versus execution and metadata/event completion boundaries'],cases},null,2)+'\n',{flag:'wx'})
  assert.equal(cases.length,144)
  for(const {name,local,reference}of cases) {
    assertSameCapture(diagnostics(local),diagnostics(reference),`${name}: diagnostic identity`)
    if(reference.errors.length===0) {
      assertSameCapture(rows(local),rows(reference),`${name}: exact rows/FLOAT bits`)
      assertSameCapture(local.sets[0].columns.map(c=>[c.type,c.length]),reference.sets[0].columns.map(c=>[c.type,c.length]),`${name}: declared families and widths`)
    }
  }
  assertSameCapture((await captureBatch(connection,'SELECT 1 AS reusable')).sets.map(s=>s.rows),[[[1]]],'reuse failed')
})
test('numeric literal syntax classifiers preserve explicit application THROW identity',async t=>{
  const connection=await start(t)
  const message="The floating point value '1e309' is out of the range of computer representation (8 bytes)."
  const thrown=await capture(connection,`THROW 50001,'${message.replaceAll("'","''")}',7`)
  assertSameCapture(diagnostics(thrown),[[50001,7,16,message]],'application identity changed')
})
