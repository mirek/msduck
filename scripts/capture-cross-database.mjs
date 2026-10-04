#!/usr/bin/env node
// Independent SQL Server evidence for three-part names that reach another
// database from the current one: reads, joins, `db..object`, DML, restored
// databases, user access and transactions that write two databases
// (issue #871).
//
// Every observation runs in a fresh pinned container against fixed database
// names, so messages are stable. Two containers must agree.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, realpath, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { capture } from './lib/compatibility.mjs'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { assertSameCapture, connect, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/cross-database.json', import.meta.url)
const fixtureSha256 = 'e93fb09f09ede94e358a924c4c3efdc0f1f3c7fcb8349069ffa606b6aec967ce'
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-cross-database.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/cross-database-reference/capture.json')

// The statements, retained with their results for
// tests/compat/cross_database.test.mjs. Each runs as its own batch from a
// connection whose database is master, unless the observation names another
// connection.
const setup = [
  'CREATE DATABASE xdb_foo',
  `USE xdb_foo; CREATE TABLE dbo.items (id int IDENTITY PRIMARY KEY, name nvarchar(50) NOT NULL, v varchar(10), d decimal(10, 2), dt datetime);
   INSERT dbo.items (name, v, d, dt) VALUES (N'hello', 'x', 1.5, '2020-01-01'); USE master`,
  `CREATE TABLE dbo.loc (id int, label nvarchar(20)); INSERT dbo.loc VALUES (1, N'one')`,
]
const observations = [
  ['read', 'SELECT id FROM xdb_foo.dbo.items'],
  ['read all columns', 'SELECT * FROM xdb_foo.dbo.items'],
  ['default schema', 'SELECT id, name FROM xdb_foo..items'],
  ['case-insensitive database', 'SELECT id FROM XDB_FOO.DBO.ITEMS'],
  ['expressions', "SELECT name + N'!' AS bang, LEN(name) AS length, UPPER(v) AS upper_v, d * 2 AS twice FROM xdb_foo.dbo.items WHERE name = N'hello'"],
  ['catalog view', "SELECT name FROM xdb_foo.sys.tables WHERE name = 'items'"],
  ['join with current database', 'SELECT l.label, i.name FROM dbo.loc l JOIN xdb_foo.dbo.items i ON i.id = l.id'],
  ['join expressions', "SELECT l.label + N'/' + i.name AS pair, LEN(i.name) AS length, UPPER(i.v) AS upper_v FROM dbo.loc l JOIN xdb_foo.dbo.items i ON i.id = l.id WHERE i.name = N'hello' AND l.label LIKE N'o%'"],
  ['exists predicate', "IF EXISTS (SELECT 1 FROM xdb_foo.dbo.items WHERE name = N'hello') SELECT 'yes' AS found"],
  ['functions keep the current database', 'SELECT DB_NAME() AS current_database, COUNT(*) AS items FROM xdb_foo.dbo.items'],
  ['insert', "INSERT xdb_foo.dbo.items (name, v, d, dt) VALUES (N'two', 'y', 2.25, '2021-01-01'); SELECT SCOPE_IDENTITY() AS id, DB_NAME() AS current_database"],
  ['update', "UPDATE xdb_foo.dbo.items SET v = 'z' WHERE id = 1"],
  ['update from current database', 'UPDATE i SET v = LEFT(l.label, 10) FROM xdb_foo.dbo.items i JOIN dbo.loc l ON l.id = i.id; SELECT id, v FROM xdb_foo.dbo.items WHERE id = 1'],
  ['update aliased target', "UPDATE i SET v = 'z' FROM xdb_foo.dbo.items i WHERE i.id = 1"],
  ['delete', 'DELETE xdb_foo.dbo.items WHERE id = 2'],
  ['after dml', 'SELECT id, name, v FROM xdb_foo.dbo.items ORDER BY id'],
  ['insert from another database', 'INSERT dbo.loc SELECT id, name FROM xdb_foo.dbo.items; SELECT id, label FROM dbo.loc ORDER BY id, label'],
  ['insert into another database from current', 'INSERT xdb_foo.dbo.items (name) SELECT label FROM dbo.loc WHERE id = 1 AND label = N\'one\''],
  ['after insert into another database', 'SELECT id, name FROM xdb_foo.dbo.items ORDER BY id'],
  ['two databases in one transaction', "BEGIN TRANSACTION; INSERT dbo.loc VALUES (5, N'five'); INSERT xdb_foo.dbo.items (name) VALUES (N'x'); SELECT @@TRANCOUNT AS trancount; ROLLBACK"],
  ['after rollback', "SELECT (SELECT COUNT(*) FROM dbo.loc WHERE id = 5) AS loc, (SELECT COUNT(*) FROM xdb_foo.dbo.items WHERE name = N'x') AS items"],
  ['unknown database', 'SELECT * FROM xdb_nope.dbo.items'],
  ['unknown object', 'SELECT * FROM xdb_foo.dbo.missing'],
  ['unknown database insert', 'INSERT xdb_nope.dbo.items (name) VALUES (N\'x\')'],
  ['create table in another database', 'CREATE TABLE xdb_foo.dbo.made (id int)'],
  ['restored', "BACKUP DATABASE xdb_foo TO DISK = N'/var/opt/mssql/data/xdb_foo.bak' WITH INIT; RESTORE DATABASE xdb_copy FROM DISK = N'/var/opt/mssql/data/xdb_foo.bak' WITH MOVE N'xdb_foo' TO N'/var/opt/mssql/data/xdb_copy.mdf', MOVE N'xdb_foo_log' TO N'/var/opt/mssql/data/xdb_copy.ldf'"],
  ['read restored', 'SELECT id, name FROM xdb_copy.dbo.items ORDER BY id'],
]
// Run by a second connection while the first holds xdb_single in SINGLE_USER.
const single = {
  setup: ['CREATE DATABASE xdb_single', 'USE xdb_single; CREATE TABLE dbo.t (id int); INSERT dbo.t VALUES (1); USE master', 'ALTER DATABASE xdb_single SET SINGLE_USER'],
  other: 'SELECT id FROM xdb_single.dbo.t',
  owner: 'SELECT id FROM xdb_single.dbo.t',
}

const strip = result => {
  // Restore progress messages carry timing; keep their numbers only.
  const info = result.info.map(m => [3211, 3014, 4035, 3021].includes(m.number) || m.message.includes('percent')
    ? { number: m.number, state: m.state, class: m.class } : m)
  return { ...result, info }
}

async function observe(config) {
  const a = await connect({ ...config, options: { ...config.options, appName: 'msduck-capture' } })
  const b = await connect(config)
  const run = []
  const on = async (name, connection, sql) => {
    const result = strip(await capture(connection, sql))
    run.push({ name, sql, result })
    return result
  }
  try {
    await on('server version', a, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)) AS version")
    for (const [index, sql] of setup.entries()) await on(`setup ${index + 1}`, a, sql)
    for (const [name, sql] of observations) await on(name, a, sql)
    for (const [index, sql] of single.setup.entries()) await on(`single setup ${index + 1}`, a, sql)
    await on('single user other session', b, single.other)
    await on('single user owner session', a, single.owner)
  } finally { a.close(); b.close() }
  validate(run)
  return run
}

const numbers = messages => messages.map(m => m.number)
function validate(run) {
  const get = name => {
    const found = run.find(item => item.name === name)
    assert(found, `${name}: missing observation`)
    return found.result
  }
  assertSameCapture(get('server version').sets[0].rows, [['17.0.4065.4']], 'reference image version changed')
  for (const [name] of observations.filter(([name]) => !['unknown database', 'unknown object', 'unknown database insert', 'create table in another database'].includes(name))) {
    assertSameCapture(numbers(get(name).errors), [], `${name}: errors changed`)
  }
  assertSameCapture(get('read').sets[0].rows, [[1]], 'read changed')
  assertSameCapture(numbers(get('unknown database').errors), [208], 'unknown database changed')
  assertSameCapture(numbers(get('unknown object').errors), [208], 'unknown object changed')
  assertSameCapture(get('two databases in one transaction').sets[0].rows, [[1]], 'two-database transaction changed')
  assertSameCapture(numbers(get('single user other session').errors), [924], 'single-user access changed')
  assertSameCapture(get('read restored').sets[0].rows, get('after insert into another database').sets[0].rows, 'restored read changed')
}

if (check) {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'independent runs missing')
  assert.equal(createHash('sha256').update(JSON.stringify(retained.runs[0])).digest('hex'), retained.captureSha256, 'capture checksum changed')
  for (const run of retained.runs) validate(run)
  assertSameCapture(retained.runs[0], retained.runs[1], 'independent runs differ')
  console.log(`Checked ${retained.runs[0].length} cross-database observations in two retained runs`)
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if (await realpath(dirname(output)).then(dir => resolve(dir, basename(output))) === fileURLToPath(fixture)) throw Error('capture output must not be the retained fixture')
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) throw Error('reference image must be pinned')
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    await withReferenceContainer(async (config, container) => {
      assert.equal(container.image, referenceImage, 'reference image must be pinned')
      const run = await observe(config)
      if (runs.length) assertSameCapture(run, runs[0], 'independent reference containers differ')
      runs.push(run)
    })
  }
  const actual = { image: referenceImage, captureSha256: createHash('sha256').update(JSON.stringify(runs[0])).digest('hex'), runs }
  await writeFile(output, JSON.stringify(actual, null, 1) + '\n', { flag: 'wx' })
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} cross-database observations in two fresh containers`)
}
