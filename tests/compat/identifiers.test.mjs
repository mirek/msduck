// Contextual identifiers and database-qualified diagnostics through tedious
// (docs/gaps-identifiers.md). Values and messages match
// reference/gaps-identifiers.json, captured from SQL Server 2022.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { start, query } from '../support/client.mjs'

async function rejects(connection, sql, number, message) {
  await assert.rejects(query(connection, sql), error => {
    assert.equal(error.number, number, error.message)
    if (message !== undefined) assert.equal(error.message, message)
    return true
  })
}

test('words DuckDB reserves are regular SQL Server identifiers', async t => {
  const connection = await start(t)
  await query(connection, 'CREATE DATABASE foo')
  await query(connection, 'USE foo')
  await query(connection, 'CREATE TABLE items(offset int NOT NULL)')
  await query(connection, 'INSERT INTO items(offset) VALUES (1), (2)')
  assert.deepEqual((await query(connection, 'SELECT offset FROM items ORDER BY offset')).rows, [[1], [2]])

  for (const word of ['offset', 'limit', 'qualify', 'at', 'using', 'window', 'interval', 'lateral', 'trim', 'returning', 'natural', 'true']) {
    await query(connection, `CREATE TABLE ${word}(${word} int NOT NULL, other int NULL)`)
    await query(connection, `INSERT INTO ${word}(${word}, other) VALUES (1, 10); INSERT ${word} VALUES (2, 20)`)
    await query(connection, `UPDATE ${word} SET ${word} = ${word} + 10 WHERE ${word} = 2`)
    const select = await query(connection, `SELECT ${word}.${word} ${word}, x.other AS other FROM dbo.${word} ${word} JOIN ${word} AS x ON x.${word} = ${word}.${word} ORDER BY ${word}.${word}`)
    assert.deepEqual(select.columns[0].map(c => c.colName), [word, 'other'], word)
    assert.deepEqual(select.rows, [[1, 10], [12, 20]], word)
    const parameter = await query(connection, `SELECT ${word} FROM ${word} WHERE ${word} = @${word}`, [[word, TYPES.Int, 12]])
    assert.deepEqual(parameter.rows, [[12]], word)
    const cte = await query(connection, `WITH ${word}(${word}) AS (SELECT ${word} FROM dbo.${word}) SELECT count(*) AS n, max(${word}) AS m FROM ${word}`)
    assert.deepEqual(cte.rows, [[2, 12]], word)
    await query(connection, `CREATE INDEX ix_${word} ON ${word}(${word})`)
    await query(connection, `CREATE VIEW v_${word} AS SELECT ${word} FROM ${word}`)
    assert.deepEqual((await query(connection, `SELECT ${word} FROM v_${word} ORDER BY ${word}`)).rows, [[1], [12]], word)
    assert.deepEqual((await query(connection, `SELECT name FROM sys.columns WHERE object_id = OBJECT_ID('dbo.${word}') ORDER BY column_id`)).rows, [[word], ['other']], word)
    await query(connection, `DELETE FROM ${word} WHERE ${word} = 12; DROP VIEW v_${word}; DROP TABLE ${word}`)
  }
})

test('scalar subqueries and T-SQL syntax around contextual words', async t => {
  const connection = await start(t)
  await query(connection, 'CREATE TABLE items(offset int NOT NULL, at int NULL)')
  await query(connection, 'INSERT items VALUES (1, 10), (2, 20), (3, 30)')
  const scalar = await query(connection, `
    DECLARE @n int = (SELECT max(offset) FROM items);
    IF EXISTS (SELECT 1 FROM items WHERE offset = @n) SET @n = (SELECT min(at) FROM items WHERE offset > 1);
    SELECT @n AS n`)
  assert.deepEqual(scalar.rows, [[20]])
  const paged = await query(connection, 'SELECT offset FROM items ORDER BY offset OFFSET 1 ROWS FETCH NEXT 1 ROWS ONLY')
  assert.deepEqual(paged.rows, [[2]])
  // A T-SQL reserved word still needs delimiters.
  await query(connection, 'CREATE TABLE [pivot]([values] int)')
  await query(connection, 'INSERT [pivot]([values]) VALUES (7)')
  assert.deepEqual((await query(connection, 'SELECT [values] FROM [pivot]')).rows, [[7]])
})

test('object-qualified diagnostics name the current database', async t => {
  const connection = await start(t)
  await query(connection, 'CREATE DATABASE foo')
  await query(connection, 'USE foo')
  await query(connection, 'CREATE TABLE items(id int NOT NULL PRIMARY KEY, name nvarchar(3) NULL, code varchar(2) NULL)')
  await query(connection, "INSERT INTO items(id, name) VALUES (1, N'abc')")
  assert.deepEqual((await query(connection, 'SELECT DB_NAME()')).rows, [['foo']])
  await rejects(connection, "INSERT INTO items(id, name) VALUES (2, N'abcdef')", 2628,
    "String or binary data would be truncated in table 'foo.dbo.items', column 'name'. Truncated value: 'abc'.")
  await rejects(connection, "INSERT INTO items(id, code) VALUES (3, 'abcdef')", 2628,
    "String or binary data would be truncated in table 'foo.dbo.items', column 'code'. Truncated value: 'ab'.")
  await rejects(connection, "UPDATE items SET name = N'wxyz' WHERE id = 1", 2628,
    "String or binary data would be truncated in table 'foo.dbo.items', column 'name'. Truncated value: 'wxy'.")
  await rejects(connection, 'INSERT INTO items(id, name) VALUES (NULL, NULL)', 515,
    "Cannot insert the value NULL into column 'id', table 'foo.dbo.items'; column does not allow nulls. INSERT fails.")
  await rejects(connection, 'UPDATE items SET id = NULL', 515,
    "Cannot insert the value NULL into column 'id', table 'foo.dbo.items'; column does not allow nulls. UPDATE fails.")
  // The duplicate key number is unchanged; SQL Server names 'dbo.items' there.
  await rejects(connection, 'INSERT INTO items(id) VALUES (1)', 2627)
  // The session is still usable and the failed rows were not written.
  assert.deepEqual((await query(connection, 'SELECT id, name FROM items')).rows, [[1, 'abc']])
  await query(connection, 'USE master')
  await query(connection, 'CREATE TABLE items(name nvarchar(2))')
  await rejects(connection, "INSERT INTO items VALUES (N'abc')", 2628,
    "String or binary data would be truncated in table 'master.dbo.items', column 'name'. Truncated value: 'ab'.")
})
