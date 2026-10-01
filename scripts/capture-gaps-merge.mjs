#!/usr/bin/env node
// SQL Server evidence for MERGE gaps (issue #722): the workload's hinted
// upsert, every WHEN family with OUTPUT, duplicate source matches that select
// one or no action, constraint and identity diagnostics, the EF Core batched
// insert shape, and batch/transaction boundaries after errors.
//
// One fresh container runs every observation in two fresh databases; the runs
// must agree once the generated database names are bound. DONE tokens are
// recorded verbatim (status, CurCmd, row count) before tedious parses them.
//
//   node scripts/capture-gaps-merge.mjs [--write-fixture] [output]
//   node scripts/capture-gaps-merge.mjs --check
//
// MSSQL_REFERENCE_IMAGE selects the image (the retained fixture names the one
// it used); see scripts/lib/reference-container.mjs.
import assert from 'node:assert/strict'
import { createHash, randomUUID } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'
import { Request } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { assertSameCapture, connect, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/gaps-merge.json', import.meta.url)
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-gaps-merge.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/gaps-merge/capture.json')

const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneTypes = new Map([[0xFD, 'done'], [0xFE, 'doneProc'], [0xFF, 'doneInProc']])
const sinks = new WeakMap()
const readToken = StreamParser.prototype.readToken
StreamParser.prototype.readToken = function (type) {
  return doneTypes.has(type) ? recordDone(this, type) : readToken.call(this, type)
}
function recordDone (parser, type) {
  const at = parser.position
  if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordDone(parser, type))
  sinks.get(parser.options)?.push({
    kind: doneTypes.get(type),
    status: parser.buffer.readUInt16LE(at),
    curCmd: parser.buffer.readUInt16LE(at + 2),
    rowCount: parser.buffer.readBigUInt64LE(at + 4).toString()
  })
  return readToken.call(parser, type)
}

function run (connection, sql) {
  return new Promise(resolve => {
    const result = { sets: [], done: [], errors: [], info: [] }
    const done = []
    sinks.set(connection.config.options, done)
    const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, message: e.message })
    const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, message: e.message })
    const finish = error => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      sinks.delete(connection.config.options)
      result.done = done
      if (error && !result.errors.length) result.transport = { code: error.code ?? null, message: error.message }
      resolve(canonical(result))
    }
    const request = new Request(sql, error => finish(error))
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    request.on('columnMetadata', metadata => result.sets.push({
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, flags: c.flags })), rows: []
    }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => c.value)))
    try { connection.execSqlBatch(request) } catch (error) { finish(error) }
  })
}

function close (connection) {
  if (connection.closed) return Promise.resolve()
  return new Promise(resolve => { connection.once('end', resolve); connection.close() })
}

const tables = `
CREATE TABLE dbo.items(id INT NOT NULL CONSTRAINT PK_items PRIMARY KEY);
CREATE TABLE dbo.t(id INT NOT NULL CONSTRAINT PK_t PRIMARY KEY, n INT NOT NULL CONSTRAINT CK_t_positive CHECK (n > 0), note VARCHAR(5) NULL);
CREATE TABLE dbo.blogs(Id INT IDENTITY(1,1) CONSTRAINT PK_blogs PRIMARY KEY, Name NVARCHAR(50) NOT NULL);
CREATE TABLE dbo.computed(id INT NOT NULL CONSTRAINT PK_computed PRIMARY KEY, n INT NOT NULL, twice AS n * 2);`
const reset = 'DELETE FROM dbo.t; INSERT dbo.t(id,n) VALUES (1,10),(2,20),(3,30)'

async function observe (config, database) {
  const observations = []
  const master = await connect({ ...config, options: { ...config.options, database: 'master' } })
  await run(master, `CREATE DATABASE [${database}]`)
  const db = await connect({ ...config, options: { ...config.options, database } })
  const on = async (name, sql) => {
    observations.push({ name, sql, result: await run(db, sql) })
    console.log(name)
  }
  try {
    await on('server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version")
    await on('tables', tables)
    const upsert = hint => `MERGE items ${hint} AS target USING (VALUES (1)) AS source(id) ON target.id=source.id WHEN NOT MATCHED THEN INSERT (id) VALUES (source.id);`
    await on('upsert serializable', upsert('WITH(SERIALIZABLE)'))
    await on('upsert serializable again', upsert('WITH(SERIALIZABLE)'))
    await on('upsert without hint', 'DELETE FROM dbo.items; ' + upsert(''))
    await on('upsert holdlock updlock rowlock', 'DELETE FROM dbo.items; ' + upsert('WITH (HOLDLOCK, UPDLOCK, ROWLOCK)'))
    await on('upsert nolock target', upsert('WITH (NOLOCK)'))
    await on('upsert unknown hint', upsert('WITH (bogus)'))
    await on('upsert hint after alias', 'MERGE items AS target WITH (HOLDLOCK) USING (VALUES (2)) AS source(id) ON target.id=source.id WHEN NOT MATCHED THEN INSERT (id) VALUES (source.id);')
    await on('items', 'SELECT id FROM dbo.items ORDER BY id')

    await on('reset', reset)
    await on('output families', `MERGE dbo.t AS t USING (VALUES (1,11),(2,22),(4,44)) AS s(id,n) ON t.id=s.id
      WHEN MATCHED AND s.id=1 THEN UPDATE SET n=s.n
      WHEN MATCHED THEN DELETE
      WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n)
      WHEN NOT MATCHED BY SOURCE THEN UPDATE SET n=t.n+1
      OUTPUT $action, inserted.*, deleted.id, s.n AS source_n;`)
    await on('output families rows', 'SELECT id,n FROM dbo.t ORDER BY id')

    await on('reset duplicate one action', reset)
    await on('duplicate match one action', 'MERGE dbo.t AS t USING (VALUES (1,11),(1,12)) AS s(id,n) ON t.id=s.id WHEN MATCHED AND s.n=12 THEN UPDATE SET n=s.n;')
    await on('duplicate match no action', 'MERGE dbo.t AS t USING (VALUES (1,13),(1,14)) AS s(id,n) ON t.id=s.id WHEN MATCHED AND s.n>100 THEN UPDATE SET n=s.n;')
    await on('duplicate match update', 'MERGE dbo.t AS t USING (VALUES (1,15),(1,16)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n OUTPUT $action, inserted.n; SELECT 1 AS after_error')
    await on('duplicate match update and delete', 'MERGE dbo.t AS t USING (VALUES (1,17),(1,18)) AS s(id,n) ON t.id=s.id WHEN MATCHED AND s.n=17 THEN UPDATE SET n=s.n WHEN MATCHED THEN DELETE;')
    await on('duplicate match rows', 'SELECT id,n FROM dbo.t ORDER BY id')
    await on('duplicate match delete', 'MERGE dbo.t AS t USING (VALUES (1),(1),(2)) AS s(id) ON t.id=s.id WHEN MATCHED THEN DELETE OUTPUT $action, deleted.id; SELECT @@ROWCOUNT AS after_delete')
    await on('duplicate delete rows', 'SELECT id,n FROM dbo.t ORDER BY id')

    await on('reset transaction', reset)
    await on('duplicate in transaction', 'BEGIN TRANSACTION; INSERT dbo.items VALUES (50); MERGE dbo.t AS t USING (VALUES (1,1),(1,2)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n; SELECT @@TRANCOUNT AS trancount, XACT_STATE() AS xact_state')
    await on('duplicate in transaction state', 'SELECT @@TRANCOUNT AS trancount, XACT_STATE() AS xact_state, (SELECT COUNT(*) FROM dbo.items WHERE id=50) AS prior_work')
    await on('check in transaction', 'BEGIN TRANSACTION; INSERT dbo.items VALUES (51); MERGE dbo.t AS t USING (VALUES (1,-1),(9,9)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n); SELECT @@TRANCOUNT AS trancount, XACT_STATE() AS xact_state, (SELECT COUNT(*) FROM dbo.items WHERE id=51) AS prior_work, (SELECT COUNT(*) FROM dbo.t) AS target_rows')
    await on('check in transaction rollback', 'ROLLBACK TRANSACTION')

    await on('reset constraints', reset)
    await on('check failure continues batch', 'MERGE dbo.t AS t USING (VALUES (1,-1)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n; SELECT @@ROWCOUNT AS rowcount_after, @@ERROR AS error_after')
    await on('not null update', 'MERGE dbo.t AS t USING (VALUES (1,CAST(NULL AS INT))) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n;')
    await on('not null insert', 'MERGE dbo.t AS t USING (VALUES (9,CAST(NULL AS INT))) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n);')
    await on('duplicate key insert', 'MERGE dbo.t AS t USING (VALUES (8,1),(8,2)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n);')
    await on('duplicate key update', 'MERGE dbo.t AS t USING (VALUES (1,2)) AS s(id,k) ON t.id=s.id WHEN MATCHED THEN UPDATE SET id=s.k;')
    await on('truncation', "MERGE dbo.t AS t USING (VALUES (1,'too long')) AS s(id,note) ON t.id=s.id WHEN MATCHED THEN UPDATE SET note=s.note;")
    await on('constraint rows', 'SELECT id,n,note FROM dbo.t ORDER BY id')

    await on('identity explicit insert', "MERGE dbo.blogs AS b USING (VALUES (5,N'x')) AS s(Id,Name) ON b.Id=s.Id WHEN NOT MATCHED THEN INSERT (Id,Name) VALUES (s.Id,s.Name);")
    await on('identity update', "MERGE dbo.blogs AS b USING (VALUES (1)) AS s(Id) ON 1=1 WHEN MATCHED THEN UPDATE SET Id=s.Id;")
    await on('computed assignment', 'MERGE dbo.computed AS c USING (VALUES (1,2)) AS s(id,n) ON c.id=s.id WHEN MATCHED THEN UPDATE SET twice=s.n;')
    await on('insert count mismatch', 'MERGE dbo.t AS t USING (VALUES (9,9)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id);')
    await on('ef core batched insert', "MERGE [dbo].[blogs] USING (VALUES (N'first', 0), (N'second', 1)) AS i ([Name], _Position) ON 1=0 WHEN NOT MATCHED THEN INSERT ([Name]) VALUES (i.[Name]) OUTPUT INSERTED.[Id], i._Position;")
    await on('computed output', 'MERGE dbo.computed AS c USING (VALUES (1,2)) AS s(id,n) ON c.id=s.id WHEN NOT MATCHED THEN INSERT (id,n) VALUES (s.id,s.n) OUTPUT inserted.*;')
    await on('default values', "CREATE TABLE dbo.defaults(id INT IDENTITY(1,1) CONSTRAINT PK_defaults PRIMARY KEY, n INT NOT NULL CONSTRAINT DF_defaults_n DEFAULT 7); MERGE dbo.defaults AS d USING (VALUES (1),(2)) AS s(k) ON 1=0 WHEN NOT MATCHED THEN INSERT DEFAULT VALUES OUTPUT $action, inserted.*, s.k;")
    await on('action expression', "MERGE dbo.defaults AS d USING (VALUES (1)) AS s(id) ON d.id=s.id WHEN MATCHED THEN UPDATE SET n=8 OUTPUT LEFT($action, 1) AS initial, inserted.n;")
  } finally {
    await close(db)
    await run(master, `DROP DATABASE [${database}]`)
    await close(master)
  }
  return observations
}

const numbers = messages => messages.map(m => m.number)
function validate (observations) {
  const get = name => {
    const found = observations.find(item => item.name === name)
    assert(found, `${name}: missing observation`)
    return found.result
  }
  const expect = (name, errors) => assertSameCapture(numbers(get(name).errors), errors, `${name}: errors changed`)
  const rows = (name, expected, set = 0) => assertSameCapture(get(name).sets[set].rows, expected, `${name}: rows changed`)
  const merges = name => get(name).done.filter(done => done.curCmd === 279).map(done => [done.status, done.rowCount])
  for (const name of ['upsert serializable', 'upsert without hint', 'upsert holdlock updlock rowlock']) expect(name, [])
  assertSameCapture(merges('upsert serializable'), [[16, '1']], 'upsert completion changed')
  assertSameCapture(merges('upsert serializable again'), [[16, '0']], 'repeated upsert completion changed')
  expect('upsert nolock target', [1065])
  expect('upsert unknown hint', [321])
  expect('upsert hint after alias', [156, 319])
  rows('items', [[1]])
  rows('output families', [['UPDATE', 1, 11, null, 1, 11], ['DELETE', null, null, null, 2, 22], ['UPDATE', 3, 31, null, 3, null], ['INSERT', 4, 44, null, null, 44]])
  assertSameCapture(get('output families').sets[0].columns.map(c => [c.name, c.type, c.flags]),
    [['$action', 'NVarChar', 0], ['id', 'IntN', 9], ['n', 'IntN', 9], ['note', 'VarChar', 9], ['id', 'IntN', 9], ['source_n', 'IntN', 1]], 'OUTPUT descriptors changed')
  rows('output families rows', [[1, 11], [3, 31], [4, 44]])
  assertSameCapture(merges('duplicate match one action'), [[16, '1']], 'one-action duplicate changed')
  assertSameCapture(merges('duplicate match no action'), [[16, '0']], 'no-action duplicate changed')
  expect('duplicate match update', [8672])
  assertSameCapture(get('duplicate match update').sets.length, 1, 'batch continued after 8672')
  expect('duplicate match update and delete', [8672])
  rows('duplicate match rows', [[1, 12], [2, 20], [3, 30]])
  expect('duplicate match delete', [])
  rows('duplicate match delete', [['DELETE', 1], ['DELETE', 2]])
  rows('duplicate match delete', [[2]], 1)
  rows('duplicate delete rows', [[3, 30]])
  expect('duplicate in transaction', [8672])
  assertSameCapture(get('duplicate in transaction state').sets[0].rows.map(row => [row[0], row[2]]), [[0, 0]], '8672 kept the transaction')
  expect('check in transaction', [547])
  rows('check in transaction', [[1, 1, 1, 3]])
  expect('check failure continues batch', [547])
  rows('check failure continues batch', [[0, 547]])
  for (const name of ['not null update', 'not null insert']) expect(name, [515])
  for (const name of ['duplicate key insert', 'duplicate key update']) expect(name, [2627])
  expect('truncation', [2628])
  rows('constraint rows', [[1, 10, null], [2, 20, null], [3, 30, null]])
  expect('identity explicit insert', [544])
  expect('identity update', [8102])
  expect('computed assignment', [271])
  expect('insert count mismatch', [109])
  assertSameCapture(get('ef core batched insert').sets[0].rows.sort((a, b) => a[1] - b[1]), [[1, 0], [2, 1]], 'EF Core rows changed')
  rows('computed output', [[1, 2, 4]])
  assertSameCapture(get('default values').sets[0].rows.sort((a, b) => a[1] - b[1]), [['INSERT', 1, 7, 1], ['INSERT', 2, 7, 2]], 'DEFAULT VALUES rows changed')
  rows('action expression', [['U', 8]])
}

const bind = (observations, database) => JSON.parse(JSON.stringify(observations).replaceAll(database, '<database>'))

if (check) {
  const retained = JSON.parse(await readFile(fixture))
  assert.equal(createHash('sha256').update(JSON.stringify(retained.runs[0])).digest('hex'), retained.captureSha256, 'capture checksum changed')
  for (const observations of retained.runs) validate(observations)
  assertSameCapture(retained.runs[0], retained.runs[1], 'independent databases differ')
  console.log(`Checked ${retained.runs[0].length} MERGE observations in two retained runs`)
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  const runs = []
  let image
  await withReferenceContainer(async (config, container) => {
    image = container.image
    for (let repeat = 0; repeat < 2; repeat++) {
      const database = `msduck_merge_${randomUUID().replaceAll('-', '')}`
      const observations = bind(await observe(config, database), database)
      validate(observations)
      if (runs.length) assertSameCapture(observations, runs[0], 'independent databases differ')
      runs.push(observations)
    }
  })
  const actual = { image, captureSha256: createHash('sha256').update(JSON.stringify(runs[0])).digest('hex'), runs }
  await writeFile(output, JSON.stringify(actual, null, 1) + '\n')
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} MERGE observations in two fresh databases`)
}
