// Stored procedures, EXEC (string) and sp_executesql through tedious
// (issue #719). Expected values, error numbers and return statuses come from
// reference/gaps-procedures.json (SQL Server 2025, captured by
// scripts/capture-gaps-procedures.mjs; see docs/gaps-procedures.md).
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { test } from 'node:test'
import { Request, TYPES } from 'tedious'
import { start } from '../support/client.mjs'

// DONE, DONEPROC and DONEINPROC bodies as sent, before tedious parses them.
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = { 0xFD: 'done', 0xFE: 'doneProc', 0xFF: 'doneInProc' }
const doneSinks = new WeakMap()
const readToken = StreamParser.prototype.readToken
StreamParser.prototype.readToken = function (type) {
  return recordDone(this, type)
}
function recordDone(parser, type) {
  const sink = doneSinks.get(parser.options)
  if (sink && doneKinds[type]) {
    const at = parser.position
    if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordDone(parser, type))
    sink.push([doneKinds[type], parser.buffer.readUInt16LE(at), parser.buffer.readUInt16LE(at + 2), parser.buffer.readBigUInt64LE(at + 4).toString()])
  }
  return readToken.call(parser, type)
}

// Run one request; collect result sets, RETURNSTATUS values (reported with
// each DONEPROC) and error numbers.
function run(connection, sql, parameters = [], { batch = true, outputs = [] } = {}) {
  return new Promise(resolve => {
    const result = { sets: [], statuses: [], errors: [], returnValues: [], dones: [] }
    const onError = e => result.errors.push(e.number)
    connection.on('errorMessage', onError)
    doneSinks.set(connection.config.options, result.dones)
    const request = new Request(sql, error => {
      connection.off('errorMessage', onError)
      doneSinks.delete(connection.config.options)
      if (error && !result.errors.length) result.errors.push(error.message)
      resolve(result)
    })
    request.on('columnMetadata', columns => result.sets.push({ columns: columns.map(c => c.colName), rows: [] }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => c.value)))
    request.on('doneProc', (_count, _more, status) => { if (status !== undefined) result.statuses.push(status) })
    request.on('returnValue', (name, value) => result.returnValues.push({ name, value }))
    for (const [name, type, value] of parameters) request.addParameter(name, type, value)
    for (const [name, type, value] of outputs) request.addOutputParameter(name, type, value)
    if (batch) connection.execSqlBatch(request)
    else connection.execSql(request)
  })
}

const rows = result => result.sets.map(set => set.rows)

async function setup(t) {
  const connection = await start(t)
  await run(connection, 'CREATE DATABASE proc_compat')
  await run(connection, 'USE proc_compat')
  return connection
}

test('CREATE PROCEDURE foo AS SELECT 7 AS value runs through EXEC', async t => {
  const connection = await setup(t)
  const created = await run(connection, 'CREATE PROCEDURE foo AS SELECT 7 AS value;')
  assert.deepEqual(created.errors, [])
  const executed = await run(connection, 'EXEC foo')
  assert.deepEqual(executed.errors, [])
  assert.deepEqual(executed.sets, [{ columns: ['value'], rows: [[7]] }])
  assert.deepEqual(executed.statuses, [0])
  // A module name alone is a call when it starts the batch.
  assert.deepEqual(rows(await run(connection, 'foo')), [[[7]]])
  // Through sp_executesql (a parameterized tedious request).
  const rpc = await run(connection, 'EXEC foo; SELECT @p AS p', [['p', TYPES.Int, 3]], { batch: false })
  assert.deepEqual(rpc.errors, [])
  assert.deepEqual(rows(rpc), [[[7]], [[3]]])
  const listed = await run(connection, "SELECT name, type, type_desc FROM sys.objects WHERE object_id = OBJECT_ID('dbo.foo')")
  assert.deepEqual(rows(listed), [[['foo', 'P ', 'SQL_STORED_PROCEDURE']]])
})

test('sp_executesql binds declared parameters, named or positional, and returns OUTPUT values', async t => {
  const connection = await setup(t)
  const named = await run(connection, "EXEC sp_executesql N'SELECT @p AS p', N'@p int', @p = 1")
  assert.deepEqual(named.errors, [])
  assert.deepEqual(named.sets, [{ columns: ['p'], rows: [[1]] }])
  assert.deepEqual(named.statuses, [0])
  assert.deepEqual(rows(await run(connection, "EXEC sp_executesql N'SELECT @p AS p, @q AS q', N'@p int, @q int', 1, 2")), [[[1, 2]]])
  const output = await run(connection, "DECLARE @o int; EXEC sp_executesql N'SET @b = @a * 3', N'@a int, @b int OUTPUT', @a = 4, @b = @o OUTPUT; SELECT @o AS o")
  assert.deepEqual(output.errors, [])
  assert.deepEqual(rows(output), [[[12]]])
  const parameterless = await run(connection, "EXEC sp_executesql N'SELECT 9 AS nine'")
  assert.deepEqual(rows(parameterless), [[[9]]])
  // The batch's variables are not visible inside (137); a missing declared
  // value is 8178 and a varchar statement 214.
  assert.deepEqual((await run(connection, "DECLARE @a int = 5; EXEC sp_executesql N'SELECT @a AS a'")).errors, [137])
  assert.deepEqual((await run(connection, "EXEC sp_executesql N'SELECT @p AS p', N'@p int'")).errors, [8178])
  assert.deepEqual((await run(connection, "EXEC sp_executesql 'SELECT 1 AS one'")).errors, [214])
  // The status of a failed statement inside is its error number.
  const raised = await run(connection, "DECLARE @r int; EXEC @r = sp_executesql N'RAISERROR(''x'', 16, 1)'; SELECT @r AS r")
  assert.deepEqual(raised.errors, [50000])
  assert.deepEqual(rows(raised), [[[50000]]])
})

test('parameters, defaults, OUTPUT arguments and RETURN status', async t => {
  const connection = await setup(t)
  await run(connection, `CREATE PROCEDURE p_out @a int, @b int = 5, @c int OUTPUT AS
BEGIN
  SET @c = @a + @b;
  SELECT @a AS a, @b AS b;
  RETURN 3;
END`)
  const positional = await run(connection, 'DECLARE @r int, @x int; EXEC @r = p_out 1, 2, @x OUTPUT; SELECT @r AS r, @x AS x')
  assert.deepEqual(positional.errors, [])
  assert.deepEqual(rows(positional), [[[1, 2]], [[3, 3]]])
  assert.deepEqual(positional.statuses, [3])
  const named = await run(connection, 'DECLARE @r int, @x int; EXEC @r = p_out @a = 1, @c = @x OUTPUT; SELECT @r AS r, @x AS x')
  assert.deepEqual(rows(named), [[[1, 5]], [[3, 6]]])
  const keyword = await run(connection, 'DECLARE @x int; EXEC p_out 1, DEFAULT, @x OUT; SELECT @x AS x')
  assert.deepEqual(rows(keyword), [[[1, 5]], [[6]]])
  // Without OUTPUT the caller's variable is unchanged.
  assert.deepEqual(rows(await run(connection, 'DECLARE @x int = 10; EXEC p_out 1, 2, @x; SELECT @x AS x')).at(-1), [[10]])
  await run(connection, "CREATE PROCEDURE p_types @i int = 1, @s nvarchar(10) = N'dflt', @v varchar(3) = 'abcdef' AS SELECT @i AS i, @s AS s, @v AS v")
  assert.deepEqual(rows(await run(connection, 'EXEC p_types')), [[[1, 'dflt', 'abc']]])
  assert.deepEqual(rows(await run(connection, "EXEC p_types '42', abc")), [[[42, 'abc', 'abc']]])
  await run(connection, 'CREATE PROCEDURE p_ret AS RETURN')
  assert.deepEqual(rows(await run(connection, 'DECLARE @r int = 9; EXEC @r = p_ret; SELECT @r AS r')), [[[0]]])
})

test('call errors match SQL Server numbers', async t => {
  const connection = await setup(t)
  await run(connection, 'CREATE PROCEDURE p_out @a int, @b int = 5, @c int OUTPUT AS SET @c = @a + @b')
  await run(connection, 'CREATE PROCEDURE p_in @a int AS SELECT @a AS a')
  for (const [sql, numbers] of [
    ['EXEC p_out 1', [201]],
    ['EXEC p_out 1, 2, 3, 4', [8144]],
    ['EXEC p_out @a = 1, @zz = 2, @c = 1', [8145]],
    ['EXEC p_out @a = 1, @a = 2, @c = 3', [8143]],
    ['DECLARE @x int = 1; EXEC p_in @x OUTPUT', [8162]],
    ["EXEC p_in 'abc'", [8114]],
    ['EXEC no_such_proc', [2812]],
    ['DECLARE @x int; EXEC p_out @a = 1, 2, @c = @x OUTPUT', [119]],
    ['EXEC p_out 1, 2, 5 OUTPUT', [179]],
    ['SELECT 1; CREATE PROCEDURE bad AS SELECT 1', [111]],
    ['CREATE PROCEDURE p_in AS SELECT 1', [2714]],
    ['ALTER PROCEDURE nope AS SELECT 1', [208]],
    ['CREATE PROCEDURE p_scope AS SELECT @outer AS o', [137]],
    ['CREATE PROCEDURE p_dup @a int, @a int AS SELECT 1', [134]],
    ['DROP PROCEDURE nope', [3701]],
  ]) assert.deepEqual((await run(connection, sql)).errors, numbers, sql)
  // A call error ends only the call.
  const continued = await run(connection, 'SELECT 1 AS before; EXEC no_such_proc; SELECT 2 AS after')
  assert.deepEqual(continued.errors, [2812])
  assert.deepEqual(rows(continued), [[[1]], [[2]]])
})

test('nested calls, multiple result sets, variable scope and nesting level', async t => {
  const connection = await setup(t)
  await run(connection, 'CREATE PROCEDURE p_inner @v int OUTPUT AS BEGIN SET @v = @v * 2; RETURN 5 END')
  await run(connection, 'CREATE PROCEDURE p_outer AS BEGIN DECLARE @v int = 21, @r int; EXEC @r = p_inner @v OUTPUT; SELECT @v AS v, @r AS r, @@NESTLEVEL AS lvl END')
  const nested = await run(connection, 'EXEC p_outer; SELECT @@NESTLEVEL AS top_level')
  assert.deepEqual(nested.errors, [])
  assert.deepEqual(rows(nested), [[[42, 5, 1]], [[0]]])
  assert.deepEqual(nested.statuses, [0])
  await run(connection, "CREATE PROCEDURE p_multi AS BEGIN SELECT 1 AS one; SELECT 2 AS two; SELECT 'x' AS three WHERE 1 = 0 END")
  const multi = await run(connection, 'EXEC p_multi')
  assert.deepEqual(multi.sets.map(set => set.columns), [['one'], ['two'], ['three']])
  assert.deepEqual(rows(multi), [[[1]], [[2]], []])
  // A procedure's DECLARE is its own; SET NOCOUNT reverts when it returns.
  await run(connection, 'CREATE PROCEDURE p_local AS BEGIN SET NOCOUNT ON; DECLARE @x int = 7; SELECT @x AS x END')
  assert.deepEqual(rows(await run(connection, 'DECLARE @x int = 1; EXEC p_local; SELECT @x AS x')), [[[7]], [[1]]])
  // Error functions inside a procedure see the caller's caught error.
  await run(connection, 'CREATE PROCEDURE p_errfn AS SELECT ERROR_NUMBER() AS n')
  assert.deepEqual(rows(await run(connection, "BEGIN TRY RAISERROR('top', 16, 1) END TRY BEGIN CATCH EXEC p_errfn END CATCH")), [[[50000]]])
  await run(connection, 'CREATE PROCEDURE p_rec @n int AS BEGIN IF @n > 0 BEGIN DECLARE @m int = @n - 1; EXEC p_rec @m END ELSE SELECT @@NESTLEVEL AS lvl END')
  assert.deepEqual(rows(await run(connection, 'EXEC p_rec 3')), [[[4]]])
  assert.deepEqual((await run(connection, 'EXEC p_rec 40')).errors, [217])
})

test('errors inside procedures: status, TRY/CATCH and propagation', async t => {
  const connection = await setup(t)
  await run(connection, "CREATE PROCEDURE p_rs AS BEGIN RAISERROR('boom', 16, 1); SELECT 'after' AS after END")
  // RAISERROR ends only its statement; the status reflects its severity.
  const continued = await run(connection, 'DECLARE @r int; EXEC @r = p_rs; SELECT @r AS r')
  assert.deepEqual(continued.errors, [50000])
  assert.deepEqual(rows(continued), [[['after']], [[-6]]])
  assert.deepEqual(continued.statuses, [-6])
  // A caller's CATCH receives the error and the rest of the procedure is skipped.
  const caught = await run(connection, "BEGIN TRY EXEC p_rs; SELECT 'not' AS n END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS num, ERROR_MESSAGE() AS msg, ERROR_PROCEDURE() AS proc_name END CATCH")
  assert.deepEqual(caught.errors, [])
  assert.deepEqual(rows(caught), [[[50000, 'boom', 'p_rs']]])
  // A procedure's own TRY/CATCH.
  await run(connection, 'CREATE PROCEDURE p_try AS BEGIN BEGIN TRY SELECT 1/0 AS x END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n, ERROR_PROCEDURE() AS p END CATCH; RETURN 9 END')
  const own = await run(connection, 'DECLARE @r int; EXEC @r = p_try; SELECT @r AS r')
  assert.deepEqual(own.errors, [])
  assert.deepEqual(rows(own), [[], [[8134, 'p_try']], [[9]]])
  // THROW aborts the batch, including the caller.
  await run(connection, "CREATE PROCEDURE p_throw AS BEGIN SELECT 'before' AS b; THROW 50001, 'thrown', 1; SELECT 'after' AS a END")
  const thrown = await run(connection, "EXEC p_throw; SELECT 'never' AS n")
  assert.deepEqual(thrown.errors, [50001])
  assert.deepEqual(rows(thrown), [[['before']]])
  const throwCaught = await run(connection, 'BEGIN TRY EXEC p_throw END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS num END CATCH')
  assert.deepEqual(rows(throwCaught), [[['before']], [[50001]]])
  // A nested procedure's error reaches the outer caller's CATCH.
  await run(connection, "CREATE PROCEDURE p_nest AS BEGIN EXEC p_rs; SELECT 'outer after' AS a END")
  const nested = await run(connection, 'BEGIN TRY EXEC p_nest END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS num, ERROR_PROCEDURE() AS p END CATCH')
  assert.deepEqual(rows(nested), [[[50000, 'p_rs']]])
  // A missing table ends only the procedure; the caller continues.
  await run(connection, "CREATE PROCEDURE p_missing AS BEGIN SELECT 'first' AS f; SELECT * FROM no_such_table; SELECT 'after' AS a END")
  const missing = await run(connection, "DECLARE @r int = 99; EXEC @r = p_missing; SELECT @r AS r, @@ERROR AS e")
  assert.deepEqual(missing.errors, [208])
  assert.deepEqual(rows(missing), [[['first']], [[99, 208]]])
})

test('CREATE OR ALTER, ALTER and DROP PROCEDURE [IF EXISTS]', async t => {
  const connection = await setup(t)
  await run(connection, 'CREATE OR ALTER PROCEDURE p AS SELECT 8 AS value')
  assert.deepEqual(rows(await run(connection, 'EXEC p')), [[[8]]])
  const id = rows(await run(connection, "SELECT OBJECT_ID('p') AS id"))[0][0][0]
  await run(connection, 'ALTER PROCEDURE p AS SELECT 9 AS value')
  assert.deepEqual(rows(await run(connection, 'EXEC dbo.p')), [[[9]]])
  await run(connection, 'CREATE OR ALTER PROC p AS SELECT 10 AS value')
  assert.deepEqual(rows(await run(connection, 'EXECUTE [dbo].[p]')), [[[10]]])
  assert.deepEqual(rows(await run(connection, "SELECT OBJECT_ID('p') AS id"))[0][0][0], id)
  assert.deepEqual((await run(connection, 'DROP PROCEDURE p')).errors, [])
  assert.deepEqual(rows(await run(connection, "SELECT OBJECT_ID('p') AS id")), [[[null]]])
  assert.deepEqual((await run(connection, 'DROP PROCEDURE IF EXISTS p')).errors, [])
  assert.deepEqual((await run(connection, 'DROP PROC p')).errors, [3701])
  await run(connection, 'CREATE TABLE t (id int)')
  assert.deepEqual((await run(connection, 'DROP PROCEDURE t')).errors, [3705])
  assert.deepEqual((await run(connection, 'ALTER PROCEDURE t AS SELECT 1')).errors, [2010])
})

test('EXEC (string) runs dynamic SQL in its own scope', async t => {
  const connection = await setup(t)
  assert.deepEqual(rows(await run(connection, "EXEC ('SELECT 5 AS five')")), [[[5]]])
  assert.deepEqual(rows(await run(connection, "DECLARE @s nvarchar(100) = N'SELECT 6 AS six'; EXEC (@s)")), [[[6]]])
  assert.deepEqual(rows(await run(connection, "EXEC ('SELECT ' + '2 AS two')")), [[[2]]])
  assert.deepEqual((await run(connection, "DECLARE @t int; EXEC ('SELECT @t')")).errors, [137])
  assert.deepEqual((await run(connection, "EXEC ('SELECT 1 AS a; RETURN 4')")).errors, [178])
  // Dynamic SQL may define a procedure.
  assert.deepEqual((await run(connection, "EXEC sp_executesql N'CREATE PROCEDURE p_dyn AS SELECT ''dyn'' AS d'")).errors, [])
  assert.deepEqual(rows(await run(connection, "EXEC ('EXEC p_dyn')")), [[['dyn']]])
})

// Every captured observation, replayed in order on one connection. Values,
// error numbers, return statuses, RETURNVALUEs and the DONE token sequence
// must match SQL Server except for the differences listed here
// (docs/gaps-procedures.md).
const reference = JSON.parse(readFileSync(new URL('../../reference/gaps-procedures.json', import.meta.url), 'utf8')).runs[0]
// A failed call reports RETURNSTATUS 1 before its DONEPROC; SQL Server sends
// no status (or, for sp_executesql, the error number).
const failedCall = { statuses: [1] }
const known = {
  'server version': null,
  'missing parameter': failedCall,
  'too many arguments': failedCall,
  'unknown parameter': failedCall,
  'repeated parameter': failedCall,
  'output to input parameter': failedCall,
  'conversion error': failedCall,
  'missing procedure': failedCall,
  'missing procedure continues': failedCall,
  'p_missing ends the procedure': failedCall,
  'p_tran mismatch': failedCall,
  'exec string transaction mismatch': failedCall,
  'executesql transaction mismatch': failedCall,
  'exec string scope': failedCall,
  'exec string return value': failedCall,
  'exec string missing table': failedCall,
  'executesql output undeclared': failedCall,
  'executesql unknown name': failedCall,
  'executesql extra name': failedCall,
  'executesql varchar statement': failedCall,
  'executesql caller scope': failedCall,
  'executesql missing table status': failedCall,
  'executesql return value': failedCall,
  // A duplicate key ends the batch in msduck (not yet a statement error).
  'p_dup status': { sets: [], statuses: [] },
  // @@ERROR after a call is 0, not the called scope's last error.
  'exec string raiserror': { sets: [[[0]]] },
  // RPC OUTPUT parameters need RETURNVALUE support in src/rpc.rs.
  'rpc output parameter': { errors: [40515], statuses: [], returnValues: [] },
  // A failed DROP ends the batch instead of the statement.
  'drop list continues': { sets: [] },
}
// Observations whose DONE tokens differ: an aborted batch ends with CurCmd 0
// instead of 253 (and 223 for DROP PROCEDURE); 8144 for a known procedure
// is a compilation error in SQL Server but a call error here; transaction
// and SET statements complete with CurCmd 0; NOCOUNT clears the batch-level
// SELECT row count.
const doneDifferences = new Set([
  'server version', 'too many arguments', 'positional after named', 'output constant', 'p_dup status',
  'p_throw aborts batch', 'p_trycatch nocount', 'xact_abort raiserror', 'p_outer_throw', 'p_rec over limit',
  'p_tran mismatch', 'executesql extra name', 'rpc output parameter', 'not first in batch', 'drop missing',
  'drop table name', 'drop list continues', 'exec string transaction mismatch',
  'executesql transaction mismatch', 'output overflow', 'positional expression',
])

test('captured SQL Server observations replay with the same results', { timeout: 120000 }, async t => {
  const connection = await start(t)
  for (const { name, sql, parameters, result } of reference) {
    if (known[name] === null) continue
    const types = { Int: TYPES.Int }
    const inputs = (parameters ?? []).filter(p => !p.output).map(p => [p.name, types[p.type], p.value])
    const outputs = (parameters ?? []).filter(p => p.output).map(p => [p.name, types[p.type], p.value])
    const actual = await run(connection, sql, inputs, { batch: !parameters, outputs })
    const expected = {
      sets: result.sets.map(set => set.rows),
      errors: result.errors.map(e => e.number),
      statuses: result.tokens.filter(token => token.token === 'returnStatus').map(token => token.value),
      returnValues: result.returnValues,
      ...known[name],
    }
    assert.deepEqual({ sets: rows(actual), errors: actual.errors, statuses: actual.statuses, returnValues: actual.returnValues }, expected, name)
    const dones = result.tokens.filter(token => token.token.startsWith('done')).map(token => [token.token, token.status, token.curCmd, token.rowCount])
    if (doneDifferences.has(name)) assert.notDeepEqual(actual.dones, dones, `${name}: now matches; remove it from doneDifferences`)
    else assert.deepEqual(actual.dones, dones, `${name}: DONE tokens`)
  }
})
