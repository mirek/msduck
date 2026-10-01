#!/usr/bin/env node
// Independent SQL Server evidence for RPC requests that name a procedure
// (tedious callProcedure, mssql request.execute) and for RPC OUTPUT
// parameters (issue #753): user and system procedures with named and
// positional input and OUTPUT parameters, RETURNSTATUS, RETURNVALUE and
// DONEPROC, call errors, errors inside procedures, sp_executesql with OUTPUT
// declarations (tedious addOutputParameter), and sp_prepexec whose statement
// fails with a duplicate key.
//
// Each run uses a fresh reference container and fixed object names, so the
// streams are stable. Tokens are recorded in order as sent: DONE-family,
// RETURNSTATUS, RETURNVALUE and COLMETADATA bytes verbatim (hex), the kind of
// each ENVCHANGE (transaction descriptors vary), ERROR and INFO through
// tedious events (their server name varies), and consecutive rows as a count.
//
//   node scripts/capture-gaps-rpc-procedures.mjs [output]         capture twice
//   node scripts/capture-gaps-rpc-procedures.mjs --write-fixture  ... and retain
//   node scripts/capture-gaps-rpc-procedures.mjs --check          check retained
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, realpath, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { assertSameCapture } from './lib/reference.mjs'

export const fixture = new URL('../reference/gaps-rpc-procedures.json', import.meta.url)

const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const names = { 0xFD: 'done', 0xFE: 'doneProc', 0xFF: 'doneInProc', 0x79: 'returnStatus', 0xAC: 'returnValue', 0xAA: 'error', 0xAB: 'info', 0x81: 'columns', 0xD1: 'row', 0xD2: 'row', 0xE3: 'envChange', 0xA9: 'order' }
const sinks = new WeakMap()
const readToken = StreamParser.prototype.readToken
StreamParser.prototype.readToken = function (type) {
  const sink = sinks.get(this.options)
  if (!sink) return readToken.call(this, type)
  // The type byte precedes `position`. A parser that waits for more data
  // keeps the unread tail, so the token then starts the new buffer.
  const start = this.position - 1
  const buffer = this.buffer
  const finish = () => {
    const bytes = this.buffer === buffer
      ? buffer.subarray(start, this.position)
      : Buffer.concat([Buffer.from([type]), this.buffer.subarray(0, this.position)])
    push(sink, type, bytes)
  }
  const token = readToken.call(this, type)
  if (token && typeof token.then === 'function') return token.then(value => { finish(); return value })
  finish()
  return token
}
function push(sink, type, bytes) {
  const name = names[type] ?? `0x${type.toString(16)}`
  if (name === 'row') {
    if (sink.at(-1)?.token === 'row') sink.at(-1).count++
    else sink.push({ token: 'row', count: 1 })
  } else if (name === 'error' || name === 'info') {
    sink.push({ token: name })
  } else if (name === 'envChange') {
    // Only the kind: transaction descriptors differ between servers.
    sink.push({ token: name, type: bytes[3] })
  } else {
    sink.push({ token: name, hex: bytes.toString('hex') })
  }
}

// One request. `kind` is 'batch' (SQL batch), 'sql' (an sp_executesql RPC
// from tedious execSql) or 'proc' (an RPC naming `text`, from callProcedure).
// Parameters are [name, tedious type, value, output, options].
export function run(connection, kind, text, parameters = []) {
  return new Promise(resolve => {
    const result = { sets: [], errors: [], info: [], returnValues: [], tokens: [] }
    sinks.set(connection.config.options, result.tokens)
    const message = e => ({ number: e.number, state: e.state, class: e.class, message: e.message, procName: e.procName, lineNumber: e.lineNumber })
    const onError = e => result.errors.push(message(e))
    const onInfo = e => result.info.push(message(e))
    const request = new Request(text, error => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      sinks.delete(connection.config.options)
      if (error && !result.errors.length) result.transport = { code: error.code ?? null, message: error.message }
      resolve(canonical(result))
    })
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    request.on('columnMetadata', metadata => result.sets.push({ columns: metadata.map(c => ({ name: c.colName, type: c.type.name })), rows: [] }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => c.value)))
    request.on('returnValue', (name, value, metadata) => result.returnValues.push({ name, value, type: metadata.type.name }))
    for (const [name, type, value, output, options] of parameters) {
      if (output) request.addOutputParameter(name, TYPES[type], value, options)
      else request.addParameter(name, TYPES[type], value, options)
    }
    if (kind === 'batch') connection.execSqlBatch(request)
    else if (kind === 'sql') connection.execSql(request)
    else connection.callProcedure(request)
  })
}

// [name, kind, text, parameters]. Order matters: later requests use earlier
// definitions.
export const observations = [
  ['server version', 'batch', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['create database', 'batch', 'CREATE DATABASE probe_rpc'],
  ['use database', 'batch', 'USE probe_rpc'],
  ['create table', 'batch', 'CREATE TABLE t (id int CONSTRAINT pk_t PRIMARY KEY)'],
  ['create p_plain', 'batch', 'CREATE PROCEDURE p_plain AS SELECT 7 AS value'],
  ['create p_out', 'batch', 'CREATE PROCEDURE p_out @a int, @b int = 5, @c int OUTPUT AS\nBEGIN\n  SET @c = @a + @b;\n  SELECT @a AS a, @b AS b;\n  RETURN 3;\nEND'],
  ['create p_none', 'batch', 'CREATE PROCEDURE p_none AS RETURN'],
  ['create p_echo', 'batch', 'CREATE PROCEDURE p_echo @io int OUTPUT, @untouched int = NULL OUTPUT AS SET @io = @io * 2'],
  ['create p_types', 'batch', "CREATE PROCEDURE p_types @i int OUTPUT, @bi bigint OUTPUT, @si smallint OUTPUT, @ti tinyint OUTPUT, @bt bit OUTPUT, @s nvarchar(10) OUTPUT, @v varchar(5) OUTPUT, @d decimal(5,2) OUTPUT, @f float OUTPUT, @r real OUTPUT, @m money OUTPUT, @dt datetime2(3) OUTPUT, @da date OUTPUT, @g uniqueidentifier OUTPUT, @b varbinary(4) OUTPUT AS SELECT @i = 1, @bi = 2, @si = 3, @ti = 4, @bt = 1, @s = N'ab', @v = 'cd', @d = 1.25, @f = 0.5, @r = 0.25, @m = 2.5, @dt = '2024-01-02T03:04:05.678', @da = '2024-01-02', @g = '00112233-4455-6677-8899-aabbccddeeff', @b = 0x0102"],
  ['create p_narrow', 'batch', 'CREATE PROCEDURE p_narrow @o tinyint OUTPUT AS SET @o = 200'],
  ['create p_rs', 'batch', "CREATE PROCEDURE p_rs @o int OUTPUT AS BEGIN SET @o = 1; RAISERROR('boom', 16, 1); SET @o = 2; SELECT 'after' AS after END"],
  ['create p_throw', 'batch', "CREATE PROCEDURE p_throw @o int OUTPUT AS BEGIN SET @o = 1; SELECT 'before' AS b; THROW 50001, 'thrown', 1; SELECT 'after' AS after END"],
  ['create p_missing', 'batch', "CREATE PROCEDURE p_missing @o int OUTPUT AS BEGIN SET @o = 1; SELECT 'first' AS f; SELECT * FROM no_such_table; SELECT 'after' AS after END"],
  ['create p_dup', 'batch', "CREATE PROCEDURE p_dup @o int OUTPUT AS BEGIN SET @o = 1; INSERT t VALUES (1); INSERT t VALUES (1); SET @o = 2; SELECT 'after' AS after END"],
  ['create p_retnull', 'batch', 'CREATE PROCEDURE p_retnull AS RETURN NULL'],
  ['create p_multi', 'batch', "CREATE PROCEDURE p_multi AS BEGIN SELECT 1 AS one; SELECT 2 AS two; SELECT 'x' AS three WHERE 1 = 0 END"],
  ['create p_nocount', 'batch', 'CREATE PROCEDURE p_nocount @o int OUTPUT AS BEGIN SET NOCOUNT ON; SET @o = 5; SELECT 1 AS one END'],
  ['create p_tran', 'batch', 'CREATE PROCEDURE p_tran AS BEGIN TRANSACTION'],
  ['create p_big', 'batch', 'CREATE PROCEDURE p_big @o int OUTPUT AS BEGIN SELECT 1 AS a; SET @o = 300 END'],
  ['create p_text', 'batch', "CREATE PROCEDURE p_text @o nvarchar(5) OUTPUT AS SET @o = N'abcdefgh'"],

  // Procedure calls by name.
  ['plain', 'proc', 'p_plain'],
  ['plain qualified', 'proc', 'dbo.p_plain'],
  ['plain bracketed', 'proc', '[dbo].[p_plain]'],
  ['plain three-part', 'proc', 'probe_rpc.dbo.p_plain'],
  ['plain case', 'proc', 'P_PLAIN'],
  ['bare return', 'proc', 'p_none'],
  ['named output', 'proc', 'p_out', [['a', 'Int', 1], ['c', 'Int', null, true]]],
  ['named all', 'proc', 'p_out', [['c', 'Int', null, true], ['b', 'Int', 2], ['a', 'Int', 1]]],
  ['positional output', 'proc', 'p_out', [['', 'Int', 1], ['', 'Int', 2], ['', 'Int', null, true]]],
  ['output without output flag', 'proc', 'p_out', [['a', 'Int', 1], ['c', 'Int', 9]]],
  ['output type differs', 'proc', 'p_out', [['a', 'Int', 1], ['c', 'BigInt', null, true]]],
  ['output nvarchar', 'proc', 'p_out', [['a', 'Int', 1], ['c', 'NVarChar', null, true]]],
  ['output input value', 'proc', 'p_echo', [['io', 'Int', 21, true]]],
  ['output untouched', 'proc', 'p_echo', [['io', 'Int', 1, true], ['untouched', 'Int', 7, true]]],
  ['output default untouched', 'proc', 'p_echo', [['io', 'Int', null, true], ['untouched', 'Int', null, true]]],
  ['output types', 'proc', 'p_types', [
    ['i', 'Int', null, true], ['bi', 'BigInt', null, true], ['si', 'SmallInt', null, true], ['ti', 'TinyInt', null, true],
    ['bt', 'Bit', null, true], ['s', 'NVarChar', null, true, { length: 10 }], ['v', 'VarChar', null, true, { length: 5 }],
    ['d', 'Decimal', null, true, { precision: 5, scale: 2 }], ['f', 'Float', null, true], ['r', 'Real', null, true],
    ['m', 'Money', null, true], ['dt', 'DateTime2', null, true, { scale: 3 }], ['da', 'Date', null, true],
    ['g', 'UniqueIdentifier', null, true], ['b', 'VarBinary', null, true, { length: 4 }],
  ]],
  ['output narrower', 'proc', 'p_narrow', [['o', 'Int', null, true]]],
  ['output fits', 'proc', 'p_narrow', [['o', 'TinyInt', null, true]]],
  ['output overflow', 'proc', 'p_big', [['o', 'TinyInt', null, true]]],
  ['output overflow then batch', 'batch', 'SELECT 1 AS one'],
  ['output longer declaration', 'proc', 'p_text', [['o', 'NVarChar', null, true, { length: 20 }]]],
  ['output shorter declaration', 'proc', 'p_text', [['o', 'NVarChar', null, true, { length: 2 }]]],
  ['multiple results', 'proc', 'p_multi'],
  ['return null', 'proc', 'p_retnull'],
  ['nocount inside', 'proc', 'p_nocount', [['o', 'Int', null, true]]],

  // Call errors.
  ['missing procedure', 'proc', 'no_such_proc'],
  ['missing parameter', 'proc', 'p_out', [['a', 'Int', 1]]],
  ['too many arguments', 'proc', 'p_out', [['', 'Int', 1], ['', 'Int', 2], ['', 'Int', null, true], ['', 'Int', 4]]],
  ['unknown parameter', 'proc', 'p_out', [['a', 'Int', 1], ['zz', 'Int', 2], ['c', 'Int', null, true]]],
  ['repeated parameter', 'proc', 'p_out', [['a', 'Int', 1], ['a', 'Int', 2], ['c', 'Int', null, true]]],
  ['positional after named', 'proc', 'p_out', [['a', 'Int', 1], ['', 'Int', 2], ['c', 'Int', null, true]]],
  ['output to input parameter', 'proc', 'p_out', [['a', 'Int', 1, true], ['c', 'Int', null, true]]],
  ['conversion error', 'proc', 'p_out', [['a', 'NVarChar', 'abc'], ['c', 'Int', null, true]]],
  ['conversion from text', 'proc', 'p_out', [['a', 'NVarChar', '42'], ['c', 'Int', null, true]]],

  // Errors inside procedures.
  ['raiserror inside', 'proc', 'p_rs', [['o', 'Int', null, true]]],
  ['throw inside', 'proc', 'p_throw', [['o', 'Int', null, true]]],
  ['missing table inside', 'proc', 'p_missing', [['o', 'Int', null, true]]],
  ['duplicate key inside', 'proc', 'p_dup', [['o', 'Int', null, true]]],
  ['transaction count', 'proc', 'p_tran'],
  ['rollback after transaction count', 'batch', 'SELECT @@TRANCOUNT AS tc; IF @@TRANCOUNT > 0 ROLLBACK'],

  // XACT_ABORT: a duplicate key inside the procedure ends the batch and
  // rolls the transaction back.
  ['xact_abort on', 'batch', 'SET XACT_ABORT ON; BEGIN TRANSACTION'],
  ['duplicate key xact_abort', 'proc', 'p_dup', [['o', 'Int', null, true]]],
  ['after xact_abort', 'batch', 'SELECT @@TRANCOUNT AS tc; IF @@TRANCOUNT > 0 ROLLBACK; SET XACT_ABORT OFF'],

  // Session NOCOUNT.
  ['nocount on', 'batch', 'SET NOCOUNT ON'],
  ['named output nocount', 'proc', 'p_out', [['a', 'Int', 1], ['c', 'Int', null, true]]],
  ['nocount off', 'batch', 'SET NOCOUNT OFF'],

  // System procedures by name.
  ['getapplock', 'proc', 'sp_getapplock', [['Resource', 'NVarChar', 'probe'], ['LockMode', 'NVarChar', 'Exclusive'], ['LockOwner', 'NVarChar', 'Session']]],
  ['releaseapplock', 'proc', 'sp_releaseapplock', [['Resource', 'NVarChar', 'probe'], ['LockOwner', 'NVarChar', 'Session']]],
  ['releaseapplock again', 'proc', 'sp_releaseapplock', [['Resource', 'NVarChar', 'probe'], ['LockOwner', 'NVarChar', 'Session']]],
  ['set session context', 'proc', 'sp_set_session_context', [['key', 'NVarChar', 'k'], ['value', 'NVarChar', 'v']]],
  ['set session context positional', 'proc', 'sys.sp_set_session_context', [['', 'NVarChar', 'k2'], ['', 'Int', 5]]],
  ['session context value', 'batch', "SELECT CAST(SESSION_CONTEXT(N'k') AS nvarchar(10)) AS v, SESSION_CONTEXT(N'k2') AS v2"],
  ['executesql by name', 'proc', 'sp_executesql', [['stmt', 'NVarChar', 'SET @x = @y + 1'], ['params', 'NVarChar', '@x int OUTPUT, @y int'], ['x', 'Int', null, true], ['y', 'Int', 4]]],
  ['executesql missing value', 'proc', 'sp_executesql', [['stmt', 'NVarChar', 'SET @x = @y + 1'], ['params', 'NVarChar', '@x int OUTPUT, @y int'], ['x', 'Int', 3, true]]],
  ['executesql extra value', 'proc', 'sp_executesql', [['stmt', 'NVarChar', 'SET @x = 1'], ['params', 'NVarChar', '@x int OUTPUT'], ['x', 'Int', 3, true], ['y', 'Int', 1]]],
  ['executesql unknown name', 'proc', 'sp_executesql', [['stmt', 'NVarChar', 'SET @x = 1'], ['params', 'NVarChar', '@x int OUTPUT'], ['x', 'Int', 3, true], ['z', 'Int', 1]]],
  ['executesql varchar statement', 'proc', 'sp_executesql', [['stmt', 'VarChar', 'SET @x = 1'], ['params', 'NVarChar', '@x int OUTPUT'], ['x', 'Int', 3, true]]],
  ['executesql bad declaration', 'proc', 'sp_executesql', [['stmt', 'NVarChar', 'SET @x = 1'], ['params', 'NVarChar', '@x int OUTPUT,'], ['x', 'Int', 3, true]]],
  ['executesql conversion', 'proc', 'sp_executesql', [['stmt', 'NVarChar', 'SET @x = 1'], ['params', 'NVarChar', '@x int OUTPUT, @y int'], ['x', 'Int', 3, true], ['y', 'NVarChar', 'abc']]],
  ['executesql output conversion', 'proc', 'sp_executesql', [['stmt', 'NVarChar', "SET @x = N'abc'"], ['params', 'NVarChar', '@x nvarchar(5) OUTPUT'], ['x', 'Int', 3, true]]],
  ['executesql output not declared output', 'proc', 'sp_executesql', [['stmt', 'NVarChar', 'SET @x = 1'], ['params', 'NVarChar', '@x int'], ['x', 'Int', 3, true]]],

  // sp_executesql with OUTPUT declarations (tedious addOutputParameter).
  ['sql output', 'sql', 'SET @x = 42', [['x', 'Int', null, true]]],
  ['sql input and output', 'sql', 'SET @b = @a * 3', [['a', 'Int', 4], ['b', 'Int', null, true]]],
  ['sql output order', 'sql', 'SET @b = @a * 3; SELECT @a AS a', [['b', 'Int', null, true], ['a', 'Int', 4]]],
  ['sql output input value', 'sql', 'SET @x = @x + 1', [['x', 'Int', 5, true]]],
  ['sql output untouched', 'sql', 'SELECT 1 AS one', [['x', 'Int', 5, true]]],
  ['sql output types', 'sql', "SELECT @s = N'abc', @d = 1.5, @v = 'xy', @g = NEWID(), @g = '00112233-4455-6677-8899-aabbccddeeff'", [['s', 'NVarChar', null, true, { length: 10 }], ['d', 'Decimal', null, true, { precision: 5, scale: 2 }], ['v', 'VarChar', null, true, { length: 5 }], ['g', 'UniqueIdentifier', null, true]]],
  ['sql output raiserror', 'sql', "SET @x = 1; RAISERROR('r', 16, 1); SET @x = 2", [['x', 'Int', null, true]]],
  ['sql output divide', 'sql', 'SET @x = 1; SELECT 1/0 AS z; SET @x = 2', [['x', 'Int', null, true]]],
  ['sql output duplicate', 'sql', 'SET @x = 1; INSERT t VALUES (1); INSERT t VALUES (1); SET @x = 2', [['x', 'Int', null, true]]],
  ['sql output throw', 'sql', "SET @x = 1; THROW 50002, 't', 1; SET @x = 2", [['x', 'Int', null, true]]],
  ['sql output missing table', 'sql', 'SET @x = 1; SELECT * FROM no_such_table; SET @x = 2', [['x', 'Int', null, true]]],
  ['sql output missing table input', 'sql', 'SET @x = 1; SELECT * FROM no_such_table; SET @x = 2', [['x', 'Int', 5, true]]],
  ['sql output conversion abort', 'sql', "SET @x = 1; SELECT CAST('a' AS int) AS c; SET @x = 2", [['x', 'Int', 5, true]]],
  ['sql output return', 'sql', 'SET @x = 1; RETURN; SET @x = 2', [['x', 'Int', null, true]]],
  ['sql output nocount', 'sql', 'SET NOCOUNT ON; SET @x = 3; SELECT 1 AS one', [['x', 'Int', null, true]]],
  ['sql output calls procedure', 'sql', 'EXEC p_out @a = @in, @c = @x OUTPUT', [['in', 'Int', 2], ['x', 'Int', null, true]]],
  ['sql output syntax error', 'sql', 'SET @x = ', [['x', 'Int', null, true]]],

  // Preparation with a failing statement.
  ['delete rows', 'batch', 'DELETE t; INSERT t VALUES (1)'],
  ['prepexec duplicate', 'proc', 'sp_prepexec', [['handle', 'Int', null, true], ['params', 'NVarChar', null], ['stmt', 'NVarChar', 'INSERT INTO t VALUES (1)']]],
  ['prepexec success', 'proc', 'sp_prepexec', [['handle', 'Int', null, true], ['params', 'NVarChar', '@v int'], ['stmt', 'NVarChar', 'INSERT INTO t VALUES (@v)'], ['v', 'Int', 2]]],
  ['prepexec duplicate parameter', 'proc', 'sp_prepexec', [['handle', 'Int', null, true], ['params', 'NVarChar', '@v int'], ['stmt', 'NVarChar', 'INSERT INTO t VALUES (@v)'], ['v', 'Int', 2]]],
  ['prepexec duplicate then select', 'proc', 'sp_prepexec', [['handle', 'Int', null, true], ['params', 'NVarChar', null], ['stmt', 'NVarChar', 'INSERT INTO t VALUES (1); SELECT COUNT(*) AS n FROM t']]],
  ['prepexec output', 'proc', 'sp_prepexec', [['handle', 'Int', null, true], ['params', 'NVarChar', '@x int OUTPUT'], ['stmt', 'NVarChar', 'SET @x = 11'], ['x', 'Int', null, true]]],
  ['prepexec throw', 'proc', 'sp_prepexec', [['handle', 'Int', null, true], ['params', 'NVarChar', '@x int OUTPUT'], ['stmt', 'NVarChar', "SET @x = 1; THROW 50003, 'p', 1"], ['x', 'Int', 5, true]]],
  ['prepexec throw without outputs', 'proc', 'sp_prepexec', [['handle', 'Int', null, true], ['params', 'NVarChar', null], ['stmt', 'NVarChar', "THROW 50004, 'q', 1"]]],
  ['execute after throw', 'proc', 'sp_execute', [['handle', 'Int', 6], ['x', 'Int', 5, true]]],
  ['execute duplicate handle', 'proc', 'sp_execute', [['handle', 'Int', 1]]],
  ['execute output', 'proc', 'sp_execute', [['handle', 'Int', 5], ['x', 'Int', 1, true]]],
  ['execute output positional', 'proc', 'sp_execute', [['', 'Int', 5], ['', 'Int', null, true]]],
  ['prepare output', 'proc', 'sp_prepare', [['handle', 'Int', null, true], ['params', 'NVarChar', '@x int OUTPUT, @y int'], ['stmt', 'NVarChar', 'SET @x = @y * 2']]],
  ['execute prepared output', 'proc', 'sp_execute', [['handle', 'Int', 6], ['x', 'Int', null, true], ['y', 'Int', 8]]],
  ['unprepare', 'proc', 'sp_unprepare', [['handle', 'Int', 6]]],
  ['unprepare again', 'proc', 'sp_unprepare', [['handle', 'Int', 6]]],
  ['rows after', 'batch', 'SELECT id FROM t ORDER BY id'],
]

async function observe(config, { connect }) {
  const connection = await connect({ ...config, options: { ...config.options, appName: 'msduck-capture', connectTimeout: 30000 } })
  const results = []
  try {
    for (const [name, kind, text, parameters] of observations) {
      results.push({ name, kind, text, ...(parameters ? { parameters } : {}), result: await run(connection, kind, text, parameters) })
      console.log(name)
    }
  } finally {
    connection.close()
  }
  return results
}

// Assertions every retained run must satisfy.
export function validate(results) {
  const get = name => {
    const found = results.find(item => item.name === name)
    assert(found, `${name}: missing observation`)
    return found.result
  }
  // The token stream without metadata, messages and ENVCHANGE: DONE-family
  // and RETURNSTATUS/RETURNVALUE bytes, and row counts.
  const stream = name => get(name).tokens
    .filter(t => !['columns', 'envChange', 'order', 'error', 'info'].includes(t.token))
    .map(t => t.token === 'row' ? `row*${t.count}` : `${t.token}:${t.hex}`)
  const errors = name => get(name).errors.map(e => e.number)
  const outputs = name => get(name).returnValues.map(v => [v.name, v.value])
  const done = (id, status, command, count = 0) => `${id}:${Buffer.from([{ doneProc: 0xfe, doneInProc: 0xff }[id], status & 255, status >> 8, command & 255, command >> 8, count, 0, 0, 0, 0, 0, 0, 0]).toString('hex')}`
  const status = value => { const b = Buffer.alloc(5); b[0] = 0x79; b.writeInt32LE(value, 1); return `returnStatus:${b.toString('hex')}` }
  assert.match(get('server version').sets[0].rows[0][0], /^1[67]\./, 'reference version')
  // A completed call: statements, RETURNSTATUS, RETURNVALUEs in request order, DONEPROC.
  assertSameCapture(stream('named output'), [
    done('doneInProc', 0x11, 193, 1), 'row*1', done('doneInProc', 0x11, 193, 1), done('doneInProc', 0x11, 193, 1), status(3),
    'returnValue:ac010002400063000100000000000026040406000000', done('doneProc', 0, 224),
  ], 'named output stream changed')
  assertSameCapture(outputs('positional output'), [['', 3]], 'positional output changed')
  assertSameCapture(outputs('positional after named'), [['c', 3]], 'ordinal binding changed')
  assertSameCapture(outputs('output untouched'), [['io', 2], ['untouched', 7]], 'input/output values changed')
  assertSameCapture(get('output without output flag').returnValues, [], 'non-output parameter returned')
  assertSameCapture(get('output types').returnValues.length, 15, 'output types changed')
  // Call errors: the error and DONEPROC, without RETURNSTATUS.
  for (const [name, number] of [['missing procedure', 2812], ['missing parameter', 201], ['too many arguments', 8144], ['unknown parameter', 8145], ['repeated parameter', 8143], ['output to input parameter', 8162], ['conversion error', 8114], ['throw inside', 50001], ['missing table inside', 208], ['output overflow', 8114]]) {
    assertSameCapture(errors(name), [number], `${name}: errors changed`)
    assertSameCapture(stream(name).filter(t => t.startsWith('returnStatus') || t.startsWith('returnValue') || t.startsWith('doneProc')), [done('doneProc', 2, 224)], `${name}: ending changed`)
  }
  assertSameCapture(stream('raiserror inside').slice(-3), [status(-6), 'returnValue:ac00000240006f000100000000000026040402000000', done('doneProc', 0, 224)], 'RAISERROR status changed')
  assertSameCapture(stream('duplicate key inside').slice(-3, -2), [status(-4)], 'duplicate key status changed')
  assertSameCapture(stream('transaction count').slice(-2), [status(0), done('doneProc', 2, 224)], '266 ending changed')
  // sp_executesql with OUTPUT parameters.
  assertSameCapture(outputs('sql output'), [['x', 42]], 'sp_executesql output changed')
  assertSameCapture(outputs('sql output raiserror'), [['x', 2]], 'statement errors keep running')
  assertSameCapture(stream('sql output missing table input').slice(-3), [status(208), 'returnValue:ac020002400078000100000000000026040405000000', done('doneProc', 2, 224)], 'failed sp_executesql returns input values')
  assertSameCapture(stream('executesql missing value').slice(-3, -2), [status(1)], '8178 status changed')
  assertSameCapture(stream('sql output throw'), [done('doneInProc', 0x11, 193, 1), done('doneProc', 2, 224)], 'aborted sp_executesql changed')
  // sp_prepexec: a duplicate key ends the statement and still returns the handle.
  assertSameCapture(stream('prepexec duplicate'), [done('doneInProc', 3, 195), status(2627), 'returnValue:ac0000074000680061006e0064006c0065000100000000000026040401000000', done('doneProc', 0, 224)], 'prepexec duplicate changed')
  assertSameCapture(outputs('prepexec output'), [['handle', 5], ['x', 11]], 'prepexec output changed')
  assertSameCapture(get('prepexec throw').returnValues, [], 'aborted prepexec returned a handle')
  assertSameCapture(outputs('prepare output'), [['handle', 6]], 'aborted preparation consumed a handle')
  assertSameCapture(errors('execute after throw'), [8179], 'missing handle changed')
  assertSameCapture(get('rows after').sets[0].rows, [[1], [2]], 'rows changed')
}

async function main() {
  const { withReferenceContainer, referenceImage } = await import('./lib/reference-container.mjs')
  const { connect, refuseExistingFixture, writeNewFixture } = await import('./lib/reference.mjs')
  const args = process.argv.slice(2)
  const check = args.includes('--check')
  const writeFixture = args.includes('--write-fixture')
  const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
  if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-gaps-rpc-procedures.mjs [--check | --write-fixture] [output]')
  const output = resolve(paths[0] ?? 'artifacts/compatibility/gaps-rpc-procedures-reference/capture.json')
  const image = process.env.MSSQL_REFERENCE_IMAGE ?? referenceImage
  if (process.env.MSSQL_PROBE_PORT) {
    // Exploration against an already running reference server.
    const config = {
      server: 'localhost',
      authentication: { type: 'default', options: { userName: 'sa', password: process.env.MSSQL_PROBE_PASSWORD } },
      options: { port: Number(process.env.MSSQL_PROBE_PORT), database: 'master', encrypt: process.env.MSSQL_PROBE_ENCRYPT !== 'false', trustServerCertificate: true, connectTimeout: 30000, requestTimeout: 30000 },
    }
    const run = await observe(config, { connect })
    await mkdir(dirname(output), { recursive: true })
    await writeFile(output, JSON.stringify(run, null, 1) + '\n')
    return
  }
  if (check) {
    const retained = JSON.parse(await readFile(fixture))
    assert.equal(retained.runs.length, 2, 'independent runs missing')
    assert.equal(createHash('sha256').update(JSON.stringify(retained.runs[0])).digest('hex'), retained.captureSha256, 'capture checksum changed')
    for (const run of retained.runs) validate(run)
    assertSameCapture(retained.runs[0], retained.runs[1], 'independent runs differ')
    console.log(`Checked ${retained.runs[0].length} RPC procedure observations in two retained runs (${retained.image})`)
    return
  }
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if (await realpath(dirname(output)).then(dir => resolve(dir, basename(output))) === fileURLToPath(fixture)) throw Error('capture output must not be the retained fixture')
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    await withReferenceContainer(async config => {
      const run = await observe(config, { connect })
      validate(run)
      if (runs.length) assertSameCapture(run, runs[0], 'independent reference containers differ')
      runs.push(run)
    }, { image })
  }
  const actual = { image, captureSha256: createHash('sha256').update(JSON.stringify(runs[0])).digest('hex'), runs }
  await writeFile(output, JSON.stringify(actual, null, 1) + '\n', { flag: 'wx' })
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} RPC procedure observations in two fresh containers`)
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) await main()
