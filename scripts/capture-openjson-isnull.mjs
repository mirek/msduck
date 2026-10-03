#!/usr/bin/env node
// SQL Server evidence for ISNULL, COALESCE, IIF and CASE over OPENJSON key
// and value columns (issue #900): default and explicit WITH schemas, NVARCHAR
// and VARCHAR documents, two OPENJSON sources compared through a correlated
// FULL JOIN, and the multirow AFTER UPDATE trigger that diffs FOR JSON
// snapshots of inserted and deleted.
//
// Every case runs in a fresh scratch database: its setup batches (CREATE
// FUNCTION and CREATE TRIGGER must be alone in a batch), then one query
// batch. tests/compat/openjson_isnull.test.mjs replays the retained SQL.
//
//   node scripts/capture-openjson-isnull.mjs [--write-fixture]
//
// Without --write-fixture the capture is printed. The image is the pinned
// reference image unless MSSQL_REFERENCE_IMAGE is set.
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { capture, canonical } from './lib/compatibility.mjs'
import { referenceImage, withReferenceContainer } from './lib/reference-container.mjs'
import { connect, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/openjson-isnull.json', import.meta.url)
const writeFixture = process.argv.includes('--write-fixture')
const image = process.env.MSSQL_REFERENCE_IMAGE ?? referenceImage

const report = (declaration, prefix) => `DECLARE @d ${declaration} = ${prefix}'{"a":1,"b":null}';
SELECT j.[key], ISNULL(j.[value], N''), COALESCE(j.[value], N'x'), IIF(j.[value] IS NULL, N'n', j.[value]), CASE WHEN j.[value] = N'1' THEN 1 END FROM OPENJSON(@d) j ORDER BY j.[key];`
const doc = `DECLARE @d NVARCHAR(MAX) = N'{"a":1,"b":null,"c":"x ","d":"\\ud83e\\udd86","e":[1,2],"f":true,"g":""}';`
// Comparisons and ordering avoid collation-sensitive text (supplementary
// characters, punctuation, case); the default collation is separate work.
const plain = `DECLARE @d NVARCHAR(MAX) = N'{"a":1,"b":null,"c":"x ","f":true,"g":"","h":"m"}';`
const sides = `CREATE TABLE items(id INT NOT NULL PRIMARY KEY, lhs NVARCHAR(MAX) NULL, rhs NVARCHAR(MAX) NULL);
INSERT INTO items VALUES
 (1, N'{"a":1,"b":2,"s":"x"}', N'{"b":3,"c":4,"s":"x  "}'),
 (2, NULL, N'{"x":1}'),
 (3, N'{"y":null}', N'{"y":""}'),
 (4, N'{"z":"same"}', N'{"z":"same"}');`
const foo = 'CREATE FUNCTION dbo.foo(@lhs NVARCHAR(MAX), @rhs NVARCHAR(MAX)) RETURNS TABLE AS RETURN (SELECT COALESCE(l.[key], r.[key]) AS [key], l.[value] AS old_value, r.[value] AS new_value FROM OPENJSON(@lhs) l FULL OUTER JOIN OPENJSON(@rhs) r ON l.[key] = r.[key]);'
const docs = `CREATE TABLE docs(id INT NOT NULL PRIMARY KEY, name NVARCHAR(20) NULL, qty INT NULL, note NVARCHAR(20) NULL);
CREATE TABLE audit(id INT NOT NULL, [key] NVARCHAR(4000) NULL, old_value NVARCHAR(MAX) NULL, new_value NVARCHAR(MAX) NULL);
INSERT INTO docs VALUES (1, N'one', 10, NULL), (2, N'two', 20, N'x'), (3, N'three', 30, N'y'), (4, N'four', NULL, N'w');`
const trigger = `CREATE TRIGGER docs_audit ON docs AFTER UPDATE AS
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
const auditRead = ' SELECT id, [key], old_value, new_value FROM audit ORDER BY id, [key];'

const cases = [
  ['report nvarchar document', [], report('NVARCHAR(MAX)', 'N')],
  ['report varchar document', [], report('VARCHAR(MAX)', '')],
  ['report bounded nvarchar document', [], report('NVARCHAR(100)', 'N')],
  ['report literal document', [], `SELECT j.[key], ISNULL(j.[value], N''), COALESCE(j.[value], N'x'), IIF(j.[value] IS NULL, N'n', j.[value]), CASE WHEN j.[value] = N'1' THEN 1 END FROM OPENJSON(N'{"a":1,"b":null}') j ORDER BY j.[key];`],
  ['isnull key and value types', [], `${doc} SELECT j.[key], ISNULL(j.[key], N'k') AS k, ISNULL(j.[value], N'') AS v, ISNULL(j.[value], 'ansi') AS va, ISNULL(j.[type], 0) AS t, LEN(ISNULL(j.[value], N'')) AS n, DATALENGTH(ISNULL(j.[value], N'')) AS b FROM OPENJSON(@d) j ORDER BY j.[key];`],
  ['coalesce mixes', [], `${doc} SELECT j.[key], COALESCE(j.[value], j.[key]) AS vk, COALESCE(j.[value], 'ansi') AS va, COALESCE(j.[value], NULL, N'z') AS vz, COALESCE(NULL, j.[value]) AS nv FROM OPENJSON(@d) j ORDER BY j.[key];`],
  ['carrier as replacement', [], `${doc} DECLARE @a VARCHAR(10) = NULL, @n NVARCHAR(10) = NULL; SELECT j.[key], ISNULL(@n, j.[value]) AS nv, ISNULL(@a, j.[value]) AS av, ISNULL(@n, j.[key]) AS nk FROM OPENJSON(@d) j ORDER BY j.[key];`],
  ['iif and case', [], `${doc} SELECT j.[key], IIF(j.[value] IS NULL, N'n', j.[value]) AS i1, IIF(j.[type] = 2, j.[value], 'other') AS i2, CASE WHEN j.[type] = 0 THEN NULL ELSE j.[value] END AS c3 FROM OPENJSON(@d) j ORDER BY j.[key];`],
  ['case comparisons', [], `${plain} SELECT j.[key], CASE j.[value] WHEN N'1' THEN N'one' WHEN N'x' THEN N'ex' ELSE j.[key] END AS c1, CASE WHEN j.[value] = N'x' THEN 1 WHEN j.[value] > N'l' THEN 2 ELSE 0 END AS c2 FROM OPENJSON(@d) j ORDER BY j.[key];`],
  ['isnull in predicates and ordering', [], `${plain} SELECT j.[key] FROM OPENJSON(@d) j WHERE ISNULL(j.[value], N'') <> N'' AND ISNULL(j.[value], N'') <> N'x' ORDER BY ISNULL(j.[value], N'') DESC, j.[key];`],
  ['comparison with literal', [], `${plain} SELECT j.[key] FROM OPENJSON(@d) j WHERE j.[value] = N'x' OR j.[value] = '1' OR j.[value] IN (N'true', N'') ORDER BY j.[key];`],
  ['explicit schema', [], `${doc} SELECT ISNULL(w.a, N'') AS a, ISNULL(w.b, N'nb') AS b, COALESCE(w.c, 'cc') AS c, IIF(w.b IS NULL, N'n', w.b) AS i, CASE WHEN w.c = N'x' THEN 1 ELSE 0 END AS e, ISNULL(w.e, N'[]') AS j FROM OPENJSON(@d) WITH (a NVARCHAR(10) '$.a', b NVARCHAR(MAX) '$.b', c VARCHAR(10) '$.c', e NVARCHAR(MAX) '$.e' AS JSON) w;`],
  ['explicit schema varchar document', [], `DECLARE @d VARCHAR(MAX) = '{"a":"p","b":null}'; SELECT ISNULL(w.a, N'') AS a, ISNULL(w.b, 'nb') AS b, COALESCE(w.b, w.a) AS c FROM OPENJSON(@d) WITH (a NVARCHAR(10), b VARCHAR(5)) w;`],
  ['two sources full join', [sides], `SELECT i.id, x.k, x.l, x.r FROM items i CROSS APPLY (SELECT COALESCE(l.[key], r.[key]) AS k, l.[value] AS l, r.[value] AS r FROM OPENJSON(i.lhs) l FULL JOIN OPENJSON(i.rhs) r ON l.[key] = r.[key] WHERE ISNULL(l.[value], N'') <> ISNULL(r.[value], N'')) x ORDER BY i.id, x.k;`],
  ['two sources through function', [sides, foo], `SELECT i.id, x.[key], x.old_value, x.new_value, ISNULL(x.old_value, N'-') AS o, COALESCE(x.new_value, N'-') AS n FROM items i OUTER APPLY dbo.foo(i.lhs, i.rhs) x WHERE ISNULL(x.old_value, N'') <> ISNULL(x.new_value, N'') ORDER BY i.id, x.[key];`],
  ['trigger diff of json snapshots', [docs, foo, trigger], `UPDATE docs SET qty = qty + 1, note = CASE id WHEN 2 THEN NULL WHEN 4 THEN N'w' ELSE N'z' END WHERE id IN (1, 2, 4);${auditRead}`],
  ['trigger with unchanged rows', [docs, foo, trigger], `UPDATE docs SET name = name;${auditRead}`],
  // Issue #901 follow-up: ISNULL over an aggregated (carrier) subquery inside concatenation.
  ['concat isnull string_agg nvarchar', [], "SELECT N'text' + ISNULL((SELECT STRING_AGG(v, N',') FROM (VALUES (N'a'),(N'b')) t(v)), N'') AS s;"],
  ['concat isnull string_agg varchar', [], "SELECT 'text' + ISNULL((SELECT STRING_AGG(v, ',') FROM (VALUES ('a'),('b')) t(v)), '') AS s;"],
  ['concat isnull aggregate subquery', [], "SELECT N'text' + ISNULL((SELECT MAX(v) FROM (VALUES (N'a'),(N'b')) t(v)), N'') AS s, ISNULL((SELECT MIN(v) FROM (VALUES (N'abc')) t(v)), N'') AS m, CASE WHEN ISNULL((SELECT MAX(v) FROM (VALUES (N'a')) t(v)), N'') = N'a' THEN 1 ELSE 0 END AS e;"],
  ['isnull stored unicode widths', ['CREATE TABLE tn(n NVARCHAR(3) NULL, c NCHAR(3) NULL); INSERT INTO tn VALUES (NULL, NULL), (N\'ab\', N\'ab\');'], "SELECT ISNULL(n, N'xyzw') AS a, ISNULL(c, N'q') AS b, N'<' + ISNULL(n, N'') + N'>' AS d, ISNULL((SELECT STRING_AGG(n, N',') FROM tn), N'') AS e FROM tn ORDER BY n;"],
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
      const database = `openjson_isnull_${index}`
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
  if (args[0] === 'run') args = ['run', '--label', 'msduck.task=v025-openjson-isnull-v1', ...args.slice(1)]
  return (await exec('docker', args, { env: { ...process.env, ...env }, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()
}

if (writeFixture) await refuseExistingFixture(fixture)
const captured = await withReferenceContainer(run, { image, docker })
if (writeFixture) await writeNewFixture(fixture, captured)
else console.log(JSON.stringify(captured, null, 2))
