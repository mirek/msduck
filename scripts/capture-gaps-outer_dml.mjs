#!/usr/bin/env node
// SQL Server evidence for UPDATE and DELETE whose FROM tree joins the target
// through LEFT, RIGHT or FULL joins, OUTER APPLY or CROSS APPLY (issue #723).
//
// Every case runs three batches in a fresh scratch database: a setup batch,
// the DML batch (followed by SELECT @@ROWCOUNT) and a readback batch. The
// setup and readback are separate so a compile error in the DML (for example
// 209, ambiguous column) is observed without aborting them. The retained
// fixture keeps each batch's SQL, so tests/compat/outer_dml.test.mjs replays
// exactly what SQL Server ran.
//
//   node scripts/capture-gaps-outer_dml.mjs [--write-fixture]
//
// Without --write-fixture the capture is printed. The reference image is
// mcr.microsoft.com/mssql/server:2022-latest unless MSSQL_REFERENCE_IMAGE is
// set; the image and server version are recorded in the fixture.
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/gaps-outer_dml.json', import.meta.url)
const writeFixture = process.argv.includes('--write-fixture')
const image = process.env.MSSQL_REFERENCE_IMAGE ?? 'mcr.microsoft.com/mssql/server:2022-latest'

const tables = `
CREATE TABLE items(id INT NOT NULL PRIMARY KEY, value INT NULL, name VARCHAR(10) NOT NULL);
CREATE TABLE foo(id INT NOT NULL, value INT NULL, other INT NULL);
CREATE TABLE bar(id INT NOT NULL, label VARCHAR(10) NOT NULL);
INSERT INTO items VALUES (1, 10, 'a'), (2, 20, 'bb'), (3, 30, 'ccc'), (4, 40, 'dddd');
INSERT INTO foo VALUES (1, 100, 7), (3, 300, 9), (5, 500, 11);
INSERT INTO bar VALUES (7, 'seven'), (11, 'eleven');`
// Two equal source rows match item 3, so the assigned value is deterministic.
const duplicates = tables + `
INSERT INTO foo VALUES (3, 300, 9);`
const items = 'SELECT id, value, name FROM items ORDER BY id;'

const cases = [
  ['left join update', tables, 'UPDATE target SET value = COALESCE(source.value, 0) FROM items target LEFT JOIN foo source ON source.id = target.id;'],
  ['left join anti delete', tables, 'DELETE target FROM items target LEFT JOIN foo source ON source.id = target.id WHERE source.id IS NULL;'],
  ['left outer join keyword', tables, 'UPDATE t SET value = ISNULL(s.value, -1) FROM items AS t LEFT OUTER JOIN foo AS s ON s.id = t.id;'],
  ['right join update', tables, 'UPDATE t SET value = s.value FROM items t RIGHT JOIN foo s ON s.id = t.id;'],
  ['full join update', tables, 'UPDATE t SET value = ISNULL(s.value, -1) FROM items t FULL JOIN foo s ON s.id = t.id;'],
  ['target on null-extended side', tables, 'UPDATE t SET value = s.value + 1 FROM foo s LEFT JOIN items t ON t.id = s.id;'],
  ['on predicate stays in join', tables, 'UPDATE t SET value = ISNULL(s.value, -1) FROM items t LEFT JOIN foo s ON s.id = t.id AND s.value > 200;'],
  ['where filters joined rows', tables, 'UPDATE t SET value = 0 FROM items t LEFT JOIN foo s ON s.id = t.id WHERE t.id > 1 AND s.id IS NULL;'],
  ['unaliased target', tables, 'UPDATE items SET value = COALESCE(foo.value, 0) FROM items LEFT JOIN foo ON foo.id = items.id;'],
  ['qualified set column', tables, 'UPDATE t SET t.value = COALESCE(s.other, t.value) FROM items t LEFT JOIN foo s ON s.id = t.id;'],
  ['unqualified source and target columns', tables, 'UPDATE t SET value = LEN(name) * 1000 + ISNULL(other, 0) FROM items t LEFT JOIN foo s ON s.id = t.id;'],
  ['ambiguous unqualified column', tables, 'UPDATE t SET value = value FROM items t LEFT JOIN foo s ON s.id = t.id;'],
  ['duplicate matches update once', duplicates, 'UPDATE t SET value = ISNULL(s.value, 0) FROM items t LEFT JOIN foo s ON s.id = t.id;'],
  ['duplicate matches delete once', duplicates, 'DELETE t FROM items t LEFT JOIN foo s ON s.id = t.id WHERE s.id IS NOT NULL OR t.id = 4;'],
  ['chained left joins', tables, "UPDATE t SET name = ISNULL(b.label, 'none') FROM items t LEFT JOIN foo s ON s.id = t.id LEFT JOIN bar b ON b.id = s.other;"],
  ['nested inner join on the outer side', tables, "UPDATE t SET name = ISNULL(b.label, 'none') FROM items t LEFT JOIN (foo s JOIN bar b ON b.id = s.other) ON s.id = t.id;"],
  ['self left join', tables, 'UPDATE t SET value = ISNULL(p.value, 0) FROM items t LEFT JOIN items p ON p.id = t.id - 1;'],
  ['outer apply update', tables, 'UPDATE t SET value = ISNULL(x.v, 0) FROM items t OUTER APPLY (SELECT TOP (1) s.value AS v FROM foo s WHERE s.id = t.id ORDER BY s.value DESC) x;'],
  ['cross apply update', tables, 'UPDATE t SET value = x.v FROM items t CROSS APPLY (SELECT s.value + t.value AS v FROM foo s WHERE s.id = t.id) x;'],
  ['outer apply delete', tables, 'DELETE t FROM items t OUTER APPLY (SELECT TOP (1) s.id FROM foo s WHERE s.id = t.id) x WHERE x.id IS NULL;'],
  ['right join delete', tables, 'DELETE t FROM items t RIGHT JOIN foo s ON s.id = t.id;'],
  ['full join delete', tables, 'DELETE t FROM items t FULL JOIN foo s ON s.id = t.id WHERE s.id IS NULL;'],
  ['delete with second from keyword', tables, 'DELETE FROM t FROM items AS t LEFT JOIN foo AS s ON s.id = t.id WHERE s.id IS NULL;'],
  ['left join update with variable', tables, 'DECLARE @fallback INT; SET @fallback = 5; UPDATE t SET value = ISNULL(s.value, @fallback) FROM items t LEFT JOIN foo s ON s.id = t.id WHERE t.id <> @fallback;'],
  // SQL Server does not order OUTPUT rows; the comparison sorts them.
  ['left join update output', tables, 'UPDATE t SET value = COALESCE(s.value, 0) OUTPUT deleted.id, deleted.value AS old_value, inserted.value AS new_value, s.other FROM items t LEFT JOIN foo s ON s.id = t.id;'],
  ['left join update rollback', tables, 'BEGIN TRANSACTION; UPDATE t SET value = COALESCE(s.value, 0) FROM items t LEFT JOIN foo s ON s.id = t.id; SELECT @@ROWCOUNT AS inner_count; ROLLBACK;'],
  ['left join update nocount', tables, 'SET NOCOUNT ON; UPDATE t SET value = COALESCE(s.value, 0) FROM items t LEFT JOIN foo s ON s.id = t.id; SET NOCOUNT OFF;'],
  ['compound assignment', tables, 'UPDATE t SET value += ISNULL(s.value, 0) FROM items t LEFT JOIN foo s ON s.id = t.id;'],
  ['default assignment', tables, 'UPDATE t SET value = DEFAULT FROM items t LEFT JOIN foo s ON s.id = t.id WHERE s.id IS NULL;'],
  ['common table expression source', tables, 'WITH f AS (SELECT id, value FROM foo WHERE value < 400) UPDATE t SET value = ISNULL(f.value, 0) FROM items t LEFT JOIN f ON f.id = t.id;'],
  ['key change', tables, 'UPDATE t SET id = t.id + 10 FROM items t LEFT JOIN foo s ON s.id = t.id WHERE s.id IS NULL;'],
  ['subquery in assignment', tables, 'UPDATE t SET value = (SELECT COUNT(*) FROM foo f2 WHERE f2.id = s.id) FROM items t LEFT JOIN foo s ON s.id = t.id;'],
  ['inner join after left join', tables, 'UPDATE t SET name = b.label FROM items t LEFT JOIN foo s ON s.id = t.id INNER JOIN bar b ON b.id = s.other;'],
  ['chained left join delete', tables, 'DELETE t FROM items t LEFT JOIN foo s ON s.id = t.id LEFT JOIN bar b ON b.id = s.other WHERE b.id IS NULL;'],
  ['table hints', tables, 'UPDATE t SET value = ISNULL(s.value, 0) FROM items t WITH (NOLOCK) LEFT JOIN foo s WITH (NOLOCK) ON s.id = t.id;'],
  ['assignment error leaves rows unchanged', tables, 'UPDATE t SET value = 1 / (s.value - 100) FROM items t LEFT JOIN foo s ON s.id = t.id;'],
  ['assignment error in try', tables, 'BEGIN TRY UPDATE t SET value = 1 / (s.value - 100) FROM items t LEFT JOIN foo s ON s.id = t.id; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error_number; END CATCH'],
  ['unknown source column', tables, 'UPDATE t SET value = s.missing FROM items t LEFT JOIN foo s ON s.id = t.id;'],
  ['unbound qualifier', tables, 'UPDATE t SET value = nosuch.value FROM items t LEFT JOIN foo s ON s.id = t.id;'],
  ['delete output deleted rows', tables, 'DELETE t OUTPUT deleted.id, deleted.value FROM items t LEFT JOIN foo s ON s.id = t.id WHERE s.id IS NULL;'],
  ['output into', tables, "UPDATE t SET value = ISNULL(s.value, 0) OUTPUT inserted.id + 100, inserted.name INTO bar(id, label) FROM items t LEFT JOIN foo s ON s.id = t.id WHERE s.id IS NULL;", items + ' SELECT id, label FROM bar ORDER BY id;'],
].map(([name, setup, dml, readback = items]) => ({ name, setup, dml: dml + ' SELECT @@ROWCOUNT AS row_count;', readback }))

const keep = result => canonical({
  sets: result.sets.map(set => ({ columns: set.columns.map(c => [c.name, c.type]), rows: set.rows })),
  errors: result.errors.map(e => ({ number: e.number, message: e.message })),
  done: result.done.filter(d => d.kind === 'done' || d.kind === 'doneInProc').map(d => d.rowCount),
})

async function run(config) {
  const admin = await connect(config)
  const version = (await capture(admin, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128))")).sets[0].rows[0][0]
  const results = []
  try {
    for (const [index, entry] of cases.entries()) {
      const database = `outer_dml_${index}`
      await capture(admin, `CREATE DATABASE ${database}`)
      const connection = await connect({ ...config, options: { ...config.options, database } })
      try {
        const setup = await capture(connection, entry.setup)
        if (setup.errors.length) throw new Error(`${entry.name}: setup failed: ${JSON.stringify(setup.errors)}`)
        const dml = keep(await capture(connection, entry.dml))
        const readback = keep(await capture(connection, entry.readback))
        results.push({ ...entry, result: { dml, readback } })
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
  if (args[0] === 'run') args = ['run', '--label', 'msduck.task=gaps-outer-dml-v1', ...args.slice(1)]
  return (await exec('docker', args, { env: { ...process.env, ...env }, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()
}

if (writeFixture) await refuseExistingFixture(fixture)
const captured = await withReferenceContainer(run, { image, docker })
if (writeFixture) await writeNewFixture(fixture, captured)
else console.log(JSON.stringify(captured, null, 2))
