// CREATE DATABASE, USE, DROP DATABASE, DB_NAME/DB_ID and LOGIN7 database
// selection. Expected tokens and errors were captured from SQL Server 2025
// (see docs/databases.md).
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Connection, Request } from 'tedious'
import { start, query } from './support/client.mjs'

function batch(connection, sql) {
  return new Promise((resolve, reject) => {
    const rows = [], infos = []
    const onInfo = info => infos.push([info.number, info.state, info.class, info.message])
    connection.on('infoMessage', onInfo)
    const request = new Request(sql, error => {
      connection.off('infoMessage', onInfo)
      error ? reject(error) : resolve({ rows, infos })
    })
    request.on('row', cells => rows.push(cells.map(cell => cell.value)))
    connection.execSqlBatch(request)
  })
}
async function failure(promise) {
  const error = await promise.then(() => assert.fail('expected an error'), error => error)
  return [error.number, error.state, error.class, error.message]
}
async function login(config, database) {
  const connection = new Connection({ ...config, options: { ...config.options, database } })
  const changes = []
  connection.on('databaseChange', name => changes.push(name))
  connection.on('error', () => {})
  const error = await new Promise(resolve => connection.connect(resolve))
  return { connection, changes, error }
}

test('USE switches databases and reports the change', { timeout: 30000 }, async t => {
  const c = await start(t)
  const changes = []
  c.on('databaseChange', name => changes.push(name))
  await batch(c, 'CREATE DATABASE sales')
  assert.deepEqual((await batch(c, 'USE sales')).infos, [[5701, 1, 0, "Changed database context to 'sales'."]])
  assert.deepEqual(changes, ['sales'])
  await batch(c, 'CREATE TABLE dbo.orders (id INT, amount DECIMAL(10, 2)); INSERT dbo.orders VALUES (1, 9.50)')
  assert.deepEqual((await batch(c, 'SELECT id, amount FROM dbo.orders')).rows, [[1, 9.5]])
  const functions = await query(c, 'SELECT DB_NAME(), DB_ID(), DB_NAME(1), DB_ID(N\'SALES\'), DB_ID(\'nope\'), DB_NAME(999)')
  assert.deepEqual(functions.rows, [['sales', 5, 'master', 5, null, null]])
  // Captured: nullable nvarchar(128) and a nullable smallint (IntN length 2).
  assert.deepEqual(functions.columns[0].map(column => [column.type.name, column.dataLength ?? null, Boolean(column.flags & 1)]),
    [['NVarChar', 256, true], ['IntN', 2, true], ['NVarChar', 256, true], ['IntN', 2, true], ['IntN', 2, true], ['NVarChar', 256, true]])
  // Tables belong to their database.
  await batch(c, 'USE master')
  assert.deepEqual(changes, ['sales', 'master'])
  assert.equal((await failure(batch(c, 'SELECT id FROM dbo.orders')))[0] > 0, true)
  assert.deepEqual((await query(c, 'SELECT DB_NAME(), DB_ID()')).rows, [['master', 1]])
  assert.deepEqual(await failure(batch(c, 'USE nope')),
    [911, 1, 16, "Database 'nope' does not exist. Make sure that the name is entered correctly."])
})

test('sys.databases lists created databases and DROP removes them', { timeout: 30000 }, async t => {
  const c = await start(t)
  await batch(c, 'CREATE DATABASE a; CREATE DATABASE b')
  assert.deepEqual((await query(c, 'SELECT name, database_id FROM sys.databases ORDER BY database_id')).rows,
    [['master', 1], ['a', 5], ['b', 6]])
  assert.deepEqual(await failure(batch(c, 'CREATE DATABASE A')),
    [1801, 3, 16, "Database 'A' already exists. Choose a different database name."])
  await batch(c, 'DROP DATABASE a, b')
  assert.deepEqual((await query(c, 'SELECT name FROM sys.databases')).rows, [['master']])
  assert.deepEqual(await failure(batch(c, 'DROP DATABASE a')),
    [3701, 1, 11, "Cannot drop the database 'a', because it does not exist or you do not have permission."])
  await batch(c, 'DROP DATABASE IF EXISTS a')
  assert.deepEqual(await failure(batch(c, 'DROP DATABASE master')),
    [3708, 4, 16, "Cannot drop the database 'master' because it is a system database."])
})

test('database statements are refused in transactions and while in use', { timeout: 30000 }, async t => {
  const c = await start(t)
  await batch(c, 'CREATE DATABASE busy')
  assert.deepEqual(await failure(batch(c, 'BEGIN TRANSACTION; CREATE DATABASE other')),
    [226, 5, 16, 'CREATE DATABASE statement not allowed within multi-statement transaction.'])
  await batch(c, 'IF @@TRANCOUNT > 0 ROLLBACK')
  assert.deepEqual(await failure(batch(c, 'BEGIN TRANSACTION; DROP DATABASE busy')),
    [574, 0, 16, 'DROP DATABASE statement cannot be used inside a user transaction.'])
  await batch(c, 'IF @@TRANCOUNT > 0 ROLLBACK')
  await batch(c, 'USE busy')
  assert.deepEqual(await failure(batch(c, 'DROP DATABASE busy')),
    [3702, 3, 16, 'Cannot drop database "busy" because it is currently in use.'])
  const { connection: other, error } = await login(c.config, 'master')
  t.after(() => other.close())
  assert.equal(error, undefined)
  assert.deepEqual(await failure(batch(other, 'DROP DATABASE busy')),
    [3702, 4, 16, 'Cannot drop database "busy" because it is currently in use.'])
  await batch(c, 'USE master')
  await batch(other, 'DROP DATABASE busy')
})

test('LOGIN7 selects any existing database and rejects unknown ones', { timeout: 30000 }, async t => {
  const c = await start(t)
  await batch(c, 'CREATE DATABASE app; USE app; CREATE TABLE dbo.items (name NVARCHAR(20)); INSERT dbo.items VALUES (N\'kept\')')
  const { connection: direct, changes, error } = await login(c.config, 'APP')
  t.after(() => direct.close())
  assert.equal(error, undefined)
  // The login response reports the database's own name.
  assert.deepEqual(changes, ['app'])
  assert.deepEqual((await query(direct, 'SELECT name FROM dbo.items')).rows, [['kept']])
  assert.deepEqual((await query(direct, 'SELECT DB_NAME()')).rows, [['app']])
  const missing = await login(c.config, 'nope')
  t.after(() => missing.connection.close())
  assert.match(missing.error?.message ?? '', /Login failed/)
})

test('database-qualified names resolve through the catalog', { timeout: 30000 }, async t => {
  const c = await start(t)
  await batch(c, 'CREATE DATABASE q1; USE q1; CREATE TABLE dbo.t (x INT); USE master')
  await batch(c, 'INSERT q1.dbo.t VALUES (6); UPDATE q1.dbo.t SET x = 7')
  assert.deepEqual((await batch(c, 'SELECT x FROM Q1.dbo.t')).rows, [[7]])
  // msduck records DDL in the current database, so DDL elsewhere is refused.
  assert.match((await failure(batch(c, 'CREATE TABLE q1.dbo.u (x INT)')))[3], /unsupported DDL on q1\.dbo\.u/)
  assert.deepEqual((await batch(c, 'SELECT name FROM master.sys.databases WHERE database_id = 1')).rows, [['master']])
  assert.deepEqual((await batch(c, 'SELECT COUNT(*) FROM q1.sys.databases')).rows, [[2]])
  assert.deepEqual(await failure(batch(c, 'SELECT x FROM nope.dbo.t')), [208, 1, 16, "Invalid object name 'nope.dbo.t'."])
  assert.deepEqual(await failure(batch(c, 'INSERT nope.dbo.t VALUES (1)')), [208, 1, 16, "Invalid object name 'nope.dbo.t'."])
  // The table lives in q1, not in master.
  assert.equal((await failure(batch(c, 'SELECT x FROM dbo.t')))[0] > 0, true)
})
