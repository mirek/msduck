#!/usr/bin/env node
// Independent SQL Server evidence for stored procedures, EXEC (string) and
// sp_executesql (issue #719): definitions, calls with positional, named,
// DEFAULT and OUTPUT arguments, return statuses, the DONE/DONEINPROC/
// DONEPROC/RETURNSTATUS token stream, nested calls, TRY/CATCH and error
// propagation, and the errors SQL Server raises for each malformed form.
//
// Every observation runs in a fresh reference container against fixed object
// names, so messages are stable (the one key constraint is named).
//
//   node scripts/capture-gaps-procedures.mjs [output]         capture twice
//   node scripts/capture-gaps-procedures.mjs --write-fixture  ... and retain
//   node scripts/capture-gaps-procedures.mjs --check          check retained
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, realpath, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { assertSameCapture, connect, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/gaps-procedures.json', import.meta.url)
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-gaps-procedures.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/gaps-procedures-reference/capture.json')
const image = process.env.MSSQL_REFERENCE_IMAGE ?? referenceImage

// The token stream in order: DONE-family bodies verbatim (TDS 7.2+: USHORT
// status, USHORT CurCmd, ULONGLONG row count), RETURNSTATUS values and the
// other token kinds by name. Rows, messages and RETURNVALUEs are recorded
// through tedious events. A parser's options object belongs to one
// connection.
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const names = { 0xFD: 'done', 0xFE: 'doneProc', 0xFF: 'doneInProc', 0x79: 'returnStatus', 0xAC: 'returnValue', 0xAA: 'error', 0xAB: 'info', 0x81: 'columns', 0xD1: 'row', 0xD2: 'row', 0xE3: 'envChange', 0xA9: 'order' }
const sinks = new WeakMap()
const readToken = StreamParser.prototype.readToken
StreamParser.prototype.readToken = function (type) {
  return record(this, type)
}
function record(parser, type) {
  const at = parser.position
  const sink = sinks.get(parser.options)
  if (sink && (type === 0xFD || type === 0xFE || type === 0xFF)) {
    if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => record(parser, type))
    sink.push({ token: names[type], status: parser.buffer.readUInt16LE(at), curCmd: parser.buffer.readUInt16LE(at + 2), rowCount: parser.buffer.readBigUInt64LE(at + 4).toString() })
  } else if (sink && type === 0x79) {
    if (parser.buffer.length < at + 4) return parser.waitForChunk().then(() => record(parser, type))
    sink.push({ token: 'returnStatus', value: parser.buffer.readInt32LE(at) })
  } else if (sink && type !== 0xD1 && type !== 0xD2) {
    // Consecutive rows are counted in their result set instead.
    sink.push({ token: names[type] ?? `0x${type.toString(16)}` })
  }
  return readToken.call(parser, type)
}

// One request: a SQL batch, or (with parameters) an sp_executesql RPC.
function run(connection, sql, parameters) {
  return new Promise(resolve => {
    const result = { sets: [], errors: [], info: [], returnValues: [], tokens: [] }
    sinks.set(connection.config.options, result.tokens)
    const message = e => ({ number: e.number, state: e.state, class: e.class, message: e.message.replace(/PK__\w+/g, 'PK__<generated>') })
    const onError = e => result.errors.push(message(e))
    const onInfo = e => result.info.push(message(e))
    const request = new Request(sql, error => {
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
    request.on('returnValue', (name, value) => result.returnValues.push({ name, value }))
    if (parameters) {
      for (const [name, type, value, output] of parameters) {
        if (output) request.addOutputParameter(name, TYPES[type], value)
        else request.addParameter(name, TYPES[type], value)
      }
      connection.execSql(request)
    } else connection.execSqlBatch(request)
  })
}

// [name, sql, rpc parameters]. Order matters: later batches use earlier
// definitions.
const observations = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['create database', 'CREATE DATABASE probe_procedures'],
  ['use database', 'USE probe_procedures'],

  // The report's repro and the plain call forms.
  ['create foo', 'CREATE PROCEDURE foo AS SELECT 7 AS value;'],
  ['exec foo', 'EXEC foo'],
  ['exec foo then select', 'EXEC foo; SELECT 1 AS after'],
  ['bare name call', 'foo'],
  ['execute qualified', 'EXECUTE [dbo].[foo]'],
  ['exec foo nocount', 'SET NOCOUNT ON; EXEC foo; SET NOCOUNT OFF'],
  ['exec foo rpc', 'EXEC foo; SELECT @p AS p', [['p', 'Int', 3]]],
  ['objects', "SELECT name, type, type_desc FROM sys.objects WHERE type = 'P' AND name = 'foo'"],

  // Parameters, defaults, OUTPUT and RETURN.
  ['create p_out', 'CREATE PROCEDURE p_out @a int, @b int = 5, @c int OUTPUT AS\nBEGIN\n  SET @c = @a + @b;\n  SELECT @a AS a, @b AS b;\n  RETURN 3;\nEND'],
  ['p_out positional output', 'DECLARE @r int, @x int; EXEC @r = p_out 1, 2, @x OUTPUT; SELECT @r AS r, @x AS x'],
  ['p_out named output', 'DECLARE @r int, @x int; EXEC @r = p_out @a = 1, @c = @x OUTPUT; SELECT @r AS r, @x AS x'],
  ['p_out default keyword', 'DECLARE @r int, @x int; EXEC @r = p_out 1, DEFAULT, @x OUT; SELECT @r AS r, @x AS x'],
  ['p_out without output', 'DECLARE @x int = 10; EXEC p_out 1, 2, @x; SELECT @x AS x'],
  ['p_out nocount', 'SET NOCOUNT ON; EXEC p_out 1, 2, 3; SET NOCOUNT OFF'],
  ['create p_types', "CREATE PROCEDURE p_types @i int = 1, @s nvarchar(10) = N'dflt', @d decimal(5,2) = 1.5, @n int = NULL, @v varchar(3) = 'abcdef' AS SELECT @i AS i, @s AS s, @d AS d, @n AS n, @v AS v"],
  ['p_types defaults', 'EXEC p_types'],
  ['p_types conversions', "EXEC p_types '42', abc, -2"],
  ['create p_outconv', 'CREATE PROCEDURE p_outconv @o tinyint OUTPUT AS SET @o = 255'],
  ['p_outconv to int', 'DECLARE @x int; EXEC p_outconv @x OUTPUT; SELECT @x AS x'],
  ['create p_ret', 'CREATE PROCEDURE p_ret AS RETURN'],
  ['p_ret status', 'DECLARE @r int = 9; EXEC @r = p_ret; SELECT @r AS r'],
  ['create p_retnull', 'CREATE PROCEDURE p_retnull AS RETURN NULL'],
  ['p_retnull status', 'DECLARE @r int; EXEC @r = p_retnull; SELECT @r AS r'],
  ['create p_multi', "CREATE PROCEDURE p_multi AS BEGIN SELECT 1 AS one; SELECT 2 AS two; SELECT 'x' AS three WHERE 1 = 0 END"],
  ['p_multi', 'EXEC p_multi'],
  ['exec module variable', "DECLARE @n nvarchar(20) = N'p_multi'; EXEC @n"],
  ['create p_rc', 'CREATE PROCEDURE p_rc AS SELECT 1 AS a UNION ALL SELECT 2'],
  ['rowcount after exec', 'EXEC p_rc; SELECT @@ROWCOUNT AS rc'],
  ['rowcount after bare return', 'SELECT 1 AS a UNION ALL SELECT 2; EXEC p_ret; SELECT @@ROWCOUNT AS rc'],

  // Call errors.
  ['missing parameter', 'EXEC p_out 1'],
  ['too many arguments', 'EXEC p_out 1, 2, 3, 4'],
  ['unknown parameter', 'EXEC p_out @a = 1, @zz = 2, @c = 1'],
  ['repeated parameter', 'EXEC p_out @a = 1, @a = 2, @c = 3'],
  ['positional after named', 'DECLARE @x int; EXEC p_out @a = 1, 2, @c = @x OUTPUT'],
  ['output constant', 'EXEC p_out 1, 2, 5 OUTPUT'],
  ['create p_in', 'CREATE PROCEDURE p_in @a int AS SELECT @a AS a'],
  ['output to input parameter', 'DECLARE @x int = 1; EXEC p_in @x OUTPUT'],
  ['conversion error', "EXEC p_in 'abc'"],
  ['missing procedure', 'EXEC no_such_proc'],
  ['missing procedure continues', 'SELECT 1 AS before; EXEC no_such_proc; SELECT 2 AS after'],

  // Errors inside procedures.
  ['create p_rs', "CREATE PROCEDURE p_rs AS BEGIN RAISERROR('boom', 16, 1); SELECT 'after' AS after END"],
  ['p_rs status', 'DECLARE @r int; EXEC @r = p_rs; SELECT @r AS r, @@ERROR AS e'],
  ['p_rs caught by caller', "BEGIN TRY EXEC p_rs; SELECT 'not' AS n END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS num, ERROR_MESSAGE() AS msg, ERROR_PROCEDURE() AS proc_name END CATCH"],
  ['create p_two', "CREATE PROCEDURE p_two AS BEGIN RAISERROR('a', 16, 1); RAISERROR('b', 11, 1); SELECT 1 AS x END"],
  ['p_two status', 'DECLARE @r int; EXEC @r = p_two; SELECT @r AS r'],
  ['create p_ret0_after', "CREATE PROCEDURE p_ret0_after AS BEGIN RAISERROR('a', 16, 1); RETURN 0 END"],
  ['p_ret0_after status', 'DECLARE @r int; EXEC @r = p_ret0_after; SELECT @r AS r'],
  ['create table', 'CREATE TABLE t (id int CONSTRAINT pk_t PRIMARY KEY)'],
  ['create p_dup', "CREATE PROCEDURE p_dup AS BEGIN INSERT t VALUES (1); INSERT t VALUES (1); SELECT 'after' AS after END"],
  ['p_dup status', 'DECLARE @r int; EXEC @r = p_dup; SELECT @r AS r, @@ERROR AS e'],
  ['create p_div', 'CREATE PROCEDURE p_div AS BEGIN SELECT 1/0 AS x; SELECT 2 AS y END'],
  ['p_div status', 'DECLARE @r int; EXEC @r = p_div; SELECT @r AS r'],
  ['p_div caught by caller', 'BEGIN TRY EXEC p_div END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS num END CATCH'],
  ['create p_throw', "CREATE PROCEDURE p_throw AS BEGIN SELECT 'before' AS b; THROW 50001, 'thrown', 1; SELECT 'after' AS after END"],
  ['p_throw aborts batch', "DECLARE @r int; EXEC @r = p_throw; SELECT 'never' AS n"],
  ['p_throw caught by caller', 'BEGIN TRY EXEC p_throw END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS num, ERROR_PROCEDURE() AS p END CATCH'],
  ['create p_missing', "CREATE PROCEDURE p_missing AS BEGIN SELECT 'first' AS f; SELECT * FROM no_such_table; SELECT 'after' AS after END"],
  ['p_missing ends the procedure', 'DECLARE @r int = 99; EXEC @r = p_missing; SELECT @r AS r, @@ERROR AS e'],
  ['p_missing caught by caller', 'BEGIN TRY EXEC p_missing END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS num, ERROR_PROCEDURE() AS p END CATCH'],
  ['create p_trycatch', 'CREATE PROCEDURE p_trycatch AS BEGIN BEGIN TRY SELECT 1/0 AS x END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n, ERROR_PROCEDURE() AS p END CATCH; RETURN 9 END'],
  ['p_trycatch', 'DECLARE @r int; EXEC @r = p_trycatch; SELECT @r AS r'],
  ['p_trycatch nocount', 'SET NOCOUNT ON; DECLARE @r int; EXEC @r = p_trycatch; SELECT @r AS r; SET NOCOUNT OFF'],
  ['create p_caught_status', "CREATE PROCEDURE p_caught_status AS BEGIN BEGIN TRY RAISERROR('in', 16, 1) END TRY BEGIN CATCH SELECT ERROR_PROCEDURE() AS p END CATCH END"],
  ['p_caught_status', 'DECLARE @r int; EXEC @r = p_caught_status; SELECT @r AS r'],
  ['xact_abort raiserror', 'SET XACT_ABORT ON; BEGIN TRAN; EXEC p_rs; SELECT @@TRANCOUNT AS tc, XACT_STATE() AS xs; ROLLBACK; SET XACT_ABORT OFF'],

  // Nested calls.
  ['create p_inner', 'CREATE PROCEDURE p_inner @v int OUTPUT AS BEGIN SET @v = @v * 2; RETURN 5 END'],
  ['create p_outer', 'CREATE PROCEDURE p_outer AS BEGIN DECLARE @v int = 21, @r int; EXEC @r = p_inner @v OUTPUT; SELECT @v AS v, @r AS r, @@NESTLEVEL AS lvl END'],
  ['p_outer', 'EXEC p_outer; SELECT @@NESTLEVEL AS top_level'],
  ['create p_nest_err', "CREATE PROCEDURE p_nest_err AS BEGIN EXEC p_rs; SELECT 'outer after' AS a END"],
  ['p_nest_err', 'EXEC p_nest_err'],
  ['p_nest_err caught by caller', 'BEGIN TRY EXEC p_nest_err END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS num, ERROR_PROCEDURE() AS p END CATCH'],
  ['create p_outer_missing', "CREATE PROCEDURE p_outer_missing AS BEGIN EXEC p_missing; SELECT 'outer after' AS a END"],
  ['p_outer_missing', 'DECLARE @r int; EXEC @r = p_outer_missing; SELECT @r AS r, @@ERROR AS e'],
  ['create p_outer_throw', "CREATE PROCEDURE p_outer_throw AS BEGIN EXEC p_throw; SELECT 'outer after' AS a END"],
  ['p_outer_throw', "EXEC p_outer_throw; SELECT 'never' AS n"],
  ['create p_catch_outer', "CREATE PROCEDURE p_catch_outer AS BEGIN BEGIN TRY EXEC p_rs; SELECT 'not' AS n END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS num END CATCH; SELECT 'end' AS e END"],
  ['p_catch_outer', 'DECLARE @r int; EXEC @r = p_catch_outer; SELECT @r AS r'],
  ['create p_nocount_nested', 'CREATE PROCEDURE p_nocount_nested AS BEGIN SET NOCOUNT ON; EXEC foo; SELECT 2 AS two END'],
  ['p_nocount_nested', 'EXEC p_nocount_nested; SELECT 3 AS three'],
  ['create p_rec', 'CREATE PROCEDURE p_rec @n int AS BEGIN IF @n > 0 BEGIN DECLARE @m int = @n - 1; EXEC p_rec @m END ELSE SELECT @@NESTLEVEL AS lvl END'],
  ['p_rec 3', 'EXEC p_rec 3'],
  ['p_rec over limit', "EXEC p_rec 40; SELECT 'never' AS n"],
  ['create p_tran', 'CREATE PROCEDURE p_tran AS BEGIN TRANSACTION'],
  ['p_tran mismatch', 'EXEC p_tran; SELECT @@TRANCOUNT AS tc; ROLLBACK'],

  // Dynamic SQL.
  ['exec string', "EXEC ('SELECT 5 AS five')"],
  ['exec variable', "DECLARE @s nvarchar(100) = N'SELECT 6 AS six'; EXEC (@s)"],
  ['exec concatenation', "EXEC ('SELECT ' + '2 AS two')"],
  ['exec string scope', "DECLARE @t int; EXEC ('SELECT @t')"],
  ['exec string return value', "EXEC ('SELECT 1 AS a; RETURN 4')"],
  ['exec string missing table', "EXEC ('SELECT * FROM no_such_table'); SELECT 'after' AS a"],
  ['exec string raiserror', "EXEC ('RAISERROR(''r'', 16, 1)'); SELECT @@ERROR AS e"],
  ['exec string calls procedure', "EXEC ('EXEC foo')"],

  // sp_executesql in SQL batches.
  ['executesql named', "EXEC sp_executesql N'SELECT @p AS p', N'@p int', @p = 1"],
  ['executesql positional', "EXEC sp_executesql N'SELECT @p AS p, @q AS q', N'@p int, @q int', 1, 2"],
  ['executesql output', "DECLARE @o int; EXEC sp_executesql N'SET @b = @a * 3', N'@a int, @b int OUTPUT', @a = 4, @b = @o OUTPUT; SELECT @o AS o"],
  ['executesql output positional', "DECLARE @o int = 1; EXEC sp_executesql N'SET @p = @p + 1', N'@p int OUTPUT', @o OUTPUT; SELECT @o AS o"],
  ['executesql output undeclared', "DECLARE @o int = 1; EXEC sp_executesql N'SET @p = @p + 1', N'@p int', @o OUTPUT; SELECT @o AS o"],
  ['executesql statement names', "EXEC sp_executesql @stmt = N'SELECT @p AS p', @params = N'@p int', @p = 3"],
  ['executesql parameterless', "EXEC sp_executesql N'SELECT 9 AS nine'"],
  ['executesql missing value', "EXEC sp_executesql N'SELECT @p AS p', N'@p int'"],
  ['executesql unknown name', "EXEC sp_executesql N'SELECT 1 AS one', N'@p int', @q = 1"],
  ['executesql extra name', "EXEC sp_executesql N'SELECT @p AS p', N'@p int', @p = 1, @q = 2"],
  ['executesql varchar statement', "EXEC sp_executesql 'SELECT 1 AS one'"],
  ['executesql caller scope', "DECLARE @a int = 5; EXEC sp_executesql N'SELECT @a AS a'"],
  ['executesql raiserror status', "DECLARE @r int; EXEC @r = sp_executesql N'RAISERROR(''x'', 16, 1)'; SELECT @r AS r"],
  ['executesql missing table status', "DECLARE @r int; EXEC @r = sp_executesql N'SELECT * FROM no_such_table'; SELECT @r AS r, @@ERROR AS e"],
  ['executesql return value', "DECLARE @o int, @r int; EXEC @r = sp_executesql N'RETURN 7', N'@a int', 4; SELECT @r AS r"],
  ['executesql creates procedure', "EXEC sp_executesql N'CREATE PROCEDURE p_dyn AS SELECT ''dyn'' AS d'"],
  ['p_dyn', 'EXEC p_dyn'],

  // RPC (tedious parameters make an sp_executesql request).
  ['rpc output parameter', 'SET @x = 42', [['x', 'Int', null, true]]],
  ['rpc calls procedure', 'DECLARE @o int; EXEC p_inner @o OUTPUT; SELECT @o AS o, @a AS a', [['a', 'Int', 4]]],
  ['rpc create with parameters', 'CREATE PROCEDURE p_rpc AS SELECT 1 AS one', [['a', 'Int', 4]]],

  // DDL forms and errors.
  ['create or alter new', 'CREATE OR ALTER PROCEDURE p_coa AS SELECT 8 AS value'],
  ['create or alter existing', 'CREATE OR ALTER PROC p_coa AS SELECT 9 AS value'],
  ['alter existing', 'ALTER PROCEDURE p_coa AS SELECT 10 AS value'],
  ['p_coa', 'EXEC p_coa'],
  ['create duplicate', 'CREATE PROCEDURE p_coa AS SELECT 1'],
  ['alter missing', 'ALTER PROCEDURE p_none AS SELECT 1'],
  ['alter table', 'ALTER PROCEDURE t AS SELECT 1'],
  ['create or alter table', 'CREATE OR ALTER PROCEDURE t AS SELECT 1'],
  ['not first in batch', 'SELECT 1; CREATE PROCEDURE p_late AS SELECT 1'],
  ['database prefix', 'CREATE PROCEDURE probe_procedures.dbo.p_db AS SELECT 1'],
  ['duplicate parameter', 'CREATE PROCEDURE p_dupparam @a int, @a int AS SELECT 1'],
  ['parameter redeclared', 'CREATE PROCEDURE p_redeclare @a int AS DECLARE @a int'],
  ['undeclared variable', 'CREATE PROCEDURE p_scope AS SELECT @outer AS o'],
  ['use in body', 'CREATE PROCEDURE p_use AS USE master'],
  ['nested create', 'CREATE PROCEDURE p_nested AS CREATE PROCEDURE p_inner2 AS SELECT 1'],
  ['parenthesized parameters', 'CREATE PROCEDURE p_paren (@a int, @b int = 2) WITH RECOMPILE, EXECUTE AS CALLER AS SELECT @a + @b AS s'],
  ['p_paren', 'EXEC p_paren 1 WITH RECOMPILE'],
  ['drop procedure', 'DROP PROCEDURE p_coa'],
  ['drop missing', 'DROP PROCEDURE p_coa'],
  ['drop if exists', 'DROP PROCEDURE IF EXISTS p_coa'],
  ['drop table name', 'DROP PROCEDURE t'],
  ['drop list continues', "DROP PROC p_none, p_paren; SELECT 'after' AS a, OBJECT_ID('p_paren') AS id"],
]

async function observe(config) {
  const connection = await connect({ ...config, options: { ...config.options, appName: 'msduck-capture', connectTimeout: 30000 } })
  const results = []
  try {
    for (const [name, sql, parameters] of observations) {
      results.push({ name, sql, ...(parameters ? { parameters: parameters.map(([n, type, value, out]) => ({ name: n, type, value, output: !!out })) } : {}), result: await run(connection, sql, parameters) })
      console.log(name)
    }
  } finally {
    connection.close()
    await writeFile(`${output}.partial.json`, JSON.stringify(results, null, 1) + '\n')
  }
  validate(results)
  return results
}

function validate(results) {
  const get = name => {
    const found = results.find(item => item.name === name)
    assert(found, `${name}: missing observation`)
    return found.result
  }
  const errors = name => get(name).errors.map(e => e.number)
  const rows = name => get(name).sets.map(set => set.rows)
  const statuses = name => get(name).tokens.filter(t => t.token === 'returnStatus').map(t => t.value)
  const dones = name => get(name).tokens.filter(t => t.token.startsWith('done')).map(t => [t.token, t.status, t.curCmd, t.rowCount])
  const expect = (name, numbers) => assertSameCapture(errors(name), numbers, `${name}: errors changed`)
  assert.match(rows('server version')[0][0][0], /^1[67]\./, 'reference version')
  assertSameCapture(dones('create foo'), [['done', 0, 222, '0']], 'create completion changed')
  assertSameCapture(rows('exec foo'), [[[7]]], 'repro rows changed')
  assertSameCapture(dones('exec foo'), [['doneInProc', 0x11, 193, '1'], ['doneProc', 0, 224, '0']], 'exec tokens changed')
  assertSameCapture(statuses('exec foo'), [0], 'exec status changed')
  assertSameCapture(rows('bare name call'), [[[7]]], 'bare call changed')
  assertSameCapture(rows('exec foo rpc'), [[[7]], [[3]]], 'rpc call changed')
  assertSameCapture(rows('p_out positional output'), [[[1, 2]], [[3, 3]]], 'positional output changed')
  assertSameCapture(rows('p_out named output'), [[[1, 5]], [[3, 6]]], 'named output changed')
  assertSameCapture(rows('p_out default keyword'), [[[1, 5]], [[3, 6]]], 'DEFAULT changed')
  assertSameCapture(rows('p_out without output'), [[[1, 2]], [[10]]], 'non-output argument changed')
  assertSameCapture(rows('p_types defaults'), [[[1, 'dflt', 1.5, null, 'abc']]], 'defaults changed')
  assertSameCapture(rows('p_ret status'), [[[0]]], 'bare RETURN status changed')
  assertSameCapture(dones('p_ret status').find(done => done[0] === 'doneInProc'), ['doneInProc', 1, 219, '0'], 'bare RETURN completion changed')
  assertSameCapture(get('p_retnull status').info.map(i => i.number), [282], 'NULL status changed')
  assertSameCapture(rows('rowcount after exec').at(-1), [[2]], 'rowcount after exec changed')
  assertSameCapture(rows('rowcount after bare return').at(-1), [[0]], 'rowcount after RETURN changed')
  for (const [name, numbers] of [
    ['missing parameter', [201]], ['too many arguments', [8144]], ['unknown parameter', [8145]],
    ['repeated parameter', [8143]], ['positional after named', [119]], ['output constant', [179]],
    ['output to input parameter', [8162]], ['conversion error', [8114]], ['missing procedure', [2812]],
    ['missing procedure continues', [2812]], ['p_rs status', [50000]], ['p_rs caught by caller', []],
    ['p_dup status', [2627]], ['p_throw aborts batch', [50001]], ['p_missing ends the procedure', [208]],
    ['p_rec over limit', [217]], ['p_tran mismatch', [266]], ['exec string scope', [137]],
    ['exec string return value', [178]], ['executesql missing value', [8178]], ['executesql unknown name', [8178]],
    ['executesql extra name', [8144]], ['executesql varchar statement', [214]], ['executesql caller scope', [137]],
    ['executesql output undeclared', [8162]], ['executesql return value', [178]], ['rpc create with parameters', [156]],
    ['create duplicate', [2714]], ['alter missing', [208]], ['alter table', [2010]], ['create or alter table', [2010]],
    ['not first in batch', [111]], ['database prefix', [166]], ['duplicate parameter', [134]],
    ['parameter redeclared', [134]], ['undeclared variable', [137]], ['use in body', [154]], ['nested create', [156]],
    ['drop missing', [3701]], ['drop table name', [3705]], ['drop list continues', [3701]],
  ]) expect(name, numbers)
  assertSameCapture(rows('missing procedure continues'), [[[1]], [[2]]], 'call errors end only the call')
  assertSameCapture(rows('p_rs status'), [[['after']], [[-6, 0]]], 'RAISERROR status changed')
  assertSameCapture(rows('p_rs caught by caller'), [[[50000, 'boom', 'p_rs']]], 'caller CATCH changed')
  assertSameCapture(rows('p_two status').at(-1), [[-6]], 'highest severity status changed')
  assertSameCapture(rows('p_ret0_after status'), [[[0]]], 'explicit status changed')
  assertSameCapture(rows('p_dup status'), [[['after']], [[-4, 0]]], 'severity 14 status changed')
  assertSameCapture(rows('p_div status'), [[], [[2]], [[-6]]], 'divide status changed')
  assertSameCapture(rows('p_throw aborts batch'), [[['before']]], 'THROW abort changed')
  assertSameCapture(rows('p_throw caught by caller'), [[['before']], [[50001, 'p_throw']]], 'THROW catch changed')
  assertSameCapture(rows('p_missing ends the procedure'), [[['first']], [[99, 208]]], 'scope abort changed')
  assertSameCapture(rows('p_missing caught by caller'), [[['first']], [[208, 'p_missing']]], 'scope abort catch changed')
  assertSameCapture(rows('p_trycatch'), [[], [[8134, 'p_trycatch']], [[9]]], 'procedure TRY changed')
  assertSameCapture(rows('p_caught_status'), [[['p_caught_status']], [[-6]]], 'caught error status changed')
  assertSameCapture(rows('xact_abort raiserror').at(-1), [[1, 1]], 'RAISERROR under XACT_ABORT changed')
  assertSameCapture(rows('p_outer'), [[[42, 5, 1]], [[0]]], 'nested output changed')
  assertSameCapture(rows('p_nest_err caught by caller'), [[[50000, 'p_rs']]], 'nested catch changed')
  assertSameCapture(rows('p_outer_missing'), [[['first']], [['outer after']], [[0, 0]]], 'nested scope abort changed')
  assertSameCapture(rows('p_catch_outer'), [[[50000]], [['end']], [[0]]], 'catch in caller procedure changed')
  assertSameCapture(rows('p_rec 3'), [[[4]]], 'recursion changed')
  assertSameCapture(rows('exec string calls procedure'), [[[7]]], 'dynamic call changed')
  assertSameCapture(rows('executesql output'), [[[12]]], 'sp_executesql OUTPUT changed')
  assertSameCapture(rows('executesql output positional'), [[[2]]], 'sp_executesql positional OUTPUT changed')
  assertSameCapture(rows('executesql raiserror status'), [[[50000]]], 'sp_executesql status changed')
  assertSameCapture(get('rpc output parameter').returnValues, [{ name: 'x', value: 42 }], 'RPC output changed')
  assertSameCapture(rows('p_dyn'), [[['dyn']]], 'dynamic definition changed')
  assertSameCapture(rows('p_coa'), [[[10]]], 'CREATE OR ALTER changed')
  assertSameCapture(rows('drop list continues'), [[['after', null]]], 'DROP list changed')
}

if (check) {
  const bytes = await readFile(fixture)
  const retained = JSON.parse(bytes)
  assert.equal(retained.runs.length, 2, 'independent runs missing')
  assert.equal(createHash('sha256').update(JSON.stringify(retained.runs[0])).digest('hex'), retained.captureSha256, 'capture checksum changed')
  for (const run of retained.runs) validate(run)
  assertSameCapture(retained.runs[0], retained.runs[1], 'independent runs differ')
  console.log(`Checked ${retained.runs[0].length} procedure observations in two retained runs (${retained.image})`)
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if (await realpath(dirname(output)).then(dir => resolve(dir, basename(output))) === fileURLToPath(fixture)) throw Error('capture output must not be the retained fixture')
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    await withReferenceContainer(async config => {
      const run = await observe(config)
      if (runs.length) assertSameCapture(run, runs[0], 'independent reference containers differ')
      runs.push(run)
    }, { image })
  }
  const actual = { image, captureSha256: createHash('sha256').update(JSON.stringify(runs[0])).digest('hex'), runs }
  await writeFile(output, JSON.stringify(actual, null, 1) + '\n', { flag: 'wx' })
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} procedure observations in two fresh containers`)
}
