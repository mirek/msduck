// User-defined scalar, inline and multi-statement table-valued functions
// (docs/gaps-functions.md). The capture cases replay against the retained
// SQL Server fixture; the remaining tests cover RPC, preparation,
// transactions and many-row queries through tedious.
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { Request, TYPES } from 'tedious'
import { cases, fixture, runCase } from '../../scripts/capture-gaps-functions.mjs'
import { capture } from '../../scripts/lib/compatibility.mjs'
import { describeFirstDifference } from '../../scripts/lib/reference.mjs'
import { isDeepStrictEqual } from 'node:util'
import { query, start } from '../support/client.mjs'

// COLMETADATA flag bits other than nullability (computed, updateable) are
// not modeled for function results; compare the nullable bit only.
function comparable(result) {
  const copy = structuredClone(result)
  for (const set of copy.sets) for (const column of set.columns) column.flags &= 1
  return copy
}

// Differences from SQL Server that msduck keeps, each asserted exactly so a
// change in either direction is noticed.
const known = {
  // NOT NULL return-table columns are reported nullable (IntN, flag 1)
  // because the rows come from a folded query, not a declared table.
  'multi-statement table-valued': {
    1: result => notNullColumn(result),
    2: result => notNullColumn(result),
  },
  // Calls with known arguments run while the statement is compiled, so a
  // nesting error arrives before the result's COLMETADATA.
  'loops and recursion': {
    4: result => {
      assert.deepEqual(result.sets, [])
      assert.deepEqual(result.errors.map(e => [e.number, e.class, e.message]), [[217, 16, 'Maximum stored procedure, function, trigger, or view nesting level exceeded (limit 32).']])
    },
  },
  // Loops and recursion run statement by statement only when every
  // argument is known before the query runs; per-row calls are refused.
  'per-row loops and recursion': {
    3: result => unsupported(result, 'unsupported: user-defined function dbo.total with column arguments: WHILE'),
    4: result => unsupported(result, 'unsupported: recursive user-defined function dbo.fact with column arguments'),
  },
  // A misplaced definition reports 111 only (line 1); SQL Server also
  // compiles the rest of the batch and reports line 2 and error 178.
  'definition statements': {
    20: result => assert.deepEqual(result.errors.map(e => [e.number, e.state, e.class, e.lineNumber]), [[111, 1, 15, 1]]),
  },
}

function unsupported(result, message) {
  assert.deepEqual(result.sets, [])
  assert.deepEqual(result.errors.map(e => [e.number, e.message]), [[40515, message]])
}

function notNullColumn(result) {
  const [i, s] = result.sets[0].columns
  assert.deepEqual([i.name, i.type, i.length, i.flags & 1], ['i', 'IntN', 4, 1])
  assert.deepEqual([s.name, s.type, s.flags & 1], ['s', 'VarChar', 1])
}

test('function cases match the SQL Server capture', { timeout: 600000 }, async t => {
  const reference = JSON.parse(await readFile(fixture, 'utf8'))
  const connection = await start(t, { options: { requestTimeout: 120000 } })
  let index = 0
  for (const entry of cases) {
    const database = `functions_case_${index++}`
    const created = await capture(connection, `CREATE DATABASE ${database}; USE ${database}`)
    assert.deepEqual(created.errors, [], entry.name)
    const results = await runCase(connection, entry)
    const expected = reference.cases.find(c => c.name === entry.name)?.results
    assert.ok(expected, `fixture has ${entry.name}`)
    assert.equal(results.length, expected.length, entry.name)
    results.forEach((result, i) => {
      const label = `${entry.name} #${i}: ${entry.batches[i].slice(0, 80)}`
      const check = known[entry.name]?.[i]
      if (check) {
        assert.ok(!isDeepStrictEqual(comparable(result), comparable(expected[i])), `${label} now matches SQL Server; remove the known difference`)
        check(result)
        return
      }
      const actual = comparable(result)
      const wanted = comparable(expected[i])
      if (!isDeepStrictEqual(actual, wanted)) assert.fail(`${label} (${describeFirstDifference(actual, wanted)})`)
    })
    await capture(connection, 'USE master')
  }
})

test('generic repros return values', async t => {
  const connection = await start(t)
  await query(connection, 'CREATE FUNCTION dbo.foo(@x int) RETURNS int AS BEGIN RETURN @x+1; END;')
  assert.deepEqual((await query(connection, 'SELECT dbo.foo(1) AS a, dbo.foo(41) AS b')).rows, [[2, 42]])
  await query(connection, "CREATE FUNCTION dbo.hello() RETURNS varchar(10) AS BEGIN RETURN 'hello' END")
  assert.deepEqual((await query(connection, 'SELECT dbo.hello()')).rows, [['hello']])
  await query(connection, 'CREATE FUNCTION dbo.caller(@x int) RETURNS int WITH EXECUTE AS CALLER AS BEGIN RETURN @x * 2 END')
  assert.deepEqual((await query(connection, 'SELECT dbo.caller(21)')).rows, [[42]])
  await query(connection, 'CREATE FUNCTION dbo.it(@x int) RETURNS TABLE AS RETURN (SELECT @x AS x, @x + 1 AS y)')
  const inline = await query(connection, 'SELECT x, y FROM dbo.it(5)')
  assert.deepEqual(inline.rows, [[5, 6]])
  assert.deepEqual(inline.columns[0].map(c => [c.colName, c.type.name]), [['x', 'IntN'], ['y', 'IntN']])
  await query(connection, 'CREATE FUNCTION dbo.mt(@x int) RETURNS @t TABLE (a int, b nvarchar(10)) AS BEGIN INSERT @t VALUES (@x, N\'one\'); INSERT @t VALUES (@x + 1, N\'two\'); RETURN END')
  assert.deepEqual((await query(connection, 'SELECT a, b FROM dbo.mt(7) ORDER BY a')).rows, [[7, 'one'], [8, 'two']])
  await assert.rejects(query(connection, 'SELECT dbo.foo(1, 2)'), error => error.number === 8144)
  await assert.rejects(query(connection, 'CREATE FUNCTION dbo.bad(@x int) RETURNS int AS BEGIN RETURN @y END'), error => error.number === 137)
})

test('functions take RPC parameters, preparation and transactions', async t => {
  const connection = await start(t)
  // sp_executesql accepts a definition only without parameters (SQL Server
  // compiles a parameterized batch around it and reports 156).
  await assert.rejects(
    query(connection, 'CREATE FUNCTION dbo.add_to(@a int, @b int) RETURNS int AS BEGIN RETURN @a + @b END', [['unused', TYPES.Int, 1]]),
    error => error.number === 156)
  await query(connection, 'CREATE FUNCTION dbo.add_to(@a int, @b int) RETURNS int AS BEGIN RETURN @a + @b END')
  const rpc = await query(connection, 'SELECT dbo.add_to(@p, 10) AS v', [['p', TYPES.Int, 32]])
  assert.deepEqual(rpc.rows, [[42]])
  assert.deepEqual(await prepareAndExecute(connection, 'SELECT dbo.add_to(@p, 1) AS v', [41, 99]), [[[42]], [[100]]])
  await query(connection, 'CREATE FUNCTION dbo.it(@n int) RETURNS TABLE AS RETURN SELECT @n AS n UNION ALL SELECT @n * 2')
  const table = await query(connection, 'SELECT n FROM dbo.it(@p) ORDER BY n', [['p', TYPES.Int, 4]])
  assert.deepEqual(table.rows, [[4], [8]])
  await query(connection, 'BEGIN TRANSACTION')
  await query(connection, 'CREATE FUNCTION dbo.temporary() RETURNS int AS BEGIN RETURN 5 END')
  assert.deepEqual((await query(connection, 'SELECT dbo.temporary()')).rows, [[5]])
  await query(connection, 'ROLLBACK')
  await assert.rejects(query(connection, 'SELECT dbo.temporary()'), error => error.number === 4121)
})

// Prepare once (sp_prepare) and execute for each value (sp_execute).
function prepareAndExecute(connection, sql, values) {
  return new Promise((resolve, reject) => {
    const results = []
    let rows = []
    const request = new Request(sql, error => {
      if (error) return reject(error)
      results.push(rows)
      rows = []
      if (results.length === values.length) resolve(results)
      else connection.execute(request, { p: values[results.length] })
    })
    request.addParameter('p', TYPES.Int)
    request.on('row', row => rows.push(row.map(cell => cell.value)))
    request.on('prepared', () => connection.execute(request, { p: values[0] }))
    connection.prepare(request)
  })
}

test('scalar functions run set-wise over many rows', async t => {
  const connection = await start(t, { options: { requestTimeout: 60000 } })
  await query(connection, 'CREATE TABLE dbo.many(id int)')
  await query(connection, 'INSERT dbo.many SELECT value FROM GENERATE_SERIES(1, 20000)')
  await query(connection, 'CREATE FUNCTION dbo.sq(@x int) RETURNS bigint AS BEGIN DECLARE @y bigint = @x; IF @y % 2 = 0 RETURN @y * @y; RETURN -@y END')
  const started = Date.now()
  const { rows } = await query(connection, 'SELECT COUNT(*), SUM(dbo.sq(id)) FROM dbo.many WHERE dbo.sq(id) > 0')
  assert.deepEqual(rows, [[10000, '1333533340000']])
  assert.ok(Date.now() - started < 30000, 'set-wise evaluation')
})
