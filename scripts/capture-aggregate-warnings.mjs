// SQL Server's NULL-elimination diagnostics are execution results, not AST flags.
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

const cases = []
const add = (id, sql) => cases.push({ id, sql })
for (const aggregate of ['MIN', 'MAX', 'SUM', 'AVG', 'COUNT', 'COUNT_BIG', 'STDEV', 'VAR']) {
  for (const [shape, source, filter] of [
    ['mixed', '(VALUES(1),(NULL),(2))', ''],
    ['all-null', '(VALUES(CAST(NULL AS INT)),(NULL))', ''],
    ['non-null', '(VALUES(1),(2))', ''],
    ['empty', '(VALUES(1),(NULL))', 'WHERE 1=0'],
    ['filtered', '(VALUES(1),(NULL),(2))', 'WHERE v IS NOT NULL'],
  ]) add(`${aggregate}-${shape}`, `SELECT ${aggregate}(v) AS result FROM ${source} d(v) ${filter}`)
}
for (const [id, sql] of [
  ['count-star', 'SELECT COUNT(*) AS result FROM (VALUES(1),(NULL)) d(v)'],
  ['count-constant', 'SELECT COUNT(1) AS result FROM (VALUES(1),(NULL)) d(v)'],
  ['count-distinct', 'SELECT COUNT(DISTINCT v) AS result FROM (VALUES(1),(NULL),(1)) d(v)'],
  ['sum-distinct', 'SELECT SUM(DISTINCT v) AS result FROM (VALUES(1),(NULL),(1)) d(v)'],
  ['min-distinct', 'SELECT MIN(DISTINCT v) AS result FROM (VALUES(1),(NULL),(1)) d(v)'],
  ['multiple', 'SELECT MIN(v) AS lo,MAX(v) AS hi,SUM(v) AS total,COUNT(v) AS n FROM (VALUES(1),(NULL),(2)) d(v)'],
  ['multiple-columns', 'SELECT MIN(a) AS lo,MAX(b) AS hi FROM (VALUES(1,NULL),(NULL,2)) d(a,b)'],
  ['two-statements', 'SELECT MIN(v) AS lo FROM (VALUES(1),(NULL)) d(v); SELECT MAX(v) AS hi FROM (VALUES(2),(NULL)) d(v)'],
  ['grouped', 'SELECT g,MIN(v) AS lo,MAX(v) AS hi FROM (VALUES(1,1),(1,NULL),(2,NULL),(2,NULL),(3,2)) d(g,v) GROUP BY g ORDER BY g'],
  ['having-no-results', 'SELECT MIN(v) AS lo FROM (VALUES(1),(NULL)) d(v) HAVING COUNT(*)>10'],
  ['top-zero', 'SELECT TOP(0) MIN(v) AS lo FROM (VALUES(1),(NULL)) d(v)'],
  ['window', 'SELECT id,MIN(v) OVER(ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS lo FROM (VALUES(1,1),(2,NULL),(3,2),(4,NULL)) d(id,v) ORDER BY id'],
  ['window-null-only', 'SELECT id,MAX(v) OVER(PARTITION BY g) AS hi FROM (VALUES(1,1,CAST(NULL AS INT)),(2,1,NULL)) d(id,g,v) ORDER BY id'],
  ['window-empty-frame', 'SELECT id,SUM(v) OVER(ORDER BY id ROWS BETWEEN 1 FOLLOWING AND 1 FOLLOWING) AS total FROM (VALUES(1,1),(2,NULL)) d(id,v) ORDER BY id'],
  ['window-count', 'SELECT id,COUNT(v) OVER() AS n FROM (VALUES(1,1),(2,NULL)) d(id,v) ORDER BY id'],
  ['coalesce-input', 'SELECT SUM(COALESCE(v,0)) AS total FROM (VALUES(1),(NULL)) d(v)'],
  ['case-input', 'SELECT SUM(CASE WHEN v=1 THEN v END) AS total FROM (VALUES(1),(2)) d(v)'],
  ['correlated', 'SELECT g,(SELECT MIN(v) FROM (VALUES(1,1),(1,NULL),(2,NULL)) d(k,v) WHERE k=q.g) AS lo FROM (VALUES(1),(2),(3)) q(g) ORDER BY g'],
  ['union', 'SELECT v FROM (VALUES(1),(NULL),(NULL)) d(v) UNION SELECT NULL ORDER BY v'],
  ['distinct-rows', 'SELECT DISTINCT v FROM (VALUES(1),(NULL),(NULL)) d(v) ORDER BY v'],
  ['character-bin2', "SELECT MIN(v COLLATE Latin1_General_100_BIN2) AS lo,MAX(v COLLATE Latin1_General_100_BIN2) AS hi FROM (VALUES(N'a'),(NULL),(N'b')) d(v)"],
  ['character-default', "SELECT MIN(v) AS lo,MAX(v) AS hi FROM (VALUES(N'a'),(NULL),(N'b')) d(v)"],
  ['catch', 'BEGIN TRY SELECT MAX(v) AS hi FROM (VALUES(1),(NULL)) d(v); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS caught; END CATCH'],
]) add(id, sql)

await withReferenceContainer(async (config, container) => {
  const connection = await connect(config)
  try {
    const version = canonical(await capture(connection, 'SELECT CAST(SERVERPROPERTY(\'ProductVersion\') AS NVARCHAR(128)) AS version'))
    const results = []
    for (const mode of ['ON', 'OFF']) for (const sample of cases) {
      await command(connection, `SET ANSI_WARNINGS ${mode}`)
      const sql = `${sample.sql}; SELECT @@ERROR AS last_error,@@ROWCOUNT AS last_rowcount`
      const result = await orderedCapture(connection, sql)
      assert.deepEqual(result.errors, [], `${mode} ${sample.id}`)
      results.push({ id: `${mode}-${sample.id}`, mode, sql, result })
    }
    await writeFile(process.argv[2] ?? 'reference/aggregate-warnings.json', JSON.stringify({ image: container.image, version, results }, null, 2) + '\n')
    console.log(`Captured ${results.length} aggregate warning programs`)
  } finally { connection.close() }
})
