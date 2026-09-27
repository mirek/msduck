#!/usr/bin/env node
// Owner-run SQL Server evidence for table variables. The copied mssqlite
// implementation is a source of hypotheses, never of expected results.
process.env.TZ = 'UTC'
if (new Date(0).getTimezoneOffset() !== 0) throw new Error('UTC client time zone is required')

import { access, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { assertSameCapture, isolatedReference } from './lib/reference.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'

const fixture = new URL('../reference/table-variable.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one scratch output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/table-variable-reference-v1/capture.json')
const limits = Object.freeze({ sets: 32, rows: 200, messages: 200 })

// Resolve aliases even when the scratch file does not yet exist. A hard link
// needs the additional device/inode check. Refuse before any container starts.
async function canonicalPath(path) {
  const absolute = resolve(path)
  try { return await realpath(absolute) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    const parent = dirname(absolute)
    return parent === absolute ? absolute : resolve(await canonicalPath(parent), basename(absolute))
  }
}
async function identity(path) {
  try { const s = await stat(path); return `${s.dev}:${s.ino}` }
  catch (error) { if (error.code === 'ENOENT') return null; throw error }
}
const fixturePath = fileURLToPath(fixture)
if (await canonicalPath(output) === await canonicalPath(fixturePath) ||
    (await identity(fixturePath) !== null && await identity(output) === await identity(fixturePath))) {
  throw new Error('refusing to write scratch output over retained fixture ' + fixturePath)
}
let fixtureExists = false
try { await access(fixture); fixtureExists = true }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (writeFixture && fixtureExists) throw new Error('refusing to overwrite retained fixture ' + fixturePath)

const messageFields = m => ({ number: m.number, state: m.state, class: m.class, lineNumber: m.lineNumber, message: m.message })
const columnFields = c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })
const fresh = () => ({ sets: [], done: [], errors: [], info: [], returnStatus: null })

// Bound each request independently, including errors and DONE tokens. A
// truncated server ERROR still suppresses a lossy callback-error replacement.
function requestCapture(connection, sql, mode = 'batch', parameters = []) {
  return new Promise(resolvePromise => {
    const result = fresh()
    let activeSet
    let serverError = false
    let finished = false
    const overflow = kind => { result.truncated ??= { sets: 0, rows: 0, messages: 0 }; result.truncated[kind]++ }
    const message = kind => token => {
      if (kind === 'errors') serverError = true
      if (result.errors.length + result.info.length + result.done.length >= limits.messages) overflow('messages')
      else result[kind].push(messageFields(token))
    }
    const onError = message('errors')
    const onInfo = message('info')
    const finish = (error, rowCount) => {
      if (finished) return
      finished = true
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.rowCount = rowCount ?? null
      if (error && !serverError) {
        if (result.errors.length + result.info.length + result.done.length >= limits.messages) overflow('messages')
        else result.errors.push({ number: error.number ?? null, message: error.message })
      }
      resolvePromise(canonical(result))
    }
    const request = new Request(sql, finish)
    for (const [name, type, value, options] of parameters) request.addParameter(name, type, value, options)
    request.on('columnMetadata', columns => {
      if (result.sets.length >= limits.sets) { activeSet = undefined; overflow('sets'); return }
      activeSet = { columns: columns.map(columnFields), rows: [] }
      result.sets.push(activeSet)
    })
    request.on('row', columns => {
      if (!activeSet || activeSet.rows.length >= limits.rows) overflow('rows')
      else activeSet.rows.push(columns.map(c => c.value))
    })
    for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => {
      if (result.errors.length + result.info.length + result.done.length >= limits.messages) overflow('messages')
      else result.done.push({ kind, rowCount: rowCount ?? null, more })
    })
    request.on('doneProc', (_count, _more, status) => { if (status !== undefined) result.returnStatus = status })
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    connection.procReturnStatusValue = undefined
    try {
      if (mode === 'batch') connection.execSqlBatch(request)
      else if (mode === 'rpc') connection.execSql(request)
      else if (mode === 'procedure') connection.callProcedure(request)
      else throw new Error('unknown request mode ' + mode)
    } catch (error) { finish(error, null) }
  })
}

// Tedious sp_prepare has a separate completion event; the Request callback is
// for execute/unprepare. Reuse the same Request and reset carried errors and
// statuses at each phase, preserving only tokens observed in that phase.
async function preparedCapture(connection, sql, parameters, valueSets) {
  const request = new Request(sql, (error, rowCount) => complete(error, rowCount))
  for (const [name, type, options] of parameters) request.addParameter(name, type, undefined, options)
  let current
  let activeSet
  let serverError = false
  let phaseFailed = false
  let complete = () => {}
  const reported = new Set()
  const overflow = kind => { current.truncated ??= { sets: 0, rows: 0, messages: 0 }; current.truncated[kind]++ }
  const message = kind => token => {
    if (!current) return
    if (kind === 'errors') serverError = true
    if (current.errors.length + current.info.length + current.done.length >= limits.messages) overflow('messages')
    else current[kind].push(messageFields(token))
  }
  const onError = message('errors')
  const onInfo = message('info')
  request.on('columnMetadata', columns => {
    if (!current) return
    if (current.sets.length >= limits.sets) { activeSet = undefined; overflow('sets'); return }
    activeSet = { columns: columns.map(columnFields), rows: [] }
    current.sets.push(activeSet)
  })
  request.on('row', columns => {
    if (!current) return
    if (!activeSet || activeSet.rows.length >= limits.rows) overflow('rows')
    else activeSet.rows.push(columns.map(c => c.value))
  })
  for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => {
    if (!current) return
    if (current.errors.length + current.info.length + current.done.length >= limits.messages) overflow('messages')
    else current.done.push({ kind, rowCount: rowCount ?? null, more })
  })
  request.on('doneProc', (_count, _more, status) => { if (current && status !== undefined) current.returnStatus = status })
  const phase = start => new Promise(resolvePromise => {
    current = fresh()
    activeSet = undefined
    serverError = false
    complete = (error, rowCount) => {
      const finished = current
      if (!finished) return
      current = undefined
      complete = () => {}
      finished.rowCount = rowCount ?? null
      phaseFailed = serverError || Boolean(error)
      if (error && !serverError && !reported.has(error)) {
        if (finished.errors.length + finished.info.length + finished.done.length >= limits.messages) {
          current = finished; overflow('messages'); current = undefined
        } else finished.errors.push({ number: error.number ?? null, message: error.message })
      }
      if (error) reported.add(error)
      resolvePromise(canonical(finished))
    }
    request.error = undefined
    connection.procReturnStatusValue = undefined
    try { start() } catch (error) { complete(error, null) }
  })
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  const onPrepared = () => complete(null, null)
  const onPrepareError = error => complete(error, null)
  try {
    request.once('prepared', onPrepared)
    request.once('error', onPrepareError)
    const prepare = await phase(() => connection.prepare(request))
    request.off('prepared', onPrepared)
    request.off('error', onPrepareError)
    if (phaseFailed || !(Number.isInteger(request.handle) && request.handle > 0)) {
      const skipped = 'prepare failed'
      return { prepare, prepared: false, executions: valueSets.map(values => ({ values, skipped })), unprepare: null }
    }
    const executions = []
    for (const values of valueSets) executions.push({ values, result: await phase(() => connection.execute(request, values)) })
    const unprepare = await phase(() => connection.unprepare(request))
    return { prepare, prepared: true, executions, unprepare }
  } finally {
    request.off('prepared', onPrepared)
    request.off('error', onPrepareError)
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

// Every case is a distinct SQL batch. Local table variables must be declared
// in the same request in which they are used. Names and constraint text stay
// constant across the four independent database captures.
const batches = [
  ['empty typed descriptor', 'DECLARE @t TABLE(i INT NOT NULL, s NVARCHAR(5) NULL, d DECIMAL(8,2), tm DATETIME2(3)); SELECT i,s,d,tm FROM @t'],
  ['insert and ordered read', "DECLARE @t TABLE(i INT NOT NULL, s NVARCHAR(5)); INSERT @t(i,s) VALUES (2,N'b'),(1,N'a'); SELECT i,s FROM @t ORDER BY i; SELECT COUNT(*) AS n FROM @t"],
  ['defaults and nullable columns', 'DECLARE @t TABLE(i INT NOT NULL DEFAULT(7), s NVARCHAR(5) NULL); INSERT @t DEFAULT VALUES; INSERT @t(s) VALUES (N\'x\'); SELECT i,s FROM @t ORDER BY s'],
  ['primary key and check', 'DECLARE @t TABLE(i INT PRIMARY KEY,n INT CHECK(n>0)); INSERT @t VALUES(1,2); SELECT i,n FROM @t'],
  ['unique and check', 'DECLARE @t TABLE(i INT UNIQUE,n INT CHECK(n>0)); INSERT @t VALUES(1,2),(2,3); SELECT i,n FROM @t ORDER BY i'],
  // The generated key/constraint name changes in every database, so catch a
  // violation and retain its diagnostic identity without fabricating a stable
  // raw message. The unhandled error text remains an explicit capture gap.
  ['primary key violation identity', 'DECLARE @t TABLE(i INT PRIMARY KEY); INSERT @t VALUES(1); BEGIN TRY INSERT @t VALUES(1); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number,ERROR_STATE() AS state,ERROR_SEVERITY() AS severity,ERROR_LINE() AS line; END CATCH; SELECT i FROM @t'],
  ['not null violation', 'DECLARE @t TABLE(i INT NOT NULL, s VARCHAR(4)); INSERT @t(s) VALUES(\'x\'); SELECT i,s FROM @t'],
  ['duplicate column', 'DECLARE @t TABLE(i INT, i INT); SELECT i FROM @t'],
  ['named table constraint rejected', 'DECLARE @t TABLE(i INT CONSTRAINT pk_tv_primary PRIMARY KEY); SELECT i FROM @t'],
  ['unsupported declaration type', 'DECLARE @t TABLE(i imaginary_type); SELECT i FROM @t'],
  ['two table declarations in one DECLARE', 'DECLARE @a TABLE(i INT), @b TABLE(i INT); SELECT 1 AS v'],
  ['scalar and table in one DECLARE', 'DECLARE @a TABLE(i INT), @x INT; SELECT 1 AS v'],
  ['undeclared table variable', 'SELECT i FROM @missing'],
  ['DML and OUTPUT', 'DECLARE @t TABLE(i INT PRIMARY KEY, n INT); INSERT @t(i,n) OUTPUT inserted.i,inserted.n VALUES(1,10),(2,20); UPDATE @t SET n=n+1 OUTPUT deleted.n,inserted.n WHERE i=1; DELETE @t OUTPUT deleted.i,deleted.n WHERE i=2; SELECT i,n FROM @t'],
  ['OUTPUT INTO table variable', 'DECLARE @sink TABLE(v INT NOT NULL); INSERT dbo.tv_base(v) OUTPUT inserted.v INTO @sink VALUES(21),(22); SELECT v FROM @sink ORDER BY v; SELECT v FROM dbo.tv_base ORDER BY v; DELETE dbo.tv_base'],
  ['INSERT SELECT into table variable', 'DECLARE @t TABLE(i INT NOT NULL,n INT); INSERT @t(i,n) SELECT id,n FROM (VALUES(1,10),(2,20)) s(id,n); SELECT i,n FROM @t ORDER BY i'],
  ['join and CTE source', 'DECLARE @t TABLE(i INT,n INT); INSERT @t VALUES(1,10),(2,20); WITH c AS (SELECT i,n FROM @t) SELECT c.i,c.n+x.delta AS total FROM c JOIN (VALUES(1,3),(2,4)) x(i,delta) ON x.i=c.i ORDER BY c.i'],
  ['failed multi-row insert is atomic', 'DECLARE @t TABLE(i INT NOT NULL); INSERT @t VALUES(7); BEGIN TRY INSERT @t VALUES(8),(NULL),(9); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number,ERROR_STATE() AS state,ERROR_SEVERITY() AS severity,XACT_STATE() AS xact_state,@@TRANCOUNT AS tran_count; END CATCH; SELECT i FROM @t ORDER BY i'],
  // The first inserted row can update (3 -> 1); the second cannot (1 -> -1).
  // Catch generated CHECK constraint names while preserving the exact stable
  // diagnostic identity, preexisting rows, descriptors and DONE tokens.
  ['failed multi-row update is atomic', 'DECLARE @t TABLE(i INT CHECK(i>0),n INT); INSERT @t VALUES(3,10),(1,20); BEGIN TRY UPDATE @t SET i=i-2; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number,ERROR_STATE() AS state,ERROR_SEVERITY() AS severity,XACT_STATE() AS xact_state,@@TRANCOUNT AS tran_count; END CATCH; SELECT i,n FROM @t ORDER BY i'],
  ['table variable and temp table names', 'DECLARE @t TABLE(v INT); CREATE TABLE #t(v INT); INSERT @t VALUES(1); INSERT #t VALUES(2); SELECT (SELECT SUM(v) FROM @t) AS local_value,(SELECT SUM(v) FROM #t) AS temp_value; DROP TABLE #t'],
  ['table variable and ordinary table names', 'DECLARE @t TABLE(v INT); INSERT @t VALUES(1); INSERT dbo.tv_base(v) VALUES(2); SELECT (SELECT SUM(v) FROM @t) AS local_value,(SELECT SUM(v) FROM dbo.tv_base) AS table_value; DELETE dbo.tv_base'],
  ['dynamic SQL cannot see caller variable', "DECLARE @t TABLE(v INT); INSERT @t VALUES(1); EXEC(N'SELECT v FROM @t'); SELECT v FROM @t"],
  ['sp_executesql cannot see caller variable', "DECLARE @t TABLE(v INT); INSERT @t VALUES(1); EXEC sp_executesql N'SELECT v FROM @t'; SELECT v FROM @t"],
  ['dynamic SQL declares its own variable', "EXEC(N'DECLARE @t TABLE(v INT); INSERT @t VALUES(5); SELECT v FROM @t')"],
  ['transaction rollback and identity', 'BEGIN TRANSACTION; DECLARE @t TABLE(id INT IDENTITY(1,1),v INT); INSERT @t(v) VALUES(10); INSERT dbo.tv_base(v) VALUES(10); ROLLBACK TRANSACTION; SELECT id,v FROM @t; SELECT v FROM dbo.tv_base; INSERT @t(v) VALUES(20); SELECT id,v FROM @t ORDER BY id'],
  ['failed insert inside transaction and identity', 'DECLARE @t TABLE(id INT IDENTITY(1,1),v INT CHECK(v>0)); BEGIN TRANSACTION; INSERT @t(v) VALUES(10); INSERT dbo.tv_base(v) VALUES(10); BEGIN TRY INSERT @t(v) VALUES(20),(-1),(30); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number,ERROR_STATE() AS state,ERROR_SEVERITY() AS severity,XACT_STATE() AS xact_state,@@TRANCOUNT AS tran_count; END CATCH; SELECT id,v FROM @t ORDER BY id; ROLLBACK TRANSACTION; SELECT id,v FROM @t ORDER BY id; INSERT @t(v) VALUES(40); SELECT id,v FROM @t ORDER BY id; SELECT v FROM dbo.tv_base'],
  ['savepoint rollback', 'DECLARE @t TABLE(v INT); BEGIN TRANSACTION; SAVE TRANSACTION tv_sp; INSERT @t VALUES(3); INSERT dbo.tv_base(v) VALUES(3); ROLLBACK TRANSACTION tv_sp; SELECT v FROM @t; SELECT v FROM dbo.tv_base; COMMIT TRANSACTION'],
  ['truncate table variable', 'DECLARE @t TABLE(v INT); INSERT @t VALUES(1); TRUNCATE TABLE @t; SELECT v FROM @t'],
  ['select into table variable', 'DECLARE @t TABLE(v INT); SELECT 1 AS v INTO @t; SELECT v FROM @t'],
]

async function observe(connection) {
  const records = []
  const record = async (name, sql, mode = 'batch', parameters = []) => {
    records.push({ name, mode, sql, result: await requestCapture(connection, sql, mode, parameters) })
  }
  await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS collation")
  await record('create ordinary table', 'CREATE TABLE dbo.tv_base(v INT NOT NULL)')
  for (const [name, sql] of batches) await record(name, sql)
  // A later request on the same connection cannot use a previous batch's
  // table variable. The first request is captured separately as evidence.
  await record('declare in earlier request', 'DECLARE @later TABLE(v INT); INSERT @later VALUES(9)')
  await record('later request cannot see variable', 'SELECT v FROM @later')
  await record('rpc declaration and bound value', 'DECLARE @t TABLE(v INT); INSERT @t VALUES(@p); SELECT v FROM @t', 'rpc', [['p', TYPES.Int, 7]])
  await record('rpc caller cannot see table variable', 'SELECT v FROM @later WHERE v=@p', 'rpc', [['p', TYPES.Int, 9]])
  await record('create local procedure', 'CREATE PROCEDURE dbo.tv_local_probe AS BEGIN DECLARE @t TABLE(v INT); INSERT @t VALUES(11); SELECT v FROM @t; END')
  await record('procedure local table variable', 'dbo.tv_local_probe', 'procedure')
  await record('caller cannot see procedure variable', 'SELECT v FROM @t')
  await record('create procedure referencing caller variable', 'CREATE PROCEDURE dbo.tv_caller_probe AS SELECT v FROM @t')
  records.push({ name: 'prepared table variable batch', mode: 'prepared', sql: 'DECLARE @t TABLE(v INT); INSERT @t VALUES(@p); SELECT v FROM @t', result: await preparedCapture(connection, 'DECLARE @t TABLE(v INT); INSERT @t VALUES(@p); SELECT v FROM @t', [['p', TYPES.Int]], [{ p: 12 }, { p: 13 }]) })
  records.push({ name: 'prepared reference to missing variable', mode: 'prepared', sql: 'SELECT v FROM @t WHERE v=@p', result: await preparedCapture(connection, 'SELECT v FROM @t WHERE v=@p', [['p', TYPES.Int]], [{ p: 1 }]) })
  await record('drop local procedure', 'DROP PROCEDURE dbo.tv_local_probe')
  await record('drop ordinary table', 'DROP TABLE dbo.tv_base')
  await record('session remains reusable', 'SELECT @@TRANCOUNT AS tran_count,1 AS reusable')
  return records
}

await mkdir(dirname(output), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
      runs.push(await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, observe))
    }
    if (!oneDatabase) assertSameCapture(runs[0], runs[1], 'table-variable observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) assertSameCapture(containers[0].runs[0], containers[1].runs[0], 'table-variable observations differ across containers')
const actual = { containers }
const retained = fixtureExists ? JSON.parse(await readFile(fixture, 'utf8')) : null
await writeFile(output, JSON.stringify(actual) + '\n')
if (retained && !oneDatabase) assertSameCapture(actual, retained, 'table-variable observations differ from retained fixture')
if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
console.log(`Captured ${containers[0].runs[0].length} table-variable observations in ${oneDatabase ? 'one diagnostic database' : 'four fresh databases across two containers'}${retained && !oneDatabase ? ' and matched retained fixture' : ''}`)
