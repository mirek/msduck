// Explicit COLLATE, styled CONVERT, FORMAT, SERVERPROPERTY, DATABASEPROPERTYEX
// and ROWCOUNT_BIG (issue #725) through tedious, against the SQL Server
// captures in reference/gaps-conversion.json and reference/format.json.
// Remaining differences are listed by name, so a regression and a fix both
// show up here (docs/gaps-conversion.md explains each one).
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'
import { start, query } from '../support/client.mjs'

const fixture = JSON.parse(readFileSync(new URL('../../reference/gaps-conversion.json', import.meta.url)))

// Cases whose rows or error numbers differ from SQL Server, by reason.
// smalldatetime values keep their seconds in msduck storage, so every case
// whose value has seconds differs.
const smalldatetime = /^smalldatetime (style|round trip) /
const known = new Map([
  ['SQL_Latin1_General_CP1_CS_AS comparisons', 'varchar uses Windows instead of SQL sort order'],
  ['Latin1_General_BIN2 order', 'duplicate ORDER BY items are not rejected (169)'],
  ['IsXTPSupported', 'no In-Memory OLTP'],
  ['ProductBuild', 'msduck version'],
  ['ProductUpdateLevel', 'msduck version'],
  ['IsFulltextEnabled', 'no full-text search'],
  ['Recovery', 'user databases report the SIMPLE recovery model'],
  ['column argument', 'no tempdb database'],
  ['read only', 'EXEC of a string'],
])

function outcome(result) {
  return { rows: canonical(result.sets.map(set => set.rows)), errors: result.errors.map(e => e.number) }
}

test('styled CONVERT, COLLATE and properties match the SQL Server capture', async t => {
  const connection = await start(t, { options: { requestTimeout: 60000 } })
  const cases = fixture.cases
  const differences = []
  let compileTime = 0
  for (const entry of cases) {
    const local = outcome(await capture(connection, entry.sql))
    const reference = { rows: entry.sets.map(set => set.rows), errors: entry.errors.map(e => e.number) }
    if (JSON.stringify(local) === JSON.stringify(reference)) continue
    // msduck raises errors for constant arguments when the statement
    // compiles, before the result metadata SQL Server sends first.
    if (JSON.stringify(local.errors) === JSON.stringify(reference.errors) && local.errors.length &&
        local.rows.length === 0 && reference.rows.every(rows => rows.length === 0)) {
      compileTime++
      continue
    }
    differences.push(entry.name)
  }
  assert.deepEqual(differences.filter(name => !known.has(name) && !smalldatetime.test(name)), [])
  assert.deepEqual([...known.keys()].filter(name => !differences.includes(name)), [])
  assert.ok(compileTime > 0 && compileTime < 250, `${compileTime} compile-time errors`)
})

// Split a SELECT list on top-level commas.
function items(list) {
  const out = []
  let depth = 0, quoted = false, current = ''
  for (const ch of list) {
    if (ch === "'") quoted = !quoted
    if (!quoted && ch === '(') depth++
    if (!quoted && ch === ')') depth--
    if (!quoted && depth === 0 && ch === ',') { out.push(current); current = ''; continue }
    current += ch
  }
  out.push(current)
  return out
}

test('FORMAT matches SQL Server for en-US and the invariant culture', async t => {
  const connection = await start(t, { options: { requestTimeout: 60000 } })
  const format = JSON.parse(readFileSync(new URL('../../reference/format.json', import.meta.url)))
  const other = /'(de-DE|fr-FR|ja-JP|ar-SA|de|fr|ja|ar|de-CH|en-IN|hi-IN|zh-Hans|sv-SE|sr-Latn-RS|zh-TW|qps-ploc|de-de|DE-DE|de_DE|en-US-POSIX)'/
  const differences = []
  let compared = 0
  for (const program of Object.values(format.containers[0].runs[0])) {
    // Single-statement SELECT lists of FORMAT calls without parameters.
    if (!/^SELECT FORMAT/.test(program.sql ?? '') || / FROM |@/i.test(program.sql) || program.result.errors.length) continue
    const list = items(program.sql.slice('SELECT '.length))
    for (const [index, item] of list.entries()) {
      if (other.test(item)) continue
      const result = await capture(connection, `SELECT ${item}`)
      const local = result.errors.length ? `error ${result.errors[0].number}` : canonical(result.sets[0].rows[0][0])
      const reference = canonical(program.result.sets[0].rows[0][index])
      compared++
      if (JSON.stringify(local) !== JSON.stringify(reference)) differences.push(item.replace(/ AS \w+$/, ''))
    }
  }
  assert.ok(compared > 500, `${compared} FORMAT calls compared`)
  assert.deepEqual(differences.filter(item => !/SMALLDATETIME|DECIMAL\(38,38\)/.test(item)), [])
})

test('generic repros return SQL Server types', async t => {
  const connection = await start(t)
  const { rows, columns } = await query(connection, `SELECT
    CASE WHEN N'A' = N'a' COLLATE Latin1_General_CI_AS THEN 1 ELSE 0 END AS ci,
    CASE WHEN N'A' = N'a' COLLATE Latin1_General_CS_AS THEN 1 ELSE 0 END AS cs,
    CONVERT(nvarchar(40), CAST('2024-01-02 03:04:05.1234567 +05:30' AS datetimeoffset), 127) AS dto,
    CONVERT(varchar(30), CAST('2024-01-02 03:04:05.123' AS datetime), 126) AS dt,
    CONVERT(varchar(6), 0x010203, 2) AS hex,
    FORMAT(CAST('2024-01-01' AS date), 'yyyy-MM-dd') AS formatted,
    SERVERPROPERTY('Collation') AS collation,
    SERVERPROPERTY('EngineEdition') AS edition,
    DATABASEPROPERTYEX(DB_NAME(), 'Status') AS status,
    SQL_VARIANT_PROPERTY(SERVERPROPERTY('ProductVersion'), 'BaseType') AS base,
    ROWCOUNT_BIG() AS row_count`)
  assert.deepEqual(rows, [[1, 0, '2024-01-01T21:34:05.1234567Z', '2024-01-02T03:04:05.123', '010203', '2024-01-01',
    'SQL_Latin1_General_CP1_CI_AS', 3, 'ONLINE', 'nvarchar', '0']])
  const types = columns[0].map(c => `${c.type.name}(${c.dataLength})`)
  assert.deepEqual(types.slice(2), ['NVarChar(80)', 'VarChar(30)', 'VarChar(6)', 'NVarChar(8000)',
    'Variant(8009)', 'Variant(8009)', 'Variant(8009)', 'Variant(8009)', 'IntN(8)'])
})

test('collations apply to columns and RPC parameters', async t => {
  const connection = await start(t)
  await query(connection, "CREATE TABLE dbo.names (id int, s nvarchar(20)); INSERT dbo.names VALUES (1,N'Alpha'),(2,N'alpha'),(3,N'ALPHA '),(4,N'Élan'),(5,N'elan'),(6,N'beta')")
  const ids = async (sql, parameters) => (await query(connection, sql, parameters)).rows.map(r => r[0])
  assert.deepEqual(await ids("SELECT id FROM dbo.names WHERE s COLLATE Latin1_General_CI_AS = @p ORDER BY id", [['p', TYPES.NVarChar, 'alpha']]), [1, 2, 3])
  assert.deepEqual(await ids("SELECT id FROM dbo.names WHERE s = @p COLLATE Latin1_General_CS_AS ORDER BY id", [['p', TYPES.NVarChar, 'alpha']]), [2])
  assert.deepEqual(await ids("SELECT id FROM dbo.names WHERE s COLLATE Latin1_General_CI_AI = N'ELAN' ORDER BY id"), [4, 5])
  assert.deepEqual(await ids("SELECT id FROM dbo.names WHERE s COLLATE Latin1_General_CI_AS IN (N'ALPHA', N'BETA') ORDER BY id"), [1, 2, 3, 6])
  assert.deepEqual(await ids("SELECT id FROM dbo.names WHERE s COLLATE Latin1_General_CI_AS LIKE N'al%' ORDER BY id"), [1, 2, 3])
  assert.deepEqual(await ids("SELECT id FROM dbo.names ORDER BY s COLLATE Latin1_General_CS_AS, id"), [2, 1, 3, 6, 5, 4])
  await assert.rejects(query(connection, "SELECT N'a' COLLATE Foo_Bar"), error => error.number === 448)
})

test('styled conversions bind RPC parameters', async t => {
  const connection = await start(t)
  const { rows } = await query(connection,
    'SELECT CONVERT(varchar(30), @d, 121), CONVERT(varchar(30), @d, 103), CONVERT(varbinary(10), @s, 1), CONVERT(datetime2, @t, 103), FORMAT(@n, @f, @c)', [
      ['d', TYPES.DateTime2, new Date('2024-01-02T03:04:05.123Z'), { scale: 3 }],
      ['s', TYPES.NVarChar, '0x0A0B'],
      ['t', TYPES.VarChar, '02/01/2024'],
      ['n', TYPES.Int, 1234],
      ['f', TYPES.NVarChar, 'N2'],
      ['c', TYPES.NVarChar, 'iv'],
    ])
  assert.equal(rows[0][1], '02/01/2024')
  assert.deepEqual(rows[0][2], Buffer.from([10, 11]))
  assert.equal(rows[0][3].toISOString(), '2024-01-02T00:00:00.000Z')
  assert.equal(rows[0][4], '1,234.00')
  assert.match(rows[0][0], /^2024-01-02 03:04:05\.123/)
})

test('bit converts to 1 and 0 text', async t => {
  const connection = await start(t)
  const { rows } = await query(connection, 'SELECT CONVERT(nvarchar, CAST(1 AS bit)), CAST(CAST(0 AS bit) AS varchar), CONVERT(varchar(1), @b)', [['b', TYPES.Bit, true]])
  assert.deepEqual(rows, [['1', '0', '1']])
})

test('date and time cast to text with SQL Server default styles', async t => {
  const connection = await start(t)
  const { rows } = await query(connection, "SELECT CAST(CAST('2024-01-02 03:04:05.1234567' AS datetime2) AS varchar(40)), CAST(CAST('2024-01-02 03:04:05.1234567 +05:30' AS datetimeoffset) AS nvarchar(40)), CAST(CAST('2024-01-02 03:04:05.123' AS datetime) AS varchar(30)), CONVERT(varchar(30), @d)", [['d', TYPES.DateTime2, new Date('2024-01-02T03:04:05.120Z'), { scale: 3 }]])
  assert.deepEqual(rows, [['2024-01-02 03:04:05.1234567', '2024-01-02 03:04:05.1234567 +05:30', 'Jan  2 2024  3:04AM', '2024-01-02 03:04:05.120']])
})
