// BACKUP DATABASE, RESTORE HEADERONLY/FILELISTONLY/DATABASE and msdb history
// through tedious (issue #714). Descriptors and error numbers come from
// reference/gaps-backup.json, captured from SQL Server.
import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'
import { Request } from 'tedious'
import { start } from '../support/client.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/gaps-backup.json', import.meta.url)))
const observed = name => reference.observations.find(o => o.name === name).result
// Known difference: ordinary query results encode numeric declarations as
// DECIMALN (0x6A), the shared msduck-tds encoder's only decimal type, where
// SQL Server sends NUMERICN (0x6C). RESTORE HEADERONLY and FILELISTONLY
// write their own descriptors and match exactly.
const engineDescriptors = name => observed(name).sets[0].columns.map(c => c.type === 'NumericN' ? { ...c, type: 'DecimalN' } : c)

// One batch: result sets with descriptors, INFO and ERROR messages.
function batch(connection, sql) {
  return new Promise(resolve => {
    const result = { sets: [], errors: [], info: [] }
    const onError = e => result.errors.push([e.number, e.state])
    const onInfo = e => { if (![5701, 5703].includes(e.number)) result.info.push(e.number) }
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    const request = new Request(sql, () => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      resolve(result)
    })
    request.on('columnMetadata', metadata => result.sets.push({
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags })),
      rows: [],
    }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => c.value)))
    connection.execSqlBatch(request)
  })
}

test('backups restore into clones with SQL Server headers, files and history', { timeout: 120000 }, async t => {
  const directory = mkdtempSync(join(tmpdir(), 'msduck-backup-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const device = join(directory, 'foo.bak')
  const connection = await start(t, { options: { requestTimeout: 60000 } })
  const ok = async sql => {
    const result = await batch(connection, sql)
    assert.deepEqual(result.errors, [], sql)
    return result
  }
  await ok('CREATE DATABASE foo')
  await ok("USE foo; CREATE TABLE dbo.t(id int IDENTITY(1,1) PRIMARY KEY, v nvarchar(20) NOT NULL); INSERT dbo.t(v) VALUES (N'a'),(N'b'); USE master")

  // The workload's statements, as SQL Server answers them.
  let result = await ok(`BACKUP DATABASE foo TO DISK=N'${device}' WITH FORMAT, NAME=N'foo', COMPRESSION;`)
  assert.deepEqual(result.info, [4035, 4035, 3014])

  result = await ok(`RESTORE HEADERONLY FROM DISK=N'${device}';`)
  assert.deepEqual(result.sets[0].columns, observed('headeronly').sets[0].columns)
  const header = Object.fromEntries(result.sets[0].columns.map((c, i) => [c.name, result.sets[0].rows[0][i]]))
  assert.equal(result.sets[0].rows.length, 1)
  for (const [name, value] of Object.entries({
    BackupName: 'foo', BackupDescription: null, BackupType: 1, Compressed: 1, Position: 1, DeviceType: 2,
    UserName: 'sa', DatabaseName: 'foo', CompatibilityLevel: 160, Collation: 'SQL_Latin1_General_CP1_CI_AS',
    IsCopyOnly: false, HasBackupChecksums: false, RecoveryModel: 'SIMPLE', BackupTypeDescription: 'Database',
    Flags: 512, CompressionAlgorithm: 'MS_XPRESS',
  })) assert.deepEqual(header[name], value, name)
  assert.match(header.FamilyGUID, /^[0-9A-F-]{36}$/)
  assert.ok(header.BackupStartDate instanceof Date)

  result = await ok(`RESTORE FILELISTONLY FROM DISK=N'${device}';`)
  assert.deepEqual(result.sets[0].columns, observed('filelistonly').sets[0].columns)
  assert.deepEqual(result.sets[0].rows.map(r => [r[0], r[2], r[3], r[6]]), [['foo', 'D', 'PRIMARY', '1'], ['foo_log', 'L', null, '2']])

  result = await ok(`RESTORE DATABASE foo_copy FROM DISK=N'${device}' WITH REPLACE, MOVE N'foo' TO N'/var/opt/mssql/data/foo_copy.mdf', MOVE N'foo_log' TO N'/var/opt/mssql/data/foo_copy.ldf', RECOVERY, STATS=50;`)
  assert.deepEqual(result.info, [3211, 3211, 4035, 4035, 3014])
  result = await ok('USE foo_copy; SELECT id, v FROM dbo.t ORDER BY id; SELECT file_id, name, physical_name FROM sys.database_files ORDER BY file_id; USE master')
  assert.deepEqual(result.sets[0].rows, [[1, 'a'], [2, 'b']])
  assert.deepEqual(result.sets[1].rows, [[1, 'foo', '/var/opt/mssql/data/foo_copy.mdf'], [2, 'foo_log', '/var/opt/mssql/data/foo_copy.ldf']])

  // msdb history, with SQL Server's descriptors.
  result = await ok('SELECT TOP(1) * FROM msdb..backupmediafamily')
  assert.deepEqual(result.sets[0].columns, engineDescriptors('msdb backupmediafamily'))
  assert.deepEqual(result.sets[0].rows[0].slice(3), [1, null, device, 2, 4096, 0])
  result = await ok('SELECT * FROM msdb.dbo.restorehistory')
  assert.deepEqual(result.sets[0].columns, engineDescriptors('msdb restorehistory'))
  assert.deepEqual(result.sets[0].rows.map(r => [r[2], r[3], r[4], r[5], r[6], r[7]]), [['foo_copy', 'sa', 1, 'D', true, true]])
  result = await ok('SELECT TOP(1) * FROM msdb.dbo.backupset')
  assert.deepEqual(result.sets[0].columns, engineDescriptors('msdb backupset'))
  result = await ok('SELECT TOP(1) * FROM msdb.dbo.backupmediaset')
  assert.deepEqual(result.sets[0].columns, engineDescriptors('msdb backupmediaset'))
  result = await ok("SELECT DB_ID('msdb') AS id, DB_NAME(4) AS name")
  assert.deepEqual(result.sets[0].rows, [[4, 'msdb']])
  result = await ok("SELECT name FROM sys.master_files WHERE database_id = DB_ID('foo_copy') ORDER BY file_id")
  assert.deepEqual(result.sets[0].rows, [['foo'], ['foo_log']])

  // SQL Server's errors.
  await ok('CREATE DATABASE bar')
  const intoBar = `RESTORE DATABASE bar FROM DISK=N'${device}' WITH MOVE N'foo' TO N'/var/opt/mssql/data/bar.mdf', MOVE N'foo_log' TO N'/var/opt/mssql/data/bar.ldf'`
  for (const [sql, name] of [
    [intoBar, 'restore other database without replace'],
    [`RESTORE DATABASE foo_x FROM DISK=N'${join(directory, 'nope.bak')}'`, 'restore missing file'],
    [`RESTORE DATABASE foo_y FROM DISK=N'${device}'`, 'restore without move onto files in use'],
    [`RESTORE DATABASE foo_z FROM DISK=N'${device}' WITH MOVE N'nope' TO N'/var/opt/mssql/data/z.mdf'`, 'restore unknown logical file'],
    ["BACKUP DATABASE nosuch TO DISK=N'/tmp/x.bak'", 'backup unknown database'],
    [`BACKUP LOG bar TO DISK=N'${join(directory, 'log.bak')}'`, 'backup log simple'],
    // SQL Server continues the batch after the error and runs ROLLBACK;
    // msduck ends the batch at a failed statement, so ROLLBACK runs next.
    [`BEGIN TRAN; BACKUP DATABASE foo TO DISK=N'${join(directory, 'x.bak')}'`, 'backup in transaction'],
    ['ROLLBACK; DROP DATABASE msdb', 'drop msdb'],
  ]) {
    result = await batch(connection, sql)
    assert.deepEqual(result.errors, observed(name).errors.map(e => [e.number, e.state]), name)
  }
  // REPLACE overwrites the other database.
  await ok(`${intoBar}, REPLACE`)
  result = await ok('USE bar; SELECT COUNT(*) FROM dbo.t; USE master')
  assert.deepEqual(result.sets[0].rows, [[2]])
})
