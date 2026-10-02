#!/usr/bin/env node
// Retain SQL Server CONCAT_WS and TRANSLATE rows, descriptors, diagnostics and completions.
// Usage: capture-concat-ws-translate.mjs [output] [--write-fixture | --one-database | --check-fixture]
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { isDeepStrictEqual } from 'node:util'
import { resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { referenceImage, withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'

const fixture = new URL('../reference/concat-ws-translate.json', import.meta.url)
const flags = ['--write-fixture', '--one-database', '--check-fixture']
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const checkFixture = args.includes('--check-fixture')
const positional = args.filter(arg => !flags.includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
if (checkFixture && (writeFixture || oneDatabase || positional.length)) throw new Error('--check-fixture takes no other arguments')
const output = resolve(positional[0] ?? 'artifacts/compatibility/concat-ws-translate/capture.json')

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
const message = token => ({ number: token.number, state: token.state, class: token.class, lineNumber: token.lineNumber, procName: token.procName, message: token.message })
const done = token => ({ status: token.raw.status, command: token.raw.command, count: token.raw.count })
for (const [method, describe] of Object.entries({
  onColMetadata: token => ({ columns: token.columns.map(column => column.colName) }),
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

const list = (count, value) => Array.from({ length: count }, () => value).join(',')

const concatWsCases = [
  ['cws ansi basic', "SELECT CONCAT_WS(',','a','b','c') AS value"],
  ['cws unicode basic', "SELECT CONCAT_WS(N',',N'a',N'b',N'c') AS value"],
  ['cws null argument skipped', "SELECT CONCAT_WS(',','a',NULL,'b') AS value"],
  ['cws typed null arguments skipped', "SELECT CONCAT_WS(',',CAST(NULL AS VARCHAR(5)),'a',CAST(NULL AS NVARCHAR(5)),'b',CAST(NULL AS INT)) AS value"],
  ['cws all null arguments', "SELECT CONCAT_WS(',',NULL,NULL) AS value"],
  ['cws all typed null arguments', "SELECT CONCAT_WS(',',CAST(NULL AS VARCHAR(5)),CAST(NULL AS VARCHAR(5))) AS value"],
  ['cws null literal separator', "SELECT CONCAT_WS(NULL,'a','b') AS value"],
  ['cws typed null separator', "SELECT CONCAT_WS(CAST(NULL AS VARCHAR(3)),'a','b') AS value"],
  ['cws all null with null separator', "SELECT CONCAT_WS(NULL,NULL,NULL) AS value"],
  ['cws empty separator', "SELECT CONCAT_WS('','a','b') AS value"],
  ['cws empty string arguments kept', "SELECT CONCAT_WS(',','a','','b','') AS value"],
  ['cws multi character separator', "SELECT CONCAT_WS(' - ','a ',' b') AS value"],
  ['cws char padding', "SELECT CONCAT_WS('|',CAST('a' AS CHAR(3)),CAST('b' AS NCHAR(2))) AS value"],
  ['cws one argument', "SELECT CONCAT_WS(',','a') AS value"],
  ['cws separator only null', "SELECT CONCAT_WS(NULL) AS value"],
  ['cws no arguments', 'SELECT CONCAT_WS() AS value'],
  ['cws 254 arguments', `SELECT CONCAT_WS('',${list(253, "'x'")}) AS value`],
  ['cws 255 arguments', `SELECT CONCAT_WS('',${list(254, "'x'")}) AS value`],
  ['cws 256 arguments', `SELECT CONCAT_WS('',${list(255, "'x'")}) AS value`],
  ['cws varchar widths', "SELECT CONCAT_WS('-',CAST('a' AS VARCHAR(10)),CAST('b' AS VARCHAR(20))) AS value"],
  ['cws varchar widths with long separator', "SELECT CONCAT_WS(CAST('--' AS VARCHAR(7)),CAST('a' AS VARCHAR(10)),CAST('b' AS VARCHAR(20)),CAST('c' AS VARCHAR(30))) AS value"],
  ['cws nvarchar widths', "SELECT CONCAT_WS(N'-',CAST(N'a' AS NVARCHAR(10)),CAST(N'b' AS NVARCHAR(20))) AS value"],
  ['cws mixed varchar nvarchar', "SELECT CONCAT_WS('-',CAST('a' AS VARCHAR(10)),CAST(N'b' AS NVARCHAR(20))) AS value"],
  ['cws unicode separator ansi arguments', "SELECT CONCAT_WS(N'-',CAST('a' AS VARCHAR(10)),CAST('b' AS VARCHAR(20))) AS value"],
  ['cws varchar width over 8000', "SELECT CONCAT_WS('-',CAST('a' AS VARCHAR(5000)),CAST('b' AS VARCHAR(5000))) AS value"],
  ['cws nvarchar width over 4000', "SELECT CONCAT_WS(N'-',CAST(N'a' AS NVARCHAR(3000)),CAST(N'b' AS NVARCHAR(3000))) AS value"],
  ['cws varchar long value', "SELECT LEN(CONCAT_WS('-',CAST(REPLICATE('x',5000) AS VARCHAR(5000)),CAST(REPLICATE('y',5000) AS VARCHAR(5000)))) AS length,DATALENGTH(CONCAT_WS('-',CAST(REPLICATE('x',5000) AS VARCHAR(5000)),CAST(REPLICATE('y',5000) AS VARCHAR(5000)))) AS bytes"],
  ['cws nvarchar long value', "SELECT LEN(CONCAT_WS(N'-',CAST(REPLICATE(N'x',3000) AS NVARCHAR(3000)),CAST(REPLICATE(N'y',3000) AS NVARCHAR(3000)))) AS length,DATALENGTH(CONCAT_WS(N'-',CAST(REPLICATE(N'x',3000) AS NVARCHAR(3000)),CAST(REPLICATE(N'y',3000) AS NVARCHAR(3000)))) AS bytes"],
  ['cws varchar max argument', "SELECT CONCAT_WS('-',CAST('a' AS VARCHAR(MAX)),'b') AS value"],
  ['cws nvarchar max argument', "SELECT CONCAT_WS('-',CAST(N'a' AS NVARCHAR(MAX)),'b') AS value"],
  ['cws varchar max separator', "SELECT CONCAT_WS(CAST('-' AS VARCHAR(MAX)),'a','b') AS value"],
  ['cws varchar max long value', "SELECT DATALENGTH(CONCAT_WS('-',CAST(REPLICATE('x',5000) AS VARCHAR(MAX)),REPLICATE('y',5000))) AS bytes"],
  ['cws integers', "SELECT CONCAT_WS(',',1,-2,CAST(3 AS TINYINT),CAST(4 AS SMALLINT),CAST(5 AS BIGINT)) AS value"],
  ['cws integer separator', "SELECT CONCAT_WS(0,'a','b') AS value"],
  ['cws decimal float money bit', "SELECT CONCAT_WS('|',CAST(1.50 AS DECIMAL(8,2)),CAST(1.5 AS FLOAT),CAST(1.5 AS REAL),CAST(2.5 AS MONEY),CAST(1 AS BIT)) AS value"],
  ['cws dates', "SELECT CONCAT_WS('|',CAST('2024-01-02' AS DATE),CAST('2024-01-02T03:04:05.123' AS DATETIME),CAST('2024-01-02T03:04:05.1234567' AS DATETIME2),CAST('2024-01-02T03:04:00' AS SMALLDATETIME),CAST('03:04:05.12' AS TIME(2)),CAST('2024-01-02T03:04:05+01:30' AS DATETIMEOFFSET(0))) AS value"],
  ['cws uniqueidentifier', "SELECT CONCAT_WS('|',CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS UNIQUEIDENTIFIER),'x') AS value"],
  ['cws binary argument', "SELECT CONCAT_WS('|',0x4142,'x') AS value"],
  ['cws sql_variant argument', "SELECT CONCAT_WS('|',CAST(1 AS SQL_VARIANT),'x') AS value"],
  ['cws xml argument', "SELECT CONCAT_WS('|',CAST('<a/>' AS XML),'x') AS value"],
  ['cws explicit collation', "SELECT CONCAT_WS(',','a' COLLATE Latin1_General_100_BIN2,'b') AS value"],
  ['cws conflicting explicit collations', "SELECT CONCAT_WS(',','a' COLLATE Latin1_General_100_BIN2,'b' COLLATE Latin1_General_100_CS_AS) AS value"],
  ['cws implicit column collations', 'SELECT CONCAT_WS(N\',\',bin,cs) AS value FROM dbo.cwt_src WHERE id=1'],
  ['cws supplementary unicode', "SELECT CONCAT_WS(N'😀',N'a',N'𝄞') AS value,LEN(CONCAT_WS(N'😀',N'a',N'𝄞')) AS length"],
  ['cws supplementary to varchar', "SELECT CONCAT_WS('-',N'😀','a') AS value"],
  ['cws lone surrogate', "SELECT CONCAT_WS(N'-',NCHAR(55357),N'a') AS value,DATALENGTH(CONCAT_WS(N'-',NCHAR(55357),N'a')) AS bytes"],
  ['cws column rows', 'SELECT id,CONCAT_WS(N\',\',a,u,n) AS value FROM dbo.cwt_src ORDER BY id'],
  ['cws column null separator', 'SELECT id,CONCAT_WS(sep,a,u) AS value FROM dbo.cwt_src ORDER BY id'],
  ['cws in predicate', "SELECT id FROM dbo.cwt_src WHERE CONCAT_WS(',',a,u)=N'a,u' ORDER BY id"],
]

const translateCases = [
  ['tr ansi basic', "SELECT TRANSLATE('2*[3+4]/{7-2}','[]{}','()()') AS value"],
  ['tr unicode basic', "SELECT TRANSLATE(N'2*[3+4]/{7-2}',N'[]{}',N'()()') AS value"],
  ['tr null input', "SELECT TRANSLATE(CAST(NULL AS VARCHAR(10)),'a','b') AS value"],
  ['tr null literal input', "SELECT TRANSLATE(NULL,'a','b') AS value"],
  ['tr null characters', "SELECT TRANSLATE('abc',NULL,'b') AS value"],
  ['tr null translations', "SELECT TRANSLATE('abc','a',NULL) AS value"],
  ['tr empty characters', "SELECT TRANSLATE('abc','','') AS value"],
  ['tr empty input', "SELECT TRANSLATE('','a','b') AS value"],
  ['tr length mismatch longer', "SELECT TRANSLATE('abc','ab','x') AS value"],
  ['tr length mismatch shorter', "SELECT TRANSLATE('abc','a','xy') AS value"],
  ['tr trailing space mismatch', "SELECT TRANSLATE('a b','a ','x') AS value"],
  ['tr trailing space translation', "SELECT TRANSLATE('a b','a ','x_') AS value"],
  ['tr duplicate characters', "SELECT TRANSLATE('aab','aa','xy') AS value"],
  ['tr no chaining', "SELECT TRANSLATE('abc','ab','bc') AS value"],
  ['tr case insensitive default', "SELECT TRANSLATE('ABCabc','abc','xyz') AS value"],
  ['tr accent insensitive probe', "SELECT TRANSLATE(N'eéE',N'e',N'x') AS value"],
  ['tr binary collation', "SELECT TRANSLATE('ABCabc' COLLATE Latin1_General_100_BIN2,'abc','xyz') AS value"],
  ['tr case sensitive collation', "SELECT TRANSLATE('ABCabc' COLLATE Latin1_General_100_CS_AS,'abc','xyz') AS value"],
  ['tr conflicting explicit collations', "SELECT TRANSLATE('abc' COLLATE Latin1_General_100_BIN2,'a' COLLATE Latin1_General_100_CS_AS,'x') AS value"],
  ['tr varchar width', "SELECT TRANSLATE(CAST('abc' AS VARCHAR(10)),CAST('a' AS VARCHAR(30)),CAST('x' AS VARCHAR(40))) AS value"],
  ['tr nvarchar width', "SELECT TRANSLATE(CAST(N'abc' AS NVARCHAR(10)),N'a',N'x') AS value"],
  ['tr varchar input unicode characters', "SELECT TRANSLATE(CAST('abc' AS VARCHAR(10)),N'a',N'x') AS value"],
  ['tr nvarchar input ansi characters', "SELECT TRANSLATE(CAST(N'abc' AS NVARCHAR(10)),'a','x') AS value"],
  ['tr char input', "SELECT TRANSLATE(CAST('ab' AS CHAR(5)),'b ','x_') AS value"],
  ['tr varchar max input', "SELECT TRANSLATE(CAST('abc' AS VARCHAR(MAX)),'a','x') AS value"],
  ['tr nvarchar max input', "SELECT TRANSLATE(CAST(N'abc' AS NVARCHAR(MAX)),N'a',N'x') AS value"],
  ['tr varchar max characters', "SELECT TRANSLATE(CAST('abc' AS VARCHAR(10)),CAST('a' AS VARCHAR(MAX)),'x') AS value"],
  ['tr long varchar max value', "SELECT DATALENGTH(TRANSLATE(REPLICATE(CAST('a' AS VARCHAR(MAX)),9000),'a','b')) AS bytes,LEFT(TRANSLATE(REPLICATE(CAST('a' AS VARCHAR(MAX)),9000),'a','b'),3) AS prefix"],
  ['tr long nvarchar max value', "SELECT DATALENGTH(TRANSLATE(REPLICATE(CAST(N'a' AS NVARCHAR(MAX)),9000),N'a',N'b')) AS bytes"],
  ['tr integer input', "SELECT TRANSLATE(12321,'1','9') AS value"],
  ['tr decimal input', "SELECT TRANSLATE(CAST(1.50 AS DECIMAL(8,2)),'.','') AS value"],
  ['tr date input', "SELECT TRANSLATE(CAST('2024-01-02' AS DATE),'-','/') AS value"],
  ['tr binary input', "SELECT TRANSLATE(0x4142,'A','B') AS value"],
  ['tr integer characters', "SELECT TRANSLATE('a1b','1',2) AS value"],
  ['tr two arguments', "SELECT TRANSLATE('abc','a') AS value"],
  ['tr four arguments', "SELECT TRANSLATE('abc','a','b','c') AS value"],
  ['tr supplementary default collation', "SELECT TRANSLATE(N'a😀b',N'😀',N'xy') AS value"],
  ['tr supplementary default collation mismatch', "SELECT TRANSLATE(N'a😀b',N'😀',N'x') AS value"],
  ['tr supplementary sc collation', "SELECT TRANSLATE(N'a😀b' COLLATE Latin1_General_100_CI_AS_SC,N'😀',N'x') AS value"],
  ['tr supplementary sc collation pair', "SELECT TRANSLATE(N'a😀b' COLLATE Latin1_General_100_CI_AS_SC,N'😀',N'xy') AS value"],
  ['tr supplementary replacement', "SELECT TRANSLATE(N'abc' COLLATE Latin1_General_100_CI_AS_SC,N'b',N'😀') AS value"],
  ['tr lone surrogate', "SELECT TRANSLATE(N'a'+NCHAR(55357)+N'b',NCHAR(55357),N'x') AS value"],
  ['tr utf8 collation', "SELECT TRANSLATE(CAST(N'aéb' AS VARCHAR(10)) COLLATE Latin1_General_100_CI_AS_SC_UTF8,N'é',N'e') AS value"],
  ['tr column rows', 'SELECT id,TRANSLATE(a,\'au\',\'AU\') AS value FROM dbo.cwt_src ORDER BY id'],
  ['tr column mismatch row', "SELECT id,TRANSLATE(N'a-b+c',sep,N'x') AS value FROM dbo.cwt_src ORDER BY id"],
]

const cases = [...concatWsCases, ...translateCases]

const errorFields = e => ({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })

async function observed(connection, sql, parameters) {
  const tokens = []
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      taps.set(request, tokens)
      if (!parameters) return connection.execSqlBatch(request)
      for (const [name, type, value, options] of parameters) request.addParameter(name, type, value, options)
      connection.execSql(request)
    },
  }
  return { ...canonical(await capture(transport, sql)), tokens }
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
  request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(x => ({ name: x.colName, type: x.type.name, length: x.dataLength ?? null, precision: x.precision ?? null, scale: x.scale ?? null, flags: x.flags, collation: canonical(x.collation ?? null) })), rows: [] }))
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

const rpcPrograms = [
  ['rpc cws unicode', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, ',', { length: 5 }], ['a', TYPES.NVarChar, 'a', { length: 10 }], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc cws ansi', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.VarChar, ',', { length: 5 }], ['a', TYPES.VarChar, 'a', { length: 10 }], ['b', TYPES.VarChar, 'b', { length: 20 }]]],
  ['rpc cws null separator', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, null, { length: 5 }], ['a', TYPES.NVarChar, 'a', { length: 10 }], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc cws null argument', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, ',', { length: 5 }], ['a', TYPES.NVarChar, null, { length: 10 }], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc cws mixed types', "SELECT CONCAT_WS(@sep,@a,@n,@d) AS value", [
    ['sep', TYPES.VarChar, '|', { length: 1 }], ['a', TYPES.NVarChar, 'a', { length: 10 }], ['n', TYPES.Int, 42], ['d', TYPES.Date, new Date('2024-01-02T00:00:00Z')]]],
  ['rpc cws inferred length argument', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, ',', { length: 5 }], ['a', TYPES.NVarChar, 'a'], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc cws max argument', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, ',', { length: 5 }], ['a', TYPES.NVarChar, 'a', { length: 5000 }], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc cws unicode replay', "SELECT CONCAT_WS(@sep,@a,@b) AS value", [
    ['sep', TYPES.NVarChar, ',', { length: 5 }], ['a', TYPES.NVarChar, 'a', { length: 10 }], ['b', TYPES.NVarChar, 'b', { length: 20 }]]],
  ['rpc tr unicode', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, '[a]', { length: 20 }], ['from', TYPES.NVarChar, '[]', { length: 5 }], ['to', TYPES.NVarChar, '()', { length: 5 }]]],
  ['rpc tr ansi', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.VarChar, '[a]', { length: 20 }], ['from', TYPES.VarChar, '[]', { length: 5 }], ['to', TYPES.VarChar, '()', { length: 5 }]]],
  ['rpc tr max input', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, '[a]', { length: 5000 }], ['from', TYPES.NVarChar, '[]', { length: 5 }], ['to', TYPES.NVarChar, '()', { length: 5 }]]],
  ['rpc tr null input', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, null, { length: 20 }], ['from', TYPES.NVarChar, '[]', { length: 5 }], ['to', TYPES.NVarChar, '()', { length: 5 }]]],
  ['rpc tr length mismatch', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, '[a]', { length: 20 }], ['from', TYPES.NVarChar, '[]', { length: 5 }], ['to', TYPES.NVarChar, '(', { length: 5 }]]],
  ['rpc tr mismatch then select', "SELECT TRANSLATE(@input,@from,@to) AS value; SELECT 1 AS after_error", [
    ['input', TYPES.NVarChar, 'abc', { length: 20 }], ['from', TYPES.NVarChar, 'ab', { length: 5 }], ['to', TYPES.NVarChar, 'x', { length: 5 }]]],
  ['rpc tr unicode replay', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, '[a]', { length: 20 }], ['from', TYPES.NVarChar, '[]', { length: 5 }], ['to', TYPES.NVarChar, '()', { length: 5 }]]],
]

const preparedPrograms = [
  ['prepared cws', 'SELECT CONCAT_WS(@sep,@a,@b) AS value', [
    ['sep', TYPES.NVarChar, { length: 5 }], ['a', TYPES.NVarChar, { length: 10 }], ['b', TYPES.VarChar, { length: 20 }]], [
    { sep: ',', a: 'a', b: 'b' },
    { sep: null, a: 'a', b: 'b' },
    { sep: ',', a: null, b: 'b' },
    { sep: ',', a: null, b: null },
    { sep: '😀', a: '𝄞', b: 'b' },
  ]],
  ['prepared tr', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.NVarChar, { length: 20 }], ['from', TYPES.NVarChar, { length: 5 }], ['to', TYPES.NVarChar, { length: 5 }]], [
    { input: '[a]', from: '[]', to: '()' },
    { input: null, from: '[]', to: '()' },
    { input: '[a]', from: '[]', to: '(' },
    { input: '[a]', from: '', to: '' },
    { input: 'a😀b', from: '😀', to: 'xy' },
    { input: '[a]', from: '[]', to: '()' },
  ]],
  ['prepared tr ansi', 'SELECT TRANSLATE(@input,@from,@to) AS value', [
    ['input', TYPES.VarChar, { length: 20 }], ['from', TYPES.VarChar, { length: 5 }], ['to', TYPES.VarChar, { length: 5 }]], [
    { input: 'abc', from: 'ab', to: 'xy' },
    { input: 'abc', from: 'ab', to: 'x' },
  ]],
]

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    if (process.env.CWT_TRACE) console.error(name)
    const result = canonical(await observed(connection, sql, parameters))
    records.push({
      name, sql,
      ...(parameters ? { parameters: parameters.map(([parameter, type, value, options]) => ({
        name: parameter, type: type.name, value: canonical(value), ...(options ? { options } : {}),
      })) } : {}),
      result,
    })
  }
  await record('server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version,CAST(SERVERPROPERTY('Collation') AS NVARCHAR(128)) AS server_collation,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
  await record('create source', 'CREATE TABLE dbo.cwt_src(id INT,sep NVARCHAR(3),a VARCHAR(10),u NVARCHAR(10),n INT,bin VARCHAR(5) COLLATE Latin1_General_100_BIN2,cs VARCHAR(5) COLLATE Latin1_General_100_CS_AS)')
  await record('insert source', "INSERT dbo.cwt_src VALUES (1,N'-','a',N'u',1,'x','y'),(2,NULL,'a',NULL,NULL,NULL,NULL),(3,N'+',NULL,N'u',3,NULL,NULL),(4,N'',NULL,NULL,NULL,NULL,NULL)")
  for (const [name, sql] of cases) await record(name, sql)
  for (const [name, sql, parameters] of rpcPrograms) await record(name, sql, parameters)
  for (const [name, sql, declarations, valueSets] of preparedPrograms) {
    if (process.env.CWT_TRACE) console.error(name)
    records.push({
      name, sql,
      declarations: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, ...(options ? { options } : {}) })),
      prepared: await prepared(connection, sql, declarations, valueSets),
    })
  }
  await record('connection reusable', 'SELECT @@TRANCOUNT AS transaction_count,1 AS reusable')
  return records
}

function validate(run) {
  assert.equal(run.length, 3 + cases.length + rpcPrograms.length + preparedPrograms.length + 1)
  const get = name => {
    const found = run.find(record => record.name === name)
    assert(found, 'missing ' + name)
    return found.result
  }
  const rows = name => get(name).sets[0]?.rows
  const errorNumber = name => get(name).errors[0]?.number
  assert.deepEqual(rows('cws ansi basic'), [['a,b,c']])
  assert.deepEqual(rows('cws null argument skipped'), [['a,b']])
  assert.deepEqual(rows('cws null literal separator'), [['ab']])
  assert.deepEqual(rows('cws empty string arguments kept'), [['a,,b,']])
  assert.deepEqual(rows('tr ansi basic'), [['2*(3+4)/(7-2)']])
  assert.deepEqual(rows('tr null input'), [[null]])
  assert.deepEqual(rows('rpc tr unicode'), [['(a)']])
  assert.equal(get('cws 254 arguments').errors.length, 0)
  // Documented diagnostics: [record, number, state].
  for (const [name, number, state] of [
    ['cws one argument', 189, 1],
    ['cws separator only null', 189, 1],
    ['cws no arguments', 189, 1],
    ['cws 255 arguments', 189, 1],
    ['cws 256 arguments', 189, 1],
    ['cws sql_variant argument', 257, 3],
    ['cws xml argument', 257, 3],
    ['cws conflicting explicit collations', 468, 9],
    ['cws implicit column collations', 451, 1],
    ['tr length mismatch longer', 9828, 1],
    ['tr length mismatch shorter', 9828, 1],
    ['tr trailing space mismatch', 9828, 1],
    ['tr decimal input', 9828, 1],
    ['tr conflicting explicit collations', 468, 9],
    ['tr integer characters', 8116, 1],
    ['tr two arguments', 174, 1],
    ['tr four arguments', 174, 1],
    ['tr supplementary default collation mismatch', 9828, 3],
    ['tr supplementary sc collation pair', 9828, 3],
    ['tr column mismatch row', 9828, 3],
    ['rpc tr length mismatch', 9828, 3],
    ['rpc tr mismatch then select', 9828, 3],
  ]) {
    const errors = get(name).errors
    assert.equal(errors.length, 1, name + ': error count')
    assert.equal(errors[0].number, number, name + ': error number')
    assert.equal(errors[0].state, state, name + ': error state')
  }
  assert.equal(get('rpc tr length mismatch').returnStatus, 9828)
  assert.equal(get('rpc tr mismatch then select').returnStatus, 0)
  for (const [name, index, number, state, status] of [
    ['prepared tr', 2, 9828, 3, -6],
    ['prepared tr ansi', 1, 9828, 1, -6],
  ]) {
    const execution = run.find(record => record.name === name).prepared.executions[index].result
    assert.equal(execution.errors.length, 1, name + ': prepared error count')
    assert.equal(execution.errors[0].number, number, name + ': prepared error number')
    assert.equal(execution.errors[0].state, state, name + ': prepared error state')
    assert.equal(execution.returnStatus, status, name + ': prepared return status')
  }
  // No other record may carry an error.
  const expectedErrors = new Set(['cws one argument', 'cws separator only null', 'cws no arguments', 'cws 255 arguments',
    'cws 256 arguments', 'cws sql_variant argument', 'cws xml argument', 'cws conflicting explicit collations',
    'cws implicit column collations', 'tr length mismatch longer', 'tr length mismatch shorter', 'tr trailing space mismatch',
    'tr decimal input', 'tr conflicting explicit collations', 'tr integer characters', 'tr two arguments', 'tr four arguments',
    'tr supplementary default collation mismatch', 'tr supplementary sc collation pair', 'tr column mismatch row',
    'rpc tr length mismatch', 'rpc tr mismatch then select'])
  for (const record of run) {
    if (record.result && !expectedErrors.has(record.name)) {
      assert.equal(record.result.errors.length, 0, record.name + ': unexpected error')
      if (record.parameters) assert.equal(record.result.returnStatus, 0, record.name + ': successful RPC return status')
    }
  }
  // Documented descriptor flags apply to each independently captured result.
  for (const record of run) {
    const results = record.result ? [record.result] :
      [record.prepared.prepare, ...record.prepared.executions.map(execution => execution.result)]
    for (const result of results) for (const set of result.sets) {
      const column = set.columns.find(c => c.name === 'value')
      if (!column) continue
      const concat = /^(cws|rpc cws|prepared cws)/.test(record.name)
      const explicit = ['cws explicit collation', 'tr binary collation', 'tr case sensitive collation'].includes(record.name)
      assert.equal(column.flags, (concat ? 32 : 33) | (explicit ? 2 : 0), record.name + ': value flags')
      if (record.prepared && !isDeepStrictEqual(set.columns, record.prepared.prepare.sets[0].columns)) {
        throw new Error(record.name + ': prepared execution descriptor differs from preparation')
      }
    }
  }
  for (const record of run) {
    if (record.prepared) {
      assert.equal(record.prepared.prepare.prepared, true, record.name + ': preparation failed')
      assert.equal(record.prepared.prepare.errors.length, 0, record.name + ': prepare error')
      assert.equal(record.prepared.prepare.returnStatus, 0, record.name + ': prepare return status')
      for (const [index, execution] of record.prepared.executions.entries()) {
        const expectedFailure = (record.name === 'prepared tr' && index === 2) ||
          (record.name === 'prepared tr ansi' && index === 1)
        if (!expectedFailure) {
          assert.equal(execution.result.errors.length, 0, record.name + ': unexpected prepared execution error at ' + index)
          assert.equal(execution.result.returnStatus, 0, record.name + ': unexpected prepared return status at ' + index)
        }
      }
      assert.equal(record.prepared.unprepare.errors.length, 0, record.name + ': unprepare error')
      assert.equal(record.prepared.unprepare.returnStatus, 0, record.name + ': unprepare return status')
      for (const phase of [record.prepared.prepare, ...record.prepared.executions.map(e => e.result), record.prepared.unprepare]) {
        assert(phase.done.length > 0, record.name + ': no prepared completion')
      }
    } else assert(record.result.done.length > 0, record.name + ': no completion')
  }
}

// Never hand whole captures to node:assert: a failing assertion would render them.
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
      if (record.parameters || record.prepared) {
        const statuses = phase.tokens?.filter(token => token.token === 'RETURNSTATUS')
        if (statuses?.length !== 1 || statuses[0].value !== phase.returnStatus) {
          throw new Error(record.name + ': missing or inconsistent raw RETURNSTATUS token')
        }
      }
      if (!Array.isArray(phase.tokens) || !phase.tokens.length || !/^DONE(PROC)?$/.test(phase.tokens.at(-1).token)) {
        throw new Error(record.name + ': missing complete ordered token evidence')
      }
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
  if (actual.containers.length !== 2) throw new Error('expected two containers')
  for (const container of actual.containers) {
    if (container.image !== referenceImage) throw new Error('unexpected reference image ' + container.image)
    if (container.runs.length !== 2) throw new Error('expected two fresh databases per container')
    for (const run of container.runs) { validate(run); validateTokens(run) }
    requireSame(container.runs[0], container.runs[1], 'across fresh databases')
  }
  requireSame(actual.containers[0].runs[0], actual.containers[1].runs[0], 'across containers')
}

async function readRetained() {
  try { return JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
}

if (checkFixture) {
  const retained = await readRetained()
  if (!retained) throw new Error('no retained fixture')
  validateFourCaptures(retained)
  console.log('Retained fixture holds ' + retained.containers[0].runs[0].length + ' matching CONCAT_WS/TRANSLATE observations in four fresh databases across two containers')
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
    })
  }
  const actual = { containers }
  // Retain the raw artifact before validation so a failing capture can be inspected.
  await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
  if (oneDatabase) { validate(containers[0].runs[0]); validateTokens(containers[0].runs[0]) }
  else validateFourCaptures(actual)
  const retained = writeFixture ? undefined : await readRetained()
  if (retained && !oneDatabase) {
    for (let c = 0; c < 2; c++) for (let d = 0; d < 2; d++) {
      requireSame(actual.containers[c].runs[d], retained.containers[c].runs[d], 'from retained fixture (container ' + c + ', database ' + d + ')')
    }
    if (!isDeepStrictEqual(actual, retained)) throw new Error('CONCAT_WS/TRANSLATE capture envelope differs from retained fixture')
  }
  if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
  console.log('Captured ' + containers[0].runs[0].length + ' CONCAT_WS/TRANSLATE observations' +
    (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
    (retained && !oneDatabase ? ' and matched retained fixture' : '') +
    (writeFixture ? '; wrote new retained fixture' : ''))
}
