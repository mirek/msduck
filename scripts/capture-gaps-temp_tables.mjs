#!/usr/bin/env node
// Owner-controlled SQL Server evidence for #temp tables and table variables
// (gaps-temp-tables-v1). Each probe runs on session A or B of one fresh
// database, as a SQL batch or an RPC (sp_executesql). The probes are also
// replayed against msduck by tests/compat/temp_tables.test.mjs.
//
//   node scripts/capture-gaps-temp_tables.mjs                 compare with the fixture
//   node scripts/capture-gaps-temp_tables.mjs --write-fixture create the fixture
//
// MSSQL_REFERENCE_IMAGE selects another SQL Server image than the pinned one.
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/gaps-temp_tables.json', import.meta.url)

// [name, session, mode, sql]. Session C replaces A after A disconnects.
export const probes = [
  ['local create and rows', 'A', 'batch', 'CREATE TABLE #foo(id int); INSERT #foo VALUES (1),(2); SELECT id FROM #foo ORDER BY id'],
  ['local private to other session', 'B', 'batch', "SELECT OBJECT_ID('tempdb..#foo') AS object_id; SELECT id FROM #foo"],
  ['same local name in other session', 'B', 'batch', 'CREATE TABLE #foo(id int); INSERT #foo VALUES (3); SELECT id FROM #foo'],
  ['local survives batch', 'A', 'batch', 'SELECT id FROM #foo ORDER BY id'],
  ['object id forms', 'A', 'batch', "SELECT CASE WHEN OBJECT_ID('tempdb..#foo') IS NULL THEN 0 ELSE 1 END AS tempdb_form, CASE WHEN OBJECT_ID(N'tempdb.dbo.#foo', N'U') IS NULL THEN 0 ELSE 1 END AS typed_form, OBJECT_ID('#foo') AS unqualified"],
  ['qualified names', 'A', 'batch', 'SELECT #foo.id FROM tempdb..#foo WHERE #foo.id = 2; SELECT f.id FROM tempdb.dbo.#foo AS f WHERE f.id = 1; SELECT id FROM dbo.#foo WHERE id = 1'],
  ['constraints and indexes', 'A', 'batch', "CREATE TABLE #c(id int IDENTITY(1,1) PRIMARY KEY, code varchar(10) NOT NULL UNIQUE, qty int CHECK (qty > 0) DEFAULT 1); CREATE INDEX ix_c ON #c(qty); INSERT #c(code) VALUES (N'a'); INSERT #c(code, qty) VALUES (N'b', 5); SELECT id, code, qty FROM #c ORDER BY id"],
  ['temp duplicate key', 'A', 'batch', "INSERT #c(code) VALUES (N'a')"],
  ['temp check constraint', 'A', 'batch', "INSERT #c(code, qty) VALUES (N'z', 0)"],
  ['temp update delete truncate', 'A', 'batch', "UPDATE #c SET qty = qty + 1 WHERE code = N'a'; DELETE FROM #c WHERE code = N'b'; SELECT id, code, qty FROM #c ORDER BY id; TRUNCATE TABLE #c; SELECT COUNT(*) AS remaining FROM #c"],
  ['select into', 'A', 'batch', 'SELECT id AS n, id * 10 AS tens INTO #s FROM #foo; SELECT n, tens FROM #s ORDER BY n'],
  ['alter temp', 'A', 'batch', 'ALTER TABLE #s ADD extra int NULL'],
  ['insert after alter', 'A', 'batch', 'INSERT #s(n, tens, extra) VALUES (9, 90, 1); SELECT n, extra FROM #s ORDER BY n'],
  ['catalog through tempdb', 'A', 'batch', "SELECT name FROM tempdb.sys.columns WHERE object_id = OBJECT_ID('tempdb..#s') ORDER BY column_id"],
  ['duplicate create', 'A', 'batch', 'CREATE TABLE #foo(id int)'],
  ['duplicate create in one batch', 'A', 'batch', 'SELECT 1 AS first; CREATE TABLE #d(a int); CREATE TABLE #d(a int)'],
  ['missing local', 'A', 'batch', 'SELECT id FROM #missing'],
  ['missing local in DML', 'A', 'batch', 'INSERT INTO #missing VALUES (1)'],
  ['drop missing local', 'A', 'batch', 'DROP TABLE #missing'],
  ['drop if exists', 'A', 'batch', "DROP TABLE IF EXISTS #missing; DROP TABLE IF EXISTS #s; SELECT OBJECT_ID('tempdb..#s') AS object_id"],
  ['conditional drop', 'A', 'batch', "CREATE TABLE #cond(id int); IF OBJECT_ID('tempdb..#cond') IS NOT NULL DROP TABLE #cond; SELECT OBJECT_ID('tempdb..#cond') AS object_id"],
  ['rpc reads session local', 'A', 'rpc', 'SELECT id FROM #foo ORDER BY id'],
  ['rpc creates scoped local', 'A', 'rpc', 'CREATE TABLE #r(id int); INSERT #r VALUES (20); SELECT id FROM #r'],
  ['rpc local gone afterward', 'A', 'batch', "SELECT OBJECT_ID('tempdb..#r') AS object_id; SELECT id FROM #r"],
  ['transaction create rollback', 'A', 'batch', "BEGIN TRAN; CREATE TABLE #x(id int); INSERT #x VALUES (1); ROLLBACK; SELECT OBJECT_ID('tempdb..#x') AS object_id"],
  ['transaction insert rollback', 'A', 'batch', 'BEGIN TRAN; INSERT #foo VALUES (9); ROLLBACK; SELECT id FROM #foo ORDER BY id'],
  ['transaction drop rollback', 'A', 'batch', 'BEGIN TRAN; DROP TABLE #foo; ROLLBACK; SELECT COUNT(*) AS remaining FROM #foo'],
  ['global create', 'A', 'batch', 'CREATE TABLE ##msduck_gt(id int NOT NULL); INSERT ##msduck_gt VALUES (40)'],
  ['global shared with other session', 'B', 'batch', 'INSERT ##msduck_gt VALUES (41); SELECT id FROM ##msduck_gt ORDER BY id'],
  ['global duplicate', 'B', 'batch', 'CREATE TABLE ##msduck_gt(id int)'],
  ['global sees other session write', 'A', 'batch', "SELECT id FROM ##msduck_gt ORDER BY id; SELECT CASE WHEN OBJECT_ID('tempdb..##msduck_gt') IS NULL THEN 0 ELSE 1 END AS found"],
  ['table variable repro', 'A', 'batch', 'DECLARE @foo TABLE(id int); INSERT @foo VALUES (1),(2); SELECT id FROM @foo ORDER BY id'],
  ['table variable isolated between sessions', 'B', 'batch', 'DECLARE @foo TABLE(id int); INSERT @foo VALUES (99); SELECT id FROM @foo'],
  ['table variable batch scope', 'A', 'batch', 'SELECT id FROM @foo'],
  ['table variable constraints', 'A', 'batch', "DECLARE @t TABLE(id int IDENTITY(1,1) PRIMARY KEY, code varchar(10) NOT NULL UNIQUE, qty int CHECK (qty > 0) DEFAULT 1); INSERT @t(code) VALUES (N'a'); INSERT @t(code, qty) VALUES (N'b', 5); BEGIN TRY INSERT @t(code) VALUES (N'a') END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error END CATCH; BEGIN TRY INSERT @t(code, qty) VALUES (N'c', 0) END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error END CATCH; BEGIN TRY INSERT @t(code) VALUES (NULL) END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error END CATCH; UPDATE @t SET qty = qty * 10 WHERE code = N'b'; DELETE FROM @t WHERE code = N'a'; SELECT id, code, qty FROM @t ORDER BY id"],
  ['table variable joins and output into', 'A', 'batch', "CREATE TABLE #src(id int, name nvarchar(10)); INSERT #src VALUES (1, N'one'), (2, N'two'); DECLARE @ids TABLE(id int PRIMARY KEY); INSERT #src OUTPUT inserted.id INTO @ids VALUES (3, N'three'); INSERT @ids VALUES (1); SELECT s.id, s.name FROM #src AS s JOIN @ids AS i ON i.id = s.id ORDER BY s.id; UPDATE i SET id = id + 10 FROM @ids AS i WHERE i.id = 1; SELECT id FROM @ids ORDER BY id"],
  ['table variable output clauses', 'A', 'batch', 'DECLARE @t TABLE(id int, v int); INSERT @t OUTPUT inserted.id, inserted.v VALUES (1, 10), (2, 20); UPDATE @t SET v = v + 1 OUTPUT deleted.v, inserted.v WHERE id = 1; DELETE @t OUTPUT deleted.id WHERE id = 2; SELECT id, v FROM @t'],
  ['table variable survives rollback', 'A', 'batch', 'DECLARE @t TABLE(id int IDENTITY, v int); CREATE TABLE #ctl(v int); BEGIN TRAN; INSERT @t(v) VALUES (1); INSERT #ctl VALUES (1); ROLLBACK; INSERT @t(v) VALUES (2); SELECT id, v FROM @t ORDER BY id; SELECT COUNT(*) AS control_rows FROM #ctl'],
  ['table variable declared in rolled back transaction', 'A', 'batch', 'BEGIN TRAN; DECLARE @t TABLE(id int IDENTITY, v int); INSERT @t(v) VALUES (5); ROLLBACK; INSERT @t(v) VALUES (6); SELECT id, v FROM @t ORDER BY id'],
  ['table variable in loop keeps rows', 'A', 'batch', 'DECLARE @i int = 0; WHILE @i < 3 BEGIN DECLARE @t TABLE(id int); INSERT @t VALUES (@i); SET @i += 1 END; SELECT id FROM @t ORDER BY id'],
  ['table variable declared in skipped branch', 'A', 'batch', 'IF 1 = 0 BEGIN DECLARE @t TABLE(id int) END; INSERT @t VALUES (7); SELECT id FROM @t'],
  ['table variable in condition', 'A', 'batch', "DECLARE @t TABLE(id int); INSERT @t VALUES (5); IF EXISTS (SELECT * FROM @t WHERE id = 5) SELECT N'found' AS result; DECLARE @n int = (SELECT COUNT(*) FROM @t); SELECT @n AS n"],
  ['duplicate table variable', 'A', 'batch', 'DECLARE @t TABLE(id int); DECLARE @t TABLE(id int)'],
  ['truncate table variable', 'A', 'batch', 'DECLARE @t TABLE(id int); TRUNCATE TABLE @t'],
  ['rpc table variable', 'A', 'rpc', 'DECLARE @t TABLE(id int); INSERT @t VALUES (12); SELECT id FROM @t'],
  ['local create before disconnect', 'A', 'batch', 'CREATE TABLE #gone(id int); INSERT #gone VALUES (50)'],
  ['disconnect', 'A', 'close', ''],
  ['local gone after reconnect', 'C', 'batch', "SELECT OBJECT_ID('tempdb..#gone') AS object_id; SELECT id FROM #gone"],
  ['global gone with creator', 'B', 'batch', "SELECT OBJECT_ID('tempdb..##msduck_gt') AS object_id; SELECT id FROM ##msduck_gt"],
]

async function close(connection) {
  if (!connection || connection.closed) return
  await new Promise(resolve => { connection.once('end', resolve); connection.close() })
}

// Run the probes over three connections to one database; `open(database)`
// connects. Returns [{ name, session, mode, sql, result }].
export async function observe(first, open) {
  const database = canonical(await capture(first, 'SELECT DB_NAME() AS database_name')).sets[0].rows[0][0]
  const sessions = { A: first, B: await open(database) }
  const run = []
  try {
    for (const [name, session, mode, sql] of probes) {
      if (mode === 'close') {
        await close(sessions[session])
        // A disconnect is processed asynchronously on the server.
        await new Promise(resolve => setTimeout(resolve, 500))
        sessions.C = await open(database)
        continue
      }
      const connection = sessions[session]
      const transport = mode === 'rpc' ? {
        on: (...rest) => connection.on(...rest),
        off: (...rest) => connection.off(...rest),
        execSqlBatch: request => connection.execSql(request),
      } : connection
      const result = canonical(await capture(transport, sql))
      // SQL Server generates constraint names per database.
      for (const error of result.errors) error.message = error.message.replace(/\b(PK|UQ|CK|DF)__[^'"]*/g, '$1__<generated>')
      run.push({ name, session, mode, sql, result })
    }
    return run
  } finally {
    await close(sessions.B)
    await close(sessions.C)
  }
}

function validate(run) {
  assert.equal(run.length, probes.length - 1, 'complete probe set')
  const result = name => run.find(entry => entry.name === name).result
  assertSameCapture(result('local create and rows').sets[0].rows, [[1], [2]], 'local rows')
  assert.equal(result('local private to other session').errors[0].number, 208)
  assertSameCapture(result('same local name in other session').sets[0].rows, [[3]], 'separate local')
  assertSameCapture(result('rpc local gone afterward').sets[0].rows, [[null]], 'RPC-created local dropped')
  assertSameCapture(result('table variable survives rollback').sets[0].rows, [[1, 1], [2, 2]], 'table variable rows survive rollback')
  assert.equal(result('table variable batch scope').errors[0].number, 1087)
  assert.equal(result('global gone with creator').errors[0].number, 208)
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const writeFixture = process.argv.includes('--write-fixture')
  if (writeFixture) await refuseExistingFixture(fixture)
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  const actual = await withReferenceContainer(async (config, container) => {
    const run = await isolatedReference(config, primary =>
      observe(primary, database => connect({ ...config, options: { ...config.options, database } })))
    validate(run)
    const version = canonical(await (async () => {
      const connection = await connect(config)
      try { return await capture(connection, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)) AS version") }
      finally { await close(connection) }
    })()).sets[0].rows[0][0]
    return { image: container.image, version, results: run }
  })
  if (retained) assertSameCapture(actual.results, retained.results, 'fresh capture differs from the retained fixture')
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${actual.results.length} observations from SQL Server ${actual.version}${retained ? '; matched the retained fixture' : ''}`)
}
