#!/usr/bin/env node
// Independent SQL Server evidence for statements a tedious-based test suite
// sends (issue #697): current-time functions, sys.server_principals with
// SUSER_SNAME(), computed columns, CLUSTERED/NONCLUSTERED key constraints,
// table hints, and ALTER DATABASE SET READ_COMMITTED_SNAPSHOT without a
// termination clause while other sessions use the database.
//
// Every observation runs in a fresh pinned container against fixed object
// names, so messages are stable. Clock values are reduced to descriptors and
// to comparisons made inside the server; login SIDs and creation dates are
// reduced to their types.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, realpath, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { assertSameCapture, connect, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/tedious-compat-gaps.json', import.meta.url)
const fixtureSha256 = 'a5cef87b425fc4eba62fb376edfb6fb9e1484f91f71bba15ff8324728297ef90'
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-tedious-compat-gaps.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/tedious-compat-gaps-reference/capture.json')

// DONE bodies (TDS 7.2+: USHORT status, USHORT CurCmd, ULONGLONG row count)
// are recorded verbatim before tedious parses them. The parser's options
// object belongs to one connection, so concurrent connections keep separate
// sinks.
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

// One batch with its descriptors, rows, messages and raw DONE tokens. With a
// bound, a batch still running after `bound` milliseconds is cancelled and
// reported as waiting; only the fact that it waited is recorded.
function run(connection, sql, bound) {
  return new Promise(resolve => {
    const result = { sets: [], done: [], errors: [], info: [] }
    const done = []
    let timer
    sinks.set(connection.config.options, done)
    const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
    const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
    const finish = error => {
      clearTimeout(timer)
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
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: c.collation ?? null })), rows: [],
    }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => c.value)))
    if (bound) timer = setTimeout(() => { result.waited = true; connection.cancel() }, bound)
    try { connection.execSqlBatch(request) } catch (error) { finish(error) }
  })
}

function open(config, database, login) {
  const authentication = login ? { type: 'default', options: login } : config.authentication
  return connect({ ...config, authentication, options: { ...config.options, database, appName: 'msduck-capture', workstationId: 'msduck-host', connectTimeout: 30000 } })
}
function close(connection) {
  if (connection.closed) return Promise.resolve()
  return new Promise(resolve => { connection.once('end', resolve); connection.close() })
}

const declarations = view => `SELECT name,column_id,system_type_id,user_type_id,max_length,precision,scale,collation_name,is_nullable FROM sys.all_columns WHERE object_id=OBJECT_ID(N'${view}') ORDER BY column_id`
const columns = table => `SELECT name,column_id,system_type_id,max_length,precision,scale,is_nullable,is_computed FROM sys.columns WHERE object_id=OBJECT_ID(N'${table}') ORDER BY column_id`
const computed = table => `SELECT name,definition,is_persisted,uses_database_collation FROM sys.computed_columns WHERE object_id=OBJECT_ID(N'${table}') ORDER BY column_id`
// SQL Server appends a random suffix to generated constraint names.
const stable = "CASE WHEN name LIKE 'PK[_][_]%' OR name LIKE 'UQ[_][_]%' THEN LEFT(name, 2) + N'__<generated>' ELSE name END AS name"
const indexes = table => `SELECT ${stable},index_id,type,type_desc,is_unique,is_primary_key,is_unique_constraint FROM sys.indexes WHERE object_id=OBJECT_ID(N'${table}') ORDER BY index_id`
const keyConstraints = table => `SELECT ${stable},type,type_desc,unique_index_id FROM sys.key_constraints WHERE parent_object_id=OBJECT_ID(N'${table}') ORDER BY 1`
// A waiting ALTER DATABASE is cancelled after this bound.
const waitBound = 5000
const probePassword = 'Probe!Login9x'

async function observe(config) {
  const observations = []
  const record = (name, value) => { observations.push({ name, ...value }); console.log(name) }
  const a = await open(config, 'master')
  const on = async (name, connection, sql, bound) => record(name, { sql, result: await run(connection, sql, bound) })
  try {
    await on('server version', a, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version")

    // Current-time functions: descriptors from empty results, then relations
    // computed inside the server. The container runs with TZ=UTC.
    await on('sysdatetimeoffset repro', a, 'select sysdatetimeoffset() where 1=0')
    await on('getutcdate repro', a, 'select getutcdate() where 1=0')
    await on('datediff_big repro', a, "select datediff_big(millisecond, '1970-01-01', getutcdate()) where 1=0")
    await on('current time descriptors', a, 'SELECT SYSDATETIMEOFFSET() AS sysdatetimeoffset, GETUTCDATE() AS getutcdate, SYSUTCDATETIME() AS sysutcdatetime, GETDATE() AS getdate, SYSDATETIME() AS sysdatetime, CURRENT_TIMESTAMP AS current_timestamp_value WHERE 1=0')
    await on('current time base types', a, "SELECT CAST(SQL_VARIANT_PROPERTY(SYSDATETIMEOFFSET(),'BaseType') AS sysname) AS a, CAST(SQL_VARIANT_PROPERTY(SYSDATETIMEOFFSET(),'Scale') AS int) AS a_scale, CAST(SQL_VARIANT_PROPERTY(GETUTCDATE(),'BaseType') AS sysname) AS b, CAST(SQL_VARIANT_PROPERTY(SYSUTCDATETIME(),'BaseType') AS sysname) AS c, CAST(SQL_VARIANT_PROPERTY(SYSUTCDATETIME(),'Scale') AS int) AS c_scale, CAST(SQL_VARIANT_PROPERTY(GETDATE(),'BaseType') AS sysname) AS d, CAST(SQL_VARIANT_PROPERTY(SYSDATETIME(),'BaseType') AS sysname) AS e, CAST(SQL_VARIANT_PROPERTY(CURRENT_TIMESTAMP,'BaseType') AS sysname) AS f")
    await on('current time relations', a, "SELECT DATEPART(TZOFFSET, SYSDATETIMEOFFSET()) AS offset_minutes, CASE WHEN ABS(DATEDIFF_BIG(millisecond, GETUTCDATE(), SYSUTCDATETIME())) < 1000 THEN 1 ELSE 0 END AS utc_agree, CASE WHEN ABS(DATEDIFF_BIG(millisecond, GETDATE(), SYSDATETIME())) < 1000 THEN 1 ELSE 0 END AS local_agree, CASE WHEN ABS(DATEDIFF_BIG(millisecond, CAST(SYSDATETIMEOFFSET() AS datetime2), SYSDATETIME())) < 1000 THEN 1 ELSE 0 END AS offset_local_agree, CASE WHEN CURRENT_TIMESTAMP = GETDATE() THEN 1 ELSE 0 END AS current_timestamp_is_getdate, CASE WHEN datediff_big(millisecond, '1970-01-01', getutcdate()) > 1700000000000 THEN 1 ELSE 0 END AS epoch_milliseconds_plausible")
    await on('getutcdate datetime rounding', a, 'SELECT CASE WHEN DATEPART(millisecond, GETUTCDATE()) % 10 IN (0,3,7) THEN 1 ELSE 0 END AS rounded, CASE WHEN CAST(GETUTCDATE() AS datetime) = GETUTCDATE() THEN 1 ELSE 0 END AS datetime_value')
    await on('getutcdate argument', a, 'SELECT GETUTCDATE(1)')
    await on('sysdatetimeoffset argument', a, 'SELECT SYSDATETIMEOFFSET(1)')
    await on('current_timestamp parentheses', a, 'SELECT CURRENT_TIMESTAMP()')

    // sys.server_principals and the login's name.
    await on('server_principals declarations', a, declarations('sys.server_principals'))
    await on('server_principals complete empty', a, 'SELECT * FROM sys.server_principals WHERE 1=0')
    await on('server_principals repro', a, 'select name, default_database_name from sys.server_principals where name = suser_sname()')
    await on('sa principal', a, "SELECT name,principal_id,type,type_desc,is_disabled,default_database_name,default_language_name,credential_id,owning_principal_id,is_fixed_role,CAST(SQL_VARIANT_PROPERTY(sid,'BaseType') AS sysname) AS sid_type,DATALENGTH(sid) AS sid_bytes,CASE WHEN create_date<=modify_date THEN 1 ELSE 0 END AS dates_ordered FROM sys.server_principals WHERE name=N'sa'")
    await on('suser_sname descriptors', a, 'SELECT SUSER_SNAME() AS suser_sname, SUSER_NAME() AS suser_name, SYSTEM_USER AS system_user_value, ORIGINAL_LOGIN() AS original_login WHERE 1=0')
    await on('suser_sname values', a, 'SELECT SUSER_SNAME() AS suser_sname, SUSER_NAME() AS suser_name, SYSTEM_USER AS system_user_value, ORIGINAL_LOGIN() AS original_login')
    await on('suser_sname unnamed', a, 'select suser_sname()')
    await on('suser_sname of sid', a, 'SELECT SUSER_SNAME(0x01) AS sa_by_sid, SUSER_SNAME(SUSER_SID()) AS own_by_sid, SUSER_SNAME(0x0102030405) AS unknown_sid')
    await on('create probe database', a, 'CREATE DATABASE [probe_db]')
    record('create probe login', { sql: 'CREATE LOGIN [probe_login] WITH PASSWORD=N<redacted>, DEFAULT_DATABASE=[probe_db], CHECK_POLICY=OFF', result: await run(a, `CREATE LOGIN [probe_login] WITH PASSWORD=N'${probePassword}', DEFAULT_DATABASE=[probe_db], CHECK_POLICY=OFF`) })
    {
      const login = await open(config, 'master', { userName: 'probe_login', password: probePassword })
      await on('probe login repro', login, 'select name, default_database_name from sys.server_principals where name = suser_sname()')
      await on('probe login visible principals', login, "SELECT name,type_desc FROM sys.server_principals WHERE type IN ('S','U','G') ORDER BY principal_id")
      await on('probe login names', login, 'SELECT SUSER_SNAME() AS suser_sname, SYSTEM_USER AS system_user_value, DB_NAME() AS database_name')
      await close(login)
    }
    await on('sa visible principal', a, "SELECT name,type_desc,default_database_name FROM sys.server_principals WHERE name IN (N'sa',N'probe_login') ORDER BY principal_id")
    await on('drop probe login', a, 'DROP LOGIN [probe_login]')

    // Computed columns: the report's DDL, then reads, writes and catalog.
    const db = await open(config, 'probe_db')
    await on('computed repro table', db, 'create table ComputedProbe (\n  id int not null identity(1, 1) primary key,\n  name varchar(100) null,\n  nameUpper as upper(name) persisted\n);')
    await on('computed repro index', db, 'create index IX_ComputedProbe_nameUpper on ComputedProbe (nameUpper);')
    await on('computed insert', db, "INSERT INTO ComputedProbe (name) VALUES ('abc'), (NULL), ('Mixed Case')")
    await on('computed insert without column list', db, "INSERT INTO ComputedProbe VALUES ('xyz')")
    await on('computed select', db, 'SELECT * FROM ComputedProbe ORDER BY id')
    await on('computed predicate', db, "SELECT id FROM ComputedProbe WHERE nameUpper = 'ABC'")
    await on('computed update source', db, "UPDATE ComputedProbe SET name = 'changed' WHERE id = 1; SELECT id, name, nameUpper FROM ComputedProbe WHERE id = 1")
    await on('computed insert target', db, "INSERT INTO ComputedProbe (name, nameUpper) VALUES ('a', 'A')")
    await on('computed update target', db, "UPDATE ComputedProbe SET nameUpper = 'X'")
    await on('computed columns catalog', db, columns('ComputedProbe'))
    await on('computed_columns catalog', db, computed('ComputedProbe'))
    await on('computed indexes catalog', db, indexes('ComputedProbe'))
    await on('computed columnproperty', db, "SELECT COLUMNPROPERTY(OBJECT_ID(N'ComputedProbe'),'nameUpper','IsComputed') AS is_computed, COLUMNPROPERTY(OBJECT_ID(N'ComputedProbe'),'name','IsComputed') AS plain")
    await on('computed virtual table', db, 'CREATE TABLE ComputedVirtual (a int NOT NULL, b int NULL, total AS a + b, doubled AS (a * 2), label AS CAST(a AS varchar(10)) + \'x\')')
    await on('computed virtual insert', db, 'INSERT INTO ComputedVirtual (a, b) VALUES (1, 2), (3, NULL)')
    await on('computed virtual select', db, 'SELECT * FROM ComputedVirtual ORDER BY a')
    await on('computed virtual columns catalog', db, columns('ComputedVirtual'))
    await on('computed virtual computed_columns', db, computed('ComputedVirtual'))
    await on('computed virtual index', db, 'CREATE INDEX IX_ComputedVirtual_total ON ComputedVirtual (total)')
    await on('computed persisted not null', db, 'CREATE TABLE ComputedNotNull (a int NOT NULL, b AS a + 1 PERSISTED NOT NULL); INSERT INTO ComputedNotNull (a) VALUES (5); SELECT * FROM ComputedNotNull')
    await on('computed persisted not null catalog', db, columns('ComputedNotNull'))
    await on('computed nondeterministic persisted', db, 'CREATE TABLE ComputedClock (a int, b AS GETDATE() PERSISTED)')
    await on('computed nondeterministic virtual', db, 'CREATE TABLE ComputedClockVirtual (a int, b AS GETDATE()); SELECT name, is_computed FROM sys.columns WHERE object_id=OBJECT_ID(N\'ComputedClockVirtual\') ORDER BY column_id')
    await on('computed nondeterministic index', db, 'CREATE INDEX IX_ComputedClockVirtual_b ON ComputedClockVirtual (b)')
    await on('computed references computed', db, 'CREATE TABLE ComputedChain (a int, b AS a + 1, c AS b + 1)')
    await on('computed unknown column', db, 'CREATE TABLE ComputedUnknown (a int, b AS missing + 1)')

    // CLUSTERED/NONCLUSTERED on PRIMARY KEY and UNIQUE constraints.
    await on('nonclustered repro', db, 'create table NonclusteredProbe (\n  content nvarchar(max) not null,\n  [version] varchar(64) not null\n    constraint NonclusteredProbe_Pk primary key nonclustered\n);')
    await on('nonclustered repro indexes', db, indexes('NonclusteredProbe'))
    await on('nonclustered repro constraints', db, keyConstraints('NonclusteredProbe'))
    await on('nonclustered repro use', db, "INSERT INTO NonclusteredProbe (content, [version]) VALUES (N'a', '1'); INSERT INTO NonclusteredProbe (content, [version]) VALUES (N'b', '1')")
    await on('column clustered keys', db, 'CREATE TABLE ClusteredColumn (id int NOT NULL PRIMARY KEY CLUSTERED, code int NULL UNIQUE NONCLUSTERED, other int NULL CONSTRAINT UQ_ClusteredColumn_other UNIQUE)')
    await on('column clustered keys indexes', db, indexes('ClusteredColumn'))
    await on('column unique clustered', db, 'CREATE TABLE UniqueClustered (id int NOT NULL PRIMARY KEY NONCLUSTERED, code int NOT NULL UNIQUE CLUSTERED)')
    await on('column unique clustered indexes', db, indexes('UniqueClustered'))
    await on('table clustered keys', db, 'CREATE TABLE ClusteredTable (a int NOT NULL, b int NOT NULL, c int NULL, CONSTRAINT PK_ClusteredTable PRIMARY KEY NONCLUSTERED (a, b DESC), CONSTRAINT UQ_ClusteredTable_c UNIQUE CLUSTERED (c ASC))')
    await on('table clustered keys indexes', db, indexes('ClusteredTable'))
    await on('table clustered keys constraints', db, keyConstraints('ClusteredTable'))
    await on('table unnamed clustered key', db, 'CREATE TABLE ClusteredUnnamed (a int NOT NULL, PRIMARY KEY CLUSTERED (a ASC))')
    await on('table key with options', db, 'CREATE TABLE ClusteredOptions (a int NOT NULL, CONSTRAINT PK_ClusteredOptions PRIMARY KEY CLUSTERED (a) WITH (FILLFACTOR = 90))')
    await on('two clustered', db, 'CREATE TABLE TwoClustered (a int NOT NULL PRIMARY KEY CLUSTERED, b int NOT NULL UNIQUE CLUSTERED)')
    await on('alter add nonclustered key', db, 'CREATE TABLE AlterKey (a int NOT NULL, b int NULL); ALTER TABLE AlterKey ADD CONSTRAINT PK_AlterKey PRIMARY KEY NONCLUSTERED (a); ALTER TABLE AlterKey ADD CONSTRAINT UQ_AlterKey_b UNIQUE CLUSTERED (b)')
    await on('alter add nonclustered key indexes', db, indexes('AlterKey'))
    await on('create clustered index', db, 'CREATE TABLE ClusteredIndex (a int NOT NULL, b int NULL); CREATE CLUSTERED INDEX CIX_ClusteredIndex ON ClusteredIndex (a); CREATE NONCLUSTERED INDEX IX_ClusteredIndex_b ON ClusteredIndex (b); CREATE UNIQUE NONCLUSTERED INDEX UX_ClusteredIndex_ab ON ClusteredIndex (a, b)')
    await on('create clustered index indexes', db, indexes('ClusteredIndex'))

    // Table hints.
    await on('hint repro', db, 'create table HintProbe (id int not null primary key);\nselect max(id) from HintProbe with (readcommittedlock);')
    await on('hint rows', db, 'INSERT INTO HintProbe VALUES (1), (2), (3)')
    for (const hint of ['nolock', 'readcommittedlock', 'updlock', 'rowlock', 'holdlock', 'readuncommitted', 'readcommitted', 'repeatableread', 'serializable', 'tablock', 'tablockx', 'paglock', 'xlock', 'readpast', 'nowait', 'forceseek', 'forcescan', 'nolock, index(0)', 'index(1)', 'index = 1', 'updlock, rowlock, holdlock', 'noexpand', 'snapshot']) {
      await on(`hint ${hint}`, db, `SELECT COUNT(*) AS n FROM HintProbe WITH (${hint})`)
    }
    await on('hint legacy without with', db, 'SELECT COUNT(*) AS n FROM HintProbe (NOLOCK)')
    await on('hint alias', db, 'SELECT COUNT(*) AS n FROM HintProbe AS h WITH (NOLOCK) JOIN HintProbe g WITH (NOLOCK) ON g.id = h.id')
    await on('hint subquery and cte', db, 'WITH c AS (SELECT id FROM HintProbe WITH (NOLOCK)) SELECT COUNT(*) AS n FROM c WHERE id IN (SELECT id FROM HintProbe WITH (READCOMMITTEDLOCK))')
    await on('hint update', db, 'UPDATE HintProbe WITH (ROWLOCK) SET id = id WHERE id = 1')
    await on('hint delete', db, 'DELETE FROM HintProbe WITH (ROWLOCK, READPAST) WHERE id = 3')
    await on('hint insert', db, 'INSERT INTO HintProbe WITH (TABLOCK) (id) VALUES (3)')
    await on('hint update from', db, 'UPDATE h SET id = h.id FROM HintProbe h WITH (UPDLOCK) WHERE h.id = 2')
    await on('hint nolock update target', db, 'UPDATE HintProbe WITH (NOLOCK) SET id = id')
    await on('hint unknown', db, 'SELECT COUNT(*) AS n FROM HintProbe WITH (bogus)')
    await on('hint conflicting', db, 'SELECT COUNT(*) AS n FROM HintProbe WITH (NOLOCK, SERIALIZABLE)')
    await on('hint missing index', db, 'SELECT COUNT(*) AS n FROM HintProbe WITH (INDEX(IX_Missing))')
    await on('hint in transaction', db, 'BEGIN TRANSACTION; SELECT COUNT(*) AS n FROM HintProbe WITH (UPDLOCK, HOLDLOCK); COMMIT')
    await close(db)

    // ALTER DATABASE without a termination clause while other sessions use
    // the database: to the current value, then to a new value.
    await on('rcsi on with rollback immediate', a, 'alter database probe_db set read_committed_snapshot on with rollback immediate;')
    {
      const own = await open(config, 'probe_db')
      const b = await open(config, 'probe_db')
      const c = await open(config, 'probe_db')
      await on('others connected', a, "SELECT COUNT(*) AS sessions FROM sys.dm_exec_sessions WHERE database_id=DB_ID(N'probe_db') AND is_user_process=1")
      await on('current unchanged rcsi', own, 'alter database current set read_committed_snapshot on;', waitBound)
      await on('named unchanged rcsi', a, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON', waitBound)
      await on('unchanged rcsi no_wait', a, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON WITH NO_WAIT')
      await on('unchanged multi_user', a, 'ALTER DATABASE [probe_db] SET MULTI_USER', waitBound)
      await on('unchanged multi_user no_wait', a, 'ALTER DATABASE [probe_db] SET MULTI_USER WITH NO_WAIT')
      await on('unchanged rcsi and multi_user', a, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON, MULTI_USER', waitBound)
      await on('changed rcsi waits', own, 'ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT OFF', waitBound)
      await on('changed rcsi state', a, "SELECT is_read_committed_snapshot_on, user_access_desc FROM sys.databases WHERE name=N'probe_db'")
      await on('changed rcsi no_wait', a, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT OFF WITH NO_WAIT')
      await on('others still connected', a, "SELECT COUNT(*) AS sessions FROM sys.dm_exec_sessions WHERE database_id=DB_ID(N'probe_db') AND is_user_process=1")
      await on('other session usable', b, 'SELECT DB_NAME() AS database_name')
      await close(own)
      await close(b)
      await close(c)
    }
    await on('rcsi off alone', a, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT OFF')
    await on('unchanged rcsi off alone', a, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT OFF')
    await on('drop probe database', a, 'DROP DATABASE [probe_db]')
    await on('session reusable', a, 'SELECT @@TRANCOUNT AS trancount, DB_NAME() AS database_name')
  } catch (error) {
    await writeFile(`${output}.partial.json`, JSON.stringify(observations, null, 1) + '\n')
    throw error
  } finally { await close(a) }
  validate(observations)
  return observations
}

const numbers = messages => messages.map(m => m.number)
function validate(run) {
  const get = name => {
    const found = run.find(item => item.name === name)
    assert(found, `${name}: missing observation`)
    return found.result ?? found
  }
  const expect = (name, errors) => assertSameCapture(numbers(get(name).errors), errors, `${name}: errors changed`)
  assertSameCapture(get('server version').sets[0].rows, [['17.0.4065.4']], 'reference image version changed')
  for (const name of ['sysdatetimeoffset repro', 'getutcdate repro', 'datediff_big repro', 'server_principals repro', 'computed repro table', 'computed repro index', 'nonclustered repro', 'hint repro']) expect(name, [])
  assertSameCapture(get('server_principals repro').sets[0].rows, [['sa', 'master']], 'repro principal changed')
  assertSameCapture(get('current time relations').sets[0].rows, [[0, 1, 1, 1, 1, 1]], 'current time relations changed')
  const types = name => get(name).sets[0].columns.map(c => [c.type, c.scale])
  assertSameCapture(types('current time descriptors'), [['DateTimeOffset', 7], ['DateTime', null], ['DateTime2', 7], ['DateTime', null], ['DateTime2', 7], ['DateTime', null]], 'current time descriptors changed')
  assertSameCapture(get('getutcdate datetime rounding').sets[0].rows, [[1, 1]], 'GETUTCDATE rounding changed')
  for (const name of ['getutcdate argument', 'sysdatetimeoffset argument']) expect(name, [174])
  expect('current_timestamp parentheses', [102])
  assert.equal(get('server_principals declarations').sets[0].rows.length, 14, 'sys.server_principals schema changed')
  assertSameCapture(get('suser_sname values').sets[0].rows, [['sa', 'sa', 'sa', 'sa']], 'login names changed')
  assertSameCapture(get('probe login repro').sets[0].rows, [['probe_login', 'probe_db']], 'probe login principal changed')
  assertSameCapture(get('computed select').sets[0].rows, [[1, 'abc', 'ABC'], [2, null, null], [3, 'Mixed Case', 'MIXED CASE'], [4, 'xyz', 'XYZ']], 'computed rows changed')
  for (const name of ['computed insert target', 'computed update target']) expect(name, [271])
  expect('computed nondeterministic persisted', [4936])
  expect('computed nondeterministic index', [2729])
  expect('computed references computed', [1759])
  expect('computed unknown column', [207])
  expect('nonclustered repro use', [2627])
  for (const name of ['column clustered keys', 'column unique clustered', 'table clustered keys', 'table unnamed clustered key', 'table key with options', 'alter add nonclustered key', 'create clustered index']) expect(name, [])
  expect('two clustered', [8112])
  for (const hint of ['nolock', 'readcommittedlock', 'updlock', 'rowlock', 'holdlock', 'legacy without with', 'alias', 'subquery and cte', 'update', 'delete', 'insert', 'update from']) expect(`hint ${hint}`, [])
  expect('hint forceseek', [8622])
  expect('hint noexpand', [8171])
  expect('hint snapshot', [367])
  expect('hint nolock update target', [1065])
  expect('hint unknown', [321])
  expect('hint conflicting', [1047])
  expect('hint missing index', [308])
  assertSameCapture(get('others connected').sets[0].rows, [[3]], 'other sessions missing')
  for (const name of ['current unchanged rcsi', 'named unchanged rcsi', 'unchanged rcsi no_wait', 'unchanged multi_user', 'unchanged multi_user no_wait', 'unchanged rcsi and multi_user']) {
    expect(name, [])
    assert.equal(get(name).waited, undefined, `${name}: waited`)
    assertSameCapture(get(name).done.map(d => [d.status, d.curCmd]), [[0, 215]], `${name}: completion changed`)
  }
  assert.equal(get('changed rcsi waits').waited, true, 'changed value did not wait')
  assertSameCapture(get('changed rcsi state').sets[0].rows, [[true, 'MULTI_USER']], 'cancelled change applied')
  expect('changed rcsi no_wait', [5070, 5069])
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
  console.log(`Checked ${retained.runs[0].length} tedious compatibility observations in two retained runs`)
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
  console.log(`Captured ${runs[0].length} tedious compatibility observations in two fresh containers`)
}
