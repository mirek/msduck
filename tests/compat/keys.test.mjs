// Key and index columns of every type, single-NULL UNIQUE and index forms
// (docs/gaps-keys.md). Expected errors and messages come from
// reference/gaps-keys.json, captured from SQL Server by
// scripts/capture-gaps-keys.mjs.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { start, query } from '../support/client.mjs'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'
import { cases } from '../../scripts/capture-gaps-keys.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/gaps-keys.json', import.meta.url), 'utf8'))

let databases = 0
async function fresh(t) {
  const connection = await start(t)
  const name = `keys_${process.pid}_${databases++}`
  await query(connection, `CREATE DATABASE ${name}`)
  await query(connection, `USE ${name}`)
  return connection
}

// Every diagnostic of one batch, with rows of every result set.
async function run(connection, sql) {
  const result = canonical(await capture(connection, sql))
  return {
    errors: result.errors.map(e => [e.number, e.state, e.class, e.message]),
    rows: result.sets.flatMap(set => set.rows),
  }
}

async function ok(connection, sql) {
  const result = await run(connection, sql)
  assert.deepEqual(result.errors, [], sql)
  return result.rows
}

async function fails(connection, sql, ...errors) {
  const result = await run(connection, sql)
  assert.deepEqual(result.errors, errors, sql)
}

const pk = (name, table, value) => [2627, 1, 14, `Violation of PRIMARY KEY constraint '${name}'. Cannot insert duplicate key in object '${table}'. The duplicate key value is (${value}).`]
const uq = (name, table, value) => [2627, 1, 14, `Violation of UNIQUE KEY constraint '${name}'. Cannot insert duplicate key in object '${table}'. The duplicate key value is (${value}).`]
const ux = (name, table, value) => [2601, 1, 14, `Cannot insert duplicate key row in object '${table}' with unique index '${name}'. The duplicate key value is (${value}).`]

test('Unicode primary keys, composite keys and unique constraints enforce SQL Server duplicates', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE items (id nvarchar(450) NOT NULL CONSTRAINT pk_items PRIMARY KEY NONCLUSTERED, n int NULL)')
  await ok(c, "INSERT items VALUES (N'abc', 1), (N'\u{1F986}', 2)")
  await fails(c, "INSERT items VALUES (N'abc', 3)", pk('pk_items', 'dbo.items', 'abc'))
  // Trailing spaces do not distinguish keys.
  await fails(c, "INSERT items VALUES (N'abc  ', 3)", pk('pk_items', 'dbo.items', 'abc'))
  await fails(c, "INSERT items VALUES (N'abd', 3), (N'abd', 4)", pk('pk_items', 'dbo.items', 'abd'))
  await fails(c, "INSERT items VALUES (N'\u{1F986}', 3)", pk('pk_items', 'dbo.items', '\u{1F986}'))
  assert.equal((await run(c, 'INSERT items VALUES (NULL, 3)')).errors[0][0], 515)
  await fails(c, "UPDATE items SET id = N'abc' WHERE n = 2", pk('pk_items', 'dbo.items', 'abc'))
  await ok(c, "UPDATE items SET id = N'abe' WHERE n = 1")
  await ok(c, "INSERT items VALUES (N'abc', 3)")
  assert.deepEqual(await ok(c, 'SELECT count(*) FROM items'), [[3]])

  await ok(c, 'CREATE TABLE tenants (tenant nvarchar(20) NOT NULL, code nvarchar(20) NOT NULL, n int, CONSTRAINT pk_tenants PRIMARY KEY CLUSTERED (tenant, code))')
  await ok(c, "INSERT tenants VALUES (N'a', N'x', 1), (N'a', N'y', 2), (N'b', N'x', 3)")
  await fails(c, "INSERT tenants VALUES (N'a', N'x', 4)", pk('pk_tenants', 'dbo.tenants', 'a, x'))
  assert.deepEqual(await ok(c, 'SELECT n FROM tenants ORDER BY n'), [[1], [2], [3]])

  await ok(c, 'CREATE TABLE names (id int NOT NULL, name nvarchar(50) NULL CONSTRAINT uq_names UNIQUE, code nchar(4) NULL)')
  await ok(c, "INSERT names VALUES (1, N'one', N'x'), (2, NULL, N'y')")
  await fails(c, "INSERT names VALUES (3, N'one', NULL)", uq('uq_names', 'dbo.names', 'one'))
  await fails(c, 'INSERT names VALUES (4, NULL, NULL)', uq('uq_names', 'dbo.names', '<NULL>'))
  await ok(c, 'CREATE UNIQUE INDEX ux_names_code ON names(code)')
  await ok(c, 'CREATE NONCLUSTERED INDEX ix_names_name ON names(name)')
  await fails(c, "INSERT names VALUES (5, N'five', N'x   ')", ux('ux_names_code', 'dbo.names', 'x   '))
  assert.deepEqual(await ok(c, 'SELECT id, name, code FROM names ORDER BY id'), [[1, 'one', 'x   '], [2, null, 'y   ']])
  await ok(c, 'DROP INDEX ux_names_code ON names')
  await ok(c, "INSERT names VALUES (5, N'five', N'x')")
  await ok(c, 'DROP INDEX ix_names_name ON names')
})

test('datetimeoffset and datetime2 keys compare instants', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE events (id int NOT NULL, occurred datetimeoffset NOT NULL, CONSTRAINT pk_events PRIMARY KEY CLUSTERED(id, occurred))')
  await ok(c, "INSERT events VALUES (1, '2024-01-02 10:00:00 +01:00')")
  // The same instant in another offset is a duplicate. msduck shows the
  // key's UTC value; SQL Server shows the inserted offset.
  await fails(c, "INSERT events VALUES (1, '2024-01-02 09:00:00 +00:00')", pk('pk_events', 'dbo.events', '1, 2024-01-02 09:00:00.0000000 +00:00'))
  await fails(c, "INSERT events VALUES (1, '2024-01-02 11:30:00 +02:30')", pk('pk_events', 'dbo.events', '1, 2024-01-02 09:00:00.0000000 +00:00'))
  await ok(c, "INSERT events VALUES (1, '2024-01-02 10:00:00 +00:00'), (2, '2024-01-02 10:00:00 +01:00')")
  assert.deepEqual(await ok(c, 'SELECT count(*) FROM events'), [[3]])

  await ok(c, 'CREATE TABLE stamps (id int NOT NULL, happened datetimeoffset(3) NULL, local datetime2(3) NULL)')
  await ok(c, 'CREATE INDEX ix_stamps_happened ON stamps(happened)')
  await ok(c, 'CREATE UNIQUE INDEX ux_stamps_happened ON stamps(happened)')
  await ok(c, 'CREATE UNIQUE INDEX ux_stamps_local ON stamps(local) WHERE local IS NOT NULL')
  await ok(c, "INSERT stamps VALUES (1, '2024-01-02 10:00:00.123 +02:00', '2024-01-02 03:04:05.678')")
  await fails(c, "INSERT stamps VALUES (2, '2024-01-02 08:00:00.123 +00:00', NULL)", ux('ux_stamps_happened', 'dbo.stamps', '2024-01-02 08:00:00.123 +00:00'))
  await fails(c, "INSERT stamps VALUES (3, NULL, '2024-01-02 03:04:05.678')", ux('ux_stamps_local', 'dbo.stamps', '2024-01-02 03:04:05.678'))
  await ok(c, 'INSERT stamps VALUES (4, NULL, NULL)')
  await fails(c, 'INSERT stamps VALUES (5, NULL, NULL)', ux('ux_stamps_happened', 'dbo.stamps', '<NULL>'))

  await ok(c, 'CREATE TABLE moments (at2 datetime2 NOT NULL PRIMARY KEY, label nvarchar(10))')
  await ok(c, "INSERT moments VALUES ('2024-01-02 03:04:05.1234567', N'a')")
  const moment = await run(c, "INSERT moments VALUES ('2024-01-02 03:04:05.1234567', N'b')")
  assert.deepEqual(moment.errors.map(e => e.slice(0, 3)), [[2627, 1, 14]])
  assert.match(moment.errors[0][3], /^Violation of PRIMARY KEY constraint 'PK__moments__[0-9A-F]{16}'\. Cannot insert duplicate key in object 'dbo\.moments'\. The duplicate key value is \(2024-01-02 03:04:05\.1234567\)\.$/)
})

test('column-level key constraints with column lists parse', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE items (id int NOT NULL CONSTRAINT uq_items UNIQUE(id))')
  await ok(c, 'INSERT items VALUES (1)')
  await fails(c, 'INSERT items VALUES (1)', uq('uq_items', 'dbo.items', '1'))
  await ok(c, 'CREATE TABLE things (id int NOT NULL CONSTRAINT pk_things PRIMARY KEY(id), v int)')
  await ok(c, 'INSERT things VALUES (1, 1)')
  await fails(c, 'INSERT things VALUES (1, 2)', pk('pk_things', 'dbo.things', '1'))
  await ok(c, 'CREATE TABLE pairs (a int NOT NULL, b int NOT NULL CONSTRAINT pk_pairs PRIMARY KEY (a, b))')
  await ok(c, 'INSERT pairs VALUES (1, 1), (1, 2)')
  await fails(c, 'INSERT pairs VALUES (1, 1)', pk('pk_pairs', 'dbo.pairs', '1, 1'))
  await ok(c, 'CREATE TABLE plain (id int PRIMARY KEY (id))')
  await ok(c, 'CREATE TABLE other (id int NOT NULL UNIQUE NONCLUSTERED (id), code nvarchar(10) NULL CONSTRAINT uq_other_code UNIQUE NONCLUSTERED (code))')
  await ok(c, "INSERT other VALUES (1, NULL), (2, N'a')")
  await fails(c, 'INSERT other VALUES (3, NULL)', uq('uq_other_code', 'dbo.other', '<NULL>'))
  // A whole schema batch with these forms.
  await ok(c, `CREATE TABLE orders (id nvarchar(36) NOT NULL CONSTRAINT pk_orders PRIMARY KEY (id), number int NOT NULL CONSTRAINT uq_orders_number UNIQUE (number), placed datetimeoffset NOT NULL)
    CREATE TABLE lines (order_id nvarchar(36) NOT NULL, line int NOT NULL CONSTRAINT pk_lines PRIMARY KEY (order_id, line))
    CREATE INDEX ix_orders_placed ON orders(placed) INCLUDE(number)`)
  await ok(c, "INSERT orders VALUES (N'o1', 1, SYSDATETIMEOFFSET()); INSERT lines VALUES (N'o1', 1)")
  await fails(c, "INSERT lines VALUES (N'o1', 1)", pk('pk_lines', 'dbo.lines', 'o1, 1'))
})

test('UNIQUE allows one NULL; filtered unique indexes allow many', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE items(value int UNIQUE)')
  await ok(c, 'INSERT items VALUES(NULL)')
  const second = await run(c, 'INSERT items VALUES(NULL)')
  assert.equal(second.errors.length, 1)
  assert.match(second.errors[0][3], /^Violation of UNIQUE KEY constraint 'UQ__items__[0-9A-F]{16}'\. Cannot insert duplicate key in object 'dbo\.items'\. The duplicate key value is \(<NULL>\)\.$/)
  assert.deepEqual(second.errors[0].slice(0, 3), [2627, 1, 14])
  await ok(c, 'INSERT items VALUES(1), (2)')
  assert.equal((await run(c, 'UPDATE items SET value = NULL WHERE value = 1')).errors[0][0], 2627)
  assert.deepEqual(await ok(c, 'SELECT value FROM items ORDER BY value'), [[null], [1], [2]])

  await ok(c, 'CREATE TABLE pairs(a int NULL, b int NULL, CONSTRAINT uq_pairs UNIQUE(a, b))')
  await ok(c, 'INSERT pairs VALUES(1, NULL), (2, NULL), (NULL, NULL), (NULL, 1)')
  await fails(c, 'INSERT pairs VALUES(1, NULL)', uq('uq_pairs', 'dbo.pairs', '1, <NULL>'))
  await fails(c, 'INSERT pairs VALUES(NULL, NULL)', uq('uq_pairs', 'dbo.pairs', '<NULL>, <NULL>'))
  // A zero is not a NULL.
  await ok(c, 'INSERT pairs VALUES(0, NULL), (NULL, 0)')

  await ok(c, 'CREATE TABLE named(value varchar(10) NULL CONSTRAINT uq_named UNIQUE)')
  await ok(c, "INSERT named VALUES(NULL), ('')")
  await fails(c, 'INSERT named VALUES(NULL)', uq('uq_named', 'dbo.named', '<NULL>'))
  await fails(c, "INSERT named VALUES('  ')", uq('uq_named', 'dbo.named', ''))

  await ok(c, 'CREATE TABLE indexed(id int, value varchar(20) NULL)')
  await ok(c, 'CREATE UNIQUE INDEX ux_indexed_value ON indexed(value)')
  await ok(c, "INSERT indexed VALUES(1, NULL), (2, 'a')")
  await fails(c, 'INSERT indexed VALUES(3, NULL)', ux('ux_indexed_value', 'dbo.indexed', '<NULL>'))
  await fails(c, "INSERT indexed VALUES(4, 'a')", ux('ux_indexed_value', 'dbo.indexed', 'a'))

  await ok(c, 'CREATE TABLE emails(id int NOT NULL, email nvarchar(200) NULL)')
  await ok(c, 'CREATE UNIQUE INDEX ux_emails ON emails(email) WHERE email IS NOT NULL')
  await ok(c, 'INSERT emails VALUES(1, NULL), (2, NULL), (3, NULL)')
  await ok(c, "INSERT emails VALUES(4, N'a@b.c')")
  await fails(c, "INSERT emails VALUES(5, N'a@b.c')", ux('ux_emails', 'dbo.emails', 'a@b.c'))
  await fails(c, "UPDATE emails SET email = N'a@b.c' WHERE id = 1", ux('ux_emails', 'dbo.emails', 'a@b.c'))
  assert.deepEqual(await ok(c, 'SELECT id, email FROM emails ORDER BY id'), [[1, null], [2, null], [3, null], [4, 'a@b.c']])
  await ok(c, 'DROP INDEX ux_emails ON emails')
  await ok(c, "INSERT emails VALUES(5, N'a@b.c')")

  await ok(c, 'CREATE TABLE ranged(id int NOT NULL, n int NULL, code varchar(5) NULL)')
  await ok(c, 'CREATE UNIQUE INDEX ux_ranged ON ranged(n) WHERE n > 0')
  await ok(c, "CREATE UNIQUE INDEX ux_ranged_code ON ranged(code) WHERE code IN ('a', 'b') AND n IS NOT NULL")
  await ok(c, "INSERT ranged VALUES(1, 0, 'a'), (2, NULL, 'a'), (3, -1, NULL), (4, -1, NULL), (5, 1, 'c')")
  await fails(c, "INSERT ranged VALUES(9, 0, 'a')", ux('ux_ranged_code', 'dbo.ranged', 'a'))
  await fails(c, 'INSERT ranged VALUES(6, 1, NULL)', ux('ux_ranged', 'dbo.ranged', '1'))
  await fails(c, 'UPDATE ranged SET n = 1 WHERE id = 1', ux('ux_ranged', 'dbo.ranged', '1'))
  await ok(c, "INSERT ranged VALUES(7, NULL, 'a'), (8, NULL, 'a')")
  assert.deepEqual(await ok(c, 'SELECT count(*) FROM ranged'), [[7]])
})

test('CLUSTERED, INCLUDE, filtered and WITH index forms, and DROP INDEX', async t => {
  const c = await fresh(t)
  // DROP INDEX must also work while other tables have key constraints.
  await ok(c, 'CREATE TABLE keyed(id int NOT NULL CONSTRAINT pk_keyed PRIMARY KEY, code int UNIQUE)')
  await ok(c, 'CREATE TABLE items(id int NOT NULL, value int NULL, label nvarchar(30) NULL)')
  await ok(c, 'CREATE CLUSTERED INDEX ix_items ON items(id)')
  await ok(c, 'CREATE INDEX ix_items_value ON items(id) INCLUDE(value, label)')
  await ok(c, 'CREATE INDEX ix_items_positive ON items(id) WHERE id>0')
  await ok(c, 'CREATE NONCLUSTERED INDEX ix_items_opts ON items(value DESC) INCLUDE(label) WHERE value IS NOT NULL WITH (FILLFACTOR = 80, PAD_INDEX = ON, SORT_IN_TEMPDB = ON, STATISTICS_NORECOMPUTE = OFF, DROP_EXISTING = OFF, ONLINE = OFF, ALLOW_ROW_LOCKS = ON, ALLOW_PAGE_LOCKS = ON, IGNORE_DUP_KEY = OFF, DATA_COMPRESSION = NONE) ON [PRIMARY]')
  await ok(c, 'CREATE INDEX ix_items_label ON items(label) WITH FILLFACTOR = 90')
  await ok(c, "INSERT items VALUES(1, 1, N'a'), (1, 2, N'b')")
  assert.deepEqual(await ok(c, 'SELECT id, value, label FROM items ORDER BY value'), [[1, 1, 'a'], [1, 2, 'b']])
  await fails(c, 'CREATE CLUSTERED INDEX ix_items_second ON items(value)', [1902, 3, 16, "Cannot create more than one clustered index on table 'items'. Drop the existing clustered index 'ix_items' before creating another."])
  await fails(c, 'CREATE INDEX ix_items ON items(value)', [1913, 1, 16, "The operation failed because an index or statistics with name 'ix_items' already exists on table 'items'."])
  await ok(c, 'DROP INDEX ix_items ON items')
  await ok(c, 'DROP INDEX ix_items_value ON dbo.items')
  await ok(c, 'DROP INDEX ix_items_positive ON items, ix_items_opts ON items')
  await ok(c, 'DROP INDEX IF EXISTS ix_items_positive ON items')
  await fails(c, 'DROP INDEX ix_items_positive ON items', [3701, 7, 11, "Cannot drop the index 'items.ix_items_positive', because it does not exist or you do not have permission."])
  await ok(c, 'DROP INDEX items.ix_items_label')
  await ok(c, 'CREATE CLUSTERED INDEX ix_items_second ON items(value)')
  await ok(c, 'DROP INDEX ix_items_second ON items WITH (ONLINE = OFF)')

  await ok(c, 'CREATE TABLE pairs(id int NOT NULL, code int NOT NULL)')
  await ok(c, 'CREATE UNIQUE CLUSTERED INDEX ux_pairs ON pairs(id, code) WITH (FILLFACTOR = 90)')
  await ok(c, 'INSERT pairs VALUES(1, 1), (1, 2)')
  await fails(c, 'INSERT pairs VALUES(1, 1)', ux('ux_pairs', 'dbo.pairs', '1, 1'))
  await ok(c, 'DROP INDEX ux_pairs ON pairs')
  await ok(c, 'INSERT pairs VALUES(1, 1)')
  assert.deepEqual(await ok(c, 'SELECT count(*) FROM pairs'), [[3]])

  await fails(c, 'DROP INDEX pk_keyed ON keyed', [3723, 4, 16, "An explicit DROP INDEX is not allowed on index 'keyed.pk_keyed'. It is being used for PRIMARY KEY constraint enforcement."])
  await ok(c, 'CREATE TABLE nc(id nvarchar(10) NOT NULL CONSTRAINT pk_nc PRIMARY KEY NONCLUSTERED, value int CONSTRAINT uq_nc UNIQUE)')
  await fails(c, 'DROP INDEX uq_nc ON nc', [3723, 4, 16, "An explicit DROP INDEX is not allowed on index 'nc.uq_nc'. It is being used for UNIQUE KEY constraint enforcement."])
  await ok(c, 'CREATE CLUSTERED INDEX ix_nc_value ON nc(value)')
  await fails(c, 'CREATE INDEX pk_nc ON nc(value)', [1913, 1, 16, "The operation failed because an index or statistics with name 'pk_nc' already exists on table 'nc'."])
  // The failed drop leaves the constraint enforced.
  await ok(c, "INSERT nc VALUES (N'a', 1)")
  await fails(c, "INSERT nc VALUES (N'a', 2)", pk('pk_nc', 'dbo.nc', 'a'))

  // Index creation and drop follow the transaction.
  await ok(c, 'BEGIN TRANSACTION; CREATE UNIQUE INDEX ux_tx ON pairs(code) WHERE code > 1; ROLLBACK')
  await fails(c, 'DROP INDEX ux_tx ON pairs', [3701, 7, 11, "Cannot drop the index 'pairs.ux_tx', because it does not exist or you do not have permission."])
})

test('key and index diagnostics match SQL Server', async t => {
  const c = await fresh(t)
  const failed = [1750, 0, 16, 'Could not create constraint or index. See previous errors.']
  await fails(c, 'CREATE TABLE t1(a int NULL PRIMARY KEY)', [8111, 1, 16, "Cannot define PRIMARY KEY constraint on nullable column in table 't1'."], failed)
  await fails(c, 'CREATE TABLE t2(a int PRIMARY KEY, b int PRIMARY KEY)', [8110, 0, 16, "Cannot add multiple PRIMARY KEY constraints to table 't2'."])
  await fails(c, 'CREATE TABLE t3(a nvarchar(max) NOT NULL PRIMARY KEY)', [1919, 1, 16, "Column 'a' in table 't3' is of a type that is invalid for use as a key column in an index."], failed)
  await fails(c, 'CREATE TABLE t4(a int, CONSTRAINT uq_t4 UNIQUE(a, a))', [1909, 1, 16, "Cannot use duplicate column names in index. Column name 'a' listed more than once."], failed)
  await fails(c, 'CREATE TABLE t5(a int, CONSTRAINT uq_t5 UNIQUE(b))', [1911, 1, 16, "Column name 'b' does not exist in the target table, index or view."], failed)
  await fails(c, 'CREATE TABLE t6(a int CONSTRAINT k6 UNIQUE, b int CONSTRAINT k6 UNIQUE)', [8168, 0, 16, "Cannot create, drop, enable, or disable more than one constraint, column, index, or trigger named 'k6' in this context. Duplicate names are not allowed."])
  await ok(c, 'CREATE TABLE t7(a int CONSTRAINT k7 PRIMARY KEY)')
  await fails(c, 'CREATE TABLE t8(a int CONSTRAINT k7 UNIQUE)', [2714, 5, 16, "There is already an object named 'k7' in the database."], [1750, 1, 16, 'Could not create constraint or index. See previous errors.'])
  for (const table of ['t1', 't2', 't3', 't4', 't5', 't6', 't8']) assert.deepEqual(await ok(c, `SELECT OBJECT_ID('${table}')`), [[null]])
  await ok(c, 'CREATE TABLE t9(a int, b nvarchar(max))')
  await fails(c, 'CREATE INDEX ix_t9 ON t9(missing)', [1911, 1, 16, "Column name 'missing' does not exist in the target table, index or view."])
  await fails(c, 'CREATE INDEX ix_t9 ON t9(a) INCLUDE(missing)', [1911, 1, 16, "Column name 'missing' does not exist in the target table, index or view."])
  await fails(c, 'CREATE INDEX ix_t9 ON t9(b)', [1919, 1, 16, "Column 'b' in table 't9' is of a type that is invalid for use as a key column in an index."])
  await fails(c, 'CREATE INDEX ix_t9 ON t9(a, a)', [1909, 1, 16, "Cannot use duplicate column names in index. Column name 'a' listed more than once."])
  await fails(c, 'CREATE INDEX ix_t9 ON t9(a) WITH (NOT_AN_OPTION = ON)', [155, 1, 15, "'NOT_AN_OPTION' is not a recognized CREATE INDEX option."])
  await fails(c, 'CREATE INDEX ix_t9 ON t9(a) WITH (IGNORE_DUP_KEY = ON)', [1916, 4, 16, 'CREATE INDEX options nonunique and ignore_dup_key are mutually exclusive.'])
  await fails(c, 'CREATE INDEX ix_t9 ON t9(a) WITH (FILLFACTOR = 0)', [129, 1, 15, 'Fillfactor 0 is not a valid percentage; fillfactor must be between 1 and 100.'])
  await fails(c, 'CREATE INDEX ix_t9 ON t9(a) WITH (DROP_EXISTING = ON)', [7999, 9, 16, "Could not find any index named 'ix_t9' for table 't9'."])
  await fails(c, 'CREATE INDEX ix_t9 ON t9(a) WHERE missing > 0', [207, 1, 16, "Invalid column name 'missing'."])
  await ok(c, 'CREATE INDEX ix_t9 ON t9(a)')
  await ok(c, 'CREATE UNIQUE INDEX ix_t9 ON t9(a) WITH (DROP_EXISTING = ON)')
  await fails(c, 'INSERT t9 VALUES(1, NULL), (1, NULL)', ux('ix_t9', 'dbo.t9', '1'))
  await ok(c, 'CREATE INDEX ix_t9 ON t9(a) WITH (DROP_EXISTING = ON)')
  await ok(c, 'INSERT t9 VALUES(1, NULL), (1, NULL)')
  assert.deepEqual(await ok(c, 'SELECT count(*) FROM t9'), [[2]])
  await fails(c, 'CREATE INDEX k7 ON t7(a)', [1913, 1, 16, "The operation failed because an index or statistics with name 'k7' already exists on table 't7'."])
  // Accepted by SQL Server, explicitly unsupported here.
  const unsupported = await run(c, 'CREATE UNIQUE INDEX ux_t9 ON t9(a) WITH (IGNORE_DUP_KEY = ON)')
  assert.deepEqual(unsupported.errors.map(e => e[0]), [40515])
})

test('CREATE UNIQUE INDEX over existing duplicates fails with 1505', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE items(id int NOT NULL, name nvarchar(20))')
  await ok(c, "INSERT items VALUES(1, N'x'), (2, N'x'), (3, NULL), (4, NULL)")
  await fails(c, 'CREATE UNIQUE INDEX ux_items_name ON items(name)', [1505, 1, 16, "The CREATE UNIQUE INDEX statement terminated because a duplicate key was found for the object name 'dbo.items' and the index name 'ux_items_name'. The duplicate key value is (<NULL>)."])
  await ok(c, 'CREATE UNIQUE INDEX ux_items_name_filtered ON items(name) WHERE id > 1 AND id < 4')
  await fails(c, 'CREATE UNIQUE INDEX ux_items_name_nn ON items(name) WHERE name IS NOT NULL', [1505, 1, 16, "The CREATE UNIQUE INDEX statement terminated because a duplicate key was found for the object name 'dbo.items' and the index name 'ux_items_name_nn'. The duplicate key value is (x)."])
  await ok(c, 'CREATE UNIQUE INDEX ux_items_id ON items(id)')
  await fails(c, 'CREATE INDEX ux_items_id ON items(name)', [1913, 1, 16, "The operation failed because an index or statistics with name 'ux_items_id' already exists on table 'items'."])
  // The failed creations left no index behind.
  await ok(c, 'CREATE UNIQUE INDEX ux_items_name ON items(name) WHERE id = 1')
})

test('duplicate keys are catchable and keep SQL Server identity', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE seq(id int NOT NULL CONSTRAINT pk_seq PRIMARY KEY, code nvarchar(10) NULL)')
  await ok(c, 'CREATE UNIQUE INDEX ux_seq_code ON seq(code)')
  await ok(c, "INSERT seq VALUES(1, N'a')")
  assert.deepEqual(await ok(c, "BEGIN TRY INSERT seq VALUES(1, N'b') END TRY BEGIN CATCH SELECT ERROR_NUMBER(), ERROR_SEVERITY(), ERROR_STATE(), ERROR_MESSAGE() END CATCH"),
    [[2627, 14, 1, "Violation of PRIMARY KEY constraint 'pk_seq'. Cannot insert duplicate key in object 'dbo.seq'. The duplicate key value is (1)."]])
  assert.deepEqual(await ok(c, "BEGIN TRY INSERT seq VALUES(2, N'a') END TRY BEGIN CATCH SELECT ERROR_NUMBER(), ERROR_SEVERITY(), ERROR_STATE(), ERROR_MESSAGE() END CATCH"),
    [[2601, 14, 1, "Cannot insert duplicate key row in object 'dbo.seq' with unique index 'ux_seq_code'. The duplicate key value is (a)."]])
  // The statement fails; the batch continues.
  assert.deepEqual(await run(c, "INSERT seq VALUES(3, N'a'); SELECT @@ERROR"), {
    errors: [ux('ux_seq_code', 'dbo.seq', 'a')],
    rows: [[2601]],
  })
  // Parameterized statements (sp_executesql) report the same error.
  await assert.rejects(query(c, 'INSERT seq VALUES(@id, @code)', [['id', TYPES.Int, 1], ['code', TYPES.NVarChar, 'z']]),
    e => e.number === 2627 && e.message === "Violation of PRIMARY KEY constraint 'pk_seq'. Cannot insert duplicate key in object 'dbo.seq'. The duplicate key value is (1).")
  // Inside a transaction the backend transaction can be aborted; the
  // message still names the constraint.
  const inside = await run(c, "BEGIN TRANSACTION; INSERT seq VALUES(1, N'q')")
  assert.deepEqual(inside.errors, [pk('pk_seq', 'dbo.seq', '1')])
  await query(c, 'ROLLBACK')
  assert.deepEqual(await ok(c, 'SELECT id, code FROM seq ORDER BY id'), [[1, 'a']])
})

test('duplicate values are shown as SQL Server shows them', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE items(a nvarchar(10) NOT NULL, b int NULL, d varchar(5) NULL, e uniqueidentifier NULL, f bit NULL, g decimal(9,2) NULL, h datetime2(3) NULL, CONSTRAINT uq_items UNIQUE(a, b, d, e, f, g, h))')
  const row = "INSERT items VALUES(N'it''s', NULL, 'v', '6F9619FF-8B86-D011-B42D-00C04FC964FF', 1, 12.5, '2024-01-02 03:04:05.678')"
  await ok(c, row)
  await fails(c, row, uq('uq_items', 'dbo.items', "it's, <NULL>, v, 6f9619ff-8b86-d011-b42d-00c04fc964ff, 1, 12.50, 2024-01-02 03:04:05.678"))
  await ok(c, 'CREATE TABLE more(m money NOT NULL, f float NOT NULL, t time(3) NOT NULL, dt datetime NOT NULL, sd smalldatetime NOT NULL, b varbinary(4) NOT NULL, c char(4) NOT NULL, d date NOT NULL, CONSTRAINT uq_more UNIQUE(m, f, t, dt, sd, b, c, d))')
  const more = "INSERT more VALUES(12.5, 1.5, '01:02:03.5', '2024-01-02 03:04:05', '2024-01-02 03:04:00', 0x01AB, 'a', '2024-01-02')"
  await ok(c, more)
  await fails(c, more, uq('uq_more', 'dbo.more', '12.50, 1.5, 01:02:03.500, Jan  2 2024  3:04AM, Jan  2 2024  3:04AM, 0x01ab, a   , 2024-01-02'))
})

test('ALTER TABLE keeps keys and indexes and refuses to drop their columns', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE items(id nvarchar(10) NOT NULL CONSTRAINT pk_items PRIMARY KEY, code int NULL CONSTRAINT uq_items_code UNIQUE, v int NULL, w int NULL, x int NULL)')
  await ok(c, 'CREATE INDEX ix_items_v ON items(v) INCLUDE(w)')
  await ok(c, 'CREATE INDEX ix_items_x ON items(x)')
  await ok(c, "CREATE UNIQUE INDEX ux_items_w ON items(w) WHERE w > 10")
  await ok(c, "INSERT items VALUES (N'a', NULL, 1, 11, 1)")
  await ok(c, 'ALTER TABLE items ADD extra int NULL')
  await ok(c, 'ALTER TABLE items ALTER COLUMN extra bigint NULL')
  await ok(c, 'ALTER TABLE items DROP COLUMN extra')
  const dependent = (kind, name, column, verb = 'DROP') => [
    [5074, 1, 16, `The ${kind} '${name}' is dependent on column '${column}'.`],
    [4922, 9, 16, `ALTER TABLE ${verb} COLUMN ${column} failed because one or more objects access this column.`],
  ]
  await fails(c, 'ALTER TABLE items DROP COLUMN code', ...dependent('object', 'uq_items_code', 'code'))
  await fails(c, 'ALTER TABLE items DROP COLUMN v', ...dependent('index', 'ix_items_v', 'v'))
  await fails(c, 'ALTER TABLE items DROP COLUMN w', ...dependent('index', 'ix_items_v', 'w'))
  await fails(c, 'ALTER TABLE items DROP COLUMN x', ...dependent('index', 'ix_items_x', 'x'))
  await fails(c, 'ALTER TABLE items ALTER COLUMN code bigint NULL', ...dependent('object', 'uq_items_code', 'code', 'ALTER'))
  // Keys and indexes still hold after the rebuilds.
  await fails(c, "INSERT items VALUES (N'b', NULL, 2, 2, 2)", uq('uq_items_code', 'dbo.items', '<NULL>'))
  await fails(c, "INSERT items VALUES (N'a', 5, 2, 2, 2)", pk('pk_items', 'dbo.items', 'a'))
  await fails(c, "INSERT items VALUES (N'c', 6, 2, 11, 2)", ux('ux_items_w', 'dbo.items', '11'))
  await ok(c, 'DROP INDEX ix_items_v ON items')
  await ok(c, 'DROP INDEX ix_items_x ON items')
  await ok(c, 'ALTER TABLE items DROP COLUMN v, x')
  await fails(c, "INSERT items VALUES (N'd', 7, 11)", ux('ux_items_w', 'dbo.items', '11'))
  await ok(c, 'DROP INDEX ux_items_w ON items')
  await ok(c, "INSERT items VALUES (N'd', 7, 11)")
  assert.deepEqual(await ok(c, 'SELECT id, code, w FROM items ORDER BY id'), [['a', null, 11], ['d', 7, 11]])
  // ALTER inside a transaction: rolled back, failed or committed, the keys
  // stay in place.
  await ok(c, 'ALTER TABLE items ADD y int NULL')
  await ok(c, 'BEGIN TRANSACTION; ALTER TABLE items DROP COLUMN y; ROLLBACK')
  await fails(c, "INSERT items VALUES (N'e', 7, 1, 1)", uq('uq_items_code', 'dbo.items', '7'))
  const failed = await run(c, 'BEGIN TRANSACTION; ALTER TABLE items ADD t time(8) NULL')
  assert.equal(failed.errors.length, 1)
  await ok(c, 'COMMIT')
  await fails(c, "INSERT items VALUES (N'e', 7, 1, 1)", uq('uq_items_code', 'dbo.items', '7'))
  await ok(c, 'BEGIN TRANSACTION; ALTER TABLE items ADD z int NULL; COMMIT')
  await fails(c, "INSERT items VALUES (N'e', 7, 1, 1, 1)", uq('uq_items_code', 'dbo.items', '7'))
  await ok(c, "INSERT items VALUES (N'e', 8, 1, 1, 1)")
  // A column a filter references is a dependency too.
  await ok(c, 'CREATE TABLE filtered(id int NOT NULL, code int NULL, status int NULL)')
  await ok(c, 'CREATE UNIQUE INDEX ux_filtered ON filtered(code) WHERE status > 0')
  await fails(c, 'ALTER TABLE filtered DROP COLUMN status', ...dependent('index', 'ux_filtered', 'status'))
  await ok(c, 'ALTER TABLE filtered DROP COLUMN id')
  await ok(c, 'INSERT filtered VALUES (1, 1), (1, 0)')
  await fails(c, 'INSERT filtered VALUES (1, 2)', ux('ux_filtered', 'dbo.filtered', '1'))
})

test('nullable unique keys stay foreign key targets; ambiguous duplicates keep DuckDB text', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE parents(id int NOT NULL PRIMARY KEY, code int NULL UNIQUE)')
  await ok(c, 'CREATE TABLE children(code int NULL REFERENCES parents(code))')
  await ok(c, 'INSERT parents VALUES (1, 5), (2, NULL)')
  await ok(c, 'INSERT children VALUES (5), (NULL)')
  assert.equal((await run(c, 'INSERT children VALUES (6)')).errors.length, 1)
  assert.equal((await run(c, 'INSERT parents VALUES (3, NULL)')).errors[0][0], 2627)
  // One statement repeating a key names neither the constraint nor its
  // columns; with two candidate constraints the error keeps DuckDB's text,
  // with SQL Server's number, and ends only the statement.
  const ambiguous = await run(c, 'INSERT parents VALUES (4, 8), (5, 8); SELECT @@ERROR')
  assert.deepEqual(ambiguous.errors.map(e => e[0]), [2627])
  assert.match(ambiguous.errors[0][3], /duplicate key "8"/)
  assert.deepEqual(ambiguous.rows, [[2627]])
})

test('filters with any text, 1505 key order and existing tables', async t => {
  const c = await fresh(t)
  await ok(c, 'CREATE TABLE items(id int NOT NULL, status varchar(20) NULL, code int NULL)')
  await ok(c, "CREATE UNIQUE INDEX ux_items ON items(code) WHERE status = 'a: b, c'")
  await ok(c, "INSERT items VALUES (1, 'a: b, c', 1), (2, 'other', 1)")
  await fails(c, "INSERT items VALUES (3, 'a: b, c', 1)", ux('ux_items', 'dbo.items', '1'))
  await ok(c, 'CREATE TABLE numbers(n int NULL)')
  await ok(c, 'INSERT numbers VALUES (10), (10), (9), (9)')
  await fails(c, 'CREATE UNIQUE INDEX ux_numbers ON numbers(n)', [1505, 1, 16, "The CREATE UNIQUE INDEX statement terminated because a duplicate key was found for the object name 'dbo.numbers' and the index name 'ux_numbers'. The duplicate key value is (9)."])
  await ok(c, 'CREATE TABLE keyed(id int NOT NULL CONSTRAINT pk_keyed PRIMARY KEY)')
  const again = await run(c, 'CREATE TABLE keyed(id int NOT NULL CONSTRAINT pk_keyed PRIMARY KEY)')
  // The existing table is reported (with the engine's existing text), not
  // the constraint name.
  assert.equal(again.errors.length, 1)
  assert.match(again.errors[0][3], /keyed/)
  assert.doesNotMatch(again.errors[0][3], /pk_keyed/)
  // DROP_EXISTING over duplicates fails before dropping, even in a transaction.
  await ok(c, 'CREATE INDEX ix_numbers ON numbers(n)')
  await fails(c, 'BEGIN TRANSACTION; CREATE UNIQUE INDEX ix_numbers ON numbers(n) WITH (DROP_EXISTING = ON)', [1505, 1, 16, "The CREATE UNIQUE INDEX statement terminated because a duplicate key was found for the object name 'dbo.numbers' and the index name 'ix_numbers'. The duplicate key value is (9)."])
  await query(c, 'COMMIT')
  await fails(c, 'CREATE INDEX ix_numbers ON numbers(n)', [1913, 1, 16, "The operation failed because an index or statistics with name 'ix_numbers' already exists on table 'numbers'."])
})

// Steps whose SQL Server result msduck does not reproduce, with the reason.
// Everything else in reference/gaps-keys.json must match exactly (errors
// with state, severity and message, and every row), except generated
// constraint names, which are random in SQL Server.
const differences = {
  'nvarchar-primary-key': {
    3: 'keys compare with BIN2 equality, not the case-insensitive default collation',
    4: 'the message shows the stored key, without trailing spaces',
    6: 'nvarchar comparison with a literal in WHERE is unsupported (comparison lowering)',
    7: 'the case-insensitive duplicate was accepted in step 3',
    8: 'NOT NULL violations keep the backend message (engine)',
  },
  'datetimeoffset-keys': {
    2: 'the message shows the UTC instant, not the inserted offset',
  },
  'unique-index-null': {
    5: 'keys compare with BIN2 equality, not the case-insensitive default collation',
    6: 'follows step 5',
  },
  'clustered-include-options': {
    7: 'sys.indexes does not yet model clustered, included or filtered indexes (index catalog)',
  },
  'clustered-conflicts': {
    1: 'the clustering of PRIMARY KEY and UNIQUE constraints is not recorded',
    5: 'the clustering of PRIMARY KEY and UNIQUE constraints is not recorded',
    8: 'sys.indexes does not yet model key constraints (index catalog)',
  },
  'ignore-dup-key': { 1: 'IGNORE_DUP_KEY = ON is unsupported', 4: 'follows step 1', 5: 'follows step 1' },
  'message-values': { 2: 'the message shows the UTC instant, not the inserted offset' },
}

test('reference programs match SQL Server except documented differences', async t => {
  const c = await start(t)
  assert.equal(reference.cases.length, cases.length)
  for (const [index, [id, statements]] of cases.entries()) {
    const expected = reference.cases[index]
    assert.equal(expected.id, id)
    const database = `keys_ref_${process.pid}_${index}`
    await query(c, `CREATE DATABASE ${database}`)
    await query(c, `USE ${database}`)
    for (const [step, sql] of statements.entries()) {
      assert.equal(expected.steps[step].sql, sql)
      const actual = await run(c, sql)
      const generated = value => JSON.parse(JSON.stringify(value).replace(/((?:PK|UQ)__[0-9A-Za-z_]+?__)[0-9A-F]{16}/g, '$1<hash>'))
      const wanted = {
        errors: expected.steps[step].result.errors.map(e => [e.number, e.state, e.class, e.message]),
        rows: expected.steps[step].result.sets.flatMap(set => set.rows),
      }
      // A listed difference must still be one, so the list stays accurate.
      if (differences[id]?.[step]) assert.notDeepEqual(generated(actual), wanted, `${id} step ${step} now matches: ${sql}`)
      else assert.deepEqual(generated(actual), wanted, `${id} step ${step}: ${sql}`)
    }
  }
})
