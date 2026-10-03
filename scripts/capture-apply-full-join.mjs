#!/usr/bin/env node
// SQL Server evidence for correlated APPLY bodies that FULL OUTER JOIN two
// row sources (issue #867): an inline function over two OPENJSON calls, the
// same shape as a derived table, and the AFTER UPDATE trigger that diffs JSON
// snapshots of inserted and deleted through that function.
//
// Every case runs in a fresh scratch database: its setup batches (CREATE
// FUNCTION and CREATE TRIGGER must be alone in a batch), then one query
// batch. tests/compat/apply_full_join.test.mjs replays the retained SQL.
//
//   node scripts/capture-apply-full-join.mjs [--write-fixture]
//
// Without --write-fixture the capture is printed. The image is the pinned
// reference image unless MSSQL_REFERENCE_IMAGE is set.
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { capture, canonical } from './lib/compatibility.mjs'
import { referenceImage, withReferenceContainer } from './lib/reference-container.mjs'
import { connect, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/apply-full-join.json', import.meta.url)
const writeFixture = process.argv.includes('--write-fixture')
const image = process.env.MSSQL_REFERENCE_IMAGE ?? referenceImage

const items = 'CREATE TABLE items(id INT NOT NULL PRIMARY KEY, lhs NVARCHAR(MAX) NULL, rhs NVARCHAR(MAX) NULL);'
const foo = 'CREATE FUNCTION dbo.foo(@lhs NVARCHAR(MAX), @rhs NVARCHAR(MAX)) RETURNS TABLE AS RETURN (SELECT COALESCE(l.[key], r.[key]) AS [key], l.[value] AS old_value, r.[value] AS new_value FROM OPENJSON(@lhs) l FULL OUTER JOIN OPENJSON(@rhs) r ON l.[key] = r.[key]);'
const single = [items + ` INSERT INTO items VALUES (1, N'{"a":1}', N'{"a":2}');`, foo]
// Keys on both sides, only one side, NULL documents, empty objects, equal values.
const multirow = [items + ` INSERT INTO items VALUES
 (1, N'{"a":1,"b":2}', N'{"b":3,"c":4}'),
 (2, NULL, N'{"x":1}'),
 (3, N'{"y":true}', NULL),
 (4, NULL, NULL),
 (5, N'{}', N'{}'),
 (6, N'{"a":"same"}', N'{"a":"same"}'),
 (7, N'[1,1,2,null]', N'[1,3,null]');`, foo]
const derived = `(SELECT COALESCE(l.[key], r.[key]) AS [key], l.[value] AS old_value, r.[value] AS new_value, l.[type] AS old_type, r.[type] AS new_type
  FROM OPENJSON(i.lhs) l FULL OUTER JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key])`
const docs = `CREATE TABLE docs(id INT NOT NULL PRIMARY KEY, name NVARCHAR(20) NULL, qty INT NULL, note NVARCHAR(20) NULL);
CREATE TABLE audit(id INT NOT NULL, [key] NVARCHAR(4000) NULL, old_value NVARCHAR(MAX) NULL, new_value NVARCHAR(MAX) NULL);
INSERT INTO docs VALUES (1, N'one', 10, NULL), (2, N'two', 20, N'x'), (3, N'three', 30, N'y');`
const snapshotTrigger = `CREATE TRIGGER docs_audit ON docs AFTER UPDATE AS
BEGIN
  SET NOCOUNT ON;
  INSERT INTO audit(id, [key], old_value, new_value)
  SELECT i.id, x.[key], x.old_value, x.new_value
  FROM inserted i
  JOIN deleted d ON d.id = i.id
  CROSS APPLY dbo.foo(
    (SELECT d.name, d.qty, d.note FOR JSON PATH, WITHOUT_ARRAY_WRAPPER),
    (SELECT i.name, i.qty, i.note FOR JSON PATH, WITHOUT_ARRAY_WRAPPER)) x
  WHERE ISNULL(x.old_value, N'') <> ISNULL(x.new_value, N'');
END`
// The same trigger without ISNULL over the OPENJSON values.
const plainTrigger = snapshotTrigger.replace("WHERE ISNULL(x.old_value, N'') <> ISNULL(x.new_value, N'')", 'WHERE x.old_value IS NULL OR x.new_value IS NULL OR x.old_value <> x.new_value')
const auditRead = ' SELECT id, [key], old_value, new_value FROM audit ORDER BY id, [key];'

const cases = [
  ['report repro cross apply', single, 'SELECT i.id, x.[key], x.old_value, x.new_value FROM items i CROSS APPLY dbo.foo(i.lhs, i.rhs) x;'],
  ['report repro outer apply', single, 'SELECT i.id, x.[key], x.old_value, x.new_value FROM items i OUTER APPLY dbo.foo(i.lhs, i.rhs) x;'],
  ['function star metadata', single, 'SELECT x.* FROM items i CROSS APPLY dbo.foo(i.lhs, i.rhs) x;'],
  ['multirow cross apply', multirow, 'SELECT i.id, x.[key], x.old_value, x.new_value FROM items i CROSS APPLY dbo.foo(i.lhs, i.rhs) x ORDER BY i.id, x.[key], x.old_value, x.new_value;'],
  ['multirow outer apply', multirow, 'SELECT i.id, x.[key], x.old_value, x.new_value FROM items i OUTER APPLY dbo.foo(i.lhs, i.rhs) x ORDER BY i.id, x.[key], x.old_value, x.new_value;'],
  ['derived cross apply', multirow, `SELECT i.id, x.[key], x.old_value, x.new_value, x.old_type, x.new_type FROM items i CROSS APPLY ${derived} x ORDER BY i.id, x.[key], x.old_value, x.new_value;`],
  ['derived outer apply', multirow, `SELECT i.id, x.[key], x.old_value, x.new_value, x.old_type, x.new_type FROM items i OUTER APPLY ${derived} x ORDER BY i.id, x.[key], x.old_value, x.new_value;`],
  // NULL values never match, and duplicate values multiply.
  ['value join keeps nulls and duplicates', multirow, 'SELECT i.id, x.lk, x.rk, x.v FROM items i CROSS APPLY (SELECT l.[key] AS lk, r.[key] AS rk, COALESCE(l.[value], r.[value]) AS v FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[value] = r.[value]) x WHERE i.id = 7 ORDER BY x.lk, x.rk;'],
  ['aggregate over full join', multirow, 'SELECT i.id, x.n, x.nl, x.nr FROM items i CROSS APPLY (SELECT COUNT(*) AS n, COUNT(l.[key]) AS nl, COUNT(r.[key]) AS nr FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key]) x ORDER BY i.id;'],
  ['where keeps right-only keys', multirow, 'SELECT i.id, x.[key] FROM items i CROSS APPLY (SELECT r.[key] FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key] WHERE l.[key] IS NULL) x ORDER BY i.id, x.[key];'],
  ['star of both sides', single, 'SELECT i.id, x.* FROM items i CROSS APPLY (SELECT * FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key]) x;'],
  ['qualified star of one side', multirow, 'SELECT i.id, x.* FROM items i CROSS APPLY (SELECT r.* FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key]) x WHERE i.id = 1 ORDER BY x.[key];'],
  ['correlation only in on clause', single, "SELECT i.id, x.a, x.b FROM items i CROSS APPLY (SELECT l.[value] AS a, r.[value] AS b FROM OPENJSON(N'[1,2]') l FULL JOIN OPENJSON(N'[2,3]') r ON l.[value] = r.[value] AND i.id = 1) x ORDER BY x.a, x.b;"],
  ['uncorrelated full join in apply', single, 'SELECT i.id, x.a, x.b FROM items i CROSS APPLY (SELECT a.k AS a, b.k AS b FROM (VALUES (1), (2)) a(k) FULL JOIN (VALUES (2), (3)) b(k) ON a.k = b.k WHERE i.id = 1) x ORDER BY x.a, x.b;'],
  ['explicit schema sides', multirow, "SELECT i.id, x.a, x.b FROM items i CROSS APPLY (SELECT l.a, r.b FROM OPENJSON(i.lhs) WITH (a INT '$.a', b INT '$.b') l FULL JOIN OPENJSON(i.rhs) WITH (b INT '$.b') r ON l.b = r.b) x WHERE i.id = 1 ORDER BY x.a, x.b;"],
  ['unqualified outer reference in condition', single, "SELECT i.id, x.a, x.b FROM items i CROSS APPLY (SELECT l.[value] AS a, r.[value] AS b FROM OPENJSON(N'[1,2]') l FULL JOIN OPENJSON(N'[2,3]') r ON l.[value] = r.[value] AND id = 1) x ORDER BY x.a, x.b;"],
  ['top and order in body', multirow, 'SELECT i.id, x.[key] FROM items i CROSS APPLY (SELECT TOP (1) COALESCE(l.[key], r.[key]) AS [key] FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key] ORDER BY COALESCE(l.[key], r.[key]) DESC) x ORDER BY i.id;'],
  ['star of distinct columns', multirow, "SELECT i.id, x.* FROM items i CROSS APPLY (SELECT * FROM OPENJSON(i.lhs) WITH (a INT '$.a') l FULL JOIN OPENJSON(i.rhs) WITH (c INT '$.c') r ON l.a = r.c) x WHERE i.id IN (1, 2) ORDER BY i.id, x.a, x.c;"],
  ['inner join before full join', multirow, 'SELECT i.id, x.k1, x.k2, x.k3 FROM items i CROSS APPLY (SELECT l.[key] AS k1, l2.[value] AS k2, r.[key] AS k3 FROM OPENJSON(i.lhs) l JOIN OPENJSON(i.lhs) l2 ON l2.[key] = l.[key] FULL JOIN OPENJSON(i.rhs) r ON r.[key] = l.[key]) x WHERE i.id = 1 ORDER BY x.k1, x.k3;'],
  ['divide by zero in body', single, 'SELECT i.id, x.v FROM items i CROSS APPLY (SELECT 1 / (LEN(COALESCE(l.[key], r.[key])) - 1) AS v FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key]) x;'],
  ['ambiguous column', single, 'SELECT i.id, x.[key] FROM items i CROSS APPLY (SELECT [key] FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key]) x;'],
  ['trigger diff of json snapshots', [docs, foo, snapshotTrigger], `UPDATE docs SET qty = qty + 1, note = CASE id WHEN 2 THEN NULL ELSE N'z' END WHERE id IN (1, 2);${auditRead}`],
  ['trigger diff without isnull', [docs, foo, plainTrigger], `UPDATE docs SET qty = qty + 1, note = CASE id WHEN 2 THEN NULL ELSE N'z' END WHERE id IN (1, 2);${auditRead}`],
  ['trigger with unchanged rows', [docs, foo, plainTrigger], `UPDATE docs SET name = name;${auditRead}`],
].map(([name, setup, query]) => ({ name, setup, query }))

const keep = result => canonical({
  sets: result.sets.map(set => ({ columns: set.columns.map(c => [c.name, c.type, c.length]), rows: set.rows })),
  errors: result.errors.map(e => ({ number: e.number, class: e.class ?? null, state: e.state ?? null, message: e.message })),
  done: result.done.filter(d => d.kind === 'done' || d.kind === 'doneInProc').map(d => d.rowCount),
})

async function run(config) {
  const admin = await connect(config)
  const version = (await capture(admin, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128))")).sets[0].rows[0][0]
  const results = []
  try {
    for (const [index, entry] of cases.entries()) {
      const database = `apply_full_join_${index}`
      await capture(admin, `CREATE DATABASE ${database}`)
      const connection = await connect({ ...config, options: { ...config.options, database } })
      try {
        for (const batch of entry.setup) {
          const setup = await capture(connection, batch)
          if (setup.errors.length) throw new Error(`${entry.name}: setup failed: ${JSON.stringify(setup.errors)}`)
        }
        results.push({ ...entry, result: keep(await capture(connection, entry.query)) })
      } finally {
        await new Promise(resolve => { connection.once('end', resolve); connection.close() })
      }
      await capture(admin, `DROP DATABASE ${database}`)
    }
  } finally { admin.close() }
  return { image, version, cases: results }
}

// Label the container with this task so parallel workers can tell it apart.
const exec = promisify(execFile)
const docker = async (args, env) => {
  if (args[0] === 'run') args = ['run', '--label', 'msduck.task=v025-apply-full-join-v1', ...args.slice(1)]
  return (await exec('docker', args, { env: { ...process.env, ...env }, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()
}

if (writeFixture) await refuseExistingFixture(fixture)
const captured = await withReferenceContainer(run, { image, docker })
if (writeFixture) await writeNewFixture(fixture, captured)
else console.log(JSON.stringify(captured, null, 2))
