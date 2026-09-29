#!/usr/bin/env node
// Capture SESSIONPROPERTY, sp_set_session_context and SESSION_CONTEXT as a
// tedious application issues them (execSql = sp_executesql RPC, and batches)
// against the pinned SQL Server 2025 image, in two fresh databases.
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/session-property-context.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1 && import.meta.url === pathToFileURL(process.argv[1]).href) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/session-property-context/capture.json')

const OPTIONS = ['ANSI_NULLS', 'ANSI_PADDING', 'ANSI_WARNINGS', 'ARITHABORT', 'CONCAT_NULL_YIELDS_NULL', 'QUOTED_IDENTIFIER', 'NUMERIC_ROUNDABORT']
const MAX_ROWS = 100, MAX_EVENTS = 200

// Same record shape as lib/compatibility.mjs capture, for batches (execSqlBatch)
// and for execSql, which tedious sends as an sp_executesql RPC. Every DONEPROC
// keeps the RETURNSTATUS that preceded it.
function run(connection, sql, { rpc = false, parameters = [] } = {}) {
  return new Promise(resolve => {
    const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
    const bounded = list => { if (list.length >= MAX_EVENTS) throw new Error('capture event bound exceeded') }
    const request = new Request(sql, (error, rowCount) => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.rowCount = rowCount
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      resolve(canonical(result))
    })
    const onError = e => { bounded(result.errors); result.errors.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, procName: e.procName, message: e.message }) }
    const onInfo = i => { bounded(result.info); result.info.push({ number: i.number, state: i.state, class: i.class, lineNumber: i.lineNumber, message: i.message }) }
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    request.on('columnMetadata', metadata => result.sets.push({
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })), rows: []
    }))
    request.on('row', row => {
      const set = result.sets.at(-1)
      if (set.rows.length >= MAX_ROWS) throw new Error('capture row bound exceeded')
      set.rows.push(row.map(c => c.value))
    })
    // tedious passes the RETURNSTATUS preceding each DONEPROC as its third argument.
    for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more, status) => {
      bounded(result.done)
      result.done.push({ kind, rowCount: rowCount ?? null, more, ...(kind === 'doneProc' ? { status: status ?? null } : {}) })
      if (kind === 'doneProc') result.returnStatus = status ?? null
    })
    for (const [name, type, value, options] of parameters) request.addParameter(name, type, value, options)
    if (rpc) connection.execSql(request)
    else connection.execSqlBatch(request)
  })
}

function reset(connection) {
  return new Promise(resolve => connection.reset(error => resolve(error ? { message: error.message } : null)))
}

const ownerProperties = `select
  cast(sessionproperty('ANSI_NULLS') as int) as ansiNulls,
  cast(sessionproperty('ANSI_PADDING') as int) as ansiPadding,
  cast(sessionproperty('ANSI_WARNINGS') as int) as ansiWarnings,
  cast(sessionproperty('ARITHABORT') as int) as arithabort,
  cast(sessionproperty('CONCAT_NULL_YIELDS_NULL') as int) as concatNullYieldsNull,
  cast(sessionproperty('QUOTED_IDENTIFIER') as int) as quotedIdentifier,
  cast(sessionproperty('NUMERIC_ROUNDABORT') as int) as numericRoundabort;`
const allProperties = OPTIONS.map(o => `SESSIONPROPERTY('${o}') AS ${o.toLowerCase()}`).join(', ')
const readContext = key => `SELECT SESSION_CONTEXT(N'${key}') AS v, CAST(SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'${key}'), 'BaseType') AS NVARCHAR(128)) AS base_type, CAST(SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'${key}'), 'MaxLength') AS INT) AS max_length`

export async function observe(connection) {
  const records = []
  const step = async (name, sql, options = {}) => {
    records.push({ name, sql, rpc: Boolean(options.rpc), parameters: (options.parameters ?? []).map(([n, t, v]) => ({ name: n, type: t.name, value: canonical(v) })), result: await run(connection, sql, options) })
  }
  const rpc = { rpc: true }

  // Login state established by tedious: @@OPTIONS and every SESSIONPROPERTY option.
  await step('login options', `SELECT @@OPTIONS AS options, ${allProperties}`)
  await step('sessionproperty base types', `SELECT ${OPTIONS.map(o => `CAST(SQL_VARIANT_PROPERTY(SESSIONPROPERTY('${o}'), 'BaseType') AS NVARCHAR(128)) AS ${o.toLowerCase()}`).join(', ')}`)

  // The owner's four statements through execSql, then as batches.
  await step('owner select 42', 'select 42', rpc)
  await step('owner sessionproperty', ownerProperties, rpc)
  await step('owner set context null', "exec sys.sp_set_session_context @key = N'email', @value = null", rpc)
  await step('owner parameter', 'select @p as v', { rpc: true, parameters: [['p', TYPES.Int, 42]] })
  await step('batch select 42', 'select 42')
  await step('batch sessionproperty', ownerProperties)
  await step('batch set context null', "exec sys.sp_set_session_context @key = N'email', @value = null")

  // SESSIONPROPERTY argument forms.
  await step('sessionproperty uncast', "SELECT SESSIONPROPERTY('ANSI_NULLS') AS v", rpc)
  await step('sessionproperty names', "SELECT SESSIONPROPERTY('ansi_nulls') AS lower_name, SESSIONPROPERTY(N'QUOTED_IDENTIFIER') AS unicode_name, SESSIONPROPERTY('NOPE') AS unknown_name, SESSIONPROPERTY('ANSI_NULL_DFLT_ON') AS not_an_option, SESSIONPROPERTY(NULL) AS null_name, SESSIONPROPERTY(' ANSI_NULLS') AS leading_space, SESSIONPROPERTY('ANSI_NULLS ') AS trailing_space")
  await step('sessionproperty variable name', "DECLARE @n NVARCHAR(128) = N'ARITHABORT'; SELECT SESSIONPROPERTY(@n) AS v")
  await step('sessionproperty parameter name', 'SELECT SESSIONPROPERTY(@n) AS v', { rpc: true, parameters: [['n', TYPES.NVarChar, 'ANSI_PADDING']] })
  await step('sessionproperty integer argument', 'SELECT SESSIONPROPERTY(1) AS v')
  await step('sessionproperty no argument', 'SELECT SESSIONPROPERTY() AS v')
  await step('sessionproperty two arguments', "SELECT SESSIONPROPERTY('ANSI_NULLS', 'ANSI_PADDING') AS v")

  // SET OFF (ON for NUMERIC_ROUNDABORT) and back, in one batch and across batches.
  for (const option of OPTIONS) {
    const [changed, restored] = option === 'NUMERIC_ROUNDABORT' ? ['ON', 'OFF'] : ['OFF', 'ON']
    await step(`set ${option} ${changed}`, `SET ${option} ${changed}; SELECT SESSIONPROPERTY('${option}') AS v, CAST(SESSIONPROPERTY('${option}') AS INT) AS n, @@OPTIONS AS options`)
    await step(`after set ${option} ${changed}`, `SELECT ${allProperties}`)
    await step(`restore ${option} ${restored}`, `SET ${option} ${restored}; SELECT ${allProperties}`)
  }
  await step('set ansi_warnings off in sp_executesql', "SET ANSI_WARNINGS OFF; SELECT CAST(SESSIONPROPERTY('ANSI_WARNINGS') AS INT) AS inside", rpc)
  await step('after sp_executesql set', `SELECT ${allProperties}`)

  // Session context: non-null values, read back, types and read_only.
  await step('set context value', "exec sys.sp_set_session_context @key = N'email', @value = N'owner@example.com'", rpc)
  await step('read context value', readContext('email'), rpc)
  await step('read context cast', "SELECT CAST(SESSION_CONTEXT(N'email') AS NVARCHAR(100)) AS email", rpc)
  await step('read context key spelling', "SELECT SESSION_CONTEXT(N'EMAIL') AS upper_key, SESSION_CONTEXT(N'Email') AS title_key, SESSION_CONTEXT(N'email ') AS trailing_space, SESSION_CONTEXT(N'missing') AS missing")
  await step('read context parameter key', 'SELECT SESSION_CONTEXT(@k) AS v', { rpc: true, parameters: [['k', TYPES.NVarChar, 'email']] })
  await step('set context int', "EXEC sp_set_session_context N'tenant', 42")
  await step('read context int', `${readContext('tenant')}, CAST(SESSION_CONTEXT(N'tenant') AS INT) AS tenant`)
  await step('set context bigint variable', "DECLARE @v BIGINT = 9000000000; EXEC sys.sp_set_session_context @key = N'big', @value = @v; SELECT SESSION_CONTEXT(N'big') AS v")
  await step('set context rpc parameter', "exec sys.sp_set_session_context @key = N'param', @value = @v", { rpc: true, parameters: [['v', TYPES.NVarChar, 'from parameter']] })
  await step('read context rpc parameter', readContext('param'))
  await step('set context return status', "DECLARE @r INT; EXEC @r = sys.sp_set_session_context @key = N'status', @value = 1; SELECT @r AS r")
  await step('set context names ignored', "EXEC sys.sp_set_session_context @value = N'x', @key = N'named'")
  await step('set context null key', 'EXEC sys.sp_set_session_context NULL, 1')
  await step('set context long key', `EXEC sys.sp_set_session_context N'${'k'.repeat(129)}', 1`)
  await step('set context empty key', "EXEC sys.sp_set_session_context N'', 1")
  await step('set context missing value', "EXEC sys.sp_set_session_context N'only'")
  await step('set context extra argument', "EXEC sys.sp_set_session_context N'extra', 1, 0, 1")
  await step('set context read_only null', "EXEC sys.sp_set_session_context N'ro_null', 1, NULL; SELECT SESSION_CONTEXT(N'ro_null') AS v")
  await step('set context read_only two', "EXEC sys.sp_set_session_context N'ro_two', 1, 2; EXEC sys.sp_set_session_context N'ro_two', 3; SELECT SESSION_CONTEXT(N'ro_two') AS v")
  await step('set context varchar value', "EXEC sys.sp_set_session_context N'ansi', 'abc'; " + readContext('ansi').replace('SELECT ', 'SELECT CAST(SESSION_CONTEXT(N\'ansi\') AS VARCHAR(10)) AS text, '))
  await step('set context nvarchar max value', "DECLARE @v NVARCHAR(MAX) = N'x'; EXEC sys.sp_set_session_context N'maxed', @v")
  await step('set context expression value', "EXEC sys.sp_set_session_context N'expr', 1+1")
  await step('set context overwrite type', "EXEC sys.sp_set_session_context N'over', 1; EXEC sys.sp_set_session_context N'over', N'two'; " + readContext('over'))
  await step('set context in transaction', "BEGIN TRAN; EXEC sys.sp_set_session_context N'tx', 5; ROLLBACK; SELECT SESSION_CONTEXT(N'tx') AS v, @@TRANCOUNT AS tc")
  await step('set context rowcount', "SELECT 1 AS one UNION ALL SELECT 2; EXEC sys.sp_set_session_context N'rc', 1; SELECT @@ROWCOUNT AS rc")
  await step('set context caught', "BEGIN TRY EXEC sys.sp_set_session_context NULL, 1 END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n, ERROR_PROCEDURE() AS p END CATCH")
  await step('session_context compare', "SELECT CASE WHEN SESSION_CONTEXT(N'tenant') = 42 THEN 1 ELSE 0 END AS eq, CAST(SESSION_CONTEXT(N'missing') AS INT) AS missing_int")
  await step('set context read_only', "exec sys.sp_set_session_context @key = N'locked', @value = N'first', @read_only = 1", rpc)
  await step('set context read_only violation rpc', "exec sys.sp_set_session_context @key = N'locked', @value = N'second'", rpc)
  await step('set context read_only violation batch', "EXEC sys.sp_set_session_context @key = N'LOCKED', @value = N'second'; SELECT SESSION_CONTEXT(N'locked') AS v, @@ERROR AS error")
  await step('session_context varchar key', "SELECT SESSION_CONTEXT('email') AS v")
  await step('session_context null key', 'SELECT SESSION_CONTEXT(NULL) AS v')
  await step('session_context no argument', 'SELECT SESSION_CONTEXT() AS v')

  // Reset (tedious connection.reset = RESETCONNECTION on the next request).
  await step('before reset', `SET ANSI_WARNINGS OFF; ${readContext('email')}; SELECT ${allProperties}`)
  records.push({ name: 'reset', result: await reset(connection) })
  await step('after reset', `${readContext('email')}; SELECT SESSION_CONTEXT(N'locked') AS locked; SELECT ${allProperties}`, rpc)
  await step('read_only after reset', "exec sys.sp_set_session_context @key = N'locked', @value = N'third', @read_only = 1; SELECT SESSION_CONTEXT(N'locked') AS v")
  return records
}

function validate(run) {
  const get = name => {
    const record = run.find(r => r.name === name)
    if (!record) throw new Error(`missing ${name}`)
    return record.result
  }
  for (const name of ['owner select 42', 'owner sessionproperty', 'owner set context null', 'owner parameter']) {
    if (get(name).errors.length) throw new Error(`${name} failed on the reference`)
  }
  if (get('set context read_only violation rpc').errors[0]?.number !== 15664) throw new Error('read_only violation not observed')
  if (get('after reset').sets[0].rows[0][0] !== null) throw new Error('reset did not clear the session context')
}

// Capture only when run directly; tests import `observe` to replay it.
if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(resolve(output, '..'), { recursive: true })
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      const records = await isolatedReference(config, observe)
      validate(records)
      runs.push(records)
    }
    assertSameCapture(runs[0], runs[1], 'session property/context capture differs across fresh databases')
    const actual = { image: container.image, runs }
    await writeFile(output, JSON.stringify(actual) + '\n')
    let retained
    try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    if (retained) assertSameCapture(actual, retained, 'session property/context capture differs from retained fixture')
    if (writeFixture) await writeNewFixture(fixture, actual)
    console.log(`Captured ${runs[0].length} observations in two fresh databases${retained ? ' and matched retained fixture' : ''}`)
  })
}
