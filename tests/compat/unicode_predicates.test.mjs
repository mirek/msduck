// Comparisons, LIKE, ordering, concatenation and character conversions over
// NVARCHAR/NCHAR (Unicode carrier) columns (docs/gaps-unicode-predicates.md).
// Expected rows and diagnostics come from reference/gaps-unicode-predicates.json,
// captured from SQL Server in a Latin1_General_100_BIN2 database (msduck's
// default binary comparison) by scripts/capture-gaps-unicode-predicates.mjs.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { start, query } from '../support/client.mjs'
import { canonical } from '../../scripts/lib/compatibility.mjs'
import { cases, run } from '../../scripts/capture-gaps-unicode-predicates.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/gaps-unicode-predicates.json', import.meta.url), 'utf8'))

let databases = 0
async function fresh(t) {
  const connection = await start(t)
  const name = `unicode_predicates_${process.pid}_${databases++}`
  await query(connection, `CREATE DATABASE ${name}`)
  await query(connection, `USE ${name}`)
  return connection
}

const outcome = result => ({
  rows: result.sets.map(set => set.rows),
  errors: result.errors.map(e => [e.number, e.state, e.class, e.message]),
  types: result.sets.map(set => set.columns.map(c => c.type)),
  lengths: result.sets.map(set => set.columns.map(c => c.length)),
})

// Known differences, kept explicit rather than normalized away:
// - an invalid LIKE escape fails before execution, so msduck sends no
//   column metadata ahead of error 506;
// - result lengths of some character expressions are not inferred and
//   fall back to nvarchar(max) (CONCAT over columns, COALESCE of nvarchar
//   and varchar, set operations mixing them, REPLACE, SUBSTRING, REVERSE
//   and STRING_AGG).
const noMetadata = new Set(['like#17'])
const maxLength = {
  'concatenation-and-conversion#5': [null, 65535, 4, 4, 42, 6],
  'concatenation-and-conversion#8': [null, 40, 65535, 40, 40, 40, 40],
  'concatenation-and-conversion#9': [65535],
  'concatenation-and-conversion#10': [null, 65535],
  'concatenation-and-conversion#11': [65535, 65535, 65535],
}

for (const [id, statements] of cases) {
  test(`SQL Server ${id}`, async t => {
    const c = await fresh(t)
    const captured = reference.cases.find(entry => entry.id === id)
    for (const [index, step] of statements.entries()) {
      const label = `${id}#${index}`
      const actual = outcome(canonical(await run(c, step)))
      const expected = outcome(captured.steps[index].result)
      const sql = typeof step === 'string' ? step : step.sql
      assert.deepEqual(actual.errors, expected.errors, `${label} ${sql}`)
      if (noMetadata.has(label)) {
        assert.deepEqual(actual.rows, [], `${label} ${sql}`)
        continue
      }
      assert.deepEqual(actual.rows, expected.rows, `${label} ${sql}`)
      assert.deepEqual(actual.types, expected.types, `${label} ${sql}`)
      if (typeof step === 'string') {
        assert.deepEqual(actual.lengths, maxLength[label] ? [maxLength[label]] : expected.lengths, `${label} ${sql}`)
      }
    }
  })
}

test('filters, updates and LIKE over nvarchar columns no longer fail', async t => {
  const c = await fresh(t)
  await query(c, "CREATE TABLE dbo.items(id int, name nvarchar(50)); INSERT dbo.items VALUES(1,N'x'),(2,N'y'),(3,N'x  ')")
  const ids = async (sql, parameters) => (await query(c, sql, parameters)).rows
  assert.deepEqual(await ids("SELECT id FROM dbo.items WHERE name = N'x' ORDER BY id"), [[1], [3]])
  assert.deepEqual(await ids("SELECT i.id FROM dbo.items AS i WHERE i.name = N'x' ORDER BY i.id"), [[1], [3]])
  assert.deepEqual(await ids("SELECT id FROM dbo.items WHERE name = 'y'"), [[2]])
  assert.deepEqual(await ids("DECLARE @n nvarchar(50) = N'y'; SELECT id FROM dbo.items WHERE name = @n"), [[2]])
  assert.deepEqual(await ids("SELECT id FROM dbo.items WHERE name LIKE N'x%' ORDER BY id"), [[1], [3]])
  assert.deepEqual(await ids("SELECT id FROM dbo.items WHERE ISNULL(name, N'') = N'x' AND UPPER(name) <> N'Y' ORDER BY id"), [[1], [3]])
  assert.deepEqual(await ids('SELECT id FROM dbo.items WHERE name = @p', [['p', TYPES.NVarChar, 'y']]), [[2]])
  assert.deepEqual(await ids('SELECT id FROM dbo.items WHERE name < @p ORDER BY id', [['p', TYPES.VarChar, 'y']]), [[1], [3]])
  await query(c, "UPDATE dbo.items SET id = id + 10 WHERE name = N'x'")
  assert.deepEqual(await ids('SELECT id FROM dbo.items ORDER BY id'), [[2], [11], [13]])
  await query(c, "DELETE FROM dbo.items WHERE name LIKE N'y'")
  assert.deepEqual(await ids('SELECT id, name FROM dbo.items ORDER BY name, id'), [[11, 'x'], [13, 'x  ']])
})

test('concatenation, CAST and CONVERT produce text, not the carrier STRUCT', async t => {
  const c = await fresh(t)
  await query(c, "CREATE TABLE t(n nvarchar(20)); INSERT t VALUES (N'ab')")
  const one = async sql => (await query(c, sql)).rows[0]
  assert.deepEqual(await one(`SELECT CONVERT(nvarchar(200), JSON_VALUE(N'{"value":"abc"}', N'$.value'))`), ['abc'])
  assert.deepEqual(await one("SELECT n + N'!', CONVERT(nvarchar(5), n), CAST(n AS nvarchar(1)), UPPER(n), LEN(n) FROM t"), ['ab!', 'ab', 'a', 'AB', 2])
  assert.deepEqual(await one("DECLARE @x nvarchar(20); SELECT @x = n FROM t; SELECT @x + N'!'"), ['ab!'])
})

test('views and scalar conditions see the same comparisons', async t => {
  const c = await fresh(t)
  await query(c, "CREATE TABLE t(id int, n nvarchar(10)); INSERT t VALUES (1, N'a'), (2, N'b '), (3, NULL)")
  await query(c, "CREATE VIEW v AS SELECT id, n FROM t WHERE n >= N'b'")
  assert.deepEqual((await query(c, 'SELECT id FROM v')).rows, [[2]])
  assert.deepEqual((await query(c, "IF EXISTS (SELECT 1 FROM t WHERE n > N'a') SELECT 1 ELSE SELECT 0")).rows, [[1]])
  assert.deepEqual((await query(c, "DECLARE @c int = (SELECT COUNT(*) FROM t WHERE n LIKE N'[ab]%'); SELECT @c")).rows, [[2]])
  assert.deepEqual((await query(c, "DECLARE @s nvarchar(10) = (SELECT n FROM t WHERE id = 2); SELECT @s + N'|', CAST((SELECT n FROM t WHERE id = 1) AS nvarchar(5)) + N'|'")).rows, [['b |', 'a|']])
  assert.deepEqual((await query(c, "SELECT id FROM t WHERE n IN (SELECT N'b') OR n NOT IN (N'a', N'b')")).rows, [[2]])
})
