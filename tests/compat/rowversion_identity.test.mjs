// rowversion/timestamp columns, decimal identity, SCOPE_IDENTITY/@@IDENTITY
// and SET IDENTITY_INSERT (docs/gaps-rowversion_identity.md). Expected rows,
// errors and messages come from reference/gaps-rowversion_identity.json,
// captured from SQL Server by scripts/capture-gaps-rowversion_identity.mjs.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { Connection, Request, TYPES } from 'tedious'
import { start, query } from '../support/client.mjs'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'
import { cases } from '../../scripts/capture-gaps-rowversion_identity.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/gaps-rowversion_identity.json', import.meta.url), 'utf8'))

let databases = 0
async function fresh(t, connection) {
  connection ??= await start(t, { options: { requestTimeout: 60000 } })
  const name = `rvid_${process.pid}_${databases++}`
  await query(connection, `CREATE DATABASE ${name}`)
  await query(connection, `USE ${name}`)
  return Object.assign(connection, { database: name })
}

// Another session on the same server and database.
async function another(t, connection) {
  const other = new Connection({
    server: '127.0.0.1',
    authentication: { type: 'default', options: { userName: 'sa', password: 'development' } },
    options: { port: connection.config.options.port, encrypt: false, database: connection.database, connectTimeout: 5000, requestTimeout: 60000 },
  })
  t.after(() => other.close())
  other.on('error', () => {})
  await new Promise((resolve, reject) => other.connect(error => error ? reject(error) : resolve()))
  return other
}

async function run(connection, sql) {
  const result = canonical(await capture(connection, sql))
  return {
    errors: result.errors.map(e => [e.number, e.state, e.class, e.message]),
    rows: result.sets.flatMap(set => set.rows),
    columns: result.sets.flatMap(set => set.columns.map(c => [c.name, c.type, c.length, c.precision, c.scale])),
  }
}

async function ok(connection, sql) {
  const result = await run(connection, sql)
  assert.deepEqual(result.errors, [], sql)
  return result.rows
}

async function fails(connection, sql, ...numbers) {
  const result = await run(connection, sql)
  assert.deepEqual(result.errors.map(e => e[0]), numbers, sql)
  return result.errors
}

const hex = value => ({ kind: 'binary', value: value.toString(16).padStart(16, '0') })

// Statements whose SQL Server result differs from msduck in a documented way
// (docs/gaps-rowversion_identity.md, "Remaining limits"): skipped entirely
// ('all') or compared without column descriptors ('columns').
const known = new Map([
  // sys.columns reports system_type_id 173 (binary) with user type timestamp.
  ...['items', 'stamps', 'nullable', 'unspecified'].map(t => [`SELECT c.name, t.name AS type_name, c.system_type_id, c.max_length, c.precision, c.scale, c.is_nullable, c.is_identity FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('${t}') ORDER BY c.column_id`, 'timestamp']),
  // binary -> bigint conversion and binary CONVERT styles are not supported.
  ['SELECT CAST(rv AS bigint) AS n, CONVERT(varchar(20), rv, 1) AS text FROM a ORDER BY id', 'all'],
  // UNION metadata widens binary(8) to varbinary.
  ["SELECT 'a' AS t, id, rv FROM a UNION ALL SELECT 'b', id, rv FROM b ORDER BY rv", 'columns'],
  // Decimal identity values are limited to the bigint range.
  ['CREATE TABLE big(id decimal(38,0) IDENTITY(1000000000000000000000,1), value int)', 'all'],
  ['INSERT big(value) VALUES(1)', 'all'],
  ['SELECT id FROM big', 'all'],
  // msduck rejects 'x' before allocating, and with DuckDB's message.
  ["INSERT items(value) VALUES(5); INSERT items(value) VALUES('x'); SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i", 'all'],
  // numeric sql_variant properties are not supported.
  ...["SCOPE_IDENTITY()", '@@IDENTITY', "IDENT_CURRENT('items')"].map(e => [`SELECT SQL_VARIANT_PROPERTY(${e},'BaseType') AS base, SQL_VARIANT_PROPERTY(${e},'Precision') AS prec, SQL_VARIANT_PROPERTY(${e},'Scale') AS scale`, 'all']),
  ["SELECT SQL_VARIANT_PROPERTY(seed_value,'BaseType') AS base, SQL_VARIANT_PROPERTY(seed_value,'Precision') AS prec, SQL_VARIANT_PROPERTY(seed_value,'Scale') AS scale FROM sys.identity_columns WHERE object_id=OBJECT_ID('n')", 'all'],
])

// Catalog result descriptors (sysname widths, bit/tinyint columns) are a
// separate catalog task; compare their rows only.
const catalog = sql => /\bFROM sys\./.test(sql)
// msduck sends numeric(38,0) as DECIMALN with the same precision and scale.
const descriptor = columns => columns.map(([name, type, ...rest]) => [name, type === 'NumericN' ? 'DecimalN' : type, ...rest])

test('every captured SQL Server statement replays with the same rows, errors and descriptors', async t => {
  const admin = await start(t, { options: { requestTimeout: 60000 } })
  assert.equal(reference.cases.length, cases.length)
  for (const [id, statements] of cases) {
    const expected = reference.cases.find(c => c.id === id)
    assert.ok(expected, id)
    // Each case runs in its own session, as the capture did.
    const c = await another(t, await fresh(t, admin))
    c.database = admin.database
    for (const [index, sql] of statements.entries()) {
      const step = expected.steps[index]
      assert.equal(step.sql, sql)
      const skip = known.get(sql)
      if (skip === 'all') {
        await run(c, sql)
        continue
      }
      const actual = await run(c, sql)
      const want = {
        errors: step.result.errors.map(e => [e.number, e.state, e.class, e.message.replaceAll('msduck_audit_<database>', c.database)]),
        rows: step.result.sets.flatMap(set => set.rows),
        columns: step.result.sets.flatMap(set => set.columns.map(c => [c.name, c.type, c.length, c.precision, c.scale])),
      }
      assert.deepEqual(actual.errors, want.errors, `${id}: ${sql}`)
      if (skip === 'timestamp') {
        // Everything but the rowversion column's system_type_id (189).
        assert.deepEqual(actual.rows.map(r => r[2] === 173 && r[1] === 'timestamp' ? [r[0], r[1], 189, ...r.slice(3)] : r), want.rows, `${id}: ${sql}`)
        continue
      }
      assert.deepEqual(actual.rows, want.rows, `${id}: ${sql}`)
      if (skip !== 'columns' && !catalog(sql)) assert.deepEqual(descriptor(actual.columns), descriptor(want.columns), `${id}: ${sql}`)
    }
  }
})

test('rowversion repro: generated binary(8) values, a database-wide counter and @@DBTS', async t => {
  const c = await fresh(t)
  assert.deepEqual(await ok(c, 'SELECT @@DBTS'), [[hex(0x7d0)]])
  await ok(c, 'CREATE TABLE items(id int, value int, rv rowversion NOT NULL)')
  await ok(c, 'CREATE TABLE stamps(id int, value int, rv timestamp NOT NULL)')
  await ok(c, 'INSERT items(id,value) VALUES(1,10)')
  await ok(c, 'INSERT stamps(id,value) VALUES(1,10)')
  await ok(c, 'INSERT items(id,value) VALUES(2,20),(3,30)')
  const result = await run(c, 'SELECT id, rv FROM items ORDER BY id')
  assert.deepEqual(result.columns, [['id', 'IntN', 4, null, null], ['rv', 'Binary', 8, null, null]])
  assert.deepEqual(result.rows, [[1, hex(0x7d1)], [2, hex(0x7d3)], [3, hex(0x7d4)]])
  assert.deepEqual(await ok(c, 'SELECT rv FROM stamps'), [[hex(0x7d2)]])
  assert.deepEqual(await ok(c, 'SELECT @@DBTS, MIN_ACTIVE_ROWVERSION()'), [[hex(0x7d4), hex(0x7d5)]])
  // Every UPDATE of a row gets a new value, also when no value changes.
  await ok(c, 'UPDATE items SET value=value WHERE id IN (1,3)')
  assert.deepEqual(await ok(c, 'SELECT id, rv FROM items ORDER BY id'), [[1, hex(0x7d5)], [2, hex(0x7d3)], [3, hex(0x7d6)]])
  // Rolled back values are not reused.
  await ok(c, 'BEGIN TRAN; UPDATE items SET value=0; ROLLBACK')
  await ok(c, 'UPDATE items SET value=7 WHERE id=2')
  assert.deepEqual(await ok(c, 'SELECT rv FROM items WHERE id=2'), [[hex(0x7da)]])
  await fails(c, 'INSERT items(id,value,rv) VALUES(4,40,0x01)', 273)
  await fails(c, 'UPDATE items SET rv=0x01', 272)
  await fails(c, 'CREATE TABLE two(a rowversion, b timestamp)', 2738)
  await fails(c, 'ALTER TABLE items ADD rv2 rowversion', 2738)
  assert.deepEqual(await ok(c, "SELECT t.name, c.max_length, c.is_nullable FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('items') AND c.name='rv'"), [['timestamp', 8, false]])
  // ALTER TABLE ADD fills existing rows.
  await ok(c, 'CREATE TABLE later(id int)')
  await ok(c, 'INSERT later VALUES(1),(2)')
  await ok(c, 'ALTER TABLE later ADD rv rowversion')
  assert.deepEqual(await ok(c, 'SELECT id, rv FROM later ORDER BY id'), [[1, hex(0x7db)], [2, hex(0x7dc)]])
})

test('rowversion optimistic concurrency through bound parameters', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE items(id int, value int, rv rowversion)')
  await ok(c, 'INSERT items(id,value) VALUES(1,10)')
  const [[rv]] = (await query(c, 'SELECT rv FROM items WHERE id=1')).rows
  assert.ok(Buffer.isBuffer(rv) && rv.length === 8)
  const update = 'UPDATE items SET value=@v WHERE id=1 AND rv=@rv; SELECT @@ROWCOUNT AS n, rv FROM items WHERE id=1'
  const first = await query(c, update, [['v', TYPES.Int, 11], ['rv', TYPES.VarBinary, rv, { length: 8 }]])
  assert.equal(first.rows[0][0], 1)
  assert.notDeepEqual(first.rows[0][1], rv)
  const stale = await query(c, update, [['v', TYPES.Int, 12], ['rv', TYPES.VarBinary, rv, { length: 8 }]])
  assert.equal(stale.rows[0][0], 0)
  assert.deepEqual((await query(c, 'SELECT value FROM items')).rows, [[11]])
})

test('decimal, tinyint and smallint identity with SCOPE_IDENTITY, @@IDENTITY and IDENT_CURRENT', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE items(id decimal(15,0) IDENTITY(1,1) NOT NULL, value int)')
  await ok(c, 'INSERT items(value) VALUES(10)')
  await ok(c, 'INSERT items(value) VALUES(20),(30)')
  assert.deepEqual(await ok(c, 'SELECT id, value FROM items ORDER BY id'), [[1, 10], [2, 20], [3, 30]])
  const result = await run(c, "SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i, IDENT_CURRENT('items') AS c")
  assert.deepEqual(result.rows, [[3, 3, 3]])
  assert.deepEqual(result.columns.map(([, , , precision, scale]) => [precision, scale]), [[38, 0], [38, 0], [38, 0]])
  assert.deepEqual(await ok(c, "SELECT name, seed_value, increment_value, last_value FROM sys.identity_columns WHERE object_id=OBJECT_ID('items')"), [['id', 1, 1, 3]])
  await ok(c, 'CREATE TABLE small(id decimal(2,0) IDENTITY(98,1), value int)')
  await ok(c, 'INSERT small(value) VALUES(1),(2)')
  const overflow = await fails(c, 'INSERT small(value) VALUES(3)', 8115)
  assert.equal(overflow[0][3], 'Arithmetic overflow error converting IDENTITY to data type decimal.')
  await fails(c, 'CREATE TABLE bad(id decimal(5,2) IDENTITY, value int)', 2749)
  await ok(c, 'CREATE TABLE t(id tinyint IDENTITY(250,2), value int)')
  await ok(c, 'INSERT t(value) VALUES(1),(2),(3)')
  assert.equal((await fails(c, 'INSERT t(value) VALUES(4)', 8115))[0][3], 'Arithmetic overflow error converting IDENTITY to data type tinyint.')
  assert.deepEqual(await ok(c, 'SELECT SCOPE_IDENTITY(), @@IDENTITY'), [[254, 254]])
  await ok(c, 'CREATE TABLE s(id smallint IDENTITY(-5,-3), value int)')
  await ok(c, 'INSERT s(value) VALUES(1),(2)')
  assert.deepEqual(await ok(c, "SELECT SCOPE_IDENTITY(), IDENT_CURRENT('s')"), [[-8, -8]])
  // DROP and TRUNCATE treat decimal identity like integer identity.
  await ok(c, 'TRUNCATE TABLE items')
  await ok(c, 'INSERT items(value) VALUES(40)')
  assert.deepEqual(await ok(c, 'SELECT id FROM items'), [[1]])
  await ok(c, 'DROP TABLE items')
  assert.deepEqual(await ok(c, "SELECT count(*) FROM sys.identity_columns WHERE object_id=OBJECT_ID('items')"), [[0]])
})

function rpc(connection, sql, parameters = []) {
  return new Promise((resolve, reject) => {
    const rows = []
    const request = new Request(sql, error => error ? reject(error) : resolve(rows))
    request.on('row', cells => rows.push(cells.map(cell => cell.value)))
    for (const [name, type, value] of parameters) request.addParameter(name, type, value)
    connection.execSql(request)
  })
}

test('SCOPE_IDENTITY is per session and per RPC scope; @@IDENTITY is session-wide', async t => {
  const a = await fresh(t)
  const b = await another(t, a)
  await ok(a, 'CREATE TABLE items(id int IDENTITY(10,10), value int)')
  await ok(a, 'CREATE TABLE plain(value int)')
  assert.deepEqual(await ok(a, 'SELECT SCOPE_IDENTITY(), @@IDENTITY'), [[null, null]])
  await ok(a, 'INSERT items(value) VALUES(1)')
  await ok(b, 'INSERT items(value) VALUES(2)')
  assert.deepEqual(await ok(a, "SELECT SCOPE_IDENTITY(), @@IDENTITY, IDENT_CURRENT('items')"), [[10, 10, 20]])
  assert.deepEqual(await ok(b, 'SELECT SCOPE_IDENTITY(), @@IDENTITY'), [[20, 20]])
  // An sp_executesql request has its own scope.
  assert.deepEqual(await rpc(a, 'INSERT items(value) VALUES(@v); SELECT SCOPE_IDENTITY(), @@IDENTITY', [['v', TYPES.Int, 3]]), [[30, 30]])
  assert.deepEqual(await rpc(a, 'SELECT SCOPE_IDENTITY(), @@IDENTITY'), [[null, 30]])
  assert.deepEqual(await ok(a, 'SELECT SCOPE_IDENTITY(), @@IDENTITY'), [[10, 30]])
  // Any successful INSERT into a table without identity clears both.
  await ok(a, 'INSERT plain VALUES(1)')
  assert.deepEqual(await ok(a, 'SELECT SCOPE_IDENTITY(), @@IDENTITY'), [[null, null]])
  // Failed inserts keep the previous values.
  await ok(a, 'INSERT items(value) VALUES(4)')
  await fails(a, 'INSERT items(value) VALUES(1/0)', 8134)
  assert.deepEqual(await ok(a, 'SELECT SCOPE_IDENTITY()'), [[40]])
})

test('concurrent sessions each see their own generated identity values', async t => {
  const first = await fresh(t)
  await ok(first, 'CREATE TABLE items(id int IDENTITY, session int)')
  const sessions = [first]
  for (let i = 1; i < 4; i++) sessions.push(await another(t, first))
  await Promise.all(sessions.map(async (c, session) => {
    for (let i = 0; i < 15; i++) {
      const [[id]] = (await query(c, `INSERT items(session) VALUES(${session}); SELECT CAST(SCOPE_IDENTITY() AS int)`)).rows
      const [[owner]] = (await query(c, `SELECT session FROM items WHERE id=${id}`)).rows
      assert.equal(owner, session)
    }
  }))
  assert.deepEqual(await ok(first, 'SELECT count(*), count(DISTINCT id) FROM items'), [[60, 60]])
})

test('SET IDENTITY_INSERT repro: explicit values, SQL Server rules and the advanced seed', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE items(id int IDENTITY(1,1), value int)')
  await ok(c, 'CREATE TABLE other(id bigint IDENTITY(100,-1), value int)')
  await ok(c, 'INSERT items(value) VALUES(1)')
  await fails(c, 'INSERT items(id,value) VALUES(10,2)', 544)
  await ok(c, 'SET IDENTITY_INSERT items ON')
  await ok(c, 'INSERT items(id,value) VALUES(10,2)')
  assert.deepEqual(await ok(c, "SELECT SCOPE_IDENTITY(), IDENT_CURRENT('items')"), [[10, 10]])
  await ok(c, 'INSERT items(id,value) VALUES(5,3)')
  assert.deepEqual(await ok(c, "SELECT SCOPE_IDENTITY(), IDENT_CURRENT('items')"), [[5, 10]])
  await fails(c, 'INSERT items VALUES(20,4)', 8101)
  await fails(c, 'INSERT items(value) VALUES(5)', 545)
  await fails(c, 'INSERT items(id,value) VALUES(NULL,6)', 339)
  await fails(c, 'SET IDENTITY_INSERT other ON', 8107)
  await ok(c, 'INSERT items(id,value) SELECT 40,10')
  await ok(c, 'SET IDENTITY_INSERT items OFF')
  await ok(c, 'INSERT items(value) VALUES(11)')
  assert.deepEqual(await ok(c, 'SELECT id FROM items ORDER BY id'), [[1], [5], [10], [40], [41]])
  await fails(c, 'SET IDENTITY_INSERT missing ON', 1088)
  await ok(c, 'CREATE TABLE plain(v int)')
  await fails(c, 'SET IDENTITY_INSERT plain ON', 8106)
  // A descending identity advances downwards only.
  await ok(c, 'SET IDENTITY_INSERT dbo.other ON')
  await ok(c, 'INSERT other(id,value) VALUES(50,1),(200,2)')
  assert.deepEqual(await ok(c, 'SELECT SCOPE_IDENTITY()'), [[200]])
  await ok(c, 'SET IDENTITY_INSERT other OFF')
  await ok(c, 'INSERT other(value) VALUES(3)')
  assert.deepEqual(await ok(c, "SELECT CAST(max(id) AS int), CAST(min(id) AS int), IDENT_CURRENT('other') FROM other"), [[200, 49, 49]])
  // Explicit values from a query, with a rowversion column alongside.
  await ok(c, 'CREATE TABLE source_rows(id int, value int)')
  await ok(c, 'INSERT source_rows VALUES(100,1),(90,2)')
  await ok(c, 'CREATE TABLE copies(id decimal(10,0) IDENTITY, value int, rv rowversion)')
  await ok(c, 'SET IDENTITY_INSERT copies ON; INSERT copies(id,value) SELECT id, value FROM source_rows; SET IDENTITY_INSERT copies OFF')
  await ok(c, 'INSERT copies(value) VALUES(3)')
  assert.deepEqual(await ok(c, 'SELECT id, value FROM copies ORDER BY id'), [[90, 2], [100, 1], [101, 3]])
})

test('SET IDENTITY_INSERT inside an RPC request does not outlive it', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE items(id int IDENTITY, value int)')
  await rpc(c, 'SET IDENTITY_INSERT items ON')
  await fails(c, 'INSERT items(id,value) VALUES(7,1)', 544)
  assert.deepEqual(await rpc(c, 'SET IDENTITY_INSERT items ON; INSERT items(id,value) VALUES(@id,@v); SELECT SCOPE_IDENTITY()', [['id', TYPES.Int, 7], ['v', TYPES.Int, 1]]), [[7]])
  await fails(c, 'INSERT items(id,value) VALUES(8,1)', 544)
  // The caller's own setting is visible to, and unchanged by, an RPC.
  await ok(c, 'SET IDENTITY_INSERT items ON')
  await rpc(c, 'SET IDENTITY_INSERT items OFF')
  await ok(c, 'INSERT items(id,value) VALUES(8,1)')
  assert.deepEqual(await ok(c, 'SELECT id FROM items ORDER BY id'), [[7], [8]])
})

test('a trigger is a scope of its own: SCOPE_IDENTITY stays with the caller, @@IDENTITY follows the trigger', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE t(id int IDENTITY, v int); CREATE TABLE audit(id int IDENTITY(100,1), v int)')
  await ok(c, 'CREATE TRIGGER t_ai ON t AFTER INSERT AS INSERT audit(v) SELECT v FROM inserted')
  assert.deepEqual(await ok(c, 'INSERT t(v) VALUES(1); SELECT SCOPE_IDENTITY(), @@IDENTITY'), [[1, 100]])
  assert.deepEqual(await ok(c, 'SET IDENTITY_INSERT t ON; INSERT t(id,v) VALUES(10,1); SET IDENTITY_INSERT t OFF; SELECT SCOPE_IDENTITY(), @@IDENTITY'), [[10, 101]])
  // The OFF after the trigger fired applies to the session.
  assert.deepEqual(await ok(c, 'INSERT t(v) VALUES(2); SELECT SCOPE_IDENTITY(), @@IDENTITY'), [[11, 102]])
  // The session's setting applies inside the trigger.
  await ok(c, 'SET IDENTITY_INSERT audit ON')
  await fails(c, 'INSERT t(v) VALUES(3)', 545)
  await ok(c, 'SET IDENTITY_INSERT audit OFF')
  // The failed INSERT consumed t's value 12.
  assert.deepEqual(await rpc(c, 'INSERT t(v) VALUES(@v); SELECT SCOPE_IDENTITY(), @@IDENTITY', [['v', TYPES.Int, 3]]), [[13, 103]])
})
