#!/usr/bin/env node
// First-party SQL Server evidence for issue #727: isolation levels
// (SET TRANSACTION ISOLATION LEVEL, transaction-manager begin requests,
// sys.dm_exec_sessions, DBCC USEROPTIONS, sp_executesql scope) and WAITFOR
// DELAY/TIME validation and waiting. Savepoint rules are retained separately
// in reference/savepoint.json. See docs/gaps-transactions.md.
//
//   node scripts/capture-gaps-transactions.mjs [output] [--write-fixture]
//
// Waits are recorded as booleans (at least the requested time, and well
// under a bound) so that two runs compare exactly.
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { ISOLATION_LEVEL } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

process.env.TZ = 'UTC'
const fixture = new URL('../reference/gaps-transactions.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/gaps-transactions/gaps-transactions.json')

const level = 'SELECT transaction_isolation_level FROM sys.dm_exec_sessions WHERE session_id = @@SPID'

function bindGeneratedDatabaseNames(run) {
  return JSON.parse(JSON.stringify(run).replaceAll(/msduck_audit_[0-9a-f]{32}/g, '<fresh-database>'))
}

async function observe(connection) {
  const run = []
  async function record(name, sql, wait) {
    const started = Date.now()
    const result = canonical(await capture(connection, sql))
    const elapsed = Date.now() - started
    const entry = { name, sql, result }
    if (wait) {
      entry.wait = wait
      entry.waited = { atLeast: elapsed >= wait.atLeast, under: elapsed < wait.under }
    }
    run.push(entry)
    return result
  }
  async function manager(name, operation, ...args) {
    const error = await new Promise(resolve => connection[operation](error => resolve(error), ...args))
    run.push({ name, request: operation, args: args.map(canonical), error: error ? { number: error.number ?? null, message: error.message } : null })
  }

  await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version, CAST(DATABASEPROPERTYEX(DB_NAME(), 'Collation') AS NVARCHAR(128)) AS database_collation")

  // Isolation levels through SQL.
  await record('default level', level)
  for (const name of ['READ UNCOMMITTED', 'REPEATABLE READ', 'SERIALIZABLE', 'SNAPSHOT', 'READ COMMITTED']) {
    await record(`set ${name}`, `SET TRANSACTION ISOLATION LEVEL ${name}`)
    await record(`level after ${name}`, level)
    await record(`useroptions after ${name}`, 'DBCC USEROPTIONS')
  }
  await record('set inside a transaction', `SET TRANSACTION ISOLATION LEVEL SERIALIZABLE; BEGIN TRAN; SET TRANSACTION ISOLATION LEVEL REPEATABLE READ; ${level}; COMMIT; ${level}`)
  await record('reset to read committed', 'SET TRANSACTION ISOLATION LEVEL READ COMMITTED')
  await record('sp_executesql scope', `EXEC sp_executesql N'SET TRANSACTION ISOLATION LEVEL SERIALIZABLE; ${level}'`)
  await record('level after sp_executesql', level)
  await record('useroptions with options', 'SET NOCOUNT ON; SET XACT_ABORT ON; SET ANSI_WARNINGS OFF; SET DATEFIRST 3; DBCC USEROPTIONS')
  await record('useroptions with all options on', 'SET ANSI_WARNINGS ON; DBCC USEROPTIONS WITH NO_INFOMSGS')
  await record('useroptions restore', 'SET NOCOUNT OFF; SET XACT_ABORT OFF; SET DATEFIRST 7; DBCC USEROPTIONS WITH NO_INFOMSGS; SELECT @@ROWCOUNT AS row_count')
  await record('useroptions inside a transaction', 'BEGIN TRAN; DBCC USEROPTIONS; SELECT @@ROWCOUNT AS row_count; COMMIT')
  await record('read committed snapshot setup', 'ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE')
  await record('useroptions under read committed snapshot', `DBCC USEROPTIONS WITH NO_INFOMSGS; ${level}`)
  await record('read committed snapshot teardown', 'ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT OFF WITH ROLLBACK IMMEDIATE')

  // Transaction-manager begin requests set the session's level.
  for (const [name, isolation] of [['SERIALIZABLE', ISOLATION_LEVEL.SERIALIZABLE], ['SNAPSHOT', ISOLATION_LEVEL.SNAPSHOT], ['REPEATABLE_READ', ISOLATION_LEVEL.REPEATABLE_READ], ['READ_UNCOMMITTED', ISOLATION_LEVEL.READ_UNCOMMITTED], ['READ_COMMITTED', ISOLATION_LEVEL.READ_COMMITTED]]) {
    await manager(`tm begin ${name}`, 'beginTransaction', '', isolation)
    await record(`level in tm ${name}`, `${level}; SELECT @@TRANCOUNT AS tran_count`)
    await manager(`tm commit ${name}`, 'commitTransaction')
    await record(`level after tm ${name}`, level)
  }
  await manager('tm begin outer snapshot', 'beginTransaction', 'outer', ISOLATION_LEVEL.SNAPSHOT)
  await manager('tm begin nested read uncommitted', 'beginTransaction', 'inner', ISOLATION_LEVEL.READ_UNCOMMITTED)
  await record('level in nested tm', `${level}; SELECT @@TRANCOUNT AS tran_count`)
  await manager('tm rollback outer', 'rollbackTransaction', 'outer')
  await record('level after tm rollback', `${level}; SELECT @@TRANCOUNT AS tran_count`)
  await manager('tm begin no change', 'beginTransaction', '', ISOLATION_LEVEL.NO_CHANGE)
  await record('level in tm no change', level)
  await manager('tm rollback no change', 'rollbackTransaction')
  await record('isolation reset', 'SET TRANSACTION ISOLATION LEVEL READ COMMITTED')

  // WAITFOR literals and variables. Long or unbounded waits are excluded.
  const waits = [
    ["delay 1 ms", "WAITFOR DELAY '00:00:00.001'", 0],
    ["delay 300 ms", "WAITFOR DELAY '00:00:00.300'", 300],
    ["delay colon milliseconds", "WAITFOR DELAY '00:00:00:200'", 200],
    ["delay short fields", "WAITFOR DELAY '0:0:0.2'", 200],
    ["delay hours and minutes", "WAITFOR DELAY '00:00'", 0],
    ["delay empty", "WAITFOR DELAY ''", 0],
    ["delay blanks", "WAITFOR DELAY ' 00:00:00.100 '", 100],
    ["delay AM", "WAITFOR DELAY '12:00:00.100 AM'", 100],
    ["delay unicode", "WAITFOR DELAY N'00:00:00.100'", 100],
    ["delay varchar", "DECLARE @d VARCHAR(20) = '00:00:00.200'; WAITFOR DELAY @d", 200],
    ["delay nvarchar", "DECLARE @d NVARCHAR(20) = N'00:00:00.200'; WAITFOR DELAY @d", 200],
    ["delay char", "DECLARE @d CHAR(12) = '00:00:00.300'; WAITFOR DELAY @d", 300],
    ["delay datetime", "DECLARE @d DATETIME = '2020-05-05T00:00:00.250'; WAITFOR DELAY @d", 250],
    ["delay int", "DECLARE @d INT = 1; WAITFOR DELAY @d", 1000],
    ["delay smallint zero", "DECLARE @d SMALLINT = 0; WAITFOR DELAY @d", 0],
    ["delay null", "DECLARE @d VARCHAR(20) = NULL; WAITFOR DELAY @d", 0],
    ["delay empty variable", "DECLARE @d VARCHAR(20) = ''; WAITFOR DELAY @d", 0],
    ["delay in transaction", "BEGIN TRAN; WAITFOR DELAY '00:00:00.050'; SELECT @@TRANCOUNT AS tran_count; COMMIT", 50],
    ["delay rowcount", "SELECT 1 AS one; WAITFOR DELAY '00:00:00.010'; SELECT @@ROWCOUNT AS row_count", 10],
    ["delay in IF", "IF 1 = 1 WAITFOR DELAY '00:00:00.010'", 10],
    ["delay through sp_executesql", "EXEC sp_executesql N'WAITFOR DELAY @p', N'@p VARCHAR(20)', @p = '00:00:00.100'", 100],
    ["time in one second", "DECLARE @t INT = DATEDIFF(SECOND, CAST(CAST(GETDATE() AS DATE) AS DATETIME), GETDATE()) + 2; WAITFOR TIME @t; SELECT 'woke' AS woke", 0],
    ["time null", "DECLARE @t VARCHAR(20) = NULL; WAITFOR TIME @t", 0]
  ]
  for (const [name, sql, atLeast] of waits) await record(`waitfor ${name}`, sql, { atLeast, under: atLeast + 3000 })
  for (const [name, sql] of [
    ['invalid literal', "WAITFOR DELAY 'abc'"],
    ['hour 25', "WAITFOR DELAY '25:00'"],
    ['hour 24', "WAITFOR DELAY '24:00:00'"],
    ['minute 60', "WAITFOR DELAY '00:60:00'"],
    ['date part', "WAITFOR DELAY '2020-01-01 00:00:00.100'"],
    ['four fraction digits', "WAITFOR DELAY '00:00:00.0005'"],
    ['seven fraction digits', "WAITFOR DELAY '00:00:00.1234567'"],
    ['four colon digits', "WAITFOR DELAY '00:00:00:1000'"],
    ['hour 0 PM', "WAITFOR TIME '00:00:00.100 PM'"],
    ['attached PM', "WAITFOR DELAY '00:00:00.100PM'"],
    ['one field', "WAITFOR DELAY '5'"],
    ['leading colon', "WAITFOR DELAY ':05'"],
    ['time literal', "WAITFOR TIME 'abc'"],
    ['compile error before earlier statement', "SELECT 1 AS one; WAITFOR DELAY 'abc'; SELECT 2 AS two"],
    ['compile error in TRY', "BEGIN TRY WAITFOR DELAY 'abc' END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS caught END CATCH"],
    ['integer literal', 'WAITFOR DELAY 5'],
    ['expression', "WAITFOR DELAY '00:00:00.1' + ''"],
    ['missing argument', 'WAITFOR TIME'],
    ['timeout clause', "WAITFOR DELAY '00:00:00.100', TIMEOUT 5"],
    ['undeclared variable', 'WAITFOR DELAY @undeclared'],
    ['invalid varchar', "DECLARE @d VARCHAR(20) = 'abc'; WAITFOR DELAY @d"],
    ['varchar hour 25', "DECLARE @d VARCHAR(30) = '25:00'; WAITFOR DELAY @d"],
    ['varchar date part', "DECLARE @d VARCHAR(30) = '2020-01-01 00:00:00.300'; WAITFOR DELAY @d"],
    ['varchar max', "DECLARE @d VARCHAR(MAX) = '00:00:00.100'; WAITFOR DELAY @d"],
    ['nvarchar max', "DECLARE @d NVARCHAR(MAX) = N'00:00:00.100'; WAITFOR DELAY @d"],
    ['caught invalid varchar', "BEGIN TRY DECLARE @d VARCHAR(20) = 'abc'; WAITFOR DELAY @d END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number, ERROR_SEVERITY() AS severity, ERROR_STATE() AS state, ERROR_MESSAGE() AS message END CATCH"],
    ...[['BIGINT', '1'], ['TINYINT', '0'], ['BIT', '1'], ['DECIMAL(5,2)', '0.5'], ['FLOAT', '0.000005'], ['MONEY', '0'],
      ['DATE', "'2020-01-01'"], ['TIME', "'00:00:00.250'"], ['DATETIME2', "'2020-01-01 00:00:00.300'"],
      ['SMALLDATETIME', "'2020-01-01 00:00'"], ['DATETIMEOFFSET', "'2020-01-01 00:00:00.300 +00:00'"],
      ['UNIQUEIDENTIFIER', 'NEWID()'], ['VARBINARY(10)', '0x01'], ['SQL_VARIANT', "'00:00:00.100'"]]
      .map(([type, value]) => [`type ${type}`, `DECLARE @d ${type} = ${value}; WAITFOR DELAY @d`]),
    ['null bigint', 'DECLARE @d BIGINT; WAITFOR DELAY @d'],
    ['null time', 'DECLARE @d TIME; WAITFOR TIME @d'],
    ['caught type', 'BEGIN TRY DECLARE @d BIT = 1; WAITFOR DELAY @d END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number, ERROR_SEVERITY() AS severity, ERROR_STATE() AS state, ERROR_MESSAGE() AS message END CATCH']
  ]) await record(`waitfor ${name}`, sql)
  return run
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(bindGeneratedDatabaseNames(await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 60000 } }, observe)))
  }
  assertSameCapture(runs[0], runs[1], 'fresh SQL Server databases differ after binding only generated database names')
  const actual = { image: container.image, run: runs[0] }
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) {
    if (actual.image !== retained.image) throw new Error(`image ${actual.image} differs from the retained ${retained.image}`)
    assertSameCapture(actual.run, retained.run, 'fresh capture differs from the retained fixture')
  }
  await writeFile(output, JSON.stringify(actual) + '\n')
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${actual.run.length} observations in two fresh databases${retained ? ' and matched the retained fixture' : ''}`)
})
