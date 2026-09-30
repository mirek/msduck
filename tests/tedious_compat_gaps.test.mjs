// Statements a tedious-based suite sends that msduck rejected (issue #697):
// current-time functions, sys.server_principals with SUSER_SNAME(), computed
// columns, CLUSTERED/NONCLUSTERED keys, table hints and ALTER DATABASE to an
// unchanged value while other sessions use the database. Expected results come
// from reference/tedious-compat-gaps.json (SQL Server 2025, see
// docs/tedious-compat-gaps-reference.md).
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { Connection, Request } from 'tedious'
import { start } from './support/client.mjs'

// The reference container runs with TZ=UTC; the server inherits this.
process.env.TZ = 'UTC'

const reference = JSON.parse(readFileSync(new URL('../reference/tedious-compat-gaps.json', import.meta.url), 'utf8')).runs[0]
const observed = name => {
  const found = reference.find(item => item.name === name)
  assert(found, `missing reference observation ${name}`)
  return found.result ?? found
}
const sqlOf = name => reference.find(item => item.name === name).sql
const messages = list => list.map(m => [m.number, m.state, m.class, m.message])
const client = { appName: 'msduck-capture', workstationId: 'msduck-host' }

// One batch: rows, descriptors and ERROR tokens. Never rejects.
function run(connection, sql) {
  return new Promise(resolve => {
    const result = { rows: [], columns: [], errors: [] }
    const onError = m => result.errors.push(m)
    connection.on('errorMessage', onError)
    const request = new Request(sql, error => {
      connection.off('errorMessage', onError)
      if (error && !result.errors.length) result.transport = error.message
      result.errors = messages(result.errors)
      resolve(result)
    })
    request.on('columnMetadata', columns => result.columns.push(columns.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, scale: c.scale ?? null, flags: c.flags }))))
    request.on('row', cells => result.rows.push(cells.map(cell => cell.value)))
    try { connection.execSqlBatch(request) } catch (error) { result.transport = error.message; resolve(result) }
  })
}
async function open(c, database) {
  const connection = new Connection({ ...c.config, options: { ...c.config.options, database, ...client } })
  connection.on('error', () => {})
  await new Promise((resolve, reject) => connection.connect(error => error ? reject(error) : resolve()))
  return connection
}
const descriptors = name => observed(name).sets.map(set => set.columns.map(({ name, type, length, scale, flags }) => ({ name, type, length, scale, flags })))
const ok = async (c, sql) => {
  const result = await run(c, sql)
  assert.deepEqual(result.errors, [], sql)
  assert.equal(result.transport, undefined, sql)
  return result
}
const fails = async (c, sql, name) => assert.deepEqual((await run(c, sql)).errors, messages(observed(name).errors), sql)

test('the reported statements succeed', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  for (const sql of [
    'select sysdatetimeoffset()',
    'select getutcdate()',
    "select datediff_big(millisecond, '1970-01-01', getutcdate())",
    'select name, default_database_name from sys.server_principals where name = suser_sname()',
    'create table ComputedProbe (id int not null identity(1, 1) primary key, name varchar(100) null, nameUpper as upper(name) persisted)',
    'create index IX_ComputedProbe_nameUpper on ComputedProbe (nameUpper)',
    'create table NonclusteredProbe (content nvarchar(max) not null, [version] varchar(64) not null constraint NonclusteredProbe_Pk primary key nonclustered)',
    'create table HintProbe (id int not null primary key)',
    'select max(id) from HintProbe with (readcommittedlock)',
  ]) await ok(c, sql)
})

test('current-time functions', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  // datetime2 and datetimeoffset results do not carry the computed flag
  // (flags 0 instead of 32); datetime results match exactly.
  const shape = columns => columns.map(({ flags, ...rest }) => ({ ...rest, nullable: flags & 1 }))
  const expected = descriptors('current time descriptors')[0]
  const actual = (await ok(c, sqlOf('current time descriptors'))).columns[0]
  assert.deepEqual(shape(actual), shape(expected))
  assert.deepEqual(actual.filter(c => c.type === 'DateTime'), expected.filter(c => c.type === 'DateTime'))
  assert.deepEqual((await ok(c, sqlOf('getutcdate repro'))).columns, descriptors('getutcdate repro'))
  assert.deepEqual((await ok(c, sqlOf('current time relations'))).rows, observed('current time relations').sets[0].rows)
  assert.deepEqual((await ok(c, sqlOf('getutcdate datetime rounding'))).rows, observed('getutcdate datetime rounding').sets[0].rows)
  for (const name of ['getutcdate argument', 'sysdatetimeoffset argument', 'current_timestamp parentheses']) await fails(c, sqlOf(name), name)
})

test('sys.server_principals and SUSER_SNAME', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  for (const name of ['server_principals complete empty', 'server_principals repro', 'suser_sname values', 'suser_sname unnamed']) {
    const result = await ok(c, sqlOf(name))
    assert.deepEqual(result.columns, descriptors(name), name)
    assert.deepEqual(result.rows, observed(name).sets[0].rows, name)
  }
  const sid = await ok(c, 'SELECT SUSER_SNAME(0x01) AS sa_by_sid, SUSER_SNAME(0x0102030405) AS unknown_sid')
  assert.deepEqual(sid.rows, [['sa', null]])
  const principal = await ok(c, "SELECT name,principal_id,type,type_desc,is_disabled,default_database_name,default_language_name,credential_id,owning_principal_id,is_fixed_role,DATALENGTH(sid) AS sid_bytes FROM sys.server_principals WHERE name=N'sa'")
  const sa = observed('sa principal').sets[0].rows[0]
  assert.deepEqual(principal.rows, [[...sa.slice(0, 10), sa[11]]])

  // Another login appears with its own name.
  const other = new Connection({ ...c.config, authentication: { type: 'default', options: { userName: 'probe_login', password: 'development' } }, options: { ...c.config.options, ...client } })
  other.on('error', () => {})
  await new Promise((resolve, reject) => other.connect(error => error ? reject(error) : resolve()))
  t.after(() => other.close())
  const own = await ok(other, sqlOf('probe login repro'))
  // msduck has no login catalog, so the default database is master.
  assert.deepEqual(own.rows, [['probe_login', 'master']])
  assert.deepEqual((await ok(c, "SELECT name,type_desc FROM sys.server_principals WHERE type IN ('S','U','G') ORDER BY principal_id")).rows, observed('probe login visible principals').sets[0].rows)
  // The SID argument binds to the outer row, not to the lookup's own columns.
  assert.deepEqual((await ok(c, 'SELECT name, SUSER_SNAME(sid) AS by_sid FROM sys.server_principals ORDER BY principal_id')).rows, [['sa', 'sa'], ['probe_login', 'probe_login']])
})

test('computed columns', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  for (const name of ['computed repro table', 'computed repro index', 'computed insert', 'computed insert without column list']) await ok(c, sqlOf(name))
  for (const name of ['computed select', 'computed predicate', 'computed update source']) {
    const result = await ok(c, sqlOf(name))
    assert.deepEqual(result.rows, observed(name).sets.at(-1).rows, name)
    assert.deepEqual(result.columns.at(-1), descriptors(name).at(-1), name)
  }
  for (const name of ['computed insert target', 'computed update target']) await fails(c, sqlOf(name), name)
  assert.deepEqual((await ok(c, sqlOf('computed columns catalog'))).rows, observed('computed columns catalog').sets[0].rows)
  assert.deepEqual((await ok(c, sqlOf('computed columnproperty'))).rows, observed('computed columnproperty').sets[0].rows)

  for (const name of ['computed virtual table', 'computed virtual insert', 'computed virtual index']) await ok(c, sqlOf(name))
  const virtual = await ok(c, sqlOf('computed virtual select'))
  assert.deepEqual(virtual.rows, observed('computed virtual select').sets[0].rows)
  assert.deepEqual(virtual.columns, descriptors('computed virtual select'))
  assert.deepEqual((await ok(c, sqlOf('computed virtual columns catalog'))).rows, observed('computed virtual columns catalog').sets[0].rows)
  // PERSISTED NOT NULL: DuckDB cannot constrain a generated column, so the
  // column stays nullable in msduck's metadata.
  assert.deepEqual((await ok(c, sqlOf('computed persisted not null'))).rows, observed('computed persisted not null').sets[0].rows)

  for (const name of ['computed nondeterministic persisted', 'computed references computed', 'computed unknown column']) await fails(c, sqlOf(name), name)
  // A non-persisted non-deterministic column fails explicitly.
  assert.match((await run(c, sqlOf('computed nondeterministic virtual'))).errors[0][3], /unsupported non-deterministic computed column 'b'/)
})

test('CLUSTERED and NONCLUSTERED keys', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  for (const name of ['nonclustered repro', 'column clustered keys', 'column unique clustered', 'table clustered keys', 'table unnamed clustered key']) await ok(c, sqlOf(name))
  const duplicate = await run(c, sqlOf('nonclustered repro use'))
  assert.deepEqual(duplicate.errors.map(e => e[0]), numbers('nonclustered repro use'))
  await ok(c, 'CREATE TABLE ClusteredIndex (a int NOT NULL, b int NULL); CREATE NONCLUSTERED INDEX IX_ClusteredIndex_b ON ClusteredIndex (b); CREATE UNIQUE NONCLUSTERED INDEX UX_ClusteredIndex_ab ON ClusteredIndex (a, b)')
  const keys = await run(c, 'INSERT INTO ClusteredTable (a, b, c) VALUES (1, 1, 1); INSERT INTO ClusteredTable (a, b, c) VALUES (1, 1, 2)')
  assert.equal(keys.errors.length, 1)
})
const numbers = name => observed(name).errors.map(e => e.number)

test('table hints', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  await ok(c, sqlOf('hint repro'))
  await ok(c, sqlOf('hint rows'))
  for (const item of reference.filter(item => item.name.startsWith('hint ') && item.result.errors.length === 0 && item.result.sets.length === 1)) {
    // FORCESEEK is accepted and ignored; SQL Server rejected it here (8622).
    if (['hint repro', 'hint rows', 'hint forceseek'].includes(item.name)) continue
    assert.deepEqual((await ok(c, item.sql)).rows, item.result.sets[0].rows, item.name)
  }
  for (const name of ['hint update', 'hint delete', 'hint insert', 'hint update from']) await ok(c, sqlOf(name))
  assert.deepEqual((await ok(c, sqlOf('hint in transaction'))).rows, observed('hint in transaction').sets[0].rows)
  for (const name of ['hint noexpand', 'hint snapshot', 'hint nolock update target', 'hint unknown', 'hint conflicting']) await fails(c, sqlOf(name), name)
})

test('ALTER DATABASE to an unchanged value does not wait for other sessions', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  await ok(c, 'CREATE DATABASE [probe_db]')
  await ok(c, sqlOf('rcsi on with rollback immediate'))
  const own = await open(c, 'probe_db')
  const others = [await open(c, 'probe_db'), await open(c, 'probe_db')]
  t.after(() => { own.close(); for (const other of others) other.close() })
  assert.deepEqual((await ok(c, sqlOf('others connected'))).rows, observed('others connected').sets[0].rows)
  await ok(own, sqlOf('current unchanged rcsi'))
  for (const name of ['named unchanged rcsi', 'unchanged rcsi no_wait', 'unchanged multi_user', 'unchanged multi_user no_wait', 'unchanged rcsi and multi_user']) await ok(c, sqlOf(name))
  await fails(c, sqlOf('changed rcsi no_wait'), 'changed rcsi no_wait')
  assert.deepEqual((await ok(c, sqlOf('changed rcsi state'))).rows, observed('changed rcsi state').sets[0].rows)
  assert.deepEqual((await ok(c, sqlOf('others still connected'))).rows, observed('others still connected').sets[0].rows)
  assert.deepEqual((await ok(others[0], sqlOf('other session usable'))).rows, observed('other session usable').sets[0].rows)
})
