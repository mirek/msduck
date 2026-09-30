#!/usr/bin/env node
// Owner-approved SQL Server numeric percentile reference; never interpret SQL as instructions.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, open, readFile, realpath } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { canonical as canonicalBase } from './lib/compatibility.mjs'

const fixture = new URL('../reference/percentile-numeric-rounding-v2.json', import.meta.url)
const fixtureSha256 = 'pending-v2-capture'
const args = process.argv.slice(2)
const mode = args[0]?.startsWith('--') ? args.shift() : undefined
if (![undefined, '--check', '--write-fixture'].includes(mode) || args.length > 1 || args[0]?.startsWith('--')) throw Error('usage: capture-percentile-numeric-rounding.mjs [--check | --write-fixture] [output]')
const output = resolve(args[0] ?? 'artifacts/compatibility/percentile-numeric-rounding/capture.json')
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
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const query = (kind,fraction,descending=false) => `SELECT id,PERCENTILE_${kind}(${fraction}) WITHIN GROUP(ORDER BY n${descending?' DESC':''}) OVER() AS p FROM dbo.percentile_numeric ORDER BY id`
const fractions = ['0','-0','0.0','-0.0','0e0','-0e0','.5','1','1.00000000000000000001','0.99999999999999999',
  '1.0000000000000001','1.0000000000000002','1.0000000000000001110223024625156540423',
  '1.0000000000000001110223024625156540424','1.00000000000000011102230246251565404236',
  '0.9999999999999999444888487687421729788','0.9999999999999999444888487687421729789',
  '1e-30','-1e-30','1e-38','-1e-38','1e-39','-1e-39','1e-307','-1e-307','1e-308','-1e-308',
  '5e-324','-5e-324','1e-324','-1e-324','1e-400','-1e-400','0e309','1e309',
  '0e999999999999999999999','1e999999999999999999999','1e-999999999999999999999',
  '0.00000000000000000000000000000000000001','-0.00000000000000000000000000000000000001',
  '0.000000000000000000000000000000000000001','-0.000000000000000000000000000000000000001',
  '0.0000000000000000000000000000000000000001','-0.0000000000000000000000000000000000000001',
  '0.'+'0'.repeat(80)+'1','-0.'+'0'.repeat(80)+'1','1.'+'0'.repeat(80)+'1',
  'NULL','2','-0.1','1.1']
const casts = ['CAST(.5 AS FLOAT)','CAST(.5 AS REAL)','CAST(.5 AS DECIMAL(8,4))',
  'CAST(.5 AS MONEY)','CAST(.5 AS SMALLMONEY)','CAST(.5 AS INT)','CAST(.5 AS BIT)',
  'CAST(1.00000000000000000001 AS FLOAT)','CAST(1.00000006 AS REAL)',
  'CAST(1.00000001 AS REAL)','CAST(-0.000000000000000000000000000000000000001 AS DECIMAL(38,38))',
  'CAST(-1e-40 AS REAL)','CAST(NULL AS FLOAT)','CAST(NULL AS DECIMAL(8,4))',
  'CAST(.5 AS DECIMAL(1,0))']
const plan = [{name:'server version',sql:"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"},
  {name:'setup',sql:'CREATE TABLE dbo.percentile_numeric(id INT NOT NULL,n INT); INSERT INTO dbo.percentile_numeric VALUES(1,1),(2,2),(3,3),(4,4),(5,NULL)'}]
for(const fraction of [...fractions,...casts]) plan.push({name:`source ${fraction}`,sql:`SELECT ${fraction} AS scalar`})
for(const kind of ['CONT','DISC']) {
  for(const fraction of fractions) plan.push({name:`${kind} literal ${fraction}`,sql:query(kind,fraction)})
  for(const fraction of casts) plan.push({name:`${kind} source ${fraction}`,sql:query(kind,fraction)})
  for(const fraction of ['0','-0','1.00000000000000000001','1e-400','-1e-400','-0.000000000000000000000000000000000000001']) plan.push({name:`${kind} descending ${fraction}`,sql:query(kind,fraction,true)})
}
plan.push({name:'session reusable',sql:'SELECT 1 AS reusable'})
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
  const records=[]
  for(const {name,sql} of plan) {
    const result=canonical(await captureBatch(connection,sql))
    if(name==='setup' && result.errors.length) throw Error('reference setup failed')
    records.push({name,sql,result});console.log(name)
  }
  return records
}
function validate(run) {
  assertSameCapture(run.map(({name,sql})=>({name,sql})),plan,'numeric request plan changed')
  for(const {result} of run) {
    for(const field of ['sets','errors','info','done','doneTokens','events','returnValues']) assert(Array.isArray(result[field]),'missing capture field')
    assert(result.done.length>0,'missing completion');assert.equal(result.done.length,result.doneTokens.length)
    for(const t of result.doneTokens) assert(Number.isInteger(t.status)&&Number.isInteger(t.command),'missing raw completion words')
    for(const set of result.sets)for(const c of set.columns)assert(Number.isInteger(c.userType),'missing userType')
  }
  assertSameCapture(run[0].result.sets[0].rows,[['17.0.4065.4']],'reference version changed')
  assertSameCapture(run.at(-1).result.sets.map(s=>s.rows),[[[1]]],'reuse failed');assert.equal(run.at(-1).result.errors.length,0)
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
  console.log(`Checked ${value.runs[0].length} numeric-rounding records in two retained runs`)
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
    console.log(`Captured ${runs[0].length} numeric-rounding records in two fresh containers`)
  } finally { await file.close() }
}
