// The database's default collation (SQL_Latin1_General_CP1_CI_AS) and column
// collations (issue #868) through tedious, against the SQL Server capture in
// reference/default-collation.json. Remaining differences are listed by name,
// so a regression and a fix both show up here (docs/unicode-collation.md
// explains each one).
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'
import { start, query } from '../support/client.mjs'

const fixture = JSON.parse(readFileSync(new URL('../../reference/default-collation.json', import.meta.url)))

// Cases that differ from SQL Server, and why: each differing field with a
// digest of msduck's current value, so a further change to it shows up too.
// Every other field of these cases must match SQL Server exactly.
const known = new Map([
  ['ignorable unicode units', [{ rows: 'cf1cbb66a638b486', errors: '7c5585b9d3735287' }, 'NCHAR of a surrogate code unit is unsupported']],
  ['surrogate pair', [{ columns: '4f53cda18c2baa0c', rows: '4f53cda18c2baa0c', errors: '7c5585b9d3735287' }, 'NCHAR of a surrogate code unit is unsupported']],
  ['accented letter order', [{ rows: 'ed6338449bc566d6' }, 'values derived from literals (VALUES) sort by code point']],
  ['ordering weights', [{ rows: '7045db03e0a7ec48' }, 'punctuation and digits follow code points, not SQL Server sort weights']],
  ['group by', [{ rows: '376c6c4fd566b688' }, 'grouping by an expression of a column keeps DuckDB grouping (trailing spaces)']],
  ['alter table add unique', [{ errors: '86db6c41a158b5de' }, 'ALTER TABLE ADD UNIQUE over nvarchar is unsupported (constraints)']],
  ['catalog', [{ columns: 'ff93e68ebc34ca4b' }, 'sys.columns names are nvarchar(max), and the _SC_UTF8 column keeps the default descriptor']],
  ['add column', [{ columns: 'b7fa9741740ab3e1' }, 'sys.columns.collation_name is nvarchar(max), not sysname']],
])

const digest = value => createHash('sha256').update(JSON.stringify(value)).digest('hex').slice(0, 16)

// System-generated constraint names end in a random hexadecimal suffix.
const message = text => text.replace(/__[0-9A-F]{16}'/g, "__<hash>'")

// Result descriptors: name, type, length and the TDS collation fields.
const describe = column => [column.name, column.type, column.length,
  column.collation?.sortId === undefined ? null
    : [column.collation.lcid, column.collation.flags, column.collation.version, column.collation.sortId]]

function outcome(result) {
  return {
    columns: result.sets.map(set => set.columns.map(describe)),
    rows: canonical(result.sets.map(set => set.rows)),
    errors: result.errors.map(e => [e.number, e.state, e.class, message(e.message)]),
  }
}

test('comparisons, grouping, keys and column collations match the SQL Server capture', async t => {
  const connection = await start(t, { options: { requestTimeout: 60000 } })
  const problems = []
  const seen = new Set()
  for (const entry of fixture.cases) {
    const local = outcome(await capture(connection, entry.sql))
    const reference = {
      columns: entry.sets.map(set => set.columns.map(describe)),
      rows: entry.sets.map(set => set.rows),
      errors: entry.errors.map(e => [e.number, e.state, e.class, message(e.message)]),
    }
    const [fields = {}] = known.get(entry.name) ?? []
    for (const field of ['columns', 'rows', 'errors']) {
      const same = JSON.stringify(local[field]) === JSON.stringify(reference[field])
      if (field in fields) {
        if (same) problems.push(`${entry.name}: ${field} now matches`)
        else if (digest(local[field]) !== fields[field]) problems.push(`${entry.name}: ${field} changed to ${JSON.stringify(local[field]).slice(0, 300)}`)
        else seen.add(entry.name)
      } else if (!same) {
        problems.push(`${entry.name}: ${field} ${JSON.stringify(local[field]).slice(0, 300)}`)
      }
    }
    for (const [, table] of entry.sql.matchAll(/CREATE TABLE dbo\.(\w+)/g)) {
      await capture(connection, `DROP TABLE IF EXISTS dbo.${table}`)
    }
  }
  assert.deepEqual(problems, [])
  assert.deepEqual([...known.keys()].filter(name => !seen.has(name)), [])
})

test('the reported reproductions behave as in SQL Server', async t => {
  const connection = await start(t)
  assert.deepEqual((await query(connection, "SELECT CASE WHEN N'A'=N'a' THEN 1 ELSE 0 END")).rows, [[1]])
  await query(connection, 'CREATE TABLE items(value nvarchar(100) UNIQUE)')
  await query(connection, "INSERT items VALUES (N'Foo')")
  await assert.rejects(query(connection, "INSERT items VALUES (N'foo')"), error =>
    error.number === 2627 && error.state === 1 && error.class === 14 &&
    /Violation of UNIQUE KEY constraint 'UQ__items__[0-9A-F]{16}'\. Cannot insert duplicate key in object 'dbo\.items'\. The duplicate key value is \(foo\)\./.test(error.message))
  assert.deepEqual((await query(connection, 'SELECT value FROM items')).rows, [['Foo']])
})

test('case-insensitive keys report the duplicate as written', async t => {
  const connection = await start(t)
  await query(connection, "CREATE TABLE dbo.tags (name varchar(20) NOT NULL PRIMARY KEY, label nvarchar(20) NULL UNIQUE)")
  await query(connection, "INSERT dbo.tags VALUES ('Red', N'Ruby')")
  await assert.rejects(query(connection, "INSERT dbo.tags VALUES ('RED', N'other')"), error => error.number === 2627 && /\(RED\)/.test(error.message))
  await assert.rejects(query(connection, "INSERT dbo.tags VALUES ('blue', N'RUBY  ')"), error => error.number === 2627 && /\(RUBY  \)/.test(error.message))
  assert.deepEqual((await query(connection, "SELECT name FROM dbo.tags WHERE label = N'ruby' AND name LIKE 'r%'")).rows, [['Red']])
  // Keys added by ALTER TABLE compare the same way, and go with the constraint.
  await query(connection, "CREATE TABLE dbo.codes (code varchar(10) NOT NULL); INSERT dbo.codes VALUES ('abc')")
  await query(connection, 'ALTER TABLE dbo.codes ADD CONSTRAINT uq_codes UNIQUE (code)')
  await assert.rejects(query(connection, "INSERT dbo.codes VALUES ('ABC')"), error => error.number === 2627 && /'uq_codes'.*\(ABC\)/.test(error.message))
  await query(connection, 'ALTER TABLE dbo.codes DROP CONSTRAINT uq_codes')
  await query(connection, "INSERT dbo.codes VALUES ('ABC')")
  assert.deepEqual((await query(connection, "SELECT count(*) FROM dbo.codes WHERE code = 'Abc'")).rows, [[2]])
})

test('LIKE, DISTINCT counts and extrema follow the column collations', async t => {
  const connection = await start(t)
  // ASCII LIKE ignores the value's trailing blanks, not the pattern's.
  assert.deepEqual((await query(connection, "SELECT CASE WHEN 'a' LIKE 'a ' THEN 1 ELSE 0 END, CASE WHEN 'a ' LIKE 'A' THEN 1 ELSE 0 END, CASE WHEN N'a ' LIKE N'A' THEN 1 ELSE 0 END")).rows, [[0, 1, 0]])
  await query(connection, "CREATE TABLE dbo.agg (n nvarchar(10), v varchar(10), cs nvarchar(10) COLLATE Latin1_General_CS_AS, ai varchar(10) COLLATE Latin1_General_CI_AI); INSERT dbo.agg VALUES (N'Foo', 'Foo', N'a', 'Fóo'), (N'foo', 'foo', N'A', 'foo'), (N'FOO  ', 'FOO  ', N'a  ', 'FOO'), (N'bar', 'bar', NULL, NULL)")
  assert.deepEqual((await query(connection, 'SELECT count(DISTINCT n), count(DISTINCT v), count(DISTINCT cs), count(DISTINCT ai) FROM dbo.agg')).rows, [[2, 2, 2, 1]])
  assert.deepEqual((await query(connection, 'SELECT MIN(n), UPPER(MAX(n)), MIN(v), UPPER(RTRIM(MAX(v))) FROM dbo.agg')).rows, [['bar', 'FOO', 'bar', 'FOO']])
  assert.deepEqual((await query(connection, 'SELECT MIN(DISTINCT n), UPPER(MAX(DISTINCT n)) FROM dbo.agg')).rows, [['bar', 'FOO']])
  assert.deepEqual((await query(connection, 'SELECT n, count(*) FROM dbo.agg GROUP BY n HAVING count(*) > 1')).rows.map(([, c]) => c), [3])
  assert.deepEqual((await query(connection, 'SELECT count(*) FROM (SELECT DISTINCT n FROM dbo.agg) d')).rows, [[2]])
  // ANSI values ignore no unit: CHAR(0) still distinguishes them.
  await query(connection, "CREATE TABLE dbo.nul (v varchar(10), n nvarchar(10)); INSERT dbo.nul VALUES ('ab', N'ab'), ('a' + CHAR(0) + 'b', N'a' + NCHAR(0) + N'b')")
  assert.deepEqual((await query(connection, 'SELECT count(DISTINCT v) FROM dbo.nul')).rows, [[2]])
  assert.deepEqual((await query(connection, 'SELECT count(*) FROM (SELECT DISTINCT v FROM dbo.nul) d')).rows, [[2]])
  assert.deepEqual((await query(connection, 'SELECT count(*) FROM (SELECT n FROM dbo.nul GROUP BY n) g')).rows, [[1]])
})

test('duplicates show each key column as written, and ORDER BY resolves qualified columns', async t => {
  const connection = await start(t)
  await query(connection, "CREATE TABLE dbo.pair (a nvarchar(10) NOT NULL, b varchar(10) NOT NULL, CONSTRAINT uq_pair UNIQUE (a, b)); INSERT dbo.pair VALUES (N'base', 'base')")
  await assert.rejects(query(connection, "INSERT dbo.pair VALUES (N'BASE', 'Base')"), error => error.number === 2627 && /\(BASE, Base\)/.test(error.message))
  await assert.rejects(query(connection, "INSERT dbo.pair (b, a) VALUES ('bAse', N'baSE')"), error => error.number === 2627 && /\(baSE, bAse\)/.test(error.message))
  await assert.rejects(query(connection, "INSERT dbo.pair (a, b) SELECT N'BASE', 'Base'"), error => error.number === 2627 && /\(BASE, Base\)/.test(error.message))
  await query(connection, "INSERT dbo.pair VALUES (N'x', 'x')")
  await assert.rejects(query(connection, "UPDATE dbo.pair SET a = N'BASE', b = 'BASE' WHERE a = N'x'"), error => error.number === 2627 && /\(BASE, BASE\)/.test(error.message))
  await query(connection, "CREATE TABLE dbo.left_side (id int, v nvarchar(10) COLLATE Latin1_General_CS_AS); CREATE TABLE dbo.right_side (id int, v int); INSERT dbo.left_side VALUES (1, N'b'), (2, N'B'); INSERT dbo.right_side VALUES (1, 20), (2, 10)")
  assert.deepEqual((await query(connection, 'SELECT r.id FROM dbo.left_side l JOIN dbo.right_side r ON l.id = r.id ORDER BY r.v')).rows, [[2], [1]])
  assert.deepEqual((await query(connection, 'SELECT l.id FROM dbo.left_side l JOIN dbo.right_side r ON l.id = r.id ORDER BY l.v')).rows, [[1], [2]])
  // Case-sensitive CHAR/VARCHAR keys still ignore trailing spaces.
  await query(connection, "CREATE TABLE dbo.cs_keys (v varchar(10) COLLATE Latin1_General_CS_AS NOT NULL UNIQUE); INSERT dbo.cs_keys VALUES ('a'), ('A')")
  await assert.rejects(query(connection, "INSERT dbo.cs_keys VALUES ('a  ')"), error => error.number === 2627 && /\(a  \)/.test(error.message))
  await query(connection, "CREATE TABLE dbo.cs_added (v varchar(10) COLLATE Latin1_General_CS_AS NOT NULL); INSERT dbo.cs_added VALUES ('b'); ALTER TABLE dbo.cs_added ADD CONSTRAINT uq_cs_added UNIQUE (v)")
  await assert.rejects(query(connection, "INSERT dbo.cs_added VALUES ('b ')"), error => error.number === 2627 && /'uq_cs_added'.*\(b \)/.test(error.message))
  await query(connection, "INSERT dbo.cs_added VALUES ('B')")
  // ANSI ranges space-pad their operands.
  assert.deepEqual((await query(connection, "SELECT count(*) FROM dbo.cs_keys WHERE v BETWEEN 'a  ' AND 'a ' COLLATE Latin1_General_CS_AS")).rows, [[1]])
  await query(connection, "CREATE TABLE dbo.plain_text (v varchar(10)); INSERT dbo.plain_text VALUES ('a')")
  assert.deepEqual((await query(connection, "SELECT count(*) FROM dbo.plain_text WHERE v BETWEEN 'A ' AND 'a  ' AND v >= 'A  ' AND v <= 'a'")).rows, [[1]])
  // NULLIF compares under its first argument's collation.
  assert.deepEqual((await query(connection, "CREATE TABLE dbo.nullif_cs (v nvarchar(5) COLLATE Latin1_General_CS_AS); INSERT dbo.nullif_cs VALUES (N'a'); SELECT NULLIF(v, N'A'), NULLIF(v, N'a') FROM dbo.nullif_cs")).rows, [['a', null]])
  // IN (subquery) keeps a column's own collation, on either side.
  assert.deepEqual((await query(connection, "SELECT count(*) FROM dbo.cs_keys WHERE v IN (SELECT 'a')")).rows, [[1]])
  assert.deepEqual((await query(connection, "SELECT CASE WHEN 'A' IN (SELECT k.v FROM dbo.cs_keys k WHERE k.v = 'a') THEN 1 ELSE 0 END")).rows, [[0]])
  // Conflicting column collations in DML predicates, LIKE included.
  await query(connection, "CREATE TABLE dbo.two (ci nvarchar(10) COLLATE Latin1_General_CI_AS, d nvarchar(10)); INSERT dbo.two VALUES (N'a', N'A')")
  for (const sql of ["UPDATE dbo.two SET ci = N'b' WHERE ci = d", "DELETE FROM dbo.two WHERE d LIKE ci"]) {
    await assert.rejects(query(connection, sql), error => error.number === 468 && error.state === 9 && /Latin1_General_CI_AS/.test(error.message), sql)
  }
  assert.deepEqual((await query(connection, 'SELECT ci FROM dbo.two')).rows, [['a']])
})

