#!/usr/bin/env node
// Capture SQL Server's behavior for the ALTER TABLE constraint lifecycle and
// foreign-key referential actions (docs/gaps-constraints.md) into
// reference/gaps-constraints.json. tests/compat/constraints.test.mjs replays
// every step against msduck and compares diagnostics and rows.
//
// Uses MSSQL_REFERENCE_HOST/USER/PASSWORD[/PORT] when set (an already running,
// owner-labeled reference), otherwise starts the pinned reference container
// (MSSQL_REFERENCE_IMAGE overrides the image). Pass --write to replace the
// retained fixture; without it the capture is compared with the fixture.
import assert from 'node:assert/strict'
import { readFile, writeFile } from 'node:fs/promises'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, referenceConfig } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/gaps-constraints.json', import.meta.url)
export const database = 'gaps_constraints'

// Each group runs in a fresh database; each step is one SQL batch.
export const groups = [
  {
    name: 'alter lifecycle',
    steps: [
      "CREATE TABLE parent(id int NOT NULL PRIMARY KEY, name varchar(10)); INSERT parent VALUES (1,'a'),(2,'b')",
      'CREATE TABLE items(id int NOT NULL, value int, parent_id int); INSERT items VALUES (1,10,1),(2,-5,2),(3,30,NULL)',
      'ALTER TABLE items ADD added int NOT NULL CONSTRAINT df_added DEFAULT 7 WITH VALUES',
      'SELECT id, added FROM items ORDER BY id',
      'ALTER TABLE items ADD CONSTRAINT ck_items CHECK(value>0)',
      'ALTER TABLE items WITH CHECK ADD CONSTRAINT ck_items CHECK(value>0)',
      'ALTER TABLE items WITH NOCHECK ADD CONSTRAINT ck_items CHECK(value>0)',
      'SELECT name, is_disabled, is_not_trusted, parent_column_id, is_system_named FROM sys.check_constraints',
      'INSERT items(id, value, parent_id) VALUES (4,-1,1)',
      'UPDATE items SET value=-2 WHERE id=1',
      'UPDATE items SET parent_id=1 WHERE id=2',
      'UPDATE items SET value=-7 WHERE id=2',
      'INSERT items(id, value, parent_id) VALUES (4,4,1),(5,-5,1)',
      'ALTER TABLE items ADD CONSTRAINT df_items DEFAULT(0) FOR value',
      'ALTER TABLE items ADD CONSTRAINT df_again DEFAULT(1) FOR value',
      'INSERT items(id, parent_id) VALUES (5,1); SELECT id, value, parent_id, added FROM items ORDER BY id',
      'INSERT items(id, value, parent_id) VALUES (1,11,1)',
      'ALTER TABLE items ADD CONSTRAINT uq_items UNIQUE(id)',
      'DELETE items WHERE value = 11',
      'ALTER TABLE items ADD CONSTRAINT uq_items UNIQUE(id)',
      'ALTER TABLE items ADD CONSTRAINT pk_items PRIMARY KEY(id)',
      'ALTER TABLE items ADD CONSTRAINT pk_other PRIMARY KEY(value)',
      'ALTER TABLE items ADD CONSTRAINT fk_items FOREIGN KEY(parent_id) REFERENCES parent(id)',
      'INSERT items(id, value, parent_id) VALUES (6,6,9)',
      'DELETE parent WHERE id=1',
      'UPDATE parent SET id=5 WHERE id=1',
      'ALTER TABLE items NOCHECK CONSTRAINT ALL',
      'INSERT items(id, value, parent_id) VALUES (6,-6,9)',
      'ALTER TABLE items WITH CHECK CHECK CONSTRAINT fk_items',
      'ALTER TABLE items CHECK CONSTRAINT ALL',
      'SELECT name, is_disabled, is_not_trusted FROM sys.check_constraints UNION ALL SELECT name, is_disabled, is_not_trusted FROM sys.foreign_keys ORDER BY name',
      'DELETE items WHERE id IN (2,6)',
      'ALTER TABLE items WITH CHECK CHECK CONSTRAINT ALL',
      'SELECT name, is_disabled, is_not_trusted FROM sys.check_constraints UNION ALL SELECT name, is_disabled, is_not_trusted FROM sys.foreign_keys ORDER BY name',
      'ALTER TABLE items NOCHECK CONSTRAINT uq_items',
      'ALTER TABLE items NOCHECK CONSTRAINT nope',
      'DROP TABLE parent',
      'TRUNCATE TABLE parent',
      'ALTER TABLE items DROP COLUMN parent_id',
      'ALTER TABLE parent DROP CONSTRAINT fk_items',
      'ALTER TABLE items DROP CONSTRAINT df_items',
      'ALTER TABLE items DROP CONSTRAINT ck_items',
      'ALTER TABLE items DROP CONSTRAINT IF EXISTS ck_items',
      'ALTER TABLE items DROP CONSTRAINT ck_items',
      'ALTER TABLE items DROP CONSTRAINT fk_items, nope',
      'ALTER TABLE items DROP CONSTRAINT fk_items, uq_items',
      'ALTER TABLE items DROP CONSTRAINT pk_items',
      "SELECT name, type FROM sys.objects WHERE parent_object_id = OBJECT_ID('items') ORDER BY name",
      'INSERT items(id, value, parent_id) VALUES (1,-1,99); SELECT id, value, parent_id, added FROM items ORDER BY id, value'
    ]
  },
  {
    name: 'definition errors',
    steps: [
      'CREATE TABLE p(id int PRIMARY KEY, code varchar(10) NOT NULL UNIQUE, other int); CREATE TABLE np(id int)',
      'CREATE TABLE c1(id int, pid int CONSTRAINT f1 REFERENCES np)',
      'CREATE TABLE c2(id int, pid int, CONSTRAINT f2 FOREIGN KEY (pid) REFERENCES p(other))',
      'CREATE TABLE c3(id int, pid int CONSTRAINT f3 REFERENCES nope(id))',
      'CREATE TABLE c4(id bigint CONSTRAINT f4 REFERENCES p(id))',
      'CREATE TABLE c5(code varchar(20) CONSTRAINT f5 REFERENCES p(code))',
      'CREATE TABLE c6(id int NOT NULL CONSTRAINT f6 REFERENCES p(id) ON DELETE SET NULL)',
      'CREATE TABLE c7(id int NOT NULL CONSTRAINT f7 REFERENCES p(id) ON DELETE SET DEFAULT)',
      'CREATE TABLE c8(id int, x int, CONSTRAINT f8 FOREIGN KEY (id, x) REFERENCES p(id))',
      'CREATE TABLE c9(id int, CONSTRAINT dup CHECK (id > 0), CONSTRAINT dup CHECK (id > 1))',
      'CREATE TABLE c10(id int CONSTRAINT c10 CHECK (id > 0))',
      'CREATE TABLE c11(id int CHECK (id > (SELECT 1)))',
      'CREATE TABLE c12(id int CHECK (missing > 1))',
      'CREATE TABLE c13(id int, CONSTRAINT f13 FOREIGN KEY (missing) REFERENCES p(id))',
      'CREATE TABLE c14(id int, CONSTRAINT f14 FOREIGN KEY (id) REFERENCES p(missing))',
      "SELECT count(*) FROM sys.objects WHERE name LIKE 'c[0-9]%' OR name LIKE 'f[0-9]%'",
      'CREATE TABLE ch(id int, pid int)',
      'ALTER TABLE ch ADD CONSTRAINT df_missing DEFAULT 1 FOR missing',
      'ALTER TABLE ch ADD CONSTRAINT df_column DEFAULT (id + 1) FOR pid',
      'ALTER TABLE ch ADD CONSTRAINT ck_sub CHECK (pid > (SELECT 1))',
      'ALTER TABLE ch ADD CONSTRAINT ck_col CHECK (missing > 1)',
      'ALTER TABLE ch ADD CONSTRAINT p CHECK (id > 0)',
      'BEGIN TRY ALTER TABLE ch ADD CONSTRAINT p CHECK (id > 0) END TRY BEGIN CATCH SELECT ERROR_NUMBER(), ERROR_MESSAGE() END CATCH',
      'ALTER TABLE ch ADD CONSTRAINT ck_a CHECK (id > 0), CONSTRAINT ck_b CHECK (id > 1)',
      'ALTER TABLE ch NOCHECK CONSTRAINT ck_a, nope',
      'ALTER TABLE ch NOCHECK CONSTRAINT ck_a, ck_b',
      'ALTER TABLE ch DROP CONSTRAINT ck_a, ck_zzz',
      'SELECT name, is_disabled, is_not_trusted FROM sys.check_constraints ORDER BY name',
      'CREATE TABLE ck_a(id int)',
      'CREATE VIEW ck_b AS SELECT 1 AS one',
      "SELECT name, type FROM sys.objects WHERE name IN ('ck_a', 'ck_b') ORDER BY name",
      'CREATE TABLE k(a int NOT NULL, b int NOT NULL); INSERT k VALUES (1,1),(1,2)',
      'ALTER TABLE k ADD PRIMARY KEY NONCLUSTERED (a, b DESC)',
      "SELECT type FROM sys.key_constraints WHERE parent_object_id = OBJECT_ID('k')",
      'INSERT k VALUES (1,1)',
      'CREATE TABLE nk(id int NULL)',
      'ALTER TABLE nk ADD CONSTRAINT pk_nk PRIMARY KEY (id)',
      'ALTER TABLE ch ADD CONSTRAINT fk_ch FOREIGN KEY (pid) REFERENCES p(id)',
      'ALTER TABLE p DROP CONSTRAINT IF EXISTS fk_ch',
      'ALTER TABLE ch DROP fk_ch',
      'CREATE TABLE tree(id int PRIMARY KEY, parent int, CONSTRAINT fk_tree FOREIGN KEY (parent) REFERENCES tree(id))',
      'INSERT tree VALUES (1, 2)',
      'INSERT tree VALUES (1, NULL), (2, 1), (3, 2)',
      'UPDATE tree SET parent = 5 WHERE id = 2',
      'UPDATE tree SET id = 7 WHERE id = 1',
      'DELETE tree WHERE id = 1',
      'DELETE tree; SELECT count(*) FROM tree'
    ]
  },
  {
    name: 'referential actions',
    steps: [
      'CREATE TABLE a(id int PRIMARY KEY, k int NOT NULL CONSTRAINT uq_a UNIQUE)',
      'CREATE TABLE b(id int PRIMARY KEY, a_id int CONSTRAINT fk_ba REFERENCES a(id) ON DELETE CASCADE ON UPDATE CASCADE, a_k int DEFAULT 2, CONSTRAINT fk_bk FOREIGN KEY (a_k) REFERENCES a(k))',
      'CREATE TABLE c(id int PRIMARY KEY, b_id int)',
      'ALTER TABLE c ADD CONSTRAINT fk_cb FOREIGN KEY (b_id) REFERENCES b(id) ON DELETE SET NULL ON UPDATE CASCADE',
      'CREATE TABLE d(id int PRIMARY KEY, c_id int, CONSTRAINT fk_dc FOREIGN KEY (c_id) REFERENCES c(id) ON DELETE CASCADE)',
      'INSERT a VALUES (1,1),(2,2),(3,3); INSERT b VALUES (10,1,1),(11,1,3),(12,2,2),(13,3,3); INSERT c VALUES (100,10),(101,11),(102,12); INSERT d VALUES (1000,100),(1001,102)',
      'DELETE a WHERE id = 1',
      'SELECT * FROM b ORDER BY id; SELECT * FROM c ORDER BY id; SELECT * FROM d ORDER BY id',
      'UPDATE a SET id = id + 10',
      'SELECT * FROM a ORDER BY id; SELECT * FROM b ORDER BY id',
      'UPDATE b SET id = id * 2',
      'SELECT * FROM b ORDER BY id; SELECT * FROM c ORDER BY id',
      'DELETE a WHERE k = 2',
      'SELECT * FROM a ORDER BY id; SELECT * FROM b ORDER BY id; SELECT * FROM c ORDER BY id; SELECT * FROM d ORDER BY id',
      'DELETE c WHERE id = 102; SELECT * FROM d ORDER BY id',
      'UPDATE a SET k = 30',
      'DELETE a; SELECT count(*) FROM b',
      'SELECT name, delete_referential_action_desc, update_referential_action_desc, is_disabled, is_not_trusted FROM sys.foreign_keys ORDER BY name',
      'CREATE TABLE b2(id int, x int CONSTRAINT fk_b2x REFERENCES a(id) ON DELETE CASCADE, y int, CONSTRAINT fk_b2y FOREIGN KEY (y) REFERENCES a(id) ON DELETE SET NULL)',
      'CREATE TABLE e(id int PRIMARY KEY, parent int, CONSTRAINT fk_e FOREIGN KEY (parent) REFERENCES e(id) ON DELETE CASCADE)',
      'CREATE TABLE e(id int PRIMARY KEY, parent int)',
      'ALTER TABLE e ADD CONSTRAINT fk_e FOREIGN KEY (parent) REFERENCES e(id) ON UPDATE CASCADE',
      'CREATE TABLE p2(id int PRIMARY KEY); INSERT p2 VALUES (1),(5)',
      'CREATE TABLE c2(id int PRIMARY KEY, p int DEFAULT 5 CONSTRAINT fk_c2 REFERENCES p2 ON DELETE SET DEFAULT ON UPDATE SET DEFAULT)',
      'CREATE TABLE c3(id int PRIMARY KEY, p int, CONSTRAINT fk_c3 FOREIGN KEY (p) REFERENCES p2(id) ON DELETE SET NULL ON UPDATE SET NULL)',
      'INSERT c2 VALUES (1,1); INSERT c3 VALUES (1,1),(2,5)',
      'DELETE p2 WHERE id = 1',
      'SELECT * FROM c2; SELECT * FROM c3 ORDER BY id',
      'UPDATE p2 SET id = 6',
      'DELETE p2',
      'DELETE c2; UPDATE p2 SET id = 6; SELECT * FROM c3 ORDER BY id',
      'CREATE TABLE m1(a int, b int, PRIMARY KEY(a,b)); CREATE TABLE m2(id int, x int, y int)',
      'ALTER TABLE m2 ADD CONSTRAINT fk_m FOREIGN KEY (x,y) REFERENCES m1(a,b) ON UPDATE CASCADE ON DELETE CASCADE',
      'INSERT m1 VALUES (1,1),(1,2); INSERT m2 VALUES (1,1,1),(2,1,NULL),(3,9,NULL)',
      'INSERT m2 VALUES (4,9,9)',
      'UPDATE m1 SET b = b + 10; SELECT * FROM m2 ORDER BY id',
      'DELETE m1 WHERE b = 11; SELECT * FROM m2 ORDER BY id',
      'INSERT m1 VALUES (2,2); INSERT m2 VALUES (5,2,2); ALTER TABLE m2 NOCHECK CONSTRAINT fk_m; DELETE m1 WHERE a = 2; SELECT id FROM m2 WHERE x = 2',
      'ALTER TABLE m2 WITH CHECK CHECK CONSTRAINT fk_m'
    ]
  },
  {
    name: 'update shapes and computed columns',
    steps: [
      'CREATE TABLE customers(id int PRIMARY KEY)',
      'CREATE TABLE invoices(id int PRIMARY KEY, customer_id int CONSTRAINT fk_invoices REFERENCES customers(id) ON UPDATE CASCADE)',
      'CREATE TABLE orders(id int PRIMARY KEY, customer_id int)',
      'INSERT customers VALUES (1),(2); INSERT invoices VALUES (10,1); INSERT orders VALUES (1,1)',
      'UPDATE orders SET id = id + 1 WHERE customer_id IN (SELECT id FROM customers)',
      'SELECT * FROM invoices; SELECT * FROM orders',
      'UPDATE customers SET id = customers.id + 10 FROM customers JOIN orders ON orders.customer_id = customers.id',
      'SELECT * FROM customers ORDER BY id; SELECT * FROM invoices',
      'UPDATE c SET id = c.id + 100 FROM customers c WHERE c.id = 11',
      'SELECT * FROM customers ORDER BY id; SELECT * FROM invoices',
      'CREATE TABLE p(id int PRIMARY KEY)',
      'CREATE TABLE n(id int PRIMARY KEY, pid int CONSTRAINT fk_n REFERENCES p(id) ON UPDATE SET NULL)',
      'CREATE TABLE d(id int PRIMARY KEY, pid int DEFAULT 100 CONSTRAINT fk_d REFERENCES p(id) ON UPDATE SET DEFAULT)',
      'INSERT p VALUES (1),(2),(100); INSERT n VALUES (10,1),(20,2),(30,100); INSERT d VALUES (10,1),(20,2)',
      'UPDATE p SET id = id + 1 WHERE id < 100',
      'SELECT * FROM p ORDER BY id; SELECT * FROM n ORDER BY id; SELECT * FROM d ORDER BY id',
      'UPDATE n SET pid = 2; UPDATE d SET pid = 3',
      'UPDATE p SET id = CASE id WHEN 2 THEN 3 ELSE 2 END WHERE id < 100',
      'SELECT * FROM p ORDER BY id; SELECT * FROM n ORDER BY id; SELECT * FROM d ORDER BY id',
      'CREATE TABLE r(RowId int, v int CONSTRAINT ck_r CHECK (v > 0)); INSERT r VALUES (5, 1)',
      'INSERT r VALUES (1, -1)',
      'INSERT r VALUES (7, 2); SELECT RowId, v FROM r ORDER BY RowId',
      'CREATE TABLE t(a int, b int, s AS a + b, CONSTRAINT ck_t CHECK (s > 0))',
      'CREATE TABLE t(a int, b int, s AS a + b PERSISTED, CONSTRAINT ck_t CHECK (s > 0))',
      'INSERT t(a, b) VALUES (1, 1)',
      'UPDATE t SET a = -100',
      'SELECT a, b, s FROM t',
      'CREATE TABLE fc(id int, x AS id, CONSTRAINT fk_fc FOREIGN KEY (x) REFERENCES p(id))',
      'CREATE TABLE w(s date, e date, CONSTRAINT ck_w CHECK (DATEDIFF(day, s, e) <= 30))',
      "INSERT w VALUES ('2020-01-01', '2020-01-15')",
      "INSERT w VALUES ('2020-01-01', '2020-03-01')",
      'CREATE TABLE [x]]y] (id int CONSTRAINT [ck]]z] CHECK (id > 0))',
      'INSERT [x]]y] VALUES (0)',
      'ALTER TABLE [x]]y] DROP CONSTRAINT [ck]]z]',
      'INSERT [x]]y] VALUES (0); SELECT count(*) FROM [x]]y]'
    ]
  },
  {
    name: 'transactions',
    steps: [
      'CREATE TABLE p(id int PRIMARY KEY); INSERT p VALUES (1)',
      'CREATE TABLE ch(id int, pid int CONSTRAINT fk_ch REFERENCES p(id), v int CONSTRAINT ck_v CHECK (v > 0))',
      'BEGIN TRANSACTION; INSERT ch VALUES (1, 1, 1)',
      'INSERT ch VALUES (2, 9, 1), (3, 1, 1)',
      'INSERT ch VALUES (4, 1, -1)',
      'SELECT @@TRANCOUNT, XACT_STATE()',
      'INSERT ch VALUES (5, 1, 5); COMMIT',
      'SELECT id FROM ch ORDER BY id',
      'UPDATE ch SET v = v - 2',
      'SELECT id, v FROM ch ORDER BY id; SELECT @@TRANCOUNT',
      'BEGIN TRY INSERT ch VALUES (9, 1, 0) END TRY BEGIN CATCH SELECT ERROR_NUMBER(), ERROR_MESSAGE() END CATCH',
      'BEGIN TRANSACTION; ALTER TABLE ch ADD CONSTRAINT ck_id CHECK (id < 100); ROLLBACK',
      "INSERT ch VALUES (200, 1, 1); SELECT count(*) FROM sys.check_constraints WHERE name = 'ck_id'"
    ]
  }
]

/** Environment messages that differ between servers for reasons unrelated to constraints. */
const ignoredInfo = new Set([5701, 5703])

export function observe(result) {
  return {
    errors: result.errors.map(error => ({ number: error.number, state: error.state, class: error.class, message: error.message })),
    info: result.info.filter(info => !ignoredInfo.has(info.number)).map(info => ({ number: info.number, message: info.message })),
    sets: result.sets.map(set => canonical(set.rows))
  }
}

export async function run(connection, reset) {
  const groupsResult = []
  for (const group of groups) {
    await reset(connection)
    const steps = []
    for (const sql of group.steps) steps.push({ sql, result: observe(await capture(connection, sql)) })
    groupsResult.push({ name: group.name, steps })
  }
  return groupsResult
}

async function main() {
  const write = process.argv.includes('--write')
  const work = async config => {
    const connection = await connect({ ...config, options: { ...config.options, requestTimeout: 120000 } })
    try {
      const version = (await capture(connection, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128))")).sets[0].rows[0][0]
      const groupsResult = await run(connection, async c => {
        const reset = await capture(c, `USE master; IF DB_ID('${database}') IS NOT NULL BEGIN ALTER DATABASE ${database} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE ${database} END; CREATE DATABASE ${database}`)
        assert.deepEqual(reset.errors, [])
        const use = await capture(c, `USE ${database}`)
        assert.deepEqual(use.errors, [])
      })
      await capture(connection, `USE master; DROP DATABASE ${database}`)
      return { version, groups: groupsResult }
    } finally { connection.close() }
  }
  const captured = process.env.MSSQL_REFERENCE_HOST
    ? { image: process.env.MSSQL_REFERENCE_IMAGE ?? 'external reference', ...await work(referenceConfig()) }
    : await withReferenceContainer(async (config, container) => ({ image: container.image, ...await work(config) }))
  if (write) {
    await writeFile(fixture, JSON.stringify(captured, null, 1) + '\n')
    console.log(`wrote ${fixture.pathname} (${captured.version})`)
    return
  }
  const retained = JSON.parse(await readFile(fixture, 'utf8'))
  assert.deepEqual(captured.groups, retained.groups, 'reference behavior differs from the retained fixture')
  console.log(`capture matches ${fixture.pathname}`)
}

if (import.meta.url === `file://${process.argv[1]}`) await main()
