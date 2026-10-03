// ALTER TABLE constraint lifecycle and foreign-key referential actions
// (docs/gaps-constraints.md). Expected rows, error numbers and messages are
// SQL Server's: with MSDUCK_CONSTRAINTS_REFERENCE='{"port":N,"password":"..."}'
// the same file runs against a SQL Server reference instead of msduck.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Connection } from 'tedious'
import { start, query } from '../support/client.mjs'
import { capture } from '../../scripts/lib/compatibility.mjs'

const reference = process.env.MSDUCK_CONSTRAINTS_REFERENCE && JSON.parse(process.env.MSDUCK_CONSTRAINTS_REFERENCE)

/** A connection using a fresh `constraints_db` database. */
async function open(t) {
  let c
  if (reference) {
    c = new Connection({
      server: '127.0.0.1',
      authentication: { type: 'default', options: { userName: 'sa', password: reference.password } },
      options: { port: reference.port, encrypt: true, trustServerCertificate: true, database: 'master', requestTimeout: 60000 }
    })
    c.on('error', () => {})
    await new Promise((resolve, reject) => c.connect(error => error ? reject(error) : resolve()))
    t.after(() => c.close())
    await query(c, "IF DB_ID('constraints_db') IS NOT NULL BEGIN ALTER DATABASE constraints_db SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE constraints_db END")
  } else {
    c = await start(t, { options: { requestTimeout: 60000 } })
  }
  await query(c, 'CREATE DATABASE constraints_db')
  await query(c, 'USE constraints_db')
  return c
}

const numbers = result => result.errors.map(error => error.number)
const rows = async (c, sql) => (await query(c, sql)).rows

/** A SQL batch (not an RPC), which may leave a transaction open. */
async function batch(c, sql) {
  const result = await capture(c, sql)
  assert.deepEqual(result.errors, [], sql)
  return result.sets.map(set => set.rows)
}

async function fails(c, sql, expected, message) {
  const result = await capture(c, sql)
  assert.deepEqual(numbers(result), expected, `${sql}: ${JSON.stringify(result.errors)}`)
  if (message) assert.match(result.errors.at(-1 - (expected.length > 1 && expected.at(-1) === 1750 ? 1 : 0)).message, message)
  return result
}

async function database(t) {
  const c = await open(t)
  await query(c, 'CREATE TABLE parent(id int NOT NULL PRIMARY KEY, name varchar(10)); INSERT parent VALUES (1,\'a\'),(2,\'b\')')
  await query(c, 'CREATE TABLE items(id int NOT NULL, value int, parent_id int); INSERT items VALUES (1,10,1),(2,20,2),(3,30,NULL)')
  return c
}

test('ALTER TABLE ADD COLUMN with a named DEFAULT WITH VALUES fills existing rows', { timeout: 60000 }, async t => {
  const c = await database(t)
  await query(c, 'ALTER TABLE items ADD added int NOT NULL CONSTRAINT df_added DEFAULT 7 WITH VALUES')
  assert.deepEqual(await rows(c, 'SELECT id, added FROM items ORDER BY id'), [[1, 7], [2, 7], [3, 7]])
  await query(c, 'INSERT items(id, value) VALUES (4, 40)')
  assert.deepEqual(await rows(c, 'SELECT added FROM items WHERE id = 4'), [[7]])
  assert.deepEqual(await rows(c, "SELECT name, type FROM sys.objects WHERE name = 'df_added'"), [['df_added', 'D ']])
  // A NOT NULL column takes its default in existing rows even without WITH VALUES.
  await query(c, 'ALTER TABLE items ADD required int NOT NULL CONSTRAINT df_required DEFAULT 3')
  assert.deepEqual(await rows(c, 'SELECT id, required FROM items ORDER BY id'), [[1, 3], [2, 3], [3, 3], [4, 3]])
  await query(c, 'ALTER TABLE items ADD nullable_added int NULL CONSTRAINT df_nullable DEFAULT 8 WITH VALUES')
  assert.deepEqual(await rows(c, 'SELECT nullable_added FROM items WHERE id = 1'), [[8]])
  await fails(c, 'ALTER TABLE items ADD other int NOT NULL CONSTRAINT df_added DEFAULT 1', [2714, 1750])
  assert.deepEqual(await rows(c, "SELECT count(*) FROM sys.columns WHERE object_id = OBJECT_ID('items') AND name = 'other'"), [[0]])
  // Constraints and columns in one ADD: columns first, then constraints.
  await query(c, 'ALTER TABLE items ADD CONSTRAINT ck_low CHECK (value < 100), extra int NOT NULL CONSTRAINT df_extra DEFAULT 5, CONSTRAINT ck_extra CHECK (extra > 0)')
  assert.deepEqual(await rows(c, 'SELECT id, extra FROM items ORDER BY id'), [[1, 5], [2, 5], [3, 5], [4, 5]])
  await fails(c, 'INSERT items(id, value, extra) VALUES (5, 50, 0)', [547], /CHECK constraint "ck_extra"/)
})

test('CHECK constraints: validation, WITH NOCHECK, NOCHECK and WITH CHECK CHECK CONSTRAINT', { timeout: 60000 }, async t => {
  const c = await database(t)
  await query(c, 'UPDATE items SET value = -5 WHERE id = 2')
  const conflict = await fails(c, 'ALTER TABLE items ADD CONSTRAINT ck_items CHECK(value>0)', [547])
  assert.equal(conflict.errors[0].message, 'The ALTER TABLE statement conflicted with the CHECK constraint "ck_items". The conflict occurred in database "constraints_db", table "dbo.items", column \'value\'.')
  await fails(c, 'ALTER TABLE items WITH CHECK ADD CONSTRAINT ck_items CHECK(value>0)', [547])
  await query(c, 'ALTER TABLE items WITH NOCHECK ADD CONSTRAINT ck_items CHECK(value>0)')
  assert.deepEqual(await rows(c, "SELECT name, is_disabled, is_not_trusted, parent_column_id, is_system_named FROM sys.check_constraints"), [['ck_items', false, true, 2, false]])
  const insert = await fails(c, 'INSERT items VALUES (4,-1,1)', [547])
  assert.equal(insert.errors[0].message, 'The INSERT statement conflicted with the CHECK constraint "ck_items". The conflict occurred in database "constraints_db", table "dbo.items", column \'value\'.')
  assert.deepEqual(insert.info.map(info => info.number), [3621])
  await fails(c, 'UPDATE items SET value=-2 WHERE id=1', [547])
  // An old violating row may be updated without touching its checked column.
  await query(c, 'UPDATE items SET parent_id=1 WHERE id=2')
  await fails(c, 'UPDATE items SET value=-7 WHERE id=2', [547])
  // Multi-row statements are atomic.
  await fails(c, 'INSERT items VALUES (5,5,1),(6,-6,1)', [547])
  assert.deepEqual(await rows(c, 'SELECT id, value, parent_id FROM items ORDER BY id'), [[1, 10, 1], [2, -5, 1], [3, 30, null]])
  await query(c, 'ALTER TABLE items NOCHECK CONSTRAINT ck_items')
  assert.deepEqual(await rows(c, 'SELECT is_disabled, is_not_trusted FROM sys.check_constraints'), [[true, true]])
  await query(c, 'INSERT items VALUES (5,-1,1)')
  await query(c, 'ALTER TABLE items CHECK CONSTRAINT ck_items')
  assert.deepEqual(await rows(c, 'SELECT is_disabled, is_not_trusted FROM sys.check_constraints'), [[false, true]])
  await fails(c, 'ALTER TABLE items WITH CHECK CHECK CONSTRAINT ck_items', [547])
  await fails(c, 'ALTER TABLE items WITH CHECK CHECK CONSTRAINT ALL', [547])
  await query(c, 'DELETE items WHERE value < 0')
  await query(c, 'ALTER TABLE items WITH CHECK CHECK CONSTRAINT ALL')
  assert.deepEqual(await rows(c, 'SELECT is_disabled, is_not_trusted FROM sys.check_constraints'), [[false, false]])
  await query(c, 'ALTER TABLE items NOCHECK CONSTRAINT ALL')
  await query(c, 'INSERT items VALUES (6,-1,1)')
  await fails(c, 'ALTER TABLE items NOCHECK CONSTRAINT nope', [4917, 4916])
  await fails(c, 'ALTER TABLE items ADD CONSTRAINT ck_sub CHECK (value > (SELECT 1))', [1046])
  await fails(c, 'ALTER TABLE items ADD CONSTRAINT ck_col CHECK (missing > 1)', [207])
  await fails(c, 'ALTER TABLE items ADD CONSTRAINT ck_bool CHECK (1)', [4145])
  await fails(c, 'ALTER TABLE items ADD CONSTRAINT parent CHECK (value > 0)', [2714, 1750])
  assert.deepEqual(await rows(c, 'BEGIN TRY ALTER TABLE items ADD CONSTRAINT ck_items CHECK (id > 0) END TRY BEGIN CATCH SELECT ERROR_NUMBER() END CATCH'), [[1750]])
  // Inline and table-level CHECK constraints of CREATE TABLE behave the same.
  await query(c, 'CREATE TABLE checked(id int CONSTRAINT ck_id CHECK (id > 0), low int, high int, CHECK (low <= high))')
  await fails(c, 'INSERT checked VALUES (0, 1, 2)', [547], /CHECK constraint "ck_id"/)
  const tableLevel = await fails(c, 'INSERT checked VALUES (1, 3, 2)', [547])
  assert.match(tableLevel.errors[0].message, /^The INSERT statement conflicted with the CHECK constraint "CK__checked__[0-9A-F]{8}"\. The conflict occurred in database "constraints_db", table "dbo\.checked"\.$/)
  await query(c, 'INSERT checked VALUES (1, NULL, 2)')
  assert.deepEqual(await rows(c, 'BEGIN TRY INSERT checked VALUES (-1, 1, 1) END TRY BEGIN CATCH SELECT ERROR_NUMBER() END CATCH'), [[547]])
})

test('DEFAULT, UNIQUE and PRIMARY KEY constraints are added and dropped by name', { timeout: 60000 }, async t => {
  const c = await database(t)
  await query(c, 'ALTER TABLE items ADD CONSTRAINT df_items DEFAULT(0) FOR value')
  await query(c, 'INSERT items(id, parent_id) VALUES (4, 1)')
  assert.deepEqual(await rows(c, 'SELECT value FROM items WHERE id = 4'), [[0]])
  await fails(c, 'ALTER TABLE items ADD CONSTRAINT df_again DEFAULT(1) FOR value', [1781, 1750])
  await fails(c, 'ALTER TABLE items ADD CONSTRAINT df_missing DEFAULT(1) FOR missing', [1752, 1750])
  await fails(c, 'ALTER TABLE items ADD CONSTRAINT df_column DEFAULT(id + 1) FOR parent_id', [128])
  await query(c, 'ALTER TABLE items DROP CONSTRAINT df_items')
  await query(c, 'INSERT items(id, parent_id) VALUES (5, 1)')
  assert.deepEqual(await rows(c, 'SELECT value FROM items WHERE id = 5'), [[null]])

  await query(c, 'INSERT items VALUES (1, 11, 1)')
  const duplicate = await fails(c, 'ALTER TABLE items ADD CONSTRAINT uq_items UNIQUE(id)', [1505, 1750])
  assert.equal(duplicate.errors[0].message, "The CREATE UNIQUE INDEX statement terminated because a duplicate key was found for the object name 'dbo.items' and the index name 'uq_items'. The duplicate key value is (1).")
  await query(c, 'DELETE items WHERE value = 11')
  await query(c, 'ALTER TABLE items ADD CONSTRAINT uq_items UNIQUE(id)')
  await fails(c, 'INSERT items VALUES (1, 1, 1)', [2627])
  // Data and other constraints survive the rebuild that adds the key.
  assert.deepEqual(await rows(c, 'SELECT id, value, parent_id FROM items ORDER BY id'), [[1, 10, 1], [2, 20, 2], [3, 30, null], [4, 0, 1], [5, null, 1]])
  await query(c, 'ALTER TABLE items ADD CONSTRAINT pk_items PRIMARY KEY(id)')
  await fails(c, 'ALTER TABLE items ADD CONSTRAINT pk_other PRIMARY KEY(value)', [1779, 1750])
  await query(c, 'ALTER TABLE items ADD n int NULL')
  await fails(c, 'ALTER TABLE parent ADD CONSTRAINT uq_name UNIQUE (name), CONSTRAINT pk_null PRIMARY KEY (name)', [1779, 1750])
  assert.deepEqual(await rows(c, "SELECT name, type FROM sys.objects WHERE parent_object_id = OBJECT_ID('items') ORDER BY name"), [['pk_items', 'PK'], ['uq_items', 'UQ']])
  assert.deepEqual(await rows(c, "SELECT name, type FROM sys.key_constraints WHERE parent_object_id = OBJECT_ID('items') ORDER BY name"), [['pk_items', 'PK'], ['uq_items', 'UQ']])
  await query(c, 'ALTER TABLE items DROP CONSTRAINT uq_items')
  await fails(c, 'INSERT items(id, value, parent_id) VALUES (1, 1, 1)', [2627])
  await query(c, 'ALTER TABLE items DROP CONSTRAINT pk_items')
  await query(c, 'INSERT items(id, value, parent_id) VALUES (1, 1, 1)')
  assert.deepEqual(await rows(c, 'SELECT count(*) FROM items WHERE id = 1'), [[2]])
  // The column stays NOT NULL after its primary key is dropped.
  await fails(c, 'INSERT items(id) VALUES (NULL)', [515])
  await fails(c, 'ALTER TABLE items DROP CONSTRAINT nope', [3728, 3727])
  await query(c, 'ALTER TABLE items DROP CONSTRAINT IF EXISTS nope')
  await fails(c, 'ALTER TABLE items NOCHECK CONSTRAINT df_missing', [4917, 4916])
  await query(c, 'CREATE TABLE nullable_key(id int NULL)')
  await fails(c, 'ALTER TABLE nullable_key ADD CONSTRAINT pk_nullable PRIMARY KEY (id)', [8111, 1750])
})

test('FOREIGN KEY constraints: ALTER TABLE forms, validation, NO ACTION and disabling', { timeout: 60000 }, async t => {
  const c = await database(t)
  await query(c, 'INSERT items VALUES (4, 40, 9)')
  const orphan = await fails(c, 'ALTER TABLE items ADD CONSTRAINT fk_items FOREIGN KEY(parent_id) REFERENCES parent(id)', [547])
  assert.equal(orphan.errors[0].message, 'The ALTER TABLE statement conflicted with the FOREIGN KEY constraint "fk_items". The conflict occurred in database "constraints_db", table "dbo.parent", column \'id\'.')
  await query(c, 'ALTER TABLE items WITH NOCHECK ADD CONSTRAINT fk_items FOREIGN KEY(parent_id) REFERENCES parent(id)')
  assert.deepEqual(await rows(c, 'SELECT name, is_disabled, is_not_trusted, delete_referential_action_desc, update_referential_action_desc FROM sys.foreign_keys'), [['fk_items', false, true, 'NO_ACTION', 'NO_ACTION']])
  const child = await fails(c, 'INSERT items VALUES (5, 50, 7)', [547])
  assert.equal(child.errors[0].message, 'The INSERT statement conflicted with the FOREIGN KEY constraint "fk_items". The conflict occurred in database "constraints_db", table "dbo.parent", column \'id\'.')
  const parentDelete = await fails(c, 'DELETE parent WHERE id = 1', [547])
  assert.equal(parentDelete.errors[0].message, 'The DELETE statement conflicted with the REFERENCE constraint "fk_items". The conflict occurred in database "constraints_db", table "dbo.items", column \'parent_id\'.')
  await fails(c, 'UPDATE parent SET id = 5 WHERE id = 1', [547], /UPDATE statement conflicted with the REFERENCE constraint "fk_items"/)
  assert.deepEqual(await rows(c, 'SELECT id FROM parent ORDER BY id'), [[1], [2]])
  // The old orphan (9) stays acceptable; a row without a key is not checked.
  await query(c, 'UPDATE items SET value = 41 WHERE id = 4')
  await query(c, 'INSERT items VALUES (6, 60, NULL)')
  await fails(c, 'ALTER TABLE items WITH CHECK CHECK CONSTRAINT fk_items', [547])
  await query(c, 'DELETE items WHERE id = 4')
  await query(c, 'ALTER TABLE items WITH CHECK CHECK CONSTRAINT fk_items')
  assert.deepEqual(await rows(c, 'SELECT is_not_trusted FROM sys.foreign_keys'), [[false]])
  await query(c, 'ALTER TABLE items NOCHECK CONSTRAINT ALL')
  await query(c, 'INSERT items VALUES (7, 70, 99)')
  await query(c, 'DELETE parent WHERE id = 2')
  await fails(c, 'ALTER TABLE items WITH CHECK CHECK CONSTRAINT ALL', [547])
  await query(c, 'DELETE items WHERE parent_id IN (2, 99)')
  await query(c, 'ALTER TABLE items WITH CHECK CHECK CONSTRAINT ALL')
  await fails(c, 'DROP TABLE parent', [3726])
  await fails(c, 'TRUNCATE TABLE parent', [4712])
  await fails(c, 'ALTER TABLE items DROP COLUMN parent_id', [5074, 4922])
  await fails(c, 'ALTER TABLE parent DROP CONSTRAINT fk_items', [3733, 3727])
  const pk = (await rows(c, "SELECT name FROM sys.key_constraints WHERE parent_object_id = OBJECT_ID('parent')"))[0][0]
  assert.match(pk, /^PK__parent__[0-9A-F]{16}$/)
  await fails(c, `ALTER TABLE parent DROP CONSTRAINT ${pk}`, [3725, 3727])
  await query(c, 'ALTER TABLE items DROP CONSTRAINT fk_items')
  await query(c, 'INSERT items VALUES (8, 80, 42)')
  await query(c, 'DROP TABLE parent')
  // Definition errors.
  await query(c, 'CREATE TABLE p(id int PRIMARY KEY, code varchar(10) NOT NULL UNIQUE, other int); CREATE TABLE np(id int)')
  await fails(c, 'CREATE TABLE c1(id int, pid int REFERENCES np)', [1773, 1750])
  await fails(c, 'CREATE TABLE c2(id int, pid int, CONSTRAINT f2 FOREIGN KEY (pid) REFERENCES p(other))', [1776, 1750])
  await fails(c, 'CREATE TABLE c3(id int, pid int REFERENCES nope(id))', [1767, 1750])
  await fails(c, 'CREATE TABLE c4(id bigint REFERENCES p(id))', [1778, 1750])
  await fails(c, 'CREATE TABLE c5(code varchar(20) REFERENCES p(code))', [1753, 1750])
  await fails(c, 'CREATE TABLE c6(id int NOT NULL REFERENCES p(id) ON DELETE SET NULL)', [1761, 1750])
  await fails(c, 'CREATE TABLE c7(id int NOT NULL REFERENCES p(id) ON DELETE SET DEFAULT)', [1762, 1750])
  await fails(c, 'CREATE TABLE c8(id int, x int, CONSTRAINT f8 FOREIGN KEY (id, x) REFERENCES p(id))', [8139])
  await fails(c, 'CREATE TABLE c9(id int, CONSTRAINT dup CHECK (id > 0), CONSTRAINT dup CHECK (id > 1))', [8168])
  assert.deepEqual(await rows(c, "SELECT count(*) FROM sys.objects WHERE name LIKE 'c[1-9]'"), [[0]])
  // A self-referencing key: SAME TABLE diagnostics.
  await query(c, 'CREATE TABLE tree(id int PRIMARY KEY, parent int, CONSTRAINT fk_tree FOREIGN KEY (parent) REFERENCES tree(id))')
  await fails(c, 'INSERT tree VALUES (1, 2)', [547], /FOREIGN KEY SAME TABLE constraint "fk_tree"/)
  await query(c, 'INSERT tree VALUES (1, NULL), (2, 1), (3, 2)')
  await fails(c, 'DELETE tree WHERE id = 1', [547], /SAME TABLE REFERENCE constraint "fk_tree"/)
  await query(c, 'DELETE tree')
})

test('referential actions cascade through several levels', { timeout: 60000 }, async t => {
  const c = await open(t)
  await query(c, 'CREATE TABLE a(id int PRIMARY KEY, k int NOT NULL UNIQUE)')
  await query(c, 'CREATE TABLE b(id int PRIMARY KEY, a_id int REFERENCES a(id) ON DELETE CASCADE ON UPDATE CASCADE, a_k int DEFAULT 2, CONSTRAINT fk_bk FOREIGN KEY (a_k) REFERENCES a(k))')
  await query(c, 'CREATE TABLE c(id int PRIMARY KEY, b_id int, CONSTRAINT fk_cb FOREIGN KEY (b_id) REFERENCES b(id) ON DELETE SET NULL ON UPDATE CASCADE)')
  await query(c, 'CREATE TABLE d(id int PRIMARY KEY, c_id int, CONSTRAINT fk_dc FOREIGN KEY (c_id) REFERENCES c(id) ON DELETE CASCADE)')
  await query(c, 'INSERT a VALUES (1,1),(2,2),(3,3); INSERT b VALUES (10,1,1),(11,1,3),(12,2,2),(13,3,3); INSERT c VALUES (100,10),(101,11),(102,12); INSERT d VALUES (1000,100),(1001,102)')
  await query(c, 'DELETE a WHERE id = 1')
  assert.deepEqual(await rows(c, 'SELECT * FROM b ORDER BY id'), [[12, 2, 2], [13, 3, 3]])
  assert.deepEqual(await rows(c, 'SELECT * FROM c ORDER BY id'), [[100, null], [101, null], [102, 12]])
  // Multi-row key updates cascade row by row.
  await query(c, 'UPDATE a SET id = id + 10')
  assert.deepEqual(await rows(c, 'SELECT * FROM b ORDER BY id'), [[12, 12, 2], [13, 13, 3]])
  await query(c, 'UPDATE b SET id = id * 2')
  assert.deepEqual(await rows(c, 'SELECT * FROM c ORDER BY id'), [[100, null], [101, null], [102, 24]])
  // NO ACTION on a_k is checked after the cascade removed the referencing row.
  await query(c, 'DELETE a WHERE k = 2')
  assert.deepEqual(await rows(c, 'SELECT * FROM a ORDER BY id'), [[13, 3]])
  assert.deepEqual(await rows(c, 'SELECT * FROM b ORDER BY id'), [[26, 13, 3]])
  assert.deepEqual(await rows(c, 'SELECT * FROM c ORDER BY id'), [[100, null], [101, null], [102, null]])
  assert.deepEqual(await rows(c, 'SELECT * FROM d ORDER BY id'), [[1000, 100], [1001, 102]])
  await query(c, 'DELETE c WHERE id = 102')
  assert.deepEqual(await rows(c, 'SELECT * FROM d ORDER BY id'), [[1000, 100]])
  await fails(c, 'UPDATE a SET k = 30', [547], /UPDATE statement conflicted with the REFERENCE constraint "fk_bk"/)
  assert.deepEqual(await rows(c, 'SELECT * FROM a'), [[13, 3]])
  await query(c, 'DELETE a')
  assert.deepEqual(await rows(c, 'SELECT count(*) FROM b'), [[0]])
  // Two cascade paths to one table, or a cascading cycle, are refused.
  await fails(c, 'CREATE TABLE b2(id int, x int REFERENCES a(id) ON DELETE CASCADE, y int, CONSTRAINT fk_b2 FOREIGN KEY (y) REFERENCES a(id) ON DELETE SET NULL)', [1785, 1750])
  await fails(c, 'CREATE TABLE e(id int PRIMARY KEY, parent int, CONSTRAINT fk_e FOREIGN KEY (parent) REFERENCES e(id) ON DELETE CASCADE)', [1785, 1750])
  await query(c, 'CREATE TABLE e(id int PRIMARY KEY, parent int)')
  await fails(c, 'ALTER TABLE e ADD CONSTRAINT fk_e FOREIGN KEY (parent) REFERENCES e(id) ON UPDATE CASCADE', [1785, 1750])
})

test('ON DELETE SET NULL continues only through ON UPDATE actions when checking cascade paths', { timeout: 60000 }, async t => {
  const c = await open(t)
  await query(c, 'CREATE TABLE parent(id int PRIMARY KEY); CREATE TABLE middle(id int PRIMARY KEY, parent_id int); CREATE TABLE child(id int PRIMARY KEY, middle_id int); CREATE TABLE leaf(id int PRIMARY KEY, child_id int, parent_id int)')
  await query(c, 'ALTER TABLE middle ADD CONSTRAINT fk_mp FOREIGN KEY (parent_id) REFERENCES parent(id) ON UPDATE CASCADE ON DELETE CASCADE')
  await query(c, 'ALTER TABLE leaf ADD CONSTRAINT fk_lp FOREIGN KEY (parent_id) REFERENCES parent(id) ON UPDATE CASCADE ON DELETE CASCADE')
  await query(c, 'ALTER TABLE leaf ADD CONSTRAINT fk_lc FOREIGN KEY (child_id) REFERENCES child(id) ON UPDATE NO ACTION ON DELETE CASCADE')
  // The SET NULL updates child; child's delete edge to leaf is not followed.
  await query(c, 'ALTER TABLE child ADD CONSTRAINT fk_cm FOREIGN KEY (middle_id) REFERENCES middle(id) ON UPDATE CASCADE ON DELETE SET NULL')
  await query(c, 'INSERT parent VALUES (1),(2); INSERT middle VALUES (1,1); INSERT child VALUES (1,1); INSERT leaf VALUES (1,1,2),(2,1,1)')
  await query(c, 'DELETE parent WHERE id = 1')
  assert.deepEqual(await rows(c, 'SELECT * FROM middle'), [])
  assert.deepEqual(await rows(c, 'SELECT * FROM child'), [[1, null]])
  assert.deepEqual(await rows(c, 'SELECT * FROM leaf ORDER BY id'), [[1, 1, 2]])
  // The updated rows continue through ON UPDATE actions, so a second path
  // to a table that the delete also reaches is refused.
  await query(c, 'CREATE TABLE p(id int PRIMARY KEY); CREATE TABLE sn(id int PRIMARY KEY, pid int CONSTRAINT uq_sn UNIQUE CONSTRAINT fsn REFERENCES p(id) ON DELETE SET NULL)')
  const refused = await fails(c, 'CREATE TABLE g2(id int PRIMARY KEY, spid int CONSTRAINT fg2s REFERENCES sn(pid) ON UPDATE CASCADE, pid int CONSTRAINT fg2p REFERENCES p(id) ON DELETE CASCADE)', [1785, 1750])
  assert.deepEqual(refused.errors.map(e => [e.number, e.state, e.class]), [[1785, 0, 16], [1750, 1, 16]])
  assert.equal(refused.errors[0].message, "Introducing FOREIGN KEY constraint 'fg2p' on table 'g2' may cause cycles or multiple cascade paths. Specify ON DELETE NO ACTION or ON UPDATE NO ACTION, or modify other FOREIGN KEY constraints.")
  await fails(c, 'CREATE TABLE two(id int, x int CONSTRAINT fx REFERENCES p(id) ON DELETE SET NULL, y int CONSTRAINT fy REFERENCES p(id) ON DELETE SET NULL)', [1785, 1750])
  await fails(c, 'CREATE TABLE self(id int PRIMARY KEY, parent int CONSTRAINT fself REFERENCES self(id) ON DELETE SET NULL)', [1785, 1750])
  // The ON UPDATE CASCADE copies the NULL; ON UPDATE SET DEFAULT applies.
  await query(c, 'CREATE TABLE g(id int PRIMARY KEY, spid int CONSTRAINT fgs REFERENCES sn(pid) ON UPDATE CASCADE)')
  await query(c, 'CREATE TABLE gd(id int PRIMARY KEY, spid int DEFAULT 2 CONSTRAINT fgds REFERENCES sn(pid) ON UPDATE SET DEFAULT)')
  await query(c, 'INSERT p VALUES (1),(2); INSERT sn VALUES (10,1),(20,2); INSERT g VALUES (100,1),(200,2); INSERT gd VALUES (100,1),(200,2)')
  await query(c, 'DELETE p WHERE id = 1')
  assert.deepEqual(await rows(c, 'SELECT * FROM sn ORDER BY id'), [[10, null], [20, 2]])
  assert.deepEqual(await rows(c, 'SELECT * FROM g ORDER BY id'), [[100, null], [200, 2]])
  assert.deepEqual(await rows(c, 'SELECT * FROM gd ORDER BY id'), [[100, 2], [200, 2]])
})

test('SET NULL and SET DEFAULT actions, composite keys and ALTER TABLE actions', { timeout: 60000 }, async t => {
  const c = await open(t)
  await query(c, 'CREATE TABLE p2(id int PRIMARY KEY); INSERT p2 VALUES (1),(5)')
  await query(c, 'CREATE TABLE c2(id int PRIMARY KEY, p int DEFAULT 5 REFERENCES p2 ON DELETE SET DEFAULT ON UPDATE SET DEFAULT)')
  await query(c, 'CREATE TABLE c3(id int PRIMARY KEY, p int, CONSTRAINT fk_c3 FOREIGN KEY (p) REFERENCES p2(id) ON DELETE SET NULL ON UPDATE SET NULL)')
  await query(c, 'INSERT c2 VALUES (1,1); INSERT c3 VALUES (1,1),(2,5)')
  await query(c, 'DELETE p2 WHERE id = 1')
  assert.deepEqual(await rows(c, 'SELECT * FROM c2'), [[1, 5]])
  assert.deepEqual(await rows(c, 'SELECT * FROM c3 ORDER BY id'), [[1, null], [2, 5]])
  // The default is not a key any more: the action's row conflicts.
  const conflict = await fails(c, 'UPDATE p2 SET id = 6', [547])
  assert.match(conflict.errors[0].message, /^The UPDATE statement conflicted with the FOREIGN KEY constraint "FK__c2__p__[0-9A-F]{8}"\. The conflict occurred in database "constraints_db", table "dbo\.p2", column 'id'\.$/)
  assert.deepEqual(await rows(c, 'SELECT * FROM c3 ORDER BY id'), [[1, null], [2, 5]])
  await query(c, 'DELETE c2')
  await query(c, 'UPDATE p2 SET id = 6')
  assert.deepEqual(await rows(c, 'SELECT * FROM c3 ORDER BY id'), [[1, null], [2, null]])

  await query(c, 'CREATE TABLE m1(a int, b int, PRIMARY KEY(a,b)); CREATE TABLE m2(id int, x int, y int)')
  await query(c, 'ALTER TABLE m2 ADD CONSTRAINT fk_m FOREIGN KEY (x,y) REFERENCES m1(a,b) ON UPDATE CASCADE ON DELETE CASCADE')
  await query(c, 'INSERT m1 VALUES (1,1),(1,2); INSERT m2 VALUES (1,1,1),(2,1,NULL),(3,9,NULL)')
  const composite = await fails(c, 'INSERT m2 VALUES (4,9,9)', [547])
  assert.equal(composite.errors[0].message, 'The INSERT statement conflicted with the FOREIGN KEY constraint "fk_m". The conflict occurred in database "constraints_db", table "dbo.m1".')
  await query(c, 'UPDATE m1 SET b = b + 10')
  assert.deepEqual(await rows(c, 'SELECT * FROM m2 ORDER BY id'), [[1, 1, 11], [2, 1, null], [3, 9, null]])
  await query(c, 'DELETE m1 WHERE b = 11')
  assert.deepEqual(await rows(c, 'SELECT * FROM m2 ORDER BY id'), [[2, 1, null], [3, 9, null]])
  assert.deepEqual(await rows(c, "SELECT delete_referential_action_desc, update_referential_action_desc FROM sys.foreign_keys WHERE name = 'fk_m'"), [['CASCADE', 'CASCADE']])
  assert.deepEqual(await rows(c, "SELECT k.constraint_column_id, COL_NAME(k.parent_object_id, k.parent_column_id), COL_NAME(k.referenced_object_id, k.referenced_column_id) FROM sys.foreign_key_columns k JOIN sys.foreign_keys f ON f.object_id = k.constraint_object_id WHERE f.name = 'fk_m' ORDER BY 1"), [[1, 'x', 'a'], [2, 'y', 'b']])
  // Disabled keys take no action.
  await query(c, 'INSERT m1 VALUES (2,2); INSERT m2 VALUES (5,2,2)')
  await query(c, 'ALTER TABLE m2 NOCHECK CONSTRAINT fk_m')
  await query(c, 'DELETE m1 WHERE a = 2')
  assert.deepEqual(await rows(c, 'SELECT id FROM m2 WHERE x = 2'), [[5]])
})

test('column-level FOREIGN KEY REFERENCES matches the REFERENCES form', { timeout: 60000 }, async t => {
  const c = await open(t)
  await query(c, 'CREATE TABLE p(id int PRIMARY KEY)')
  await query(c, 'CREATE TABLE c(id int PRIMARY KEY, pid int FOREIGN KEY REFERENCES p(id) ON UPDATE CASCADE ON DELETE CASCADE)')
  await query(c, 'CREATE TABLE d(id int PRIMARY KEY, pid int CONSTRAINT fk_d FOREIGN KEY REFERENCES p(id) ON DELETE SET NULL NOT FOR REPLICATION)')
  await query(c, 'CREATE TABLE a(id int PRIMARY KEY); INSERT a VALUES (1)')
  await query(c, 'ALTER TABLE a ADD pid int CONSTRAINT fk_a FOREIGN KEY REFERENCES p(id) ON DELETE CASCADE, qid int FOREIGN KEY REFERENCES p')
  const keys = "SELECT CASE WHEN f.is_system_named = 1 THEN OBJECT_NAME(f.parent_object_id) ELSE f.name END, f.is_system_named, f.delete_referential_action_desc, f.update_referential_action_desc, COL_NAME(k.parent_object_id, k.parent_column_id), OBJECT_NAME(k.referenced_object_id), COL_NAME(k.referenced_object_id, k.referenced_column_id) FROM sys.foreign_keys f JOIN sys.foreign_key_columns k ON k.constraint_object_id = f.object_id ORDER BY 1, 5"
  assert.deepEqual(await rows(c, keys), [
    ['a', true, 'NO_ACTION', 'NO_ACTION', 'qid', 'p', 'id'],
    ['c', true, 'CASCADE', 'CASCADE', 'pid', 'p', 'id'],
    ['fk_a', false, 'CASCADE', 'NO_ACTION', 'pid', 'p', 'id'],
    ['fk_d', false, 'SET_NULL', 'NO_ACTION', 'pid', 'p', 'id']
  ])
  await query(c, 'INSERT p VALUES (1),(2); INSERT c VALUES (10,1),(20,2); INSERT d VALUES (10,2),(20,NULL)')
  await fails(c, 'INSERT d VALUES (30,7)', [547], /INSERT statement conflicted with the FOREIGN KEY constraint "fk_d"/)
  await fails(c, 'INSERT c VALUES (30,7)', [547], /INSERT statement conflicted with the FOREIGN KEY constraint "FK__c__pid__/)
  await query(c, 'UPDATE p SET id = 101 WHERE id = 1')
  assert.deepEqual(await rows(c, 'SELECT * FROM c ORDER BY id'), [[10, 101], [20, 2]])
  await fails(c, 'UPDATE p SET id = 102 WHERE id = 2', [547], /UPDATE statement conflicted with the REFERENCE constraint "fk_d"/)
  await query(c, 'DELETE p WHERE id = 2')
  assert.deepEqual(await rows(c, 'SELECT * FROM c ORDER BY id'), [[10, 101]])
  assert.deepEqual(await rows(c, 'SELECT * FROM d ORDER BY id'), [[10, null], [20, null]])
  await query(c, 'UPDATE a SET qid = 101')
  await fails(c, 'UPDATE a SET pid = 9', [547], /UPDATE statement conflicted with the FOREIGN KEY constraint "fk_a"/)
  await fails(c, 'DELETE p WHERE id = 101', [547], /DELETE statement conflicted with the REFERENCE constraint "FK__a__qid__/)
  await query(c, 'UPDATE a SET pid = 101, qid = NULL; DELETE p WHERE id = 101')
  assert.deepEqual(await rows(c, 'SELECT count(*) FROM a'), [[0]])
  assert.deepEqual(await rows(c, 'SELECT count(*) FROM c'), [[0]])
})

test('constraint failures inside transactions', { timeout: 60000 }, async t => {
  const c = await open(t)
  await query(c, 'CREATE TABLE p(id int PRIMARY KEY); INSERT p VALUES (1)')
  await query(c, 'CREATE TABLE ch(id int, pid int REFERENCES p(id), v int CHECK (v > 0))')
  // A failed INSERT is undone; the transaction stays usable and commits.
  await batch(c, 'BEGIN TRANSACTION')
  await batch(c, 'INSERT ch VALUES (1, 1, 1)')
  await fails(c, 'INSERT ch VALUES (2, 9, 1), (3, 1, 1)', [547])
  await fails(c, 'INSERT ch VALUES (4, 1, -1)', [547])
  assert.deepEqual(await batch(c, 'SELECT @@TRANCOUNT, XACT_STATE()'), [[[1, 1]]])
  await batch(c, 'INSERT ch VALUES (5, 1, 5)')
  await batch(c, 'COMMIT')
  assert.deepEqual(await rows(c, 'SELECT id FROM ch ORDER BY id'), [[1], [5]])
  // In autocommit mode nothing of a failed statement remains.
  await fails(c, 'UPDATE ch SET v = v - 2', [547])
  assert.deepEqual(await rows(c, 'SELECT id, v FROM ch ORDER BY id'), [[1, 1], [5, 5]])
  assert.deepEqual(await rows(c, 'SELECT @@TRANCOUNT'), [[0]])
  // ROLLBACK undoes constraint definitions.
  await query(c, 'BEGIN TRANSACTION; ALTER TABLE ch ADD CONSTRAINT ck_id CHECK (id < 100); ROLLBACK')
  await query(c, 'INSERT ch VALUES (200, 1, 1)')
  assert.deepEqual(await rows(c, "SELECT count(*) FROM sys.check_constraints WHERE name = 'ck_id'"), [[0]])
})

// Replay of reference/gaps-constraints.json (scripts/capture-gaps-constraints.mjs):
// every step's diagnostics, informational messages and rows must match.
const known = new Map([
  // SQL Server ends a failed ADD UNIQUE with informational 3621 after 1750;
  // msduck reports the two errors without it.
  ['ALTER TABLE items ADD CONSTRAINT uq_items UNIQUE(id)', 'missing 3621'],
  // Duplicate keys report 2627 with DuckDB's text and class 16, without
  // 3621 (key diagnostics belong to gaps-keys-v1).
  ['INSERT k VALUES (1,1)', 'number']
])

test('reference replay: ALTER TABLE constraints and referential actions', { timeout: 300000 }, async t => {
  if (reference) return
  const { readFile } = await import('node:fs/promises')
  const fixture = JSON.parse(await readFile(new URL('../../reference/gaps-constraints.json', import.meta.url), 'utf8'))
  const c = await start(t, { options: { requestTimeout: 60000 } })
  for (const group of fixture.groups) {
    const reset = await capture(c, "USE master; IF DB_ID('gaps_constraints') IS NOT NULL DROP DATABASE gaps_constraints; CREATE DATABASE gaps_constraints")
    assert.deepEqual(reset.errors, [])
    assert.deepEqual((await capture(c, 'USE gaps_constraints')).errors, [])
    for (const step of group.steps) {
      const actual = await capture(c, step.sql)
      const label = `${group.name}: ${step.sql}`
      const shape = known.get(step.sql) === 'number'
        ? e => ({ number: e.number, state: e.state })
        : e => ({ number: e.number, state: e.state, class: e.class, message: e.message })
      assert.deepEqual(actual.errors.map(shape), step.result.errors.map(shape), label)
      const info = actual.info.filter(i => i.number !== 5701 && i.number !== 5703).map(i => ({ number: i.number, message: i.message }))
      if (known.get(step.sql) === 'number') {
        // Diagnostics compared by number and state only.
      } else if (known.get(step.sql) === 'missing 3621' && step.result.errors.length) {
        assert.deepEqual(info, step.result.info.filter(i => i.number !== 3621), label)
      } else {
        assert.deepEqual(info, step.result.info, label)
      }
      assert.deepEqual(actual.sets.map(set => JSON.parse(JSON.stringify(set.rows))), step.result.sets, label)
    }
  }
})
