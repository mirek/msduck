#!/usr/bin/env node
// Retain SQL Server DATETRUNC and DATE_BUCKET rows, descriptors, diagnostics and completions.
import assert from 'node:assert/strict'
import { access, mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { isDeepStrictEqual } from 'node:util'
import { Request, TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'

// tedious derives a DATETIMEOFFSET parameter's offset from the host time zone.
// Pin it so bound datetimeoffset values do not depend on the capturing host.
process.env.TZ = 'UTC'

const fixture = new URL('../reference/datetrunc-bucket.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/datetrunc-bucket/capture.json')

// Every captured expression is projected twice: as the typed value (descriptor
// and tedious value) and as style 121 text, which retains offsets and all
// fractional digits that a JavaScript Date cannot represent.
const project = expr => `SELECT ${expr} AS value,CONVERT(VARCHAR(50),${expr},121) AS text`

const scales = [0, 1, 2, 3, 4, 5, 6, 7]
const types = [
  ['date', "CAST('2024-05-15' AS DATE)"],
  ...scales.map(s => [`time(${s})`, `CAST('13:47:39.1234567' AS TIME(${s}))`]),
  ...scales.map(s => [`datetime2(${s})`, `CAST('2024-05-15T13:47:39.1234567' AS DATETIME2(${s}))`]),
  ...scales.map(s => [`datetimeoffset(${s})`, `CAST('2024-05-15T13:47:39.1234567+05:30' AS DATETIMEOFFSET(${s}))`]),
  ['datetime', "CAST('2024-05-15T13:47:39.123' AS DATETIME)"],
  ['smalldatetime', "CAST('2024-05-15T13:47:29' AS SMALLDATETIME)"],
]
const truncParts = ['year', 'quarter', 'month', 'dayofyear', 'day', 'week', 'iso_week', 'hour', 'minute', 'second', 'millisecond', 'microsecond', 'weekday', 'nanosecond', 'tzoffset']
const bucketParts = ['year', 'quarter', 'month', 'week', 'day', 'hour', 'minute', 'second', 'millisecond', 'dayofyear', 'iso_week', 'microsecond', 'nanosecond']

const cases = []
for (const [type, expr] of types) {
  for (const part of truncParts) cases.push([`datetrunc ${part} ${type}`, project(`DATETRUNC(${part},${expr})`)])
}
for (const [type, expr] of types) {
  for (const part of bucketParts) cases.push([`date_bucket ${part} 2 ${type}`, project(`DATE_BUCKET(${part},2,${expr})`)])
}

const dt2 = "CAST('2024-05-15T13:47:39.1234567' AS DATETIME2(7))"
cases.push(
  // Datepart abbreviations and spellings.
  ...['yy', 'yyyy', 'qq', 'q', 'mm', 'm', 'dy', 'y', 'dd', 'd', 'wk', 'ww', 'isowk', 'isoww', 'hh', 'mi', 'n', 'ss', 's', 'ms', 'mcs', 'dw', 'w', 'ns', 'tz']
    .map(part => [`datetrunc abbreviation ${part}`, project(`DATETRUNC(${part},${dt2})`)]),
  ...['yy', 'qq', 'mm', 'wk', 'dd', 'd', 'hh', 'mi', 'n', 'ss', 's', 'ms', 'dy', 'isowk', 'mcs']
    .map(part => [`date_bucket abbreviation ${part}`, project(`DATE_BUCKET(${part},1,${dt2})`)]),
  ['datetrunc bracketed datepart', project(`DATETRUNC([day],${dt2})`)],
  ['datetrunc uppercase datepart', project(`DATETRUNC(DAY,${dt2})`)],
  ['datetrunc unknown datepart', project(`DATETRUNC(fortnight,${dt2})`)],
  ['datetrunc string datepart', project(`DATETRUNC('day',${dt2})`)],
  ['datetrunc variable datepart', `DECLARE @p VARCHAR(10)='day'; ${project(`DATETRUNC(@p,${dt2})`)}`],
  ['date_bucket unknown datepart', project(`DATE_BUCKET(fortnight,1,${dt2})`)],
  ['date_bucket string datepart', project(`DATE_BUCKET('day',1,${dt2})`)],

  // String literal and non-temporal inputs.
  ['datetrunc string date only', project("DATETRUNC(day,'2024-05-15')")],
  ['datetrunc string datetime', project("DATETRUNC(hour,'2024-05-15 13:47:39.1234567')")],
  ['datetrunc string microsecond', project("DATETRUNC(microsecond,'2024-05-15T13:47:39.1234567')")],
  ['datetrunc string time only', project("DATETRUNC(minute,'13:47:39.1234567')")],
  ['datetrunc string with offset', project("DATETRUNC(day,'2024-05-15T13:47:39.1234567+05:30')")],
  ['datetrunc unicode string', project("DATETRUNC(month,N'2024-05-15T13:47:39')")],
  ['datetrunc varchar variable', `DECLARE @s VARCHAR(40)='2024-05-15T13:47:39.1234567'; ${project('DATETRUNC(hour,@s)')}`],
  ['datetrunc nvarchar variable', `DECLARE @s NVARCHAR(40)=N'2024-05-15T13:47:39.1234567'; ${project('DATETRUNC(hour,@s)')}`],
  ['datetrunc unparseable string', project("DATETRUNC(day,'not a date')")],
  ['datetrunc integer input', project('DATETRUNC(day,1)')],
  ['datetrunc decimal input', project('DATETRUNC(day,1.5)')],
  ['date_bucket string date', project("DATE_BUCKET(day,1,'2024-05-15T13:47:39.1234567')")],
  ['date_bucket string date and origin', project("DATE_BUCKET(day,1,'2024-05-15T13:47:39','2024-05-01T12:00:00')")],
  ['date_bucket integer input', project('DATE_BUCKET(day,1,1)')],
  ['date_bucket unparseable string', project("DATE_BUCKET(day,1,'not a date')")],

  // NULL inputs.
  ['datetrunc untyped null', project('DATETRUNC(day,NULL)')],
  ['datetrunc typed null date', project('DATETRUNC(day,CAST(NULL AS DATE))')],
  ['datetrunc typed null datetimeoffset', project('DATETRUNC(hour,CAST(NULL AS DATETIMEOFFSET(3)))')],
  ['date_bucket null date', project('DATE_BUCKET(day,1,CAST(NULL AS DATETIME2(3)))')],
  ['date_bucket untyped null date', project('DATE_BUCKET(day,1,NULL)')],
  ['date_bucket null width', project(`DATE_BUCKET(day,CAST(NULL AS INT),${dt2})`)],
  ['date_bucket untyped null width', project(`DATE_BUCKET(day,NULL,${dt2})`)],
  ['date_bucket null origin', project(`DATE_BUCKET(day,1,${dt2},CAST(NULL AS DATETIME2(7)))`)],
  ['date_bucket untyped null origin', project(`DATE_BUCKET(day,1,${dt2},NULL)`)],

  // DATE_BUCKET widths.
  ...[1, 3, 7, 10, 100, 2147483647].map(width => [`date_bucket day width ${width}`, project(`DATE_BUCKET(day,${width},${dt2})`)]),
  ['date_bucket width zero', project(`DATE_BUCKET(day,0,${dt2})`)],
  ['date_bucket width negative', project(`DATE_BUCKET(day,-1,${dt2})`)],
  ['date_bucket width bigint', project(`DATE_BUCKET(day,CAST(2 AS BIGINT),${dt2})`)],
  ['date_bucket width smallint', project(`DATE_BUCKET(day,CAST(2 AS SMALLINT),${dt2})`)],
  ['date_bucket width tinyint', project(`DATE_BUCKET(day,CAST(2 AS TINYINT),${dt2})`)],
  ['date_bucket width decimal', project(`DATE_BUCKET(day,2.5,${dt2})`)],
  ['date_bucket width float', project(`DATE_BUCKET(day,CAST(2 AS FLOAT),${dt2})`)],
  ['date_bucket width string', project(`DATE_BUCKET(day,'2',${dt2})`)],
  ['date_bucket width variable', `DECLARE @w INT=3; ${project(`DATE_BUCKET(day,@w,${dt2})`)}`],
  ['date_bucket width zero variable', `DECLARE @w INT=0; ${project(`DATE_BUCKET(day,@w,${dt2})`)}`],
  ['date_bucket width expression', project(`DATE_BUCKET(day,1+1,${dt2})`)],
  ['date_bucket year overflow width', project(`DATE_BUCKET(year,5000,${dt2})`)],
  ['date_bucket millisecond large width', project(`DATE_BUCKET(millisecond,2147483647,${dt2})`)],

  // DATE_BUCKET origins and dates before the origin.
  ['date_bucket default origin week', project("DATE_BUCKET(week,1,CAST('2024-05-15' AS DATE))")],
  ['date_bucket explicit default origin', project("DATE_BUCKET(day,3,CAST('2024-05-15' AS DATE),CAST('1900-01-01' AS DATE))")],
  ['date_bucket origin after date', project("DATE_BUCKET(day,3,CAST('2024-05-15' AS DATE),CAST('2024-06-01' AS DATE))")],
  ['date_bucket origin equals date', project("DATE_BUCKET(day,3,CAST('2024-05-15' AS DATE),CAST('2024-05-15' AS DATE))")],
  ['date_bucket date before default origin', project("DATE_BUCKET(day,7,CAST('1899-12-30' AS DATE))")],
  ['date_bucket date before origin month', project("DATE_BUCKET(month,5,CAST('2024-01-15' AS DATE),CAST('2024-05-31' AS DATE))")],
  ['date_bucket month end origin', project("DATE_BUCKET(month,1,CAST('2024-02-29' AS DATE),CAST('2024-01-31' AS DATE))")],
  ['date_bucket month end origin march', project("DATE_BUCKET(month,1,CAST('2024-03-30' AS DATE),CAST('2024-01-31' AS DATE))")],
  ['date_bucket year origin leap day', project("DATE_BUCKET(year,1,CAST('2025-03-01' AS DATE),CAST('2024-02-29' AS DATE))")],
  ['date_bucket quarter origin', project("DATE_BUCKET(quarter,1,CAST('2024-05-15' AS DATE),CAST('2024-02-10' AS DATE))")],
  ['date_bucket hour origin fraction', project("DATE_BUCKET(hour,1,CAST('2024-05-15T13:47:39.1234567' AS DATETIME2(7)),CAST('2024-05-15T00:30:00.5' AS DATETIME2(7)))")],
  ['date_bucket origin type mismatch', project("DATE_BUCKET(day,1,CAST('2024-05-15' AS DATE),CAST('2024-05-01' AS DATETIME2(7)))")],
  ['date_bucket origin scale mismatch', project("DATE_BUCKET(day,1,CAST('2024-05-15' AS DATETIME2(3)),CAST('2024-05-01' AS DATETIME2(7)))")],
  ['date_bucket origin datetime for datetime2', project("DATE_BUCKET(day,1,CAST('2024-05-15' AS DATETIME2(7)),CAST('2024-05-01' AS DATETIME))")],
  ['date_bucket origin string', project("DATE_BUCKET(day,1,CAST('2024-05-15' AS DATE),'2024-05-02')")],
  ['date_bucket datetimeoffset origin different offset', project("DATE_BUCKET(hour,1,CAST('2024-05-15T13:47:39+05:30' AS DATETIMEOFFSET(0)),CAST('2024-05-15T00:30:00+00:00' AS DATETIMEOFFSET(0)))")],
  ['date_bucket datetimeoffset day', project("DATE_BUCKET(day,1,CAST('2024-05-15T02:00:00+05:30' AS DATETIMEOFFSET(0)))")],
  ['date_bucket time origin', project("DATE_BUCKET(minute,15,CAST('13:47:39' AS TIME(0)),CAST('00:05:00' AS TIME(0)))")],
  ['date_bucket time day', project("DATE_BUCKET(day,1,CAST('13:47:39' AS TIME(0)))")],
  ['date_bucket date lower bound', project("DATE_BUCKET(day,7,CAST('0001-01-01' AS DATE))")],
  ['date_bucket date upper bound', project("DATE_BUCKET(year,3,CAST('9999-12-31' AS DATE))")],
  ['date_bucket datetime lower bound', project("DATE_BUCKET(week,1,CAST('1753-01-01' AS DATETIME))")],
  ['date_bucket smalldatetime lower bound', project("DATE_BUCKET(week,1,CAST('1900-01-01' AS SMALLDATETIME))")],
  ['date_bucket date below minimum', project("DATE_BUCKET(day,10,CAST('0001-01-01' AS DATE))")],
  ['date_bucket datetime below minimum', project("DATE_BUCKET(day,3,CAST('1753-01-01' AS DATETIME))")],
  ['date_bucket negative interval', project("DATE_BUCKET(hour,5,CAST('1899-12-31T01:00:00' AS DATETIME2(0)))")],

  // DATETRUNC range edges.
  ['datetrunc week date lower bound', project("DATETRUNC(week,CAST('0001-01-01' AS DATE))")],
  ['datetrunc iso_week date lower bound', project("DATETRUNC(iso_week,CAST('0001-01-01' AS DATE))")],
  ['datetrunc week datetime lower bound', project("DATETRUNC(week,CAST('1753-01-01' AS DATETIME))")],
  ['datetrunc week smalldatetime lower bound', project("DATETRUNC(week,CAST('1900-01-01' AS SMALLDATETIME))")],
  ['datetrunc iso_week year boundary', project("DATETRUNC(iso_week,CAST('2021-01-01' AS DATE))")],
  ['datetrunc week year boundary', project("DATETRUNC(week,CAST('2021-01-01' AS DATE))")],
  ['datetrunc year upper bound', project("DATETRUNC(year,CAST('9999-12-31T23:59:59.9999999' AS DATETIME2(7)))")],
  ['datetrunc datetime millisecond rounding', project("DATETRUNC(millisecond,CAST('2024-05-15T13:47:39.997' AS DATETIME))")],
  ['datetrunc datetime second tick', project("DATETRUNC(second,CAST('2024-05-15T23:59:59.997' AS DATETIME))")],
  ['datetrunc datetimeoffset negative offset', project("DATETRUNC(day,CAST('2024-05-15T02:00:00-08:00' AS DATETIMEOFFSET(0)))")],
  ['datetrunc datetimeoffset iso_week', project("DATETRUNC(iso_week,CAST('2024-05-13T02:00:00+14:00' AS DATETIMEOFFSET(0)))")],

  // Column and multi-row inputs.
  ['create source', 'CREATE TABLE dbo.datetrunc_src(id INT,d DATE,t TIME(3),dt2 DATETIME2(4),dto DATETIMEOFFSET(2),dt DATETIME,sdt SMALLDATETIME)'],
  ['insert source', "INSERT dbo.datetrunc_src VALUES (1,'2024-05-15','13:47:39.123','2024-05-15T13:47:39.1234','2024-05-15T13:47:39.12+05:30','2024-05-15T13:47:39.123','2024-05-15T13:47:29'),(2,'1999-12-31','23:59:59.999','1999-12-31T23:59:59.9999','1999-12-31T23:59:59.99-08:00','1999-12-31T23:59:59.997','1999-12-31T23:59:00'),(3,NULL,NULL,NULL,NULL,NULL,NULL)"],
  ['datetrunc columns', "SELECT id,DATETRUNC(month,d) AS d,DATETRUNC(minute,t) AS t,DATETRUNC(hour,dt2) AS dt2,DATETRUNC(day,dto) AS dto,DATETRUNC(second,dt) AS dt,DATETRUNC(hour,sdt) AS sdt,CONVERT(VARCHAR(50),DATETRUNC(day,dto),121) AS dto_text,CONVERT(VARCHAR(50),DATETRUNC(hour,dt2),121) AS dt2_text FROM dbo.datetrunc_src ORDER BY id"],
  ['date_bucket columns', "SELECT id,DATE_BUCKET(week,1,d) AS d,DATE_BUCKET(minute,15,t) AS t,DATE_BUCKET(hour,6,dt2) AS dt2,DATE_BUCKET(day,2,dto) AS dto,DATE_BUCKET(second,10,dt) AS dt,DATE_BUCKET(hour,3,sdt) AS sdt,CONVERT(VARCHAR(50),DATE_BUCKET(day,2,dto),121) AS dto_text FROM dbo.datetrunc_src ORDER BY id"],
  ['group by datetrunc', 'SELECT DATETRUNC(year,dt2) AS y,COUNT(*) AS n FROM dbo.datetrunc_src GROUP BY DATETRUNC(year,dt2) ORDER BY y'],
  ['select into datetrunc', 'SELECT DATETRUNC(day,dto) AS dto,DATE_BUCKET(hour,1,dt2) AS dt2,DATETRUNC(second,sdt) AS sdt INTO dbo.datetrunc_into FROM dbo.datetrunc_src'],
  ['select into catalog', "SELECT c.name,t.name AS type_name,c.max_length,c.precision,c.scale,c.is_nullable FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('dbo.datetrunc_into') ORDER BY c.column_id"],
  ['sql_variant properties', "SELECT CAST(SQL_VARIANT_PROPERTY(DATETRUNC(hour,CAST('2024-05-15T13:47:39.12' AS DATETIME2(2))),'BaseType') AS VARCHAR(30)) AS base_type,CAST(SQL_VARIANT_PROPERTY(DATETRUNC(hour,CAST('2024-05-15T13:47:39.12' AS DATETIME2(2))),'Scale') AS INT) AS scale,CAST(SQL_VARIANT_PROPERTY(DATETRUNC(day,'2024-05-15'),'BaseType') AS VARCHAR(30)) AS literal_type,CAST(SQL_VARIANT_PROPERTY(DATETRUNC(day,'2024-05-15'),'Scale') AS INT) AS literal_scale"],
)

// DATEFIRST changes DATETRUNC(week) but the batch restores the session default.
const weekProbe = "DATETRUNC(week,CAST('2024-05-15' AS DATE))"
for (const first of [1, 2, 3, 4, 5, 6, 7]) {
  cases.push([`datefirst ${first}`, `SET DATEFIRST ${first}; SELECT @@DATEFIRST AS first_day,${weekProbe} AS week_start,DATETRUNC(iso_week,CAST('2024-05-15' AS DATE)) AS iso_week_start,DATE_BUCKET(week,1,CAST('2024-05-15' AS DATE)) AS bucket_week,DATETRUNC(wk,CAST('2024-05-12T10:00:00' AS DATETIME2(0))) AS sunday_week; SET DATEFIRST 7`])
}
cases.push(['language datefirst', "SET LANGUAGE British; SELECT @@DATEFIRST AS first_day,DATETRUNC(week,CAST('2024-05-15' AS DATE)) AS week_start,DATE_BUCKET(week,1,CAST('2024-05-15' AS DATE)) AS bucket_week; SET LANGUAGE us_english"])
cases.push(['dateformat dmy string', "SET DATEFORMAT dmy; SELECT DATETRUNC(day,'05/04/2024 13:47') AS value,CONVERT(VARCHAR(50),DATETRUNC(day,'05/04/2024 13:47'),121) AS text; SET DATEFORMAT mdy"])
cases.push(['session restored', 'SELECT @@DATEFIRST AS first_day,@@LANGUAGE AS language'])

function rpc(connection, sql, parameters) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      for (const [name, type, value, options] of parameters) request.addParameter(name, type, value, options)
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}

async function prepared(connection, sql, declarations, executions) {
  const request = new Request(sql, (...values) => complete(...values))
  let complete = () => {}
  for (const [name, type, options] of declarations) request.addParameter(name, type, undefined, options)
  await new Promise((resolve, reject) => {
    request.once('prepared', resolve)
    request.once('error', reject)
    connection.prepare(request)
  })
  const results = []
  try {
    for (const values of executions) {
      const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
      const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
      const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
      const onMetadata = metadata => result.sets.push({ columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })), rows: [] })
      const onRow = row => result.sets.at(-1).rows.push(row.map(c => c.value))
      const done = Object.fromEntries(['done', 'doneInProc', 'doneProc'].map(kind => [kind, (rowCount, more, status) => {
        result.done.push({ kind, rowCount: rowCount ?? null, more })
        if (kind === 'doneProc') result.returnStatus = status
      }]))
      connection.on('errorMessage', onError); connection.on('infoMessage', onInfo)
      request.on('columnMetadata', onMetadata); request.on('row', onRow)
      for (const [kind, listener] of Object.entries(done)) request.on(kind, listener)
      try {
        await new Promise(resolve => {
          complete = (error, rowCount) => {
            result.rowCount = rowCount
            if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          }
          request.error = undefined
          connection.execute(request, values)
        })
      } finally {
        connection.off('errorMessage', onError); connection.off('infoMessage', onInfo)
        request.off('columnMetadata', onMetadata); request.off('row', onRow)
        for (const [kind, listener] of Object.entries(done)) request.off(kind, listener)
      }
      results.push({ values: canonical(values), result: canonical(bounded(sql, result)) })
    }
  } finally {
    await new Promise((resolve, reject) => {
      complete = error => error ? reject(error) : resolve()
      request.error = undefined
      connection.unprepare(request)
    })
  }
  return results
}

const at = iso => new Date(iso)
const rpcCases = [
  ['rpc datetime2 truncate', "SELECT DATETRUNC(hour,@d) AS value,CONVERT(VARCHAR(50),DATETRUNC(hour,@d),121) AS text", [['d', TYPES.DateTime2, at('2024-05-15T13:47:39.123Z'), { scale: 7 }]]],
  ['rpc datetime2 scale 2 truncate', "SELECT DATETRUNC(millisecond,@d) AS value,CONVERT(VARCHAR(50),DATETRUNC(millisecond,@d),121) AS text", [['d', TYPES.DateTime2, at('2024-05-15T13:47:39.123Z'), { scale: 2 }]]],
  ['rpc date truncate week', "SELECT DATETRUNC(week,@d) AS value,CONVERT(VARCHAR(50),DATETRUNC(week,@d),121) AS text", [['d', TYPES.Date, at('2024-05-15T00:00:00Z')]]],
  ['rpc date truncate hour rejected', "SELECT DATETRUNC(hour,@d) AS value", [['d', TYPES.Date, at('2024-05-15T00:00:00Z')]]],
  ['rpc time truncate', "SELECT DATETRUNC(minute,@d) AS value,CONVERT(VARCHAR(50),DATETRUNC(minute,@d),121) AS text", [['d', TYPES.Time, at('1970-01-01T13:47:39.123Z'), { scale: 3 }]]],
  ['rpc datetimeoffset truncate', "SELECT DATETRUNC(day,@d) AS value,CONVERT(VARCHAR(50),DATETRUNC(day,@d),121) AS text", [['d', TYPES.DateTimeOffset, at('2024-05-15T20:47:39.123Z'), { scale: 3 }]]],
  ['rpc datetime truncate', "SELECT DATETRUNC(second,@d) AS value,CONVERT(VARCHAR(50),DATETRUNC(second,@d),121) AS text", [['d', TYPES.DateTime, at('2024-05-15T13:47:39.123Z')]]],
  ['rpc smalldatetime truncate', "SELECT DATETRUNC(hour,@d) AS value,CONVERT(VARCHAR(50),DATETRUNC(hour,@d),121) AS text", [['d', TYPES.SmallDateTime, at('2024-05-15T13:47:00Z')]]],
  ['rpc nvarchar truncate', "SELECT DATETRUNC(hour,@d) AS value,CONVERT(VARCHAR(50),DATETRUNC(hour,@d),121) AS text", [['d', TYPES.NVarChar, '2024-05-15T13:47:39.1234567', { length: 40 }]]],
  ['rpc null datetime2 truncate', "SELECT DATETRUNC(hour,@d) AS value", [['d', TYPES.DateTime2, null, { scale: 7 }]]],
  ['rpc bucket width parameter', "SELECT DATE_BUCKET(day,@w,@d) AS value,CONVERT(VARCHAR(50),DATE_BUCKET(day,@w,@d),121) AS text", [['w', TYPES.Int, 3], ['d', TYPES.DateTime2, at('2024-05-15T13:47:39.123Z'), { scale: 7 }]]],
  ['rpc bucket zero width parameter', "SELECT DATE_BUCKET(day,@w,@d) AS value", [['w', TYPES.Int, 0], ['d', TYPES.DateTime2, at('2024-05-15T13:47:39.123Z'), { scale: 7 }]]],
  ['rpc bucket negative width parameter', "SELECT DATE_BUCKET(day,@w,@d) AS value", [['w', TYPES.Int, -2], ['d', TYPES.DateTime2, at('2024-05-15T13:47:39.123Z'), { scale: 7 }]]],
  ['rpc bucket null width parameter', "SELECT DATE_BUCKET(day,@w,@d) AS value", [['w', TYPES.Int, null], ['d', TYPES.DateTime2, at('2024-05-15T13:47:39.123Z'), { scale: 7 }]]],
  ['rpc bucket bigint width parameter', "SELECT DATE_BUCKET(day,@w,@d) AS value", [['w', TYPES.BigInt, 3], ['d', TYPES.DateTime2, at('2024-05-15T13:47:39.123Z'), { scale: 7 }]]],
  ['rpc bucket origin parameter', "SELECT DATE_BUCKET(week,1,@d,@o) AS value,CONVERT(VARCHAR(50),DATE_BUCKET(week,1,@d,@o),121) AS text", [['d', TYPES.Date, at('2024-05-15T00:00:00Z')], ['o', TYPES.Date, at('2024-05-01T00:00:00Z')]]],
  ['rpc bucket origin type mismatch', "SELECT DATE_BUCKET(day,1,@d,@o) AS value", [['d', TYPES.Date, at('2024-05-15T00:00:00Z')], ['o', TYPES.DateTime2, at('2024-05-01T00:00:00Z'), { scale: 7 }]]],
  ['rpc bucket null origin parameter', "SELECT DATE_BUCKET(day,1,@d,@o) AS value", [['d', TYPES.Date, at('2024-05-15T00:00:00Z')], ['o', TYPES.Date, null]]],
  ['rpc datetrunc replay', "SELECT DATETRUNC(hour,@d) AS value,CONVERT(VARCHAR(50),DATETRUNC(hour,@d),121) AS text", [['d', TYPES.DateTime2, at('2024-05-15T13:47:39.123Z'), { scale: 7 }]]],
]

const preparedCases = [
  ['prepared datetrunc and bucket', 'SELECT DATETRUNC(day,@d) AS truncated,DATE_BUCKET(day,@w,@d) AS bucket,CONVERT(VARCHAR(50),DATE_BUCKET(day,@w,@d),121) AS text',
    [['d', TYPES.DateTime2, { scale: 7 }], ['w', TYPES.Int]],
    [{ d: at('2024-05-15T13:47:39.123Z'), w: 3 }, { d: at('1899-12-30T06:00:00Z'), w: 7 }, { d: null, w: 3 }, { d: at('2024-05-15T13:47:39.123Z'), w: 0 }, { d: at('2024-05-15T13:47:39.123Z'), w: -1 }, { d: at('2024-05-15T13:47:39.123Z'), w: 3 }]],
  ['prepared datefirst week', 'SET DATEFIRST @f; SELECT @@DATEFIRST AS first_day,DATETRUNC(week,@d) AS week_start,DATE_BUCKET(week,1,@d) AS bucket_week; SET DATEFIRST 7',
    [['f', TYPES.Int], ['d', TYPES.Date]],
    [{ f: 1, d: at('2024-05-15T00:00:00Z') }, { f: 3, d: at('2024-05-15T00:00:00Z') }, { f: 7, d: at('2024-05-15T00:00:00Z') }]],
]

// Every program returns at most a handful of rows; fail loudly on anything
// that looks like unbounded accumulation instead of retaining it.
const limits = { sets: 4, rows: 16, messages: 16, done: 8 }
function bounded(name, result) {
  const rows = result.sets.reduce((total, set) => total + set.rows.length, 0)
  if (result.sets.length > limits.sets || rows > limits.rows || result.errors.length + result.info.length > limits.messages || result.done.length > limits.done) {
    throw new Error(`${name}: result exceeds capture bounds (${result.sets.length} sets, ${rows} rows, ${result.errors.length + result.info.length} messages, ${result.done.length} DONE tokens)`)
  }
  return result
}

async function observe(connection) {
  const records = []
  async function record(name, sql, parameters) {
    const result = canonical(bounded(name, await (parameters ? rpc(connection, sql, parameters) : capture(connection, sql))))
    records.push({
      name, sql,
      ...(parameters ? { parameters: parameters.map(([parameter, type, value, options]) => ({
        name: parameter, type: type.name, value: canonical(value), ...(options ? { options } : {}),
      })) } : {}),
      result,
    })
  }
  for (const [name, sql] of cases) await record(name, sql)
  for (const [name, sql, parameters] of rpcCases) await record(name, sql, parameters)
  for (const [name, sql, declarations, executions] of preparedCases) {
    records.push({
      name, sql, prepared: declarations.map(([parameter, type, options]) => ({ name: parameter, type: type.name, ...(options ? { options } : {}) })),
      executions: await prepared(connection, sql, declarations, executions),
    })
  }
  return records
}

function validate(run) {
  assert.equal(run.length, cases.length + rpcCases.length + preparedCases.length)
  const get = name => run.find(record => record.name === name)?.result
  for (const record of run) {
    if (record.executions) for (const execution of record.executions) assert(execution.result.done.length > 0, record.name + ': no completion')
    else assert(record.result.done.length > 0, record.name + ': no completion')
  }
  assert.deepEqual(get('session restored').sets[0].rows, [[7, 'us_english']])
  const text = name => get(name).sets[0].rows[0][1]
  const error = name => get(name).errors[0]?.number
  for (const [name, expected] of [
    ['datetrunc week date', '2024-05-12'],
    ['datetrunc iso_week date', '2024-05-13'],
    ['datetrunc microsecond datetime2(6)', '2024-05-15 13:47:39.123457'],
    ['datetrunc microsecond datetime2(7)', '2024-05-15 13:47:39.1234560'],
    ['datetrunc day datetimeoffset(0)', '2024-05-15 00:00:00 +05:30'],
    ['date_bucket day 2 datetimeoffset(0)', '2024-05-14 05:30:00 +05:30'],
    ['date_bucket week 2 date', '2024-05-06'],
    ['datetrunc string time only', '1900-01-01 13:47:00.0000000'],
    ['date_bucket month end origin march', '2024-02-29'],
    ['date_bucket year origin leap day', '2025-02-28'],
    ['date_bucket date before default origin', '1899-12-25'],
  ]) assert.equal(text(name), expected, name)
  for (const [name, type, scale] of [
    ['datetrunc year datetime2(2)', 'DateTime2', 2],
    ['datetrunc hour time(4)', 'Time', 4],
    ['datetrunc string date only', 'DateTime2', 7],
    ['date_bucket origin scale mismatch', 'DateTime2', 7],
  ]) {
    assert.equal(get(name).sets[0].columns[0].type, type, name)
    assert.equal(get(name).sets[0].columns[0].scale, scale, name)
  }
  for (const [name, number] of [
    ['datetrunc hour date', 9810],
    ['datetrunc millisecond datetime2(2)', 9810],
    ['datetrunc nanosecond datetime2(7)', 9810],
    ['date_bucket iso_week 2 date', 9810],
    ['datetrunc unknown datepart', 155],
    ['datetrunc string datepart', 1023],
    ['datetrunc integer input', 8116],
    ['date_bucket string date', 8116],
    ['date_bucket width zero', 9834],
    ['date_bucket width negative', 9834],
    ['date_bucket origin type mismatch', 8116],
    ['datetrunc week date lower bound', 9837],
  ]) assert.equal(error(name), number, name)
  assert.deepEqual(get('datefirst 3').sets[0].rows[0].slice(0, 1), [3])
  return get
}

// Never hand whole captures to node:assert. On Node 24, building the
// AssertionError for a failed strictEqual/deepStrictEqual with this ~3 MB
// capture as an operand inspects it without a practical bound: the process
// exceeded 3 GB RSS even under --max-old-space-size=2048, and an unguarded
// run reached ~123 GB and was OOM-killed. Compare with isDeepStrictEqual and
// raise plain errors that name only the first differing record.
function same(left, right, message) {
  if (isDeepStrictEqual(left, right)) return
  const a = left?.containers ? left.containers.flatMap(c => c.runs.flat()) : left
  const b = right?.containers ? right.containers.flatMap(c => c.runs.flat()) : right
  const index = Array.isArray(a) && Array.isArray(b)
    ? a.findIndex((record, i) => !isDeepStrictEqual(record, b[i])) : -1
  throw new Error(message + (index >= 0 ? ` (first difference at record ${index}: ${JSON.stringify(a[index]?.name ?? null)})` : ''))
}

let fixtureExists = false
try { await access(fixture); fixtureExists = true }
catch (error) { if (error.code !== 'ENOENT') throw error }
// Refuse before starting any container, not after a full capture.
if (writeFixture && fixtureExists) throw new Error('refusing to overwrite retained fixture ' + fixture.pathname)

await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < 2; containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let databaseIndex = 0; databaseIndex < 2; databaseIndex++) {
      const run = await isolatedReference({
        ...config, options: { ...config.options, requestTimeout: 120000 },
      }, observe)
      validate(run)
      runs.push(run)
    }
    same(runs[0], runs[1], 'DATETRUNC/DATE_BUCKET observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
same(containers[0].runs[0], containers[1].runs[0], 'DATETRUNC/DATE_BUCKET observations differ across containers')
const actual = { containers }
await writeFile(output, JSON.stringify(actual) + '\n')
const retained = fixtureExists ? JSON.parse(await readFile(fixture, 'utf8')) : undefined
if (retained !== undefined) same(actual, retained, 'DATETRUNC/DATE_BUCKET observations differ from retained fixture')
if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
console.log('Captured ' + containers[0].runs[0].length + ' DATETRUNC/DATE_BUCKET observations in four fresh databases across two containers' + (retained ? ' and matched retained fixture' : ''))
