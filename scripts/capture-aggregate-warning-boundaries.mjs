// First-party SQL Server execution-boundary evidence. Errors are observations.
import assert from 'node:assert/strict'
import { writeFile } from 'node:fs/promises'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, command } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

async function orderedCapture(connection, sql) {
  const events = []
  const info = message => events.push({ kind: 'info', number: message.number })
  const error = message => events.push({ kind: 'error', number: message.number })
  connection.on('infoMessage', info)
  connection.on('errorMessage', error)
  // Adapt the capture helper's connection interface without mutating a shared
  // connection method or evaluating the SQL twice.
  const observed = {
    on: connection.on.bind(connection),
    off: connection.off.bind(connection),
    execSqlBatch(request) {
      request.on('columnMetadata', () => events.push({ kind: 'metadata' }))
      request.on('row', () => events.push({ kind: 'row' }))
      for (const kind of ['done', 'doneInProc', 'doneProc']) {
        request.on(kind, (count, more) => events.push({ kind, rowCount: count ?? null, more }))
      }
      connection.execSqlBatch(request)
    },
  }
  try { return canonical({ ...await capture(observed, sql), events }) }
  finally {
    connection.off('infoMessage', info)
    connection.off('errorMessage', error)
  }
}

const cases = [
  { id: 'stored-view', setup: ['CREATE VIEW dbo.warning_view AS SELECT MIN(v) AS result FROM dbo.warning_source'], sql: 'SELECT result FROM dbo.warning_view' },
  { id: 'stored-view-unused', setup: ['CREATE VIEW dbo.warning_view AS SELECT MIN(v) AS result FROM dbo.warning_source'], sql: 'SELECT result FROM dbo.warning_view WHERE 1=0' },
  { id: 'stored-view-twice', setup: ['CREATE VIEW dbo.warning_view AS SELECT MIN(v) AS result FROM dbo.warning_source'], sql: 'SELECT result FROM dbo.warning_view; SELECT result FROM dbo.warning_view' },
  { id: 'insert-select', sql: 'INSERT INTO dbo.warning_sink SELECT MIN(v) FROM dbo.warning_source' },
  { id: 'insert-empty-source', sql: 'INSERT INTO dbo.warning_sink SELECT MIN(v) FROM dbo.warning_source WHERE 1=0' },
  { id: 'insert-grouped', sql: 'INSERT INTO dbo.warning_sink SELECT MIN(v) FROM dbo.warning_source GROUP BY g' },
  { id: 'insert-check-failure', setup: ['ALTER TABLE dbo.warning_sink ADD CONSTRAINT warning_check CHECK(v<0)'], sql: 'INSERT INTO dbo.warning_sink SELECT MIN(v) FROM dbo.warning_source' },
  { id: 'update-scalar', setup: ['INSERT INTO dbo.warning_sink VALUES(9)'], sql: 'UPDATE dbo.warning_sink SET v=(SELECT MAX(v) FROM dbo.warning_source)' },
  { id: 'update-no-targets', sql: 'UPDATE dbo.warning_sink SET v=(SELECT MAX(v) FROM dbo.warning_source)' },
  { id: 'delete-subquery', setup: ['INSERT INTO dbo.warning_sink VALUES(1),(9)'], sql: 'DELETE FROM dbo.warning_sink WHERE v IN(SELECT MIN(v) FROM dbo.warning_source)' },
  { id: 'select-into', sql: 'SELECT MIN(v) AS v INTO dbo.warning_into FROM dbo.warning_source', followup: 'SELECT v FROM dbo.warning_into' },
  { id: 'select-assignment', sql: 'DECLARE @v INT; SELECT @v=MIN(v) FROM dbo.warning_source; SELECT @v AS result' },
  { id: 'set-subquery', sql: 'DECLARE @v INT; SET @v=(SELECT MIN(v) FROM dbo.warning_source); SELECT @v AS result' },
  { id: 'declare-subquery', sql: 'DECLARE @v INT=(SELECT MIN(v) FROM dbo.warning_source); SELECT @v AS result' },
  { id: 'window-null-never-consumed', sql: 'SELECT id,MIN(v) OVER(ORDER BY id ROWS BETWEEN 1 FOLLOWING AND 1 FOLLOWING) AS result FROM (VALUES(1,CAST(NULL AS INT)),(2,2)) d(id,v) ORDER BY id' },
  { id: 'window-all-frames-empty', sql: 'SELECT MIN(v) OVER(ORDER BY id ROWS BETWEEN 1 FOLLOWING AND 1 FOLLOWING) AS result FROM (VALUES(1,CAST(NULL AS INT))) d(id,v)' },
  { id: 'window-null-consumed', sql: 'SELECT id,MIN(v) OVER(ORDER BY id ROWS BETWEEN 1 PRECEDING AND 1 PRECEDING) AS result FROM (VALUES(1,CAST(NULL AS INT)),(2,2)) d(id,v) ORDER BY id' },
  { id: 'window-following-count', sql: 'SELECT id,COUNT(v) OVER(ORDER BY id ROWS BETWEEN 1 FOLLOWING AND 1 FOLLOWING) AS result FROM (VALUES(1,CAST(NULL AS INT)),(2,2)) d(id,v) ORDER BY id' },
  { id: 'window-lag', sql: 'SELECT id,LAG(v) OVER(ORDER BY id) AS result FROM (VALUES(1,CAST(NULL AS INT)),(2,2)) d(id,v) ORDER BY id' },
  { id: 'conversion-failure', sql: "SELECT MIN(CONVERT(INT,v)) AS result FROM (VALUES('1'),(NULL),('bad')) d(v)" },
  { id: 'sum-overflow', sql: 'SELECT SUM(v) AS result FROM (VALUES(2147483647),(1),(NULL)) d(v)' },
  { id: 'divide-by-zero', sql: 'SELECT SUM(10/v) AS result FROM (VALUES(1),(NULL),(0)) d(v)' },
  { id: 'catch-conversion', sql: "BEGIN TRY SELECT MIN(CONVERT(INT,v)) AS result FROM (VALUES('1'),(NULL),('bad')) d(v); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS caught; END CATCH" },
  { id: 'warning-then-error', sql: "SELECT MIN(v) AS result FROM dbo.warning_source; SELECT CONVERT(INT,'bad') AS failed" },
  { id: 'unused-scalar-subquery', sql: 'SELECT (SELECT MIN(v) FROM dbo.warning_source) AS result WHERE 1=0' },
]
const reset = [
  'DROP VIEW IF EXISTS dbo.warning_view',
  'DROP TABLE IF EXISTS dbo.warning_into; DROP TABLE IF EXISTS dbo.warning_sink; DROP TABLE IF EXISTS dbo.warning_source',
  'CREATE TABLE dbo.warning_source(id INT,g INT,v INT); INSERT INTO dbo.warning_source VALUES(1,1,1),(2,1,NULL),(3,2,2),(4,2,NULL)',
  'CREATE TABLE dbo.warning_sink(v INT)',
]
await withReferenceContainer(async (config, container) => {
  const connection = await connect(config)
  try {
    const version = canonical(await capture(connection, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version"))
    const sessionDefaults = canonical(await capture(connection, "SELECT @@OPTIONS AS options,SESSIONPROPERTY('ANSI_WARNINGS') AS ansi_warnings,SESSIONPROPERTY('ARITHABORT') AS arithabort"))
    const results = []
    for (const mode of ['ON', 'OFF']) for (const sample of cases) {
      for (const sql of [...reset, ...(sample.setup ?? []), `SET ANSI_WARNINGS ${mode}`]) await command(connection, sql)
      const executions = []
      for (let repeat = 0; repeat < (sample.id === 'stored-view' ? 2 : 1); repeat++) {
        const result = await orderedCapture(connection, sample.sql)
        const state = await orderedCapture(connection, 'SELECT @@ERROR AS last_error,@@ROWCOUNT AS last_rowcount,@@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state')
        const contents = await orderedCapture(connection, sample.followup ?? 'SELECT v FROM dbo.warning_sink ORDER BY v')
        executions.push({ result, state, contents })
      }
      results.push({ id: `${mode}-${sample.id}`, mode, setup: [...reset, ...(sample.setup ?? [])], sql: sample.sql, followup: sample.followup ?? 'SELECT v FROM dbo.warning_sink ORDER BY v', executions })
    }
    assert.equal(results.length, 50)
    await writeFile(process.argv[2] ?? 'reference/aggregate-warning-boundaries.json', JSON.stringify({ image: container.image, version, sessionDefaults, results }, null, 2) + '\n')
    console.log(`Captured ${results.length} aggregate execution boundaries`)
  } finally { connection.close() }
})
