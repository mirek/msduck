#!/usr/bin/env node
// Independent SQL Server evidence for BACKUP DATABASE, RESTORE HEADERONLY,
// RESTORE FILELISTONLY, RESTORE DATABASE, msdb backup history and the file
// catalogs (issue #714).
//
// Every observation runs in a fresh reference container against fixed
// database names and backup paths. Values that identify the environment or
// the moment (dates, GUIDs, LSNs, sizes, page counts, timings, host names)
// are reduced to markers, so two runs compare equal. Descriptors, error
// numbers, states, messages and raw DONE tokens are kept verbatim.
//
//   node scripts/capture-gaps-backup.mjs [output]        capture into output
//   node scripts/capture-gaps-backup.mjs --write-fixture  capture into reference/
//   node scripts/capture-gaps-backup.mjs --check          validate the fixture
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'
import { Request } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/gaps-backup.json', import.meta.url)
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-gaps-backup.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/gaps-backup-reference/capture.json')

// Raw DONE bodies (status, CurCmd, row count), recorded before tedious parses
// them. The parser options object belongs to one connection.
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneTypes = new Map([[0xFD, 'done'], [0xFE, 'doneProc'], [0xFF, 'doneInProc']])
const sinks = new WeakMap()
const readToken = StreamParser.prototype.readToken
StreamParser.prototype.readToken = function (type) {
  return doneTypes.has(type) ? recordDone(this, type) : readToken.call(this, type)
}
function recordDone(parser, type) {
  const at = parser.position
  if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordDone(parser, type))
  sinks.get(parser.options)?.push({
    kind: doneTypes.get(type),
    status: parser.buffer.readUInt16LE(at),
    curCmd: parser.buffer.readUInt16LE(at + 2),
    rowCount: parser.buffer.readBigUInt64LE(at + 4).toString(),
  })
  return readToken.call(parser, type)
}

const guid = /^[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}$/i
// Columns whose values depend on the run, by lower-case name.
const volatile = new Set([
  'backupstartdate', 'backupfinishdate', 'databasecreationdate', 'backupsize', 'compressedbackupsize',
  'firstlsn', 'lastlsn', 'checkpointlsn', 'databasebackuplsn', 'differentialbaselsn', 'forkpointlsn',
  'servername', 'machinename', 'backupsizeinbytes', 'size', 'createlsn', 'droplsn', 'readonlylsn', 'readwritelsn',
  'backup_start_date', 'backup_finish_date', 'database_creation_date', 'backup_size', 'compressed_backup_size',
  'first_lsn', 'last_lsn', 'checkpoint_lsn', 'database_backup_lsn', 'differential_base_lsn', 'differential_base_time',
  'server_name', 'machine_name', 'restore_date', 'file_size', 'backed_up_page_count', 'create_lsn', 'redo_start_lsn',
  'redo_target_lsn', 'backup_lsn', 'timezone', 'time_zone', 'media_set_id', 'backup_set_id', 'restore_history_id',
  'family_guid', 'database_guid', 'media_family_id',
])
function marker(column, value) {
  if (value === null || value === undefined) return value ?? null
  if (value instanceof Date) return '<datetime>'
  if (typeof value === 'string' && guid.test(value)) return '<guid>'
  if (volatile.has(column.toLowerCase())) return `<${typeof value}>`
  return value
}
const message = text => text
  .replace(/Processed \d+ pages/, 'Processed <n> pages')
  .replace(/processed \d+ pages in [\d.]+ seconds \([\d.]+ MB\/sec\)/, 'processed <n> pages in <t> seconds (<r> MB/sec)')

// One batch with its descriptors, rows, messages and raw DONE tokens.
function run(connection, sql) {
  return new Promise(resolve => {
    const result = { sets: [], done: [], errors: [], info: [] }
    const done = []
    sinks.set(connection.config.options, done)
    const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, message: message(e.message) })
    const onInfo = e => { if (![5701, 5703].includes(e.number)) result.info.push({ number: e.number, state: e.state, class: e.class, message: message(e.message) }) }
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
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags })), rows: [],
    }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => marker(c.metadata.colName, c.value))))
    try { connection.execSqlBatch(request) } catch (error) { finish(error) }
  })
}

async function observe(config) {
  const observations = []
  const open = database => connect({ ...config, options: { ...config.options, database, appName: 'msduck-capture', requestTimeout: 120000 } })
  const a = await open('master')
  const on = async (name, connection, sql) => observations.push({ name, sql, result: await run(connection, sql) })
  try {
    await on('server version', a, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)) AS version")
    await on('setup', a, `CREATE DATABASE foo; ALTER DATABASE foo SET RECOVERY SIMPLE;
      EXEC('USE foo; CREATE TABLE dbo.t(id int IDENTITY(1,1) PRIMARY KEY, v nvarchar(20) NOT NULL); INSERT dbo.t(v) VALUES (N''a''),(N''b'')')`)
    await on('backup format compression', a, "BACKUP DATABASE foo TO DISK=N'/tmp/foo.bak' WITH FORMAT, NAME=N'foo', COMPRESSION")
    await on('headeronly', a, "RESTORE HEADERONLY FROM DISK=N'/tmp/foo.bak'")
    await on('filelistonly', a, "RESTORE FILELISTONLY FROM DISK=N'/tmp/foo.bak'")
    await on('restore replace move', a, "RESTORE DATABASE foo_copy FROM DISK=N'/tmp/foo.bak' WITH REPLACE, MOVE N'foo' TO N'/var/opt/mssql/data/foo_copy.mdf', MOVE N'foo_log' TO N'/var/opt/mssql/data/foo_copy.ldf', RECOVERY, STATS=50")
    await on('restored contents', a, 'SELECT id, v FROM foo_copy.dbo.t ORDER BY id')
    await on('restore same family without replace', a, "RESTORE DATABASE foo_copy FROM DISK=N'/tmp/foo.bak' WITH MOVE N'foo' TO N'/var/opt/mssql/data/foo_copy.mdf', MOVE N'foo_log' TO N'/var/opt/mssql/data/foo_copy.ldf'")
    await on('other database setup', a, 'CREATE DATABASE bar; ALTER DATABASE bar SET RECOVERY SIMPLE')
    await on('restore other database without replace', a, "RESTORE DATABASE bar FROM DISK=N'/tmp/foo.bak' WITH MOVE N'foo' TO N'/var/opt/mssql/data/bar2.mdf', MOVE N'foo_log' TO N'/var/opt/mssql/data/bar2.ldf'")
    await on('restore without move onto files in use', a, "RESTORE DATABASE foo_y FROM DISK=N'/tmp/foo.bak'")
    await on('restore missing file', a, "RESTORE DATABASE foo_x FROM DISK=N'/tmp/nope.bak'")
    await on('headeronly missing file', a, "RESTORE HEADERONLY FROM DISK=N'/tmp/nope.bak'")
    await on('restore unknown logical file', a, "RESTORE DATABASE foo_z FROM DISK=N'/tmp/foo.bak' WITH MOVE N'nope' TO N'/var/opt/mssql/data/z.mdf'")
    await on('backup unknown database', a, "BACKUP DATABASE nosuch TO DISK=N'/tmp/x.bak'")
    await on('backup in transaction', a, "BEGIN TRAN; BACKUP DATABASE foo TO DISK=N'/tmp/x.bak'; ROLLBACK")
    await on('restore in transaction', a, "BEGIN TRAN; RESTORE DATABASE foo_copy FROM DISK=N'/tmp/foo.bak' WITH REPLACE; ROLLBACK")
    await on('backup log simple', a, "BACKUP LOG bar TO DISK=N'/tmp/barlog.bak'")
    await on('backup missing directory', a, "BACKUP DATABASE foo TO DISK=N'/nodir/x.bak'")
    await on('backup noinit append', a, "BACKUP DATABASE foo TO DISK=N'/tmp/foo.bak' WITH STATS=50, DESCRIPTION=N'second', CHECKSUM, COPY_ONLY")
    await on('headeronly two sets', a, "RESTORE HEADERONLY FROM DISK=N'/tmp/foo.bak'")
    await on('filelistonly file 2', a, "RESTORE FILELISTONLY FROM DISK=N'/tmp/foo.bak' WITH FILE=2")
    await on('restore missing position', a, "RESTORE DATABASE foo_copy FROM DISK=N'/tmp/foo.bak' WITH FILE=3, REPLACE")
    await on('backup init uncompressed', a, "BACKUP DATABASE bar TO DISK=N'/tmp/bar.bak' WITH INIT")
    await on('headeronly uncompressed', a, "RESTORE HEADERONLY FROM DISK=N'/tmp/bar.bak'")
    await on('restore in use by this session', a, "USE foo_copy; RESTORE DATABASE foo_copy FROM DISK=N'/tmp/foo.bak' WITH REPLACE")
    await on('back to master', a, 'USE master')
    const b = await open('foo_copy')
    try {
      await on('restore in use by another session', a, "RESTORE DATABASE foo_copy FROM DISK=N'/tmp/foo.bak' WITH REPLACE")
    } finally { b.close() }
    await on('verifyonly', a, "RESTORE VERIFYONLY FROM DISK=N'/tmp/foo.bak' WITH FILE=2")
    await on('backup unknown option', a, "BACKUP DATABASE foo TO DISK=N'/tmp/x.bak' WITH BOGUS")
    await on('msdb backupmediafamily', a, 'SELECT TOP(1) * FROM msdb..backupmediafamily')
    await on('msdb backupmediaset', a, 'SELECT TOP(1) * FROM msdb.dbo.backupmediaset')
    await on('msdb backupset', a, 'SELECT TOP(1) * FROM msdb.dbo.backupset ORDER BY backup_set_id')
    await on('msdb restorehistory', a, 'SELECT * FROM msdb.dbo.restorehistory ORDER BY restore_history_id')
    await on('msdb backupfile', a, 'SELECT TOP(2) * FROM msdb.dbo.backupfile ORDER BY backup_set_id, file_number')
    await on('msdb restorefile', a, 'SELECT * FROM msdb.dbo.restorefile ORDER BY restore_history_id, file_number')
    await on('database_files', a, 'USE foo_copy; SELECT * FROM sys.database_files ORDER BY file_id; USE master')
    await on('master_files', a, "SELECT * FROM sys.master_files WHERE database_id IN (1, 4, DB_ID('foo_copy')) ORDER BY database_id, file_id")
    await on('msdb identity', a, "SELECT DB_ID('msdb') AS id, DB_NAME(4) AS name")
    await on('drop msdb', a, 'DROP DATABASE msdb')
  } finally { a.close() }
  return observations
}

const numbers = list => list.map(m => [m.number, m.state])
const done = result => result.done.map(d => [d.kind, d.status, d.curCmd])
function validate(observations) {
  const get = name => {
    const found = observations.find(item => item.name === name)
    assert(found, `${name}: missing observation`)
    return found.result
  }
  const expect = (name, errors, info = []) => {
    assert.deepEqual(numbers(get(name).errors), errors, `${name}: errors changed`)
    assert.deepEqual(get(name).info.map(m => m.number), info, `${name}: messages changed`)
  }
  expect('backup format compression', [], [4035, 4035, 3014])
  assert.equal(get('headeronly').sets[0].columns.length, 59, 'HEADERONLY column set changed')
  assert.equal(get('filelistonly').sets[0].columns.length, 22, 'FILELISTONLY column set changed')
  assert.deepEqual(get('filelistonly').sets[0].rows.map(r => [r[0], r[2]]), [['foo', 'D'], ['foo_log', 'L']])
  expect('restore replace move', [], [3211, 3211, 4035, 4035, 3014])
  assert.deepEqual(get('restored contents').sets[0].rows, [[1, 'a'], [2, 'b']])
  expect('restore same family without replace', [], [4035, 4035, 3014])
  expect('restore other database without replace', [[3154, 4], [3013, 1]])
  expect('restore without move onto files in use', [[1834, 1], [3156, 4], [1834, 1], [3156, 4], [3119, 1], [3013, 1]])
  expect('restore missing file', [[3201, 2], [3013, 1]])
  expect('headeronly missing file', [[3201, 2], [3013, 1]])
  expect('restore unknown logical file', [[3234, 2], [3013, 1]])
  expect('backup unknown database', [[911, 11], [3013, 1]])
  expect('backup in transaction', [[3021, 0], [3013, 1]])
  expect('restore in transaction', [[3021, 0], [3013, 1]])
  expect('backup log simple', [[4208, 1], [3013, 1]])
  expect('backup missing directory', [[3201, 1], [3013, 1]])
  expect('backup noinit append', [], [3211, 3211, 4035, 4035, 3014])
  assert.deepEqual(get('headeronly two sets').sets[0].rows.map(r => r[5]), [1, 2], 'positions changed')
  expect('restore missing position', [[3287, 1], [3013, 1]])
  expect('restore in use by this session', [[3102, 1], [3013, 1]])
  expect('restore in use by another session', [[3101, 1], [3013, 1]])
  expect('drop msdb', [[3708, 4]])
  expect('verifyonly', [], [3262])
  expect('backup unknown option', [[155, 1]])
  assert.deepEqual(get('msdb identity').sets[0].rows, [[4, 'msdb']])
}

if (check) {
  const bytes = await readFile(fixture)
  const retained = JSON.parse(bytes)
  assert.equal(createHash('sha256').update(JSON.stringify(retained.observations)).digest('hex'), retained.captureSha256, 'capture checksum changed')
  validate(retained.observations)
  console.log(`Checked ${retained.observations.length} backup and restore observations from ${retained.image}`)
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  await withReferenceContainer(async (config, container) => {
    const observations = await observe(config)
    const actual = { image: container.image, captureSha256: createHash('sha256').update(JSON.stringify(observations)).digest('hex'), observations }
    await writeFile(output, JSON.stringify(actual, null, 1) + '\n')
    validate(observations)
    if (writeFixture) await writeNewFixture(fixture, actual)
    console.log(`Captured ${observations.length} backup and restore observations from ${container.image}`)
  })
}
