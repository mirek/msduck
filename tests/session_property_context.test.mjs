import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { isDeepStrictEqual } from 'node:util'
import { Request, TYPES } from 'tedious'
import { start } from './support/client.mjs'
import { describeFirstDifference } from '../scripts/lib/reference.mjs'
import { observe } from '../scripts/capture-session-property-context.mjs'

// SQL Server 2025 observations from scripts/capture-session-property-context.mjs.
const fixture = JSON.parse(readFileSync(new URL('../reference/session-property-context.json', import.meta.url), 'utf8')).runs[0]

// The owner's "Sql wrapper" probe: each statement through tedious execSql.
const ownerStatements = [
  ['select 42', 'select 42'],
  ['sessionproperty', `select
  cast(sessionproperty('ANSI_NULLS') as int) as ansiNulls,
  cast(sessionproperty('ANSI_PADDING') as int) as ansiPadding,
  cast(sessionproperty('ANSI_WARNINGS') as int) as ansiWarnings,
  cast(sessionproperty('ARITHABORT') as int) as arithabort,
  cast(sessionproperty('CONCAT_NULL_YIELDS_NULL') as int) as concatNullYieldsNull,
  cast(sessionproperty('QUOTED_IDENTIFIER') as int) as quotedIdentifier,
  cast(sessionproperty('NUMERIC_ROUNDABORT') as int) as numericRoundabort;`],
  ['sp_set_session_context', "exec sys.sp_set_session_context @key = N'email', @value = null"],
  ['parameter', 'select @p as v', [['p', TYPES.Int, 42]]],
]
function execSql(connection, sql, parameters = []) {
  return new Promise(resolve => {
    const rows = []
    const request = new Request(sql, (error, rowCount) => resolve({ error, rowCount, rows }))
    request.on('row', cells => rows.push(Object.fromEntries(cells.map(c => [c.metadata.colName, c.value]))))
    for (const parameter of parameters) request.addParameter(...parameter)
    connection.execSql(request)
  })
}

test('owner Sql wrapper statements succeed with SQL Server rows', { timeout: 30000 }, async t => {
  const connection = await start(t)
  const results = []
  for (const [name, sql, parameters] of ownerStatements) {
    const { error, rowCount, rows } = await execSql(connection, sql, parameters)
    results.push({ name, ok: !error, rowCount, rows })
  }
  assert.deepEqual(results, [
    { name: 'select 42', ok: true, rowCount: 1, rows: [{ '': 42 }] },
    { name: 'sessionproperty', ok: true, rowCount: 1, rows: [{ ansiNulls: 1, ansiPadding: 1, ansiWarnings: 1, arithabort: 1, concatNullYieldsNull: 1, quotedIdentifier: 1, numericRoundabort: 0 }] },
    { name: 'sp_set_session_context', ok: true, rowCount: 0, rows: [] },
    { name: 'parameter', ok: true, rowCount: 1, rows: [{ v: 42 }] },
  ])
})

// Divergences that remain explicit. Each record names its known differences;
// they are patched narrowly and the rest of the record (rows, descriptors,
// errors and DONE tokens) must still be identical to SQL Server.
const OPTIONS = ['ANSI_NULLS', 'ANSI_PADDING', 'ANSI_WARNINGS', 'ARITHABORT', 'CONCAT_NULL_YIELDS_NULL', 'QUOTED_IDENTIFIER', 'NUMERIC_ROUNDABORT']
const known = new Map()
const add = (name, ...patches) => known.set(name, [...(known.get(name) ?? []), ...patches])
// A result set with a sql_variant column loses its projection facts: every
// column reports flags 1 instead of 33 (also on main for CAST(1 AS SQL_VARIANT)).
for (const name of ['sessionproperty uncast', 'sessionproperty names', 'sessionproperty variable name',
  'sessionproperty parameter name', 'sessionproperty integer argument', 'after sp_executesql set',
  'set context bigint variable', 'set context read_only null', 'set context read_only two',
  'set context in transaction', 'after reset',
  ...OPTIONS.flatMap(o => o === 'NUMERIC_ROUNDABORT' ? [`after set ${o} ON`, `restore ${o} OFF`] : [`after set ${o} OFF`, `restore ${o} ON`])]) add(name, 'flags')
// Diagnostics carry no procedure name, and ERROR_PROCEDURE() is NULL.
for (const name of ['set context null key', 'set context long key', 'set context empty key', 'set context missing value',
  'set context extra argument', 'set context nvarchar max value', 'set context read_only violation rpc',
  'set context read_only null', 'set context read_only two', 'set context caught']) add(name, 'procName')
// SET <option> OFF is refused except for ANSI_WARNINGS, so SESSIONPROPERTY
// keeps reporting the session's real (login) value afterwards.
for (const [index, option] of OPTIONS.entries()) if (option !== 'ANSI_WARNINGS') add(`after set ${option} ${option === 'NUMERIC_ROUNDABORT' ? 'ON' : 'OFF'}`, ['option', index])

// Must still differ, as an explicit error or a known pre-existing defect:
const divergent = new Map([
  ['login options', 'error: @@OPTIONS is unsupported'],
  ['set context return status', 'error: EXEC @status = procedure does not parse'],
  ['set context varchar value', 'error: varchar session values are unsupported'],
  ['read context value', 'error: an nvarchar value has no sql_variant carrier'],
  ['read context key spelling', 'error: an nvarchar value has no sql_variant carrier'],
  ['read context parameter key', 'error: an nvarchar value has no sql_variant carrier'],
  ['read context rpc parameter', 'error: an nvarchar value has no sql_variant carrier'],
  ['set context overwrite type', 'error: an nvarchar value has no sql_variant carrier'],
  ['set context read_only violation batch', 'error: an nvarchar value has no sql_variant carrier'],
  ['before reset', 'error: an nvarchar value has no sql_variant carrier'],
  ['read_only after reset', 'error: an nvarchar value has no sql_variant carrier'],
  ...OPTIONS.filter(o => o !== 'NUMERIC_ROUNDABORT').map(o => [`set ${o} OFF`, 'error: @@OPTIONS (and SET OFF except ANSI_WARNINGS) is unsupported']),
  ['set NUMERIC_ROUNDABORT ON', 'error: SET NUMERIC_ROUNDABORT ON is unsupported'],
  ['sessionproperty base types', 'value: CAST(SQL_VARIANT_PROPERTY(..., \'BaseType\') AS NVARCHAR) returns struct text'],
  ['read context int', 'value: CAST(SQL_VARIANT_PROPERTY(..., \'BaseType\') AS NVARCHAR) returns struct text'],
  ['restore QUOTED_IDENTIFIER ON', 'tokens: SET QUOTED_IDENTIFIER emits a DONE that SQL Server does not'],
])

function patched(local, reference) {
  const copy = structuredClone(local)
  for (const patch of known.get(reference.name) ?? []) {
    if (patch === 'flags') {
      for (const [i, set] of copy.result.sets.entries()) {
        for (const [j, column] of set.columns.entries()) {
          if (column.flags === 1 && reference.result.sets[i]?.columns[j]?.flags === 33) column.flags = 33
        }
      }
    } else if (patch === 'procName') {
      for (const error of copy.result.errors) if (error.procName === '') error.procName = 'sys.sp_set_session_context'
      const row = copy.result.sets[0]?.rows[0]
      if (reference.name === 'set context caught' && row?.[1] === null) row[1] = 'sys.sp_set_session_context'
    } else {
      const [, index] = patch
      const row = copy.result.sets[0].rows[0]
      const login = OPTIONS[index] === 'NUMERIC_ROUNDABORT' ? 0 : 1
      if (row[index] === login) row[index] = reference.result.sets[0].rows[0][index]
    }
  }
  return copy
}

test('SESSIONPROPERTY and session context replay the SQL Server capture', { timeout: 60000 }, async t => {
  const connection = await start(t, { options: { requestTimeout: 10000 } })
  const local = await observe(connection)
  assert.equal(local.length, fixture.length)
  const problems = []
  let identical = 0
  for (const [index, reference] of fixture.entries()) {
    const actual = local[index]
    if (actual.name !== reference.name) { problems.push(`record ${index} is ${actual.name}, expected ${reference.name}`); continue }
    if (divergent.has(reference.name)) {
      const reason = divergent.get(reference.name)
      if (isDeepStrictEqual(actual, reference)) problems.push(`${reference.name}: now matches; remove the known divergence`)
      else if (reason.startsWith('error:') && !actual.result.errors.length) problems.push(`${reference.name}: expected an explicit error`)
      continue
    }
    if (known.has(reference.name) && isDeepStrictEqual(actual, reference)) problems.push(`${reference.name}: known gap is fixed; update the test`)
    const candidate = known.has(reference.name) ? patched(actual, reference) : actual
    if (!isDeepStrictEqual(candidate, reference)) problems.push(`${reference.name}: ${describeFirstDifference(candidate, reference)}`)
    else if (!known.has(reference.name)) identical++
  }
  assert.deepEqual(problems, [])
  // The owner's statements are among the byte-for-byte identical records.
  for (const name of ['owner select 42', 'owner sessionproperty', 'owner set context null', 'owner parameter']) {
    assert(isDeepStrictEqual(local.find(r => r.name === name), fixture.find(r => r.name === name)), name)
  }
  assert.equal(identical, fixture.length - new Set([...known.keys(), ...divergent.keys()]).size)
})

test('RESETCONNECTION clears keys and read_only locks', { timeout: 30000 }, async t => {
  const connection = await start(t)
  const read = sql => execSql(connection, sql)
  assert.equal((await read("exec sys.sp_set_session_context @key = N'tenant', @value = 7, @read_only = 1")).error, undefined)
  assert.equal((await read("exec sys.sp_set_session_context @key = N'tenant', @value = 8")).error.number, 15664)
  assert.deepEqual((await read("select cast(session_context(N'tenant') as int) as tenant")).rows, [{ tenant: 7 }])
  await new Promise((resolve, reject) => connection.reset(error => error ? reject(error) : resolve()))
  assert.deepEqual((await read("select cast(session_context(N'tenant') as int) as tenant")).rows, [{ tenant: null }])
  assert.equal((await read("exec sys.sp_set_session_context @key = N'tenant', @value = 9")).error, undefined)
  assert.deepEqual((await read("select cast(session_context(N'tenant') as int) as tenant")).rows, [{ tenant: 9 }])
})
