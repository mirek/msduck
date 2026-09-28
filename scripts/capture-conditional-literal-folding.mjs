// Run with Node 24 on the Docker-capable reference host. Use --replay after a
// workspace build to compare the local server with the retained raw capture.
// Keep every descriptor, row, error and completion event.
import { Request, TYPES } from 'tedious'
import { readFile } from 'node:fs/promises'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, command, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical, differences } from './lib/compatibility.mjs'
import { start } from '../tests/support/client.mjs'

const replay = process.argv[2] === '--replay'
const output = (replay ? process.argv[3] : process.argv[2]) ?? 'reference/conditional-literal-folding.json'
const cases = [
  ['CASE true Unicode', "SELECT CASE WHEN 1=1 THEN N'a' ELSE N'longer' END AS value"],
  ['CASE false Unicode', "SELECT CASE WHEN 1=0 THEN N'a' ELSE N'longer' END AS value"],
  ['CASE no ELSE Unicode', "SELECT CASE WHEN 1=0 THEN N'a' END AS value"],
  ['CASE runtime predicate', "SELECT id,CASE WHEN id=1 THEN N'a' ELSE N'longer' END AS value FROM (VALUES(1),(2)) v(id) ORDER BY id"],
  ['CASE empty input', "SELECT CASE WHEN id=1 THEN N'a' ELSE N'longer' END AS value FROM (VALUES(1)) v(id) WHERE 1=0"],
  ['CASE ANSI', "SELECT CASE WHEN 1=1 THEN 'a' ELSE 'longer' END AS value"],
  ['mixed family selected ANSI', "SELECT CASE WHEN 1=1 THEN 'a' ELSE N'longer' END AS case_result,IIF(1=1,'a',N'longer') AS iif_result,COALESCE('a',N'longer') AS coalesce_result"],
  ['mixed family selected Unicode', "SELECT CASE WHEN 1=1 THEN N'a' ELSE 'longer' END AS case_result,IIF(1=1,N'a','longer') AS iif_result,COALESCE(N'a','longer') AS coalesce_result"],
  ['mixed family three branches', "SELECT CASE WHEN 1=1 THEN 'a' WHEN 1=0 THEN 'verylong' ELSE N'x' END AS case_result,COALESCE('a','verylong',N'x') AS coalesce_result"],
  ['mixed family typed NULL', "SELECT CASE WHEN 1=1 THEN 'a' ELSE CAST(NULL AS NVARCHAR(12)) END AS case_result,IIF(1=1,'a',CAST(NULL AS NVARCHAR(12))) AS iif_result,COALESCE('a',CAST(NULL AS NVARCHAR(12))) AS coalesce_result"],
  ['CASE typed NULL', "SELECT CASE WHEN 1=1 THEN CAST(NULL AS NVARCHAR(12)) ELSE N'🦆' END AS value"],
  ['CASE MAX branch', "SELECT CASE WHEN 1=1 THEN N'a' ELSE CAST(N'longer' AS NVARCHAR(MAX)) END AS value"],
  ['IIF true and false', "SELECT IIF(1=1,N'a',N'longer') AS yes,IIF(1=0,N'a',N'longer') AS no"],
  ['IIF selected untyped NULL', "SELECT IIF(1=1,NULL,N'x') AS unicode_result,IIF(1=1,NULL,'abc') AS ansi_result"],
  ['IIF runtime predicate', "SELECT id,IIF(id=1,N'a',N'longer') AS value FROM (VALUES(1),(2)) v(id) ORDER BY id"],
  ['IIF typed NULL and MAX', "SELECT IIF(1=1,CAST(NULL AS NVARCHAR(12)),N'🦆') AS bounded,IIF(1=1,N'a',CAST(N'longer' AS NVARCHAR(MAX))) AS unbounded"],
  ['COALESCE first nonnull', "SELECT COALESCE(N'a',N'longer') AS short,COALESCE(NULL,N'longer') AS long,COALESCE(N'',N'🦆') AS empty"],
  ['COALESCE typed NULL', "SELECT COALESCE(CAST(NULL AS NVARCHAR(12)),N'🦆') AS bounded,COALESCE(CAST(NULL AS NVARCHAR(MAX)),N'a') AS unbounded"],
  ['COALESCE runtime column', "SELECT id,COALESCE(CAST(v AS NVARCHAR(12)),N'🦆') AS value FROM (VALUES(1,N'a'),(2,NULL)) s(id,v) ORDER BY id"],
  ['COALESCE ANSI', "SELECT COALESCE('a','longer') AS short,COALESCE('','longer') AS empty"],
  ['surrogate and empty branches', "SELECT CASE WHEN 1=1 THEN N'🦆' ELSE N'longer' END AS bird,IIF(1=1,N'',N'🦆') AS empty,COALESCE(N'🦆',N'a') AS pair"],
  ['set after folded CASE', "SELECT CASE WHEN 1=1 THEN N'a' ELSE N'longer' END AS value UNION ALL SELECT N'xx'"],
  ['set after runtime CASE', "SELECT CASE WHEN id=1 THEN N'a' ELSE N'longer' END AS value FROM (VALUES(1)) v(id) UNION ALL SELECT N'xx'"],
]

function preparedCapture(connection, query, values) {
  return new Promise((resolve, reject) => {
    const results = []
    let result
    let complete = () => {}
    const request = new Request(query, (error, rowCount) => complete(error, rowCount))
    request.addParameter('flag', TYPES.Bit, undefined)
    request.addParameter('text', TYPES.NVarChar, undefined, { length: 12 })
    const fields = e => ({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
    const onError = e => result?.errors.push(fields(e))
    const onInfo = e => result?.info.push(fields(e))
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(x => ({ name: x.colName, type: x.type.name, length: x.dataLength ?? null, precision: x.precision ?? null, scale: x.scale ?? null, flags: x.flags, collation: canonical(x.collation ?? null) })), rows: [] }))
    request.on('row', row => result?.sets.at(-1).rows.push(row.map(x => x.value)))
    for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => result?.done.push({ kind, rowCount: rowCount ?? null, more }))
    request.on('doneProc', (_count, _more, status) => { if (result) result.returnStatus = status })
    ;(async () => {
      try {
        await new Promise((done, fail) => {
          complete = error => error ? fail(error) : done()
          request.once('prepared', done)
          request.once('error', fail)
          connection.prepare(request)
        })
        for (const parameters of values) {
          result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
          await new Promise((done, fail) => {
            complete = (error, rowCount) => { result.rowCount = rowCount; error ? fail(error) : done() }
            request.error = undefined
            connection.execute(request, parameters)
          })
          results.push({ parameters, reference: canonical(result) })
        }
        result = undefined
        await new Promise((done, fail) => {
          complete = error => error ? fail(error) : done()
          connection.unprepare(request)
        })
        resolve(results)
      } catch (error) { reject(error) }
      finally {
        connection.off('errorMessage', onError)
        connection.off('infoMessage', onInfo)
      }
    })()
  })
}

if (replay) {
  const fixture = JSON.parse(await readFile(output, 'utf8'))
  const close = []
  const connection = await start({ after: fn => close.push(fn) })
  try {
    let mismatches = 0
    for (const test of fixture.results) {
      const delta = differences(canonical(await capture(connection, test.query)), test.reference)
      mismatches += delta.length
      console.log(delta.length ? JSON.stringify({ name: test.name, differences: delta }) : `matched: ${test.name}`)
    }
    const runs = await preparedCapture(connection, fixture.prepared.query, fixture.prepared.runs.map(run => run.parameters))
    for (const [index, run] of runs.entries()) {
      const delta = differences(run, fixture.prepared.runs[index])
      mismatches += delta.length
      console.log(delta.length ? JSON.stringify({ name: `prepared ${index}`, differences: delta }) : `matched: prepared ${index}`)
    }
    if (mismatches) process.exitCode = 1
  } finally { for (const fn of close.reverse()) fn() }
} else {
  await withReferenceContainer(async (config, container) => {
    const connection = await connect(config)
    try {
      const version = await command(connection, 'SELECT @@VERSION AS version')
      const results = []
      for (const [name, query] of cases) {
        results.push({ name, query, reference: canonical(await capture(connection, query)) })
        console.log(name)
      }
      const query = "SELECT CASE WHEN @flag=1 THEN N'a' ELSE N'longer' END AS conditional,IIF(@flag=1,N'a',N'longer') AS iif,COALESCE(@text,N'🦆') AS fallback"
      const prepared = await preparedCapture(connection, query, [
        { flag: true, text: 'a' },
        { flag: false, text: null },
        { flag: true, text: '🦆' },
      ])
      await writeNewFixture(output, { image: container.image, version, results, prepared: { query, protocol: 'tedious.prepare/execute/unprepare; same handle reused', runs: prepared } })
    } finally { connection.close() }
  })
}
