// Run on the Docker-capable reference host. --check validates the retained
// fixture against this case list without starting SQL Server.
import { Request, TYPES } from 'tedious'
import { readFile } from 'node:fs/promises'
import { isDeepStrictEqual } from 'node:util'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, command, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const output = process.argv[3] ?? (process.argv[2] === '--check' ? 'reference/at-time-zone.json' : process.argv[2] ?? 'reference/at-time-zone.json')
const zone = 'Central European Standard Time'
const pacific = 'Pacific Standard Time'
const show = (expression, suffix = '') => `SELECT value,CONVERT(VARCHAR(40),value) AS rendered,DATEPART(TZOFFSET,value) AS offset_minutes FROM (SELECT ${expression} AS value) v ${suffix}`
const at = (date, type, target) => show(`CAST('${date}' AS ${type}) AT TIME ZONE '${target}'`)
const cases = [
  ['datetime2 UTC', at('2024-01-02T03:04:05.1234567', 'DATETIME2(7)', 'UTC')],
  ['datetime UTC', at('2024-01-02T03:04:05.123', 'DATETIME', 'UTC')],
  ['smalldatetime UTC', at('2024-01-02T03:04:00', 'SMALLDATETIME', 'UTC')],
  ['datetime2 named zone', at('2024-01-02T03:04:05.1234567', 'DATETIME2(7)', zone)],
  ['datetimeoffset changes zone', at('2024-01-02T03:04:05+02:00', 'DATETIMEOFFSET(7)', 'UTC')],
  ['chained zone conversion', show(`CAST('2024-01-02T03:04:05' AS DATETIME2(7)) AT TIME ZONE '${pacific}' AT TIME ZONE '${zone}'`)],
  ['datetimeoffset same zone', at('2024-01-02T03:04:05+02:00', 'DATETIMEOFFSET(7)', zone)],
  ...['2022-03-27T01:59:59', '2022-03-27T02:00:00', '2022-03-27T02:30:00', '2022-03-27T02:59:59', '2022-03-27T03:00:00'].map(date => [`Central Europe spring ${date}`, at(date, 'DATETIME2(7)', zone)]),
  ...['2022-10-30T01:59:59', '2022-10-30T02:00:00', '2022-10-30T02:30:00', '2022-10-30T02:59:59', '2022-10-30T03:00:00'].map(date => [`Central Europe autumn ${date}`, at(date, 'DATETIME2(7)', zone)]),
  ['Pacific spring gap', at('2024-03-10T02:30:00', 'DATETIME2(7)', pacific)],
  ['Pacific autumn overlap', at('2024-11-03T01:30:00', 'DATETIME2(7)', pacific)],
  ['typed NULL input', show("CAST(NULL AS DATETIME2(7)) AT TIME ZONE 'UTC'")],
  ['NULL zone', show("CAST('2024-01-02' AS DATETIME2(7)) AT TIME ZONE CAST(NULL AS NVARCHAR(128))")],
  ['invalid zone', "SELECT CAST('2024-01-02' AS DATETIME2(7)) AT TIME ZONE 'Not A Time Zone' AS value"],
  ['unsupported date input', "SELECT CAST('2024-01-02' AS DATE) AT TIME ZONE 'UTC' AS value"],
  ['unsupported integer input', "SELECT CAST(1 AS INT) AT TIME ZONE 'UTC' AS value"],
  ['empty result metadata', show("CAST('2024-01-02' AS DATETIME2(7)) AT TIME ZONE 'UTC'", 'WHERE 1=0')],
]
const preparedQuery = show('@stamp AT TIME ZONE @zone')
const preparedValues = [
  { stamp: '2024-01-02T03:04:05.000Z', zone: 'UTC' },
  { stamp: '2022-03-27T02:30:00.000Z', zone },
  { stamp: '2022-10-30T02:30:00.000Z', zone },
  { stamp: null, zone: 'UTC' },
  { stamp: '2024-11-03T01:30:00.000Z', zone: pacific },
]

function preparedCapture(connection) {
  return new Promise((resolve, reject) => {
    const runs = []
    let result
    let complete = () => {}
    const request = new Request(preparedQuery, (error, rowCount) => complete(error, rowCount))
    request.addParameter('stamp', TYPES.DateTime2, undefined, { scale: 7 })
    request.addParameter('zone', TYPES.NVarChar, undefined, { length: 128 })
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
        for (const parameters of preparedValues) {
          result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
          await new Promise((done, fail) => {
            complete = (error, rowCount) => { result.rowCount = rowCount; error ? fail(error) : done() }
            request.error = undefined
            connection.execute(request, { stamp: parameters.stamp === null ? null : new Date(parameters.stamp), zone: parameters.zone })
          })
          runs.push({ parameters, reference: canonical(result) })
        }
        result = undefined
        await new Promise((done, fail) => {
          complete = error => error ? fail(error) : done()
          connection.unprepare(request)
        })
        resolve(runs)
      } catch (error) { reject(error) }
      finally {
        connection.off('errorMessage', onError)
        connection.off('infoMessage', onInfo)
      }
    })()
  })
}

if (process.argv[2] === '--check') {
  const fixture = JSON.parse(await readFile(output, 'utf8'))
  if (fixture.image !== referenceImage || !fixture.version?.sets?.length) throw new Error('reference image or version is missing')
  if (!isDeepStrictEqual(fixture.results.map(({ name, query }) => [name, query]), cases)) throw new Error('case list differs from retained fixture')
  if (fixture.results.some(({ reference }) => !Array.isArray(reference?.sets) || !Array.isArray(reference?.done) || !Array.isArray(reference?.errors) || !Array.isArray(reference?.info))) throw new Error('incomplete batch capture')
  if (fixture.prepared.query !== preparedQuery || !isDeepStrictEqual(fixture.prepared.runs.map(({ parameters }) => parameters), preparedValues)) throw new Error('prepared bindings differ from retained fixture')
  if (fixture.prepared.runs.some(({ reference }) => !Array.isArray(reference?.sets) || !Array.isArray(reference?.done) || !Array.isArray(reference?.errors) || !Array.isArray(reference?.info))) throw new Error('incomplete prepared capture')
  console.log(`checked ${fixture.results.length} batch cases and ${fixture.prepared.runs.length} prepared executions`)
} else {
  await withReferenceContainer(async (config, container) => {
    const connection = await connect(config)
    try {
      const version = canonical(await command(connection, 'SELECT @@VERSION AS version'))
      const results = []
      for (const [name, query] of cases) {
        results.push({ name, query, reference: canonical(await capture(connection, query)) })
        console.log(name)
      }
      const prepared = await preparedCapture(connection)
      await writeNewFixture(output, { image: container.image, version, results, prepared: { query: preparedQuery, protocol: 'tedious.prepare/execute/unprepare; same handle reused', runs: prepared } })
    } finally { connection.close() }
  })
}
