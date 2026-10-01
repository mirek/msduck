// RPC requests that name a procedure (tedious callProcedure; mssql's
// request.execute sends the same RPC) and RPC OUTPUT parameters (issue
// #753). Expected values, error numbers, RETURNSTATUS, RETURNVALUE and DONE
// tokens come from reference/gaps-rpc-procedures.json (SQL Server 2022,
// captured by scripts/capture-gaps-rpc-procedures.mjs; see
// docs/gaps-rpc-procedures.md).
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { run, observations } from '../../scripts/capture-gaps-rpc-procedures.mjs'
import { start } from '../support/client.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/gaps-rpc-procedures.json', import.meta.url), 'utf8')).runs[0]

// Everything a client sees, without column metadata, ENVCHANGE and message
// texts: DONE, DONEPROC, DONEINPROC, RETURNSTATUS and RETURNVALUE bytes in
// order, row counts, rows, error and info numbers, and output values.
function observed(result) {
  return {
    errors: result.errors.map(e => e.number),
    info: result.info.map(e => e.number),
    sets: result.sets.map(set => set.rows),
    returnValues: result.returnValues,
    tokens: result.tokens
      .filter(t => !['columns', 'envChange', 'order', 'error', 'info'].includes(t.token))
      .map(t => t.token === 'row' ? `row*${t.count}` : `${t.token}:${t.hex}`),
  }
}

// Replace the CurCmd of the `index`th token (a DONE-family token).
const command = (index, value) => expected => {
  const tokens = [...expected.tokens]
  const [name, hex] = tokens.at(index).split(':')
  const bytes = Buffer.from(hex, 'hex')
  bytes.writeUInt16LE(value, 3)
  tokens.splice(index < 0 ? tokens.length + index : index, 1, `${name}:${bytes.toString('hex')}`)
  return { ...expected, tokens }
}

// Differences that remain (docs/gaps-rpc-procedures.md). Each must still
// differ, so a fix is noticed.
const known = {
  // SERVERPROPERTY is not supported.
  'server version': null,
  // BEGIN TRANSACTION, ROLLBACK and SET XACT_ABORT complete with CurCmd 0
  // (SQL Server: 212, 210, 185 and 186).
  'transaction count': command(0, 0),
  'rollback after transaction count': command(-1, 0),
  'xact_abort on': expected => command(1, 0)(command(0, 0)(expected)),
  'after xact_abort': command(-1, 0),
  // An aborted engine RPC batch ends with DONEPROC CurCmd 0, not 224.
  'prepexec throw without outputs': command(-1, 0),
  // sp_prepare sends no metadata DONEINPROC before its status.
  'prepare output': expected => ({ ...expected, tokens: expected.tokens.slice(1) }),
}

test('captured SQL Server observations replay with the same results', { timeout: 180000 }, async t => {
  assert.deepEqual(reference.map(o => o.name), observations.map(([name]) => name), 'fixture and observations differ')
  const connection = await start(t)
  for (const { name, kind, text, parameters, result } of reference) {
    if (known[name] === null) continue
    const actual = observed(await run(connection, kind, text, parameters))
    const expected = observed(result)
    if (known[name]) {
      assert.notDeepEqual(actual, expected, `${name}: now matches; remove it from known`)
      assert.deepEqual(actual, known[name](expected), name)
    } else {
      assert.deepEqual(actual, expected, name)
    }
  }
})

test('a procedure call by name returns rows, its status and OUTPUT values', async t => {
  const connection = await start(t)
  await run(connection, 'batch', 'CREATE PROCEDURE dbo.add_one @value int, @step int = 1, @result bigint OUTPUT AS BEGIN SET @result = @value + @step; SELECT @value AS value; RETURN 7 END')
  const named = await run(connection, 'proc', 'dbo.add_one', [['value', 'Int', 41], ['result', 'BigInt', null, true]])
  assert.deepEqual(named.errors, [])
  assert.deepEqual(named.sets.map(set => set.rows), [[[41]]])
  assert.deepEqual(named.returnValues, [{ name: 'result', value: '42', type: 'IntN' }])
  const tail = named.tokens.slice(-3).map(t => t.token)
  assert.deepEqual(tail, ['returnStatus', 'returnValue', 'doneProc'])
  assert.equal(named.tokens.at(-3).hex, '7907000000')
  // Positional, with an explicit step, and a case-insensitive name.
  const positional = await run(connection, 'proc', 'ADD_ONE', [['', 'Int', 1], ['', 'Int', 10], ['', 'BigInt', null, true]])
  assert.deepEqual(positional.returnValues, [{ name: '', value: '11', type: 'IntN' }])
  // Call errors end the call with the error and DONEPROC only.
  const missing = await run(connection, 'proc', 'dbo.add_one', [['step', 'Int', 1]])
  assert.deepEqual(missing.errors.map(e => e.number), [201])
  assert.deepEqual(missing.tokens.map(t => t.token), ['error', 'doneProc'])
  assert.deepEqual((await run(connection, 'proc', 'no_such_procedure')).errors.map(e => e.number), [2812])
  // The name is a name, never SQL.
  assert.deepEqual((await run(connection, 'proc', "add_one; SELECT 'injected'")).errors.map(e => e.number), [2812])
})

test('sp_executesql with addOutputParameter returns the values', async t => {
  const connection = await start(t)
  const result = await run(connection, 'sql', 'SET @total = @a + @b; SELECT @total AS total', [['a', 'Int', 2], ['b', 'Int', 3], ['total', 'Int', null, true]])
  assert.deepEqual(result.errors, [])
  assert.deepEqual(result.sets.map(set => set.rows), [[[5]]])
  assert.deepEqual(result.returnValues, [{ name: 'total', value: 5, type: 'IntN' }])
  const text = await run(connection, 'sql', "SET @s = N'añb'", [['s', 'NVarChar', null, true, { length: 10 }]])
  assert.deepEqual(text.returnValues, [{ name: 's', value: 'añb', type: 'NVarChar' }])
})

test('a duplicate key in sp_prepexec ends the statement and keeps the handle', async t => {
  const connection = await start(t)
  await run(connection, 'batch', 'CREATE TABLE dbo.items (id int CONSTRAINT pk_items PRIMARY KEY); INSERT dbo.items VALUES (1)')
  const prepared = await run(connection, 'proc', 'sp_prepexec', [['handle', 'Int', null, true], ['params', 'NVarChar', '@id int'], ['stmt', 'NVarChar', 'INSERT dbo.items VALUES (@id); SELECT COUNT(*) AS n FROM dbo.items'], ['id', 'Int', 1]])
  assert.deepEqual(prepared.errors.map(e => e.number), [2627])
  assert.deepEqual(prepared.info.map(e => e.number), [3621])
  assert.deepEqual(prepared.sets.map(set => set.rows), [[[1]]])
  assert.deepEqual(prepared.returnValues.map(v => v.name), ['handle'])
  const handle = prepared.returnValues[0].value
  const again = await run(connection, 'proc', 'sp_execute', [['handle', 'Int', handle], ['id', 'Int', 2]])
  assert.deepEqual(again.errors, [])
  assert.deepEqual(again.sets.map(set => set.rows), [[[2]]])
})
