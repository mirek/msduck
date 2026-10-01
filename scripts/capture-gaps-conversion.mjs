#!/usr/bin/env node
// Capture SQL Server behavior for styled CONVERT, explicit COLLATE,
// SERVERPROPERTY, DATABASEPROPERTYEX and ROWCOUNT_BIG (issue #725).
//
//   node scripts/capture-gaps-conversion.mjs [output]
//
// Each case runs in a fresh database of one reference container and keeps
// its result descriptors (type and length), rows and diagnostics. FORMAT is
// covered by reference/format.json (scripts/capture-format.mjs). The image
// defaults to the repository's pinned reference image; MSSQL_REFERENCE_IMAGE
// selects another one, and the fixture records which image produced it.
import { resolve } from 'node:path'
import { canonical, capture } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

process.env.TZ = 'UTC'
const output = resolve(process.argv[2] ?? 'reference/gaps-conversion.json')
await refuseExistingFixture(output)

const quote = text => `'${text.replaceAll("'", "''")}'`
const cases = []
const add = (group, name, sql) => cases.push({ group, name, sql })

// Styled date/time to character conversions. Two values per type: a morning
// value with a fractional second and an evening one, so AM/PM, hour padding
// and rounding all show.
const temporal = [
  ['datetime', '2024-01-02 03:04:05.123', '2024-12-31 23:59:59.997'],
  ['smalldatetime', '2024-01-02 03:04:05', '2024-12-31 23:29:31'],
  ['date', '2024-01-02', '2024-12-31'],
  ['time(7)', '03:04:05.1234567', '23:59:59.9999999'],
  ['time(3)', '03:04:05.123', '23:59:59.997'],
  ['time(0)', '03:04:05', '23:59:59'],
  ['datetime2(7)', '2024-01-02 03:04:05.1234567', '2024-12-31 23:59:59.9999999'],
  ['datetime2(3)', '2024-01-02 03:04:05.123', '2024-12-31 23:59:59.997'],
  ['datetime2(0)', '2024-01-02 03:04:05', '2024-12-31 23:59:59'],
  ['datetimeoffset(7)', '2024-01-02 03:04:05.1234567 +05:30', '2024-12-31 23:59:59.9999999 -08:00'],
  ['datetimeoffset(3)', '2024-01-02 03:04:05.123 +00:00', '2024-12-31 23:59:59.997 -08:00'],
  ['datetimeoffset(0)', '2024-01-02 03:04:05 +05:30', '2024-12-31 23:59:59 -08:00'],
]
const styles = [
  ...Array.from({ length: 15 }, (_, i) => i), 20, 21, 22, 23, 24, 25,
  ...Array.from({ length: 15 }, (_, i) => 100 + i), 120, 121, 126, 127, 130, 131,
  15, 99, 115, 128, 200,
]
for (const [type, morning, evening] of temporal) {
  for (const style of styles) {
    add('format', `${type} style ${style}`,
      `SELECT CONVERT(varchar(60), CAST(${quote(morning)} AS ${type}), ${style}) AS morning, CONVERT(nvarchar(60), CAST(${quote(evening)} AS ${type}), ${style}) AS evening`)
  }
  // Round trips: the formatted text converts back with the same style.
  for (const style of styles.filter(s => s < 15 || (s >= 20 && s <= 25) || (s >= 100 && s <= 114) || [120, 121, 126, 127].includes(s))) {
    add('parse', `${type} round trip ${style}`,
      `SELECT CONVERT(${type}, CONVERT(varchar(60), CAST(${quote(morning)} AS ${type}), ${style}), ${style}) AS morning, TRY_CONVERT(${type}, CONVERT(varchar(60), CAST(${quote(evening)} AS ${type}), ${style}), ${style}) AS evening`)
  }
}
// Truncation and padding of the formatted text.
for (const target of ['varchar(10)', 'char(25)', 'nvarchar(8)', 'nchar(25)', 'varchar(max)', 'nvarchar(max)', 'varchar']) {
  add('format', `datetime 121 into ${target}`, `SELECT CONVERT(${target}, CAST('2024-01-02 03:04:05.123' AS datetime), 121) AS v`)
}
add('format', 'null value', 'SELECT CONVERT(varchar(30), CAST(NULL AS datetime), 126) AS v, CONVERT(varchar(30), CAST(NULL AS datetimeoffset), 127) AS w')
add('format', 'datetimeoffset 127 utc', "SELECT CONVERT(nvarchar(40), CAST('2024-01-02 03:04:05.1234567 +00:00' AS datetimeoffset), 127) AS v")
add('format', 'datetime 127 zero fraction', "SELECT CONVERT(varchar(40), CAST('2024-01-02 03:04:05' AS datetime), 127) AS v, CONVERT(varchar(40), CAST('2024-01-02 03:04:05' AS datetime), 126) AS w")
add('format', 'datetime2 127 zero fraction', "SELECT CONVERT(varchar(40), CAST('2024-01-02 03:04:05' AS datetime2), 127) AS v, CONVERT(varchar(40), CAST('2024-01-02 03:04:05.5' AS datetime2), 126) AS w")
add('format', 'zero fractions 126', "SELECT CONVERT(varchar(40), CAST('2024-01-02 03:04:05' AS datetime2), 126) AS a, CONVERT(varchar(40), CAST('03:04:05' AS time), 126) AS b, CONVERT(varchar(40), CAST('2024-01-02 03:04:05 +01:00' AS datetimeoffset), 126) AS c")
add('format', 'try style invalid', "SELECT TRY_CONVERT(varchar(30), CAST('2024-01-02' AS datetime), 99) AS v")
add('format', 'style from variable', "DECLARE @s int = 112; SELECT CONVERT(varchar(30), CAST('2024-01-02' AS datetime), @s) AS v")

// Character to date/time with styles.
const parses = [
  ['datetime', '20240102', 112], ['datetime', '01/02/2024', 101], ['datetime', '02/01/2024', 103],
  ['datetime', '1/2/2024', 101], ['datetime', '2/1/24', 3], ['datetime', '02.01.2024', 104],
  ['datetime', '02-01-2024', 105], ['datetime', '2024.01.02', 102], ['datetime', '2024/01/02', 111],
  ['datetime', '02 Jan 2024', 106], ['datetime', 'Jan 02, 2024', 107], ['datetime', '01-02-2024', 110],
  ['datetime', '2024-01-02 03:04:05', 120], ['datetime', '2024-01-02 03:04:05.123', 121],
  ['datetime', '2024-01-02T03:04:05.123', 126], ['datetime', '2024-01-02T03:04:05.123Z', 127],
  ['datetime', '2024-01-02T03:04:05', 126], ['datetime', 'Jan  2 2024  3:04AM', 100],
  ['datetime', 'Jan  2 2024  3:04:05:123AM', 109], ['datetime', '02 Jan 2024 03:04:05:123', 113],
  ['datetime', '03:04:05', 108], ['datetime', '03:04:05:123', 114], ['datetime', '2024-01-02', 23],
  ['datetime', '01/02/24  3:04:05 PM', 22], ['datetime', '2024-13-02', 120], ['datetime', 'garbage', 120],
  ['datetime', '2024-01-02', 101], ['datetime', '2024-01-02', 103], ['datetime', '', 120],
  ['date', '20240102', 112], ['date', '02/01/2024', 103], ['date', '2024-01-02T03:04:05.123', 126],
  ['date', '2024-01-02T03:04:05Z', 127], ['date', '31/02/2024', 103], ['date', '2/1/2024', 103],
  ['datetime2', '2024-01-02T03:04:05.1234567', 126], ['datetime2', '2024-01-02T03:04:05.1234567Z', 127],
  ['datetime2', '20240102', 112], ['datetime2', '02.01.2024', 104], ['datetime2', '2024-01-02 03:04:05.1234567', 121],
  ['datetimeoffset', '2024-01-02T03:04:05.1234567+05:30', 127], ['datetimeoffset', '2024-01-02T03:04:05.1234567Z', 127],
  ['datetimeoffset', '2024-01-02T03:04:05.1234567+05:30', 126], ['datetimeoffset', '2024-01-02 03:04:05 +05:30', 120],
  ['datetimeoffset', '20240102', 112],
  ['time', '03:04:05.1234567', 114], ['time', '03:04:05', 108], ['time', '2024-01-02T03:04:05.123', 126],
  ['smalldatetime', '2024-01-02 03:04:31', 120], ['smalldatetime', '20240102', 112],
]
for (const [type, text, style] of parses) {
  add('parse', `${type} from ${JSON.stringify(text)} style ${style}`, `SELECT CONVERT(${type}, ${quote(text)}, ${style}) AS v`)
  add('parse', `${type} try from ${JSON.stringify(text)} style ${style}`, `SELECT TRY_CONVERT(${type}, ${quote(text)}, ${style}) AS v`)
}
add('parse', 'nvarchar source', "SELECT CONVERT(datetime, N'02/01/2024', 103) AS v")
add('parse', 'invalid style for parse', "SELECT CONVERT(datetime, '2024-01-02', 99) AS v")

// Binary styles in both directions.
const binary = [
  "CONVERT(varchar(6), 0x010203, 2)", "CONVERT(varchar(20), 0x0A0B, 1)", "CONVERT(varchar(20), 0x414243, 0)",
  "CONVERT(nvarchar(20), 0x0A0B, 1)", "CONVERT(char(10), 0x0A0B, 2)", "CONVERT(varchar(3), 0x0A0B, 1)",
  "CONVERT(varchar(3), 0x0A0B0C, 2)", "CONVERT(varchar(max), 0xDEADBEEF, 1)", "CONVERT(varchar(20), 0x, 1)",
  "CONVERT(varchar(20), 0x, 2)", "CONVERT(varchar(20), CAST(NULL AS varbinary(4)), 1)",
  "CONVERT(varbinary(4), '0x0102', 1)", "CONVERT(varbinary(4), '0102', 2)", "CONVERT(varbinary(4), N'0x0a0B', 1)",
  "CONVERT(varbinary(4), 'abc', 0)", "CONVERT(binary(4), '0x0102', 1)", "CONVERT(varbinary(2), '0x010203', 1)",
  "CONVERT(varbinary(4), '0x123', 1)", "CONVERT(varbinary(4), '123', 2)", "CONVERT(varbinary(4), '0102', 1)",
  "CONVERT(varbinary(4), '0x0102', 2)", "CONVERT(varbinary(4), '0xZZ', 1)", "CONVERT(varbinary(4), 'ZZ', 2)",
  "CONVERT(varbinary(4), '', 1)", "CONVERT(varbinary(4), '', 2)", "CONVERT(varbinary(4), '0x', 1)",
  "CONVERT(varbinary(max), '0xDEADBEEF', 1)", "TRY_CONVERT(varbinary(4), '0xZZ', 1)",
  "CONVERT(varbinary(4), 'abc', 3)", "CONVERT(varchar(20), 0x0A0B, 3)", "CONVERT(varbinary(4), CAST(NULL AS varchar(4)), 1)",
]
for (const expr of binary) add('binary', expr, `SELECT ${expr} AS v`)
add('binary', 'column round trip', "CREATE TABLE dbo.conversion_binary (b varbinary(10), s varchar(20)); INSERT dbo.conversion_binary VALUES (0x0A0B, '0x0A0B'); SELECT CONVERT(varchar(20), b, 1) AS a, CONVERT(varchar(20), b, 2) AS b2, CONVERT(varbinary(10), s, 1) AS c FROM dbo.conversion_binary")

// Styles that do not apply to the source type.
for (const expr of [
  "CONVERT(varchar(10), 42, 1)", "CONVERT(varchar(10), 12.5, 0)", "CONVERT(varchar(10), 'abc', 1)", "CONVERT(int, '12', 112)",
  "CONVERT(varchar(30), CAST(1.5 AS float), 2)", "CONVERT(varchar(30), CAST(1.5 AS float), 1)", "CONVERT(varchar(30), CAST(1.5 AS float), 3)",
  "CONVERT(varchar(30), CAST(1.5 AS float), 0)", "CONVERT(varchar(30), CAST(123456789.5 AS float), 0)",
]) add('other', expr, `SELECT ${expr} AS v`)

// Explicit COLLATE in comparisons and ordering.
const collations = ['Latin1_General_CI_AS', 'Latin1_General_CS_AS', 'Latin1_General_CI_AI', 'Latin1_General_CS_AI',
  'SQL_Latin1_General_CP1_CI_AS', 'SQL_Latin1_General_CP1_CS_AS', 'Latin1_General_100_CI_AS', 'Latin1_General_100_CS_AS',
  'Latin1_General_100_CI_AI', 'Latin1_General_BIN2', 'Latin1_General_100_BIN2', 'Latin1_General_BIN', 'Latin1_General_100_CI_AS_SC_UTF8']
const pairs = [["N'A'", "N'a'"], ["N'é'", "N'e'"], ["N'É'", "N'e'"], ["N'a '", "N'a'"], ["N'abc'", "N'ABD'"], ["'A'", "'a'"]]
for (const collation of collations) {
  add('collate', `${collation} comparisons`, 'SELECT ' + pairs.map(([l, r], i) =>
    `CASE WHEN ${l} = ${r} COLLATE ${collation} THEN 1 ELSE 0 END AS eq${i}, CASE WHEN ${l} < ${r} COLLATE ${collation} THEN 1 ELSE 0 END AS lt${i}`).join(', '))
  add('collate', `${collation} order`, `SELECT x FROM (VALUES (N'b'),(N'a'),(N'B'),(N'A'),(N'é'),(N'e'),(N'f'),(N'É'),(N'Z'),(N'z')) t(x) ORDER BY x COLLATE ${collation}, x COLLATE Latin1_General_BIN2`)
  add('collate', `${collation} distinct`, `SELECT COUNT(DISTINCT x COLLATE ${collation}) AS n FROM (VALUES (N'b'),(N'a'),(N'B'),(N'A'),(N'é'),(N'e'),(N'É')) t(x)`)
  add('collate', `${collation} projection`, `SELECT N'Ab' COLLATE ${collation} AS v`)
}
add('collate', 'invalid collation', "SELECT N'a' COLLATE Foo_Bar AS v")
add('collate', 'numeric collate', 'SELECT 1 COLLATE Latin1_General_CI_AS AS v')
add('collate', 'conflict', "SELECT CASE WHEN N'a' COLLATE Latin1_General_CI_AS = N'A' COLLATE Latin1_General_CS_AS THEN 1 ELSE 0 END AS v")
add('collate', 'column where', "CREATE TABLE dbo.conversion_names (s nvarchar(10)); INSERT dbo.conversion_names VALUES (N'Alpha'),(N'alpha'),(N'ALPHA '),(N'beta'); SELECT COUNT(*) AS n FROM dbo.conversion_names WHERE s = N'alpha' COLLATE Latin1_General_CI_AS")
add('collate', 'like', "SELECT CASE WHEN N'Alpha' LIKE N'al%' COLLATE Latin1_General_CI_AS THEN 1 ELSE 0 END AS ci, CASE WHEN N'Alpha' LIKE N'al%' COLLATE Latin1_General_CS_AS THEN 1 ELSE 0 END AS cs")
add('collate', 'in list', "SELECT CASE WHEN N'Alpha' COLLATE Latin1_General_CI_AS IN (N'ALPHA', N'x') THEN 1 ELSE 0 END AS v")
add('collate', 'group by', "SELECT COUNT(*) AS n FROM (SELECT x COLLATE Latin1_General_CI_AS AS k FROM (VALUES (N'b'),(N'a'),(N'B'),(N'A')) t(x)) s GROUP BY k")

// Server and database properties.
const server = ['Collation', 'CollationID', 'ComparisonStyle', 'Edition', 'EditionID', 'EngineEdition', 'InstanceName',
  'IsCaseSensitive', 'IsClustered', 'IsFullTextInstalled', 'IsHadrEnabled', 'IsIntegratedSecurityOnly', 'IsLocalDB',
  'IsSingleUser', 'IsXTPSupported', 'LCID', 'LicenseType', 'NumLicenses', 'ProductLevel', 'ProductMajorVersion',
  'ProductMinorVersion', 'ProductBuild', 'ProductUpdateLevel', 'SqlCharSet', 'SqlCharSetName', 'SqlSortOrder',
  'SqlSortOrderName', 'NoSuchProperty']
for (const property of server) {
  add('serverproperty', property, `SELECT SERVERPROPERTY('${property}') AS v, SQL_VARIANT_PROPERTY(SERVERPROPERTY('${property}'),'BaseType') AS t, SQL_VARIANT_PROPERTY(SERVERPROPERTY('${property}'),'MaxLength') AS l`)
}
add('serverproperty', 'shape of machine dependent values', "SELECT SQL_VARIANT_PROPERTY(SERVERPROPERTY('ServerName'),'BaseType') AS a, SQL_VARIANT_PROPERTY(SERVERPROPERTY('MachineName'),'BaseType') AS b, SQL_VARIANT_PROPERTY(SERVERPROPERTY('ProductVersion'),'BaseType') AS c, SQL_VARIANT_PROPERTY(SERVERPROPERTY('ProcessID'),'BaseType') AS d, CASE WHEN SERVERPROPERTY('ServerName') = SERVERPROPERTY('MachineName') THEN 1 ELSE 0 END AS e")
add('serverproperty', 'converted', "SELECT CONVERT(nvarchar(128), SERVERPROPERTY('Edition')) AS e, CAST(SERVERPROPERTY('EngineEdition') AS int) AS n, CASE WHEN SERVERPROPERTY('EngineEdition') = 3 THEN 1 ELSE 0 END AS eq")
add('serverproperty', 'null argument', 'SELECT SERVERPROPERTY(NULL) AS v')
add('serverproperty', 'variable argument', "DECLARE @p nvarchar(128) = N'EngineEdition'; SELECT SERVERPROPERTY(@p) AS v")
add('serverproperty', 'no arguments', 'SELECT SERVERPROPERTY() AS v')
add('serverproperty', 'integer argument', 'SELECT SERVERPROPERTY(1) AS v')
const database = ['Collation', 'ComparisonStyle', 'IsAutoClose', 'IsAutoShrink', 'IsAutoCreateStatistics', 'IsAutoUpdateStatistics',
  'IsAnsiNullDefault', 'IsAnsiNullsEnabled', 'IsAnsiPaddingEnabled', 'IsAnsiWarningsEnabled', 'IsArithmeticAbortEnabled',
  'IsFulltextEnabled', 'IsQuotedIdentifiersEnabled', 'IsRecursiveTriggersEnabled', 'LCID', 'Recovery', 'SQLSortOrder',
  'Status', 'Updateability', 'UserAccess', 'Version', 'NoSuchProperty']
for (const property of database) {
  add('databasepropertyex', property, `SELECT DATABASEPROPERTYEX(DB_NAME(), '${property}') AS v, SQL_VARIANT_PROPERTY(DATABASEPROPERTYEX(DB_NAME(), '${property}'),'BaseType') AS t, SQL_VARIANT_PROPERTY(DATABASEPROPERTYEX(DB_NAME(), '${property}'),'MaxLength') AS l`)
}
add('databasepropertyex', 'master', "SELECT DATABASEPROPERTYEX('master','Status') AS s, DATABASEPROPERTYEX('MASTER','Updateability') AS u, DATABASEPROPERTYEX('master','Recovery') AS r")
add('databasepropertyex', 'missing database', "SELECT DATABASEPROPERTYEX('no_such_database','Status') AS v")
add('databasepropertyex', 'null arguments', "SELECT DATABASEPROPERTYEX(NULL,'Status') AS a, DATABASEPROPERTYEX('master',NULL) AS b")
add('databasepropertyex', 'one argument', "SELECT DATABASEPROPERTYEX('master') AS v")
add('databasepropertyex', 'column argument', "SELECT name, DATABASEPROPERTYEX(name,'Status') AS s FROM sys.databases WHERE name IN ('master','tempdb') ORDER BY name")
add('databasepropertyex', 'read only', "DECLARE @d nvarchar(128) = DB_NAME(); EXEC('ALTER DATABASE [' + @d + '] SET READ_ONLY'); SELECT DATABASEPROPERTYEX(DB_NAME(),'Updateability') AS v; EXEC('ALTER DATABASE [' + @d + '] SET READ_WRITE')")

// ROWCOUNT_BIG.
add('rowcount_big', 'after select', 'SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3; SELECT ROWCOUNT_BIG() AS n, SQL_VARIANT_PROPERTY(ROWCOUNT_BIG(),\'BaseType\') AS t')
add('rowcount_big', 'after set', 'DECLARE @x int; SET @x = 1; SELECT ROWCOUNT_BIG() AS n')
add('rowcount_big', 'after dml', 'CREATE TABLE dbo.conversion_rows (i int); INSERT dbo.conversion_rows VALUES (1),(2); SELECT ROWCOUNT_BIG() AS n, @@ROWCOUNT AS m')
add('rowcount_big', 'with argument', 'SELECT ROWCOUNT_BIG(1) AS n')

// Run cases in a fresh database of a reference container.
async function run(config, selected) {
  return isolatedReference(config, async connection => {
    const version = (await capture(connection, "SELECT CONVERT(nvarchar(128), SERVERPROPERTY('ProductVersion')) AS v")).sets[0].rows[0][0]
    const captured = []
    for (const entry of selected) {
      const result = await capture(connection, entry.sql)
      captured.push({
        ...entry,
        sets: result.sets.map(set => ({
          columns: set.columns.map(c => ({ name: c.name, type: c.type, length: c.length, precision: c.precision, scale: c.scale })),
          rows: canonical(set.rows),
        })),
        errors: result.errors.map(e => ({ number: e.number, state: e.state, class: e.class, message: e.message })),
      })
    }
    return { version, captured }
  })
}

// Label each owned container with this task, so parallel workers can tell
// them apart; `extra` adds container options.
const docker = extra => async (args, env) => {
  const { execFile } = await import('node:child_process')
  const { promisify } = await import('node:util')
  const command = args[0] === 'run' ? ['run', '--label', 'msduck.task=gaps-conversion-v1', ...extra, ...args.slice(1)] : args
  return (await promisify(execFile)('docker', command, { env: { ...process.env, ...env }, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()
}

const main = await withReferenceContainer(async (config, container) => ({ image: container.image, ...await run(config, cases) }), { docker: docker([]) })
await writeNewFixture(output, { image: main.image, version: main.version, timeZone: 'UTC', cases: main.captured })
console.log('Captured', main.captured.length, 'cases from', main.version)
