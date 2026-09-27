#!/usr/bin/env node
// First-party SQL Server evidence for savepoints and nested transactions:
// SAVE TRANSACTION, ROLLBACK TRANSACTION <savepoint>, nested BEGIN/COMMIT,
// duplicate/unknown/long/case-variant names, errors, XACT_ABORT, TRY/CATCH,
// procedures, sp_executesql/procedure/prepared RPCs and TDS transaction-manager
// requests, with their effects on table data, identity and sequence values.
//
// Conventions follow docs/reference-captures.md (PR #312). The fixed prepared
// helper and UTC helper from that PR are not on main, so their logic is
// replicated here instead of editing scripts/lib.
process.env.TZ = 'UTC'
if (new Date(0).getTimezoneOffset() !== 0) throw new Error('could not switch the process time zone to UTC')

import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { RequestTokenHandler } from 'tedious/lib/token/handler.js'
import { Transaction } from 'tedious/lib/transaction.js'
import { TYPE as PACKET } from 'tedious/lib/packet.js'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, describeFirstDifference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { canonical } from './lib/compatibility.mjs'
import { isDeepStrictEqual } from 'node:util'

const fixture = new URL('../reference/savepoint.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/savepoint-reference-v1/savepoint.json')
const CONTAINERS = 2
const DATABASES = 2

// Bounds per request; overflow is counted, never retained.
const LIMITS = Object.freeze({ rows: 1000, messages: 200 })

// Transaction ENVCHANGE tokens (types 8/9/10) are not exposed as request
// events by tedious. Record their kind and descriptor byte lengths while a
// captured request is outstanding. Descriptor values are server allocated and
// are not retained.
let envSink
for (const [method, type] of [['onBeginTransaction', 'begin'], ['onCommitTransaction', 'commit'], ['onRollbackTransaction', 'rollback']]) {
  const original = RequestTokenHandler.prototype[method]
  RequestTokenHandler.prototype[method] = function (token) {
    envSink?.(type, token)
    return original.call(this, token)
  }
}

const messageFields = m => ({ number: m.number, state: m.state, class: m.class, lineNumber: m.lineNumber, message: m.message })
const columnFields = c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })

// Captures one request. The return status is recorded only when a RETURNSTATUS
// token arrives during this request: tedious carries the last status on the
// connection until a DONEPROC token, so the carried value is cleared first.
function captureRequest(connection, start) {
  return new Promise(resolvePromise => {
    const result = { sets: [], done: [], errors: [], info: [], envChanges: [], returnStatus: null }
    const overflow = kind => { result.truncated ??= { rows: 0, messages: 0 }; result.truncated[kind]++ }
    const message = list => token => {
      if (result.errors.length + result.info.length >= LIMITS.messages) overflow('messages')
      else result[list].push(messageFields(token))
    }
    const onError = message('errors')
    const onInfo = message('info')
    let finished = false
    const finish = (error, rowCount) => {
      if (finished) return
      finished = true
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      envSink = undefined
      result.rowCount = rowCount ?? null
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      resolvePromise(result)
    }
    const request = new Request(start.sql, (error, rowCount) => finish(error, rowCount))
    for (const [name, type, value, options] of start.parameters ?? []) request.addParameter(name, type, value, options)
    request.on('columnMetadata', columns => result.sets.push({ columns: columns.map(columnFields), rows: [] }))
    request.on('row', columns => {
      const set = result.sets.at(-1)
      if (!set || set.rows.length >= LIMITS.rows) overflow('rows')
      else set.rows.push(columns.map(c => c.value))
    })
    for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => {
      if (result.done.length >= LIMITS.messages) overflow('messages')
      else result.done.push({ kind, rowCount: rowCount ?? null, more })
    })
    request.on('doneProc', (_count, _more, status) => { if (status !== undefined) result.returnStatus = status })
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    connection.procReturnStatusValue = undefined
    envSink = (type, token) => {
      if (result.envChanges.length >= LIMITS.messages) overflow('messages')
      else result.envChanges.push({ type, newLength: token.newValue?.length ?? null, oldLength: token.oldValue?.length ?? null })
    }
    try { start.run(request) }
    catch (error) { finish(error, undefined) }
  })
}

const batch = (connection, sql) => captureRequest(connection, { sql, run: request => connection.execSqlBatch(request) })
const executeSql = (connection, sql, parameters) => captureRequest(connection, { sql, parameters, run: request => connection.execSql(request) })
const procedure = (connection, name, parameters) => captureRequest(connection, { sql: name, parameters, run: request => connection.callProcedure(request) })
// TDS transaction-manager request (packet type 14) on a Request we own, so its
// tokens are captured like any other request.
const manager = (connection, operation, name) => captureRequest(connection, {
  sql: undefined,
  run: request => {
    const transaction = new Transaction(name)
    const payload = transaction[`${operation}Payload`](connection.currentTransactionDescriptor())
    connection.makeRequest(request, PACKET.TRANSACTION_MANAGER, payload)
  }
})

// sp_prepare/sp_execute/sp_unprepare on one reusable Request. Replicates the
// fixed capturePrepared helper from docs/reference-captures.md (PR #312):
// preparation completes through the 'prepared'/'error' events, request.error
// and the carried return status are cleared before each phase, and only
// errors raised during a phase are attributed to it.
async function capturePrepared(connection, sql, declarations, valueSets) {
  let result
  let complete = () => {}
  const reported = new Set()
  const fresh = () => ({ sets: [], done: [], errors: [], info: [], envChanges: [], returnStatus: null })
  const overflow = kind => { result.truncated ??= { rows: 0, messages: 0 }; result.truncated[kind]++ }
  const message = list => token => {
    if (!result) return
    if (result.errors.length + result.info.length >= LIMITS.messages) overflow('messages')
    else result[list].push(messageFields(token))
  }
  const onError = message('errors')
  const onInfo = message('info')
  const request = new Request(sql, (error, rowCount) => complete(error, rowCount))
  for (const [name, type, options] of declarations) request.addParameter(name, type, undefined, options)
  request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(columnFields), rows: [] }))
  request.on('row', columns => {
    if (!result) return
    const set = result.sets.at(-1)
    if (!set || set.rows.length >= LIMITS.rows) overflow('rows')
    else set.rows.push(columns.map(c => c.value))
  })
  for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => {
    if (!result) return
    if (result.done.length >= LIMITS.messages) overflow('messages')
    else result.done.push({ kind, rowCount: rowCount ?? null, more })
  })
  request.on('doneProc', (_count, _more, status) => { if (result && status !== undefined) result.returnStatus = status })
  const phase = start => new Promise(resolvePromise => {
    result = fresh()
    complete = (error, rowCount) => {
      complete = () => {}
      const finished = result
      result = undefined
      envSink = undefined
      finished.rowCount = rowCount ?? null
      if (error && !reported.has(error) && !finished.errors.length) finished.errors.push({ message: error.message, number: error.number ?? null })
      if (error) reported.add(error)
      resolvePromise(finished)
    }
    request.error = undefined
    connection.procReturnStatusValue = undefined
    const sink = result
    envSink = (type, token) => sink.envChanges.push({ type, newLength: token.newValue?.length ?? null, oldLength: token.oldValue?.length ?? null })
    start()
  })
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  const onPrepared = () => complete(undefined, undefined)
  const onPrepareError = error => complete(error, undefined)
  try {
    const prepare = await phase(() => {
      request.once('prepared', onPrepared)
      request.once('error', onPrepareError)
      connection.prepare(request)
    })
    request.off('prepared', onPrepared)
    request.off('error', onPrepareError)
    if (request.handle === undefined) return { prepare, prepared: false, executions: [], unprepare: null }
    const executions = []
    for (const values of valueSets) executions.push({ values, result: await phase(() => connection.execute(request, values)) })
    const unprepare = await phase(() => connection.unprepare(request))
    return { prepare, prepared: true, executions, unprepare }
  } finally {
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

const table = key => `dbo.svp_${key}`
const createTable = key => `CREATE TABLE ${table(key)}(id INT IDENTITY(1,1) CONSTRAINT pk_svp_${key} PRIMARY KEY, v INT NOT NULL CONSTRAINT ck_svp_${key}_v CHECK (v > 0))`
const dataProbe = key => `SELECT id, v FROM ${table(key)} ORDER BY id; SELECT IDENT_CURRENT(N'${table(key)}') AS ident_current, @@TRANCOUNT AS tran_count, XACT_STATE() AS xact_state`
const long32 = 'n'.repeat(32)
const long33 = 'n'.repeat(33)
const long40 = 'n'.repeat(40)

// Each program runs on its own table. Steps are
//   [name, sql]                           SQL batch
//   ['rpc', name, sql, parameters]        sp_executesql
//   ['proc', name, procedure, parameters] stored-procedure RPC
//   ['tm', name, operation, txName]       TDS transaction-manager request
//   ['prepared', name, sql, declarations, valueSets]
// A '#data' step expands to the program's row/identity/state probe. Every step
// except a probe is followed by a @@TRANCOUNT/XACT_STATE probe. Each program
// ends with a cleanup that rolls back any open transaction.
const programs = [
  { key: 'basic', steps: [
    ['begin', 'BEGIN TRANSACTION'],
    ['insert 10', 'INSERT dbo.svp_basic(v) VALUES (10)'],
    ['save s1', 'SAVE TRANSACTION s1'],
    ['insert 20', 'INSERT dbo.svp_basic(v) VALUES (20)'],
    '#data',
    ['rollback s1', 'ROLLBACK TRANSACTION s1'],
    '#data',
    ['insert 30', 'INSERT dbo.svp_basic(v) VALUES (30)'],
    ['rollback s1 again', 'ROLLBACK TRAN s1'],
    '#data',
    ['insert 40', 'INSERT dbo.svp_basic(v) VALUES (40)'],
    ['save without transaction keyword', 'SAVE TRAN s2'],
    ['rollback without transaction keyword', 'ROLLBACK TRAN s2'],
    ['commit', 'COMMIT TRANSACTION'],
    '#data'
  ] },
  { key: 'none', steps: [
    ['save outside transaction', 'SAVE TRANSACTION s_none'],
    ['rollback savepoint outside transaction', 'ROLLBACK TRANSACTION s_none'],
    ['commit outside transaction', 'COMMIT TRANSACTION'],
    ['rollback outside transaction', 'ROLLBACK TRANSACTION'],
    ['batch continues after save error', 'INSERT dbo.svp_none(v) VALUES (1); SAVE TRANSACTION s_none; INSERT dbo.svp_none(v) VALUES (2); SELECT @@TRANCOUNT AS tran_count'],
    '#data',
    ['begin', 'BEGIN TRANSACTION'],
    ['save without name', 'SAVE TRANSACTION'],
    ['rollback', 'ROLLBACK TRANSACTION']
  ] },
  { key: 'names', steps: [
    ['begin', 'BEGIN TRANSACTION'],
    ['save CaseName', 'SAVE TRANSACTION CaseName'],
    ['insert 1', 'INSERT dbo.svp_names(v) VALUES (1)'],
    ['rollback casename', 'ROLLBACK TRANSACTION casename'],
    ['rollback CASENAME', 'ROLLBACK TRANSACTION CASENAME'],
    '#data',
    ['rollback CaseName', 'ROLLBACK TRANSACTION CaseName'],
    '#data',
    ['rollback missing', 'ROLLBACK TRANSACTION missing_name'],
    ['save MixedCase', 'SAVE TRANSACTION MixedCase'],
    ['insert 4', 'INSERT dbo.svp_names(v) VALUES (4)'],
    ['save mixedcase', 'SAVE TRANSACTION mixedcase'],
    ['insert 5', 'INSERT dbo.svp_names(v) VALUES (5)'],
    ['rollback MIXEDCASE', 'ROLLBACK TRANSACTION MIXEDCASE'],
    '#data',
    ['rollback MixedCase', 'ROLLBACK TRANSACTION MixedCase'],
    '#data',
    ['save accented', 'SAVE TRANSACTION [r\u00e9sum\u00e9]'],
    ['insert 6', 'INSERT dbo.svp_names(v) VALUES (6)'],
    ['rollback unaccented', 'ROLLBACK TRANSACTION resume'],
    '#data',
    ['rollback accented', 'ROLLBACK TRANSACTION [r\u00e9sum\u00e9]'],
    '#data',
    ['save bracket quoted', 'SAVE TRANSACTION [quoted name]'],
    ['insert 2', 'INSERT dbo.svp_names(v) VALUES (2)'],
    ['rollback bracket quoted', 'ROLLBACK TRANSACTION [quoted name]'],
    '#data',
    ['save double quoted', 'SAVE TRANSACTION "dq name"'],
    ['save unicode', 'SAVE TRANSACTION [żółw]'],
    ['insert 3', 'INSERT dbo.svp_names(v) VALUES (3)'],
    ['rollback unicode', 'ROLLBACK TRANSACTION [żółw]'],
    ['rollback double quoted', 'ROLLBACK TRANSACTION "dq name"'],
    ['commit', 'COMMIT TRANSACTION'],
    '#data'
  ] },
  { key: 'length', steps: [
    ['begin', 'BEGIN TRANSACTION'],
    ['save 32 characters', `SAVE TRANSACTION ${long32}`],
    ['insert 1', 'INSERT dbo.svp_length(v) VALUES (1)'],
    ['save 33 characters', `SAVE TRANSACTION ${long33}`],
    ['rollback 33 characters', `ROLLBACK TRANSACTION ${long33}`],
    '#data',
    ['rollback 32 characters', `ROLLBACK TRANSACTION ${long32}`],
    '#data',
    ['save nvarchar variable of 40', `DECLARE @n NVARCHAR(64) = N'${long40}'; SAVE TRANSACTION @n; INSERT dbo.svp_length(v) VALUES (2); SELECT @@TRANCOUNT AS tran_count`],
    ['rollback variable of 32', `DECLARE @m NVARCHAR(64) = N'${long32}'; ROLLBACK TRANSACTION @m`],
    '#data',
    ['save variable of 40 then rollback 40', `DECLARE @n NVARCHAR(64) = N'${long40}'; SAVE TRANSACTION @n; INSERT dbo.svp_length(v) VALUES (3); ROLLBACK TRANSACTION @n; SELECT @@TRANCOUNT AS tran_count`],
    '#data',
    ['save varchar variable of 33', `DECLARE @vc VARCHAR(40) = '${long33}'; SAVE TRANSACTION @vc; INSERT dbo.svp_length(v) VALUES (4); ROLLBACK TRANSACTION @vc; SELECT @@TRANCOUNT AS tran_count`],
    '#data',
    ['save empty variable', "DECLARE @e NVARCHAR(10) = N''; SAVE TRANSACTION @e; SELECT @@TRANCOUNT AS tran_count"],
    ['insert 6', 'INSERT dbo.svp_length(v) VALUES (6)'],
    ['rollback empty variable', "DECLARE @e NVARCHAR(10) = N''; ROLLBACK TRANSACTION @e; SELECT @@TRANCOUNT AS tran_count"],
    '#data',
    ['begin after empty', 'BEGIN TRANSACTION'],
    ['save null variable', 'DECLARE @z NVARCHAR(10) = NULL; SAVE TRANSACTION @z; SELECT @@TRANCOUNT AS tran_count'],
    ['insert 7', 'INSERT dbo.svp_length(v) VALUES (7)'],
    ['rollback null variable', 'DECLARE @z NVARCHAR(10) = NULL; ROLLBACK TRANSACTION @z; SELECT @@TRANCOUNT AS tran_count'],
    '#data',
    ['begin after null', 'BEGIN TRANSACTION'],
    ['save blank-padded literal', 'SAVE TRANSACTION [pad2   ]'],
    ['insert 8', 'INSERT dbo.svp_length(v) VALUES (8)'],
    ['rollback unpadded literal', 'ROLLBACK TRANSACTION pad2'],
    '#data',
    ['rollback', 'ROLLBACK TRANSACTION'],
    ['begin', 'BEGIN TRANSACTION'],
    ['save int variable', 'DECLARE @i INT = 5; SAVE TRANSACTION @i'],
    ['save char variable', "DECLARE @c CHAR(5) = 'pad'; SAVE TRANSACTION @c; INSERT dbo.svp_length(v) VALUES (5); ROLLBACK TRANSACTION pad; SELECT @@TRANCOUNT AS tran_count"],
    '#data',
    ['begin with 33 character name', `BEGIN TRANSACTION ${long33}`],
    ['begin with 32 character name', `BEGIN TRANSACTION ${long32}`],
    ['rollback', 'ROLLBACK TRANSACTION']
  ] },
  { key: 'duplicate', steps: [
    ['begin', 'BEGIN TRANSACTION'],
    ['insert 1', 'INSERT dbo.svp_duplicate(v) VALUES (1)'],
    ['save d first', 'SAVE TRANSACTION d'],
    ['insert 2', 'INSERT dbo.svp_duplicate(v) VALUES (2)'],
    ['save d second', 'SAVE TRANSACTION d'],
    ['insert 3', 'INSERT dbo.svp_duplicate(v) VALUES (3)'],
    ['rollback d', 'ROLLBACK TRANSACTION d'],
    '#data',
    ['insert 4', 'INSERT dbo.svp_duplicate(v) VALUES (4)'],
    ['rollback d repeated', 'ROLLBACK TRANSACTION d'],
    '#data',
    ['rollback d third time', 'ROLLBACK TRANSACTION d'],
    '#data',
    ['commit', 'COMMIT TRANSACTION'],
    '#data'
  ] },
  { key: 'order', steps: [
    ['begin', 'BEGIN TRANSACTION'],
    ['save a', 'SAVE TRANSACTION a'],
    ['insert 1', 'INSERT dbo.svp_order(v) VALUES (1)'],
    ['save b', 'SAVE TRANSACTION b'],
    ['insert 2', 'INSERT dbo.svp_order(v) VALUES (2)'],
    ['rollback a', 'ROLLBACK TRANSACTION a'],
    '#data',
    ['rollback b after earlier rollback', 'ROLLBACK TRANSACTION b'],
    '#data',
    ['insert 3', 'INSERT dbo.svp_order(v) VALUES (3)'],
    ['commit', 'COMMIT TRANSACTION'],
    '#data'
  ] },
  { key: 'nested', steps: [
    ['begin 1', 'BEGIN TRANSACTION'],
    ['insert 1', 'INSERT dbo.svp_nested(v) VALUES (1)'],
    ['begin 2', 'BEGIN TRANSACTION'],
    ['insert 2', 'INSERT dbo.svp_nested(v) VALUES (2)'],
    ['commit inner', 'COMMIT TRANSACTION'],
    ['commit outer', 'COMMIT TRANSACTION'],
    '#data',
    ['begin outer_tx', 'BEGIN TRANSACTION outer_tx'],
    ['begin inner_tx', 'BEGIN TRANSACTION inner_tx'],
    ['insert 3', 'INSERT dbo.svp_nested(v) VALUES (3)'],
    ['rollback inner_tx', 'ROLLBACK TRANSACTION inner_tx'],
    ['commit named other', 'COMMIT TRANSACTION unrelated_name'],
    ['rollback outer_tx', 'ROLLBACK TRANSACTION outer_tx'],
    '#data',
    ['begin 1 again', 'BEGIN TRANSACTION'],
    ['insert 4', 'INSERT dbo.svp_nested(v) VALUES (4)'],
    ['begin 2 again', 'BEGIN TRANSACTION'],
    ['insert 5', 'INSERT dbo.svp_nested(v) VALUES (5)'],
    ['rollback all', 'ROLLBACK TRANSACTION'],
    '#data',
    ['commit after rollback', 'COMMIT TRANSACTION'],
    ['begin named tx_a', 'BEGIN TRANSACTION tx_a'],
    ['begin unnamed nested', 'BEGIN TRANSACTION'],
    ['insert 6', 'INSERT dbo.svp_nested(v) VALUES (6)'],
    ['rollback tx_a from nested', 'ROLLBACK TRANSACTION tx_a'],
    '#data',
    ['begin named CaseTx', 'BEGIN TRANSACTION CaseTx'],
    ['insert 7', 'INSERT dbo.svp_nested(v) VALUES (7)'],
    ['rollback casetx', 'ROLLBACK TRANSACTION casetx'],
    ['rollback CaseTx', 'ROLLBACK TRANSACTION CaseTx'],
    '#data',
    ['batch nested counts', 'BEGIN TRANSACTION; SELECT @@TRANCOUNT AS c1; BEGIN TRANSACTION; SELECT @@TRANCOUNT AS c2; SAVE TRANSACTION nested_sp; SELECT @@TRANCOUNT AS c3; COMMIT TRANSACTION; SELECT @@TRANCOUNT AS c4; COMMIT TRANSACTION; SELECT @@TRANCOUNT AS c5']
  ] },
  { key: 'innersave', steps: [
    ['begin 1', 'BEGIN TRANSACTION'],
    ['begin 2', 'BEGIN TRANSACTION'],
    ['save s', 'SAVE TRANSACTION s'],
    ['insert 1', 'INSERT dbo.svp_innersave(v) VALUES (1)'],
    ['rollback s at level 2', 'ROLLBACK TRANSACTION s'],
    ['insert 2', 'INSERT dbo.svp_innersave(v) VALUES (2)'],
    ['save t', 'SAVE TRANSACTION t'],
    ['insert 3', 'INSERT dbo.svp_innersave(v) VALUES (3)'],
    ['commit inner', 'COMMIT TRANSACTION'],
    ['rollback t after inner commit', 'ROLLBACK TRANSACTION t'],
    '#data',
    ['commit outer', 'COMMIT TRANSACTION'],
    '#data',
    ['begin same_name', 'BEGIN TRANSACTION same_name'],
    ['insert 4', 'INSERT dbo.svp_innersave(v) VALUES (4)'],
    ['save same_name', 'SAVE TRANSACTION same_name'],
    ['insert 5', 'INSERT dbo.svp_innersave(v) VALUES (5)'],
    ['rollback same_name', 'ROLLBACK TRANSACTION same_name'],
    '#data',
    ['rollback same_name again', 'ROLLBACK TRANSACTION same_name'],
    '#data',
    ['commit', 'COMMIT TRANSACTION'],
    '#data'
  ] },
  { key: 'errors', steps: [
    ['begin', 'BEGIN TRANSACTION'],
    ['insert 1', 'INSERT dbo.svp_errors(v) VALUES (1)'],
    ['save s', 'SAVE TRANSACTION s'],
    ['insert 2', 'INSERT dbo.svp_errors(v) VALUES (2)'],
    ['check violation', 'INSERT dbo.svp_errors(v) VALUES (-1)'],
    ['multirow check violation', 'INSERT dbo.svp_errors(v) VALUES (5),(-5)'],
    '#data',
    ['save after error', 'SAVE TRANSACTION s_after'],
    ['insert 3', 'INSERT dbo.svp_errors(v) VALUES (3)'],
    ['rollback s', 'ROLLBACK TRANSACTION s'],
    '#data',
    ['batch with error between save and rollback', 'SAVE TRANSACTION s2; INSERT dbo.svp_errors(v) VALUES (4); INSERT dbo.svp_errors(v) VALUES (-2); ROLLBACK TRANSACTION s2; SELECT @@TRANCOUNT AS tran_count, XACT_STATE() AS xact_state'],
    '#data',
    ['save s3', 'SAVE TRANSACTION s3'],
    ['insert 5', 'INSERT dbo.svp_errors(v) VALUES (5)'],
    ['batch-terminating conversion error', "INSERT dbo.svp_errors(v) VALUES (6); SELECT CONVERT(INT, N'x') AS bad; SELECT 1 AS not_reached"],
    '#data',
    ['rollback s3 after batch abort', 'ROLLBACK TRANSACTION s3'],
    '#data',
    ['commit', 'COMMIT TRANSACTION'],
    '#data'
  ] },
  { key: 'abort', steps: [
    ['xact_abort on', 'SET XACT_ABORT ON'],
    ['begin', 'BEGIN TRANSACTION'],
    ['insert 1', 'INSERT dbo.svp_abort(v) VALUES (1)'],
    ['save s', 'SAVE TRANSACTION s'],
    ['check violation', 'INSERT dbo.svp_abort(v) VALUES (-1)'],
    '#data',
    ['rollback s after abort', 'ROLLBACK TRANSACTION s'],
    ['doomed catch rollback savepoint', 'BEGIN TRANSACTION; INSERT dbo.svp_abort(v) VALUES (2); SAVE TRANSACTION s; BEGIN TRY INSERT dbo.svp_abort(v) VALUES (-2); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error_number, XACT_STATE() AS xact_state, @@TRANCOUNT AS tran_count; ROLLBACK TRANSACTION s; SELECT XACT_STATE() AS after_rollback_state, @@TRANCOUNT AS after_rollback_count; END CATCH; SELECT 1 AS after_catch'],
    '#data',
    ['doomed catch save', 'BEGIN TRANSACTION; INSERT dbo.svp_abort(v) VALUES (3); BEGIN TRY INSERT dbo.svp_abort(v) VALUES (-3); END TRY BEGIN CATCH SELECT XACT_STATE() AS xact_state; SAVE TRANSACTION s_doomed; SELECT XACT_STATE() AS after_save_state, @@TRANCOUNT AS after_save_count; END CATCH; SELECT 1 AS after_catch'],
    '#data',
    ['doomed catch commit', 'BEGIN TRANSACTION; INSERT dbo.svp_abort(v) VALUES (4); BEGIN TRY INSERT dbo.svp_abort(v) VALUES (-4); END TRY BEGIN CATCH COMMIT TRANSACTION; SELECT XACT_STATE() AS after_commit_state; END CATCH'],
    '#data',
    ['doomed catch full rollback then save', 'BEGIN TRANSACTION; INSERT dbo.svp_abort(v) VALUES (5); BEGIN TRY INSERT dbo.svp_abort(v) VALUES (-5); END TRY BEGIN CATCH ROLLBACK TRANSACTION; SELECT XACT_STATE() AS after_rollback_state; BEGIN TRANSACTION; SAVE TRANSACTION s_fresh; INSERT dbo.svp_abort(v) VALUES (6); ROLLBACK TRANSACTION s_fresh; COMMIT TRANSACTION; END CATCH'],
    '#data',
    ['xact_abort off', 'SET XACT_ABORT OFF']
  ] },
  { key: 'catch', steps: [
    ['catch rollback savepoint', 'BEGIN TRANSACTION; INSERT dbo.svp_catch(v) VALUES (1); SAVE TRANSACTION s; BEGIN TRY INSERT dbo.svp_catch(v) VALUES (2); INSERT dbo.svp_catch(v) VALUES (-1); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error_number, XACT_STATE() AS xact_state, @@TRANCOUNT AS tran_count; ROLLBACK TRANSACTION s; SELECT XACT_STATE() AS after_rollback_state, @@TRANCOUNT AS after_rollback_count; END CATCH'],
    '#data',
    ['catch raiserror', "SAVE TRANSACTION r; INSERT dbo.svp_catch(v) VALUES (4); BEGIN TRY RAISERROR(N'savepoint probe', 16, 1); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error_number, XACT_STATE() AS xact_state; ROLLBACK TRANSACTION r; END CATCH"],
    '#data',
    ['catch unknown savepoint', 'BEGIN TRY ROLLBACK TRANSACTION no_such_sp; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error_number, ERROR_SEVERITY() AS severity, ERROR_STATE() AS state, XACT_STATE() AS xact_state, @@TRANCOUNT AS tran_count; END CATCH'],
    ['catch divide by zero', 'SAVE TRANSACTION z; INSERT dbo.svp_catch(v) VALUES (5); BEGIN TRY SELECT 1 / 0 AS boom; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error_number, XACT_STATE() AS xact_state; ROLLBACK TRANSACTION z; END CATCH'],
    '#data',
    ['catch conversion error', "SAVE TRANSACTION c; INSERT dbo.svp_catch(v) VALUES (3); BEGIN TRY INSERT dbo.svp_catch(v) SELECT CONVERT(INT, N'x'); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error_number, XACT_STATE() AS xact_state; IF XACT_STATE() = 1 ROLLBACK TRANSACTION c; SELECT XACT_STATE() AS after_state, @@TRANCOUNT AS after_count; END CATCH"],
    '#data',
    ['commit after conversion', 'COMMIT TRANSACTION'],
    ['catch save outside transaction', 'BEGIN TRY SAVE TRANSACTION no_tx_sp; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error_number, XACT_STATE() AS xact_state, @@TRANCOUNT AS tran_count; END CATCH; SELECT 1 AS after_catch'],
    ['nested try inner catch rollback', 'BEGIN TRANSACTION; SAVE TRANSACTION outer_sp; INSERT dbo.svp_catch(v) VALUES (7); BEGIN TRY SAVE TRANSACTION inner_sp; INSERT dbo.svp_catch(v) VALUES (8); BEGIN TRY INSERT dbo.svp_catch(v) VALUES (-8); END TRY BEGIN CATCH ROLLBACK TRANSACTION inner_sp; SELECT @@TRANCOUNT AS inner_count, XACT_STATE() AS inner_state; THROW; END CATCH END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS rethrown, XACT_STATE() AS outer_state; ROLLBACK TRANSACTION inner_sp; END CATCH'],
    '#data',
    ['commit', 'COMMIT TRANSACTION'],
    '#data'
  ] },
  { key: 'proc', setup: [
    'CREATE PROCEDURE dbo.svp_proc_save @v INT AS BEGIN SAVE TRANSACTION proc_sp; INSERT dbo.svp_proc(v) VALUES (@v); IF @v > 100 ROLLBACK TRANSACTION proc_sp; SELECT @@TRANCOUNT AS proc_tran_count, XACT_STATE() AS proc_xact_state; RETURN 7; END',
    'CREATE PROCEDURE dbo.svp_proc_caller_savepoint AS BEGIN ROLLBACK TRANSACTION caller_sp; SELECT @@TRANCOUNT AS proc_tran_count; RETURN 5; END',
    'CREATE PROCEDURE dbo.svp_proc_begin_rollback AS BEGIN BEGIN TRANSACTION; ROLLBACK TRANSACTION; RETURN 3; END',
    'CREATE PROCEDURE dbo.svp_proc_begin_commit AS BEGIN BEGIN TRANSACTION; INSERT dbo.svp_proc(v) VALUES (50); SAVE TRANSACTION inner_proc_sp; INSERT dbo.svp_proc(v) VALUES (51); ROLLBACK TRANSACTION inner_proc_sp; COMMIT TRANSACTION; RETURN 9; END',
    'CREATE PROCEDURE dbo.svp_proc_error @v INT AS BEGIN SAVE TRANSACTION err_sp; INSERT dbo.svp_proc(v) VALUES (@v); SELECT XACT_STATE() AS proc_xact_state; RETURN 11; END'
  ], steps: [
    ['begin', 'BEGIN TRANSACTION'],
    ['insert 1', 'INSERT dbo.svp_proc(v) VALUES (1)'],
    ['save caller_sp', 'SAVE TRANSACTION caller_sp'],
    ['exec proc rolls back own savepoint', 'DECLARE @r INT; EXEC @r = dbo.svp_proc_save 200; SELECT @r AS return_value, @@TRANCOUNT AS tran_count'],
    '#data',
    ['exec proc keeps savepoint', 'DECLARE @r INT; EXEC @r = dbo.svp_proc_save 20; SELECT @r AS return_value'],
    '#data',
    ['caller rolls back proc savepoint', 'ROLLBACK TRANSACTION proc_sp'],
    '#data',
    ['exec proc rolls back caller savepoint', 'DECLARE @r INT; EXEC @r = dbo.svp_proc_caller_savepoint; SELECT @r AS return_value'],
    '#data',
    ['proc', 'rpc proc keeps savepoint', 'dbo.svp_proc_save', [['v', TYPES.Int, 30]]],
    ['proc', 'rpc proc rolls back own savepoint', 'dbo.svp_proc_save', [['v', TYPES.Int, 300]]],
    '#data',
    ['exec nested begin commit', 'DECLARE @r INT; EXEC @r = dbo.svp_proc_begin_commit; SELECT @r AS return_value'],
    '#data',
    ['exec proc with error after savepoint', 'DECLARE @r INT; EXEC @r = dbo.svp_proc_error -1; SELECT @r AS return_value, XACT_STATE() AS xact_state'],
    '#data',
    ['proc', 'rpc proc with error after savepoint', 'dbo.svp_proc_error', [['v', TYPES.Int, -2]]],
    ['rollback err_sp from caller', 'ROLLBACK TRANSACTION err_sp'],
    '#data',
    ['exec proc full rollback', 'DECLARE @r INT; EXEC @r = dbo.svp_proc_begin_rollback; SELECT @r AS return_value, @@TRANCOUNT AS tran_count'],
    '#data',
    ['begin again', 'BEGIN TRANSACTION'],
    ['insert 2', 'INSERT dbo.svp_proc(v) VALUES (2)'],
    ['proc', 'rpc proc full rollback', 'dbo.svp_proc_begin_rollback', []],
    '#data',
    ['proc', 'rpc proc outside transaction', 'dbo.svp_proc_save', [['v', TYPES.Int, 40]]],
    '#data'
  ] },
  { key: 'rpc', steps: [
    ['begin', 'BEGIN TRANSACTION'],
    ['rpc', 'rpc save', 'SAVE TRANSACTION @name', [['name', TYPES.NVarChar, 'rpc_sp']]],
    ['rpc', 'rpc insert 1', 'INSERT dbo.svp_rpc(v) VALUES (@v)', [['v', TYPES.Int, 1]]],
    ['rpc', 'rpc rollback different case', 'ROLLBACK TRANSACTION @name', [['name', TYPES.NVarChar, 'RPC_SP']]],
    '#data',
    ['rpc', 'rpc rollback', 'ROLLBACK TRANSACTION @name', [['name', TYPES.NVarChar, 'rpc_sp']]],
    '#data',
    ['rpc', 'rpc dynamic scope save', 'SAVE TRANSACTION dyn_sp; INSERT dbo.svp_rpc(v) VALUES (@v); SELECT @@TRANCOUNT AS tran_count', [['v', TYPES.Int, 2]]],
    ['batch rollback of rpc savepoint', 'ROLLBACK TRANSACTION dyn_sp'],
    '#data',
    ['rpc', 'rpc save 40 character name', 'SAVE TRANSACTION @name; INSERT dbo.svp_rpc(v) VALUES (3)', [['name', TYPES.NVarChar, long40]]],
    ['rpc', 'rpc rollback 32 character prefix', 'ROLLBACK TRANSACTION @name', [['name', TYPES.NVarChar, long32]]],
    '#data',
    ['rpc', 'rpc save null name', 'SAVE TRANSACTION @name', [['name', TYPES.NVarChar, null]]],
    ['rpc', 'rpc begin nested', 'BEGIN TRANSACTION; INSERT dbo.svp_rpc(v) VALUES (@v)', [['v', TYPES.Int, 4]]],
    ['rpc', 'rpc commit nested', 'COMMIT TRANSACTION', []],
    ['rpc', 'rpc begin without commit', 'BEGIN TRANSACTION; SELECT @@TRANCOUNT AS inner_count', []],
    ['rpc', 'rpc rollback all', 'ROLLBACK TRANSACTION', []],
    '#data',
    ['begin prepared', 'BEGIN TRANSACTION'],
    ['prepared', 'prepared savepoints',
      'SAVE TRANSACTION @sp; INSERT dbo.svp_rpc(v) VALUES (@v); IF @v > 100 ROLLBACK TRANSACTION @sp; SELECT @@TRANCOUNT AS tran_count, XACT_STATE() AS xact_state /*prepared savepoint*/',
      [['sp', TYPES.NVarChar, { length: 64 }], ['v', TYPES.Int]],
      [{ sp: 'prep_a', v: 10 }, { sp: 'prep_b', v: 200 }, { sp: 'prep_c', v: -1 }, { sp: 'prep_d', v: 11 }]],
    '#data',
    ['rollback prep_b from batch', 'ROLLBACK TRANSACTION prep_b'],
    '#data',
    ['rollback prep_a from batch', 'ROLLBACK TRANSACTION prep_a'],
    '#data',
    ['commit', 'COMMIT TRANSACTION'],
    '#data'
  ] },
  { key: 'tm', steps: [
    ['tm', 'tm save outside transaction', 'save', 'tm_none'],
    ['tm', 'tm begin', 'begin', 'tm_tx'],
    ['insert 1', 'INSERT dbo.svp_tm(v) VALUES (1)'],
    ['tm', 'tm save', 'save', 'tm_sp'],
    ['insert 2', 'INSERT dbo.svp_tm(v) VALUES (2)'],
    ['tm', 'tm rollback savepoint', 'rollback', 'tm_sp'],
    '#data',
    ['tm', 'tm rollback savepoint again', 'rollback', 'tm_sp'],
    ['tm', 'tm rollback unknown', 'rollback', 'tm_missing'],
    ['tm', 'tm save MixedTm', 'save', 'MixedTm'],
    ['insert 7', 'INSERT dbo.svp_tm(v) VALUES (7)'],
    ['tm', 'tm rollback different case', 'rollback', 'MIXEDTM'],
    '#data',
    ['sql save', 'SAVE TRANSACTION sql_sp'],
    ['insert 3', 'INSERT dbo.svp_tm(v) VALUES (3)'],
    ['tm', 'tm rollback sql savepoint', 'rollback', 'sql_sp'],
    '#data',
    ['tm', 'tm save 33 characters', 'save', long33],
    ['insert 4', 'INSERT dbo.svp_tm(v) VALUES (4)'],
    ['tm', 'tm rollback 32 character prefix', 'rollback', long32],
    '#data',
    ['tm', 'tm begin nested', 'begin', ''],
    ['tm', 'tm save in nested', 'save', 'tm_nested_sp'],
    ['insert 5', 'INSERT dbo.svp_tm(v) VALUES (5)'],
    ['sql rollback tm savepoint', 'ROLLBACK TRANSACTION tm_nested_sp'],
    ['tm', 'tm commit nested', 'commit', ''],
    ['tm', 'tm rollback outer name', 'rollback', 'tm_tx'],
    '#data',
    ['tm', 'tm begin again', 'begin', ''],
    ['insert 6', 'INSERT dbo.svp_tm(v) VALUES (6)'],
    ['tm', 'tm commit', 'commit', ''],
    '#data',
    ['tm', 'tm begin for empty savepoint', 'begin', ''],
    ['insert 8', 'INSERT dbo.svp_tm(v) VALUES (8)'],
    ['tm', 'tm save empty name', 'save', ''],
    '#data',
    ['tm', 'tm begin named for sql rollback', 'begin', 'tm_named'],
    ['insert 9', 'INSERT dbo.svp_tm(v) VALUES (9)'],
    ['sql rollback tm transaction name', 'ROLLBACK TRANSACTION tm_named'],
    '#data',
    ['sql begin for tm savepoint rollback', 'BEGIN TRANSACTION'],
    ['insert 10', 'INSERT dbo.svp_tm(v) VALUES (10)'],
    ['tm', 'tm rollback without name', 'rollback', ''],
    '#data'
  ] },
  { key: 'effects', setup: [
    'CREATE SEQUENCE dbo.svp_effects_seq AS INT START WITH 1 INCREMENT BY 1',
    'CREATE TABLE #svp_effects_temp(v INT NOT NULL)'
  ], steps: [
    ['table variable survives savepoint rollback', 'DECLARE @tv TABLE(v INT NOT NULL); BEGIN TRANSACTION; SAVE TRANSACTION tv_sp; INSERT @tv(v) VALUES (1); INSERT dbo.svp_effects(v) VALUES (1); ROLLBACK TRANSACTION tv_sp; SELECT v FROM @tv; SELECT COUNT(*) AS table_rows FROM dbo.svp_effects; COMMIT TRANSACTION'],
    '#data',
    ['begin', 'BEGIN TRANSACTION'],
    ['save temp_sp', 'SAVE TRANSACTION temp_sp'],
    ['temp insert', 'INSERT #svp_effects_temp(v) VALUES (1); SELECT v FROM #svp_effects_temp'],
    ['sequence and identity', 'SELECT NEXT VALUE FOR dbo.svp_effects_seq AS seq; INSERT dbo.svp_effects(v) VALUES (2); SELECT SCOPE_IDENTITY() AS scope_identity, @@IDENTITY AS session_identity'],
    ['create table after savepoint', 'CREATE TABLE dbo.svp_effects_ddl(id INT NOT NULL CONSTRAINT pk_sp_effects_ddl PRIMARY KEY); SELECT CASE WHEN OBJECT_ID(N\'dbo.svp_effects_ddl\') IS NULL THEN 0 ELSE 1 END AS ddl_exists'],
    ['rollback temp_sp', 'ROLLBACK TRANSACTION temp_sp'],
    ['after rollback', "SELECT v FROM #svp_effects_temp; SELECT CASE WHEN OBJECT_ID(N'dbo.svp_effects_ddl') IS NULL THEN 0 ELSE 1 END AS ddl_exists; SELECT SCOPE_IDENTITY() AS scope_identity, @@IDENTITY AS session_identity; SELECT CAST(current_value AS INT) AS seq_current FROM sys.sequences WHERE name = N'svp_effects_seq'"],
    '#data',
    ['sequence after rollback', 'SELECT NEXT VALUE FOR dbo.svp_effects_seq AS seq'],
    ['insert after rollback', 'INSERT dbo.svp_effects(v) VALUES (3); SELECT SCOPE_IDENTITY() AS scope_identity'],
    ['commit', 'COMMIT TRANSACTION'],
    '#data'
  ] },
  // Runs last: switches this fresh database to a case-sensitive collation.
  // Earlier tables are dropped first: their CHECK constraints depend on the
  // database collation (error 5075).
  { key: 'cs', before: ['DROP TABLE ' + ['basic', 'none', 'names', 'length', 'duplicate', 'order', 'nested', 'innersave', 'errors', 'abort', 'catch', 'proc', 'rpc', 'tm', 'effects'].map(table).join(', '), 'ALTER DATABASE CURRENT COLLATE Latin1_General_100_CS_AS'], steps: [
    ['database collation', "SELECT CAST(DATABASEPROPERTYEX(DB_NAME(), 'Collation') AS NVARCHAR(128)) AS database_collation, CAST(SERVERPROPERTY('Collation') AS NVARCHAR(128)) AS server_collation"],
    ['begin', 'BEGIN TRANSACTION CsTx'],
    ['save CaseName', 'SAVE TRANSACTION CaseName'],
    ['insert 1', 'INSERT dbo.svp_cs(v) VALUES (1)'],
    ['rollback casename', 'ROLLBACK TRANSACTION casename'],
    '#data',
    ['rollback CaseName', 'ROLLBACK TRANSACTION CaseName'],
    '#data',
    ['rollback cstx', 'ROLLBACK TRANSACTION cstx'],
    '#data',
    ['commit', 'COMMIT TRANSACTION'],
    '#data'
  ] }
]

async function observe(connection) {
  const run = []
  const push = (program, name, kind, request, result) => {
    run.push({ program, name, kind, ...request, result: canonical(result) })
    return run.at(-1).result
  }
  const version = await batch(connection, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version, CAST(DATABASEPROPERTYEX(DB_NAME(), 'Collation') AS NVARCHAR(128)) AS database_collation, @@TRANCOUNT AS tran_count, XACT_STATE() AS xact_state")
  push('session', 'server identity', 'batch', { sql: 'server identity' }, version)
  for (const program of programs) {
    const { key } = program
    const tag = index => ` /*savepoint ${key} ${index}*/`
    let index = 0
    for (const sql of program.before ?? []) assert.equal(push(key, 'before', 'batch', { sql }, await batch(connection, sql)).errors.length, 0, `${key}: before failed`)
    const setup = [createTable(key), ...(program.setup ?? [])]
    for (const sql of setup) {
      const result = push(key, 'setup', 'batch', { sql }, await batch(connection, sql))
      assert.equal(result.errors.length, 0, `${key}: setup failed`)
    }
    for (const step of program.steps) {
      index++
      if (step === '#data') {
        const sql = dataProbe(key) + tag(index)
        push(key, 'data', 'batch', { sql }, await batch(connection, sql))
        continue
      }
      let name
      if (step[0] === 'rpc') {
        const [, label, sql, parameters] = step
        name = label
        const text = sql + tag(index)
        push(key, name, 'rpc', { sql: text, parameters: parameters.map(([n, type, value]) => [n, type.name, value]) }, await executeSql(connection, text, parameters))
      } else if (step[0] === 'proc') {
        const [, label, procedureName, parameters] = step
        name = label
        push(key, name, 'procedure', { procedure: procedureName, parameters: parameters.map(([n, type, value]) => [n, type.name, value]) }, await procedure(connection, procedureName, parameters))
      } else if (step[0] === 'tm') {
        const [, label, operation, transactionName] = step
        name = label
        push(key, name, 'transaction-manager', { operation, transactionName }, await manager(connection, operation, transactionName))
      } else if (step[0] === 'prepared') {
        const [, label, sql, declarations, valueSets] = step
        name = label
        const prepared = await capturePrepared(connection, sql, declarations, valueSets)
        run.push({
          program: key, name, kind: 'prepared', sql,
          declarations: declarations.map(([n, type, options]) => [n, type.name, options ?? null]),
          prepare: canonical(prepared.prepare),
          prepared: prepared.prepared,
          executions: prepared.executions.map(({ values, result }) => ({ values: canonical(values), result: canonical(result) })),
          unprepare: canonical(prepared.unprepare)
        })
      } else {
        const [label, sql] = step
        name = label
        const text = sql + tag(index)
        push(key, name, 'batch', { sql: text }, await batch(connection, text))
      }
      const sql = 'SELECT @@TRANCOUNT AS tran_count, XACT_STATE() AS xact_state' + tag(`${index} state`)
      push(key, `${name}: state`, 'batch', { sql }, await batch(connection, sql))
    }
    const cleanupSql = 'IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION; SET XACT_ABORT OFF; SELECT @@TRANCOUNT AS tran_count, XACT_STATE() AS xact_state' + tag('cleanup')
    const cleanup = push(key, 'cleanup', 'batch', { sql: cleanupSql }, await batch(connection, cleanupSql))
    assert.equal(cleanup.errors.length, 0, `${key}: cleanup failed`)
    assert.deepEqual(cleanup.sets.at(-1).rows, [[0, 0]], `${key}: transaction leaked`)
  }
  return run
}

function bindGeneratedDatabaseNames(value) {
  if (typeof value === 'string') return value.replaceAll(/msduck_audit_[0-9a-f]{32}/g, '<fresh-database>')
  if (Array.isArray(value)) return value.map(bindGeneratedDatabaseNames)
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, bindGeneratedDatabaseNames(v)]))
  return value
}

// Structural sanity only; the retained rows are the evidence.
function validate(run) {
  for (const entry of run) {
    const results = entry.kind === 'prepared' ? [entry.prepare, ...entry.executions.map(e => e.result), entry.unprepare].filter(Boolean) : [entry.result]
    for (const result of results) {
      assert(!result.truncated, `${entry.program}/${entry.name}: bounded capture overflowed`)
      assert(result.done.length > 0 || result.errors.length > 0 || entry.kind === 'prepared', `${entry.program}/${entry.name}: no completion`)
    }
  }
  const server = run[0].result.sets[0].rows[0]
  assert.equal(server[2], 0)
  // Targeted checks of the rules docs/savepoint.md states; small values only.
  const get = (program, name) => {
    const entry = run.find(item => item.program === program && item.name === name)
    assert(entry, `missing ${program}/${name}`)
    return entry.result
  }
  const errorNumbers = (program, name) => get(program, name).errors.map(e => e.number)
  assert.deepEqual(errorNumbers('basic', 'rollback s1'), [])
  assert.deepEqual(errorNumbers('basic', 'rollback s1 again'), [6401])
  assert.deepEqual(errorNumbers('none', 'save outside transaction'), [628])
  assert.deepEqual(errorNumbers('names', 'rollback MIXEDCASE'), [])
  assert.deepEqual(errorNumbers('nested', 'rollback casetx'), [6401])
  assert.deepEqual(errorNumbers('cs', 'rollback casename'), [6401])
  assert.deepEqual(errorNumbers('length', 'save 33 characters'), [103])
  assert.deepEqual(errorNumbers('abort', 'doomed catch rollback savepoint'), [3931])
  assert.deepEqual(errorNumbers('tm', 'tm save empty name'), [3977])
  assert.deepEqual(get('effects', 'sequence after rollback').sets[0].rows, [[2]])
  assert.deepEqual(get('cs', 'database collation').sets[0].rows, [['Latin1_General_100_CS_AS', 'SQL_Latin1_General_CP1_CI_AS']])
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
const captures = []
let image
for (let container = 0; container < CONTAINERS; container++) {
  await withReferenceContainer(async (config, info) => {
    image = info.image
    for (let database = 0; database < DATABASES; database++) {
      const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, observe)
      validate(run)
      captures.push(bindGeneratedDatabaseNames(run))
      console.log(`container ${container + 1} database ${database + 1}: ${run.length} observations`)
    }
  })
}
for (let index = 1; index < captures.length; index++) {
  assertSameCapture(captures[index], captures[0], `capture ${index + 1} differs from capture 1 after binding generated database names`)
}
const actual = { image, containers: CONTAINERS, databasesPerContainer: DATABASES, run: captures[0] }
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (retained && !isDeepStrictEqual(actual, retained)) throw new Error(`fresh capture differs from retained fixture (${describeFirstDifference(actual, retained)})`)
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${programs.length} programs and ${captures[0].length} observations identically in ${captures.length} fresh databases across ${CONTAINERS} containers${retained ? ' and matched the retained fixture' : ''}${writeFixture ? '; wrote the fixture' : ''}`)
