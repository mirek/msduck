#!/usr/bin/env node
// First-party SQL Server ROWVERSION/TIMESTAMP rows, descriptors and diagnostics.
import assert from 'node:assert/strict'
import { mkdir, readFile, stat, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/rowversion.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('retained fixture requires four fresh databases')
const output = resolve(positional[0] ?? 'artifacts/compatibility/rowversion/capture.json')

async function rejectFixtureOutput() {
  const fixturePath = fileURLToPath(fixture)
  if (output === fixturePath) throw new Error('output path must not be the retained fixture')
  const info = async path => {
    try { return await stat(path) }
    catch (error) { if (error.code === 'ENOENT') return null; throw error }
  }
  const [outInfo, fixtureInfo] = await Promise.all([info(output), info(fixturePath)])
  if (outInfo && fixtureInfo && outInfo.dev === fixtureInfo.dev && outInfo.ino === fixtureInfo.ino) {
    throw new Error('output path aliases the retained fixture')
  }
}

function rpc(connection, sql, params) {
  return capture({
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      for (const [name, value] of Object.entries(params)) request.addParameter(name, TYPES.Int, value)
      connection.execSql(request)
    },
  }, sql)
}

async function observe(connection) {
  const records = []
  async function record(name, sql, params) {
    const result = canonical(await (params ? rpc(connection, sql, params) : capture(connection, sql)))
    records.push({ name, sql, ...(params ? { params } : {}), result })
    return result
  }
  await record('initial dbts', 'SELECT @@DBTS AS dbts')
  await record('create rowversion table', 'CREATE TABLE dbo.rv_a(id INT PRIMARY KEY, v INT, rv ROWVERSION)')
  await record('create timestamp table', 'CREATE TABLE dbo.rv_b(id INT PRIMARY KEY, rv TIMESTAMP)')
  await record('create nullable rowversion table', 'CREATE TABLE dbo.rv_nullable(id INT PRIMARY KEY, rv ROWVERSION NULL)')
  await record('create unnamed timestamp column', 'CREATE TABLE dbo.rv_legacy(id INT PRIMARY KEY, timestamp)')
  await record('declared column catalog', `SELECT OBJECT_NAME(c.object_id) AS table_name,c.name,t.name AS type_name,c.max_length,c.is_nullable,c.is_identity FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id IN (OBJECT_ID('dbo.rv_a'),OBJECT_ID('dbo.rv_b'),OBJECT_ID('dbo.rv_nullable'),OBJECT_ID('dbo.rv_legacy')) ORDER BY table_name,c.column_id`)
  await record('dbts after DDL', 'SELECT @@DBTS AS dbts')
  await record('insert first', 'INSERT dbo.rv_a(id,v) VALUES(1,10); SELECT id,v,rv,@@DBTS AS dbts FROM dbo.rv_a ORDER BY id')
  await record('insert second table', 'INSERT dbo.rv_b(id) VALUES(1); SELECT id,rv,@@DBTS AS dbts FROM dbo.rv_b ORDER BY id')
  await record('no-op update', 'UPDATE dbo.rv_a SET v=v WHERE id=1; SELECT id,v,rv,@@DBTS AS dbts FROM dbo.rv_a ORDER BY id')
  await record('zero-row update', 'UPDATE dbo.rv_a SET v=99 WHERE id=999; SELECT id,v,rv,@@DBTS AS dbts FROM dbo.rv_a ORDER BY id')
  await record('insert nullable declaration', 'INSERT dbo.rv_nullable(id) VALUES(1); SELECT id,rv,@@DBTS AS dbts FROM dbo.rv_nullable ORDER BY id')
  await record('insert timestamp synonym', 'INSERT dbo.rv_legacy(id) VALUES(1); SELECT *,@@DBTS AS dbts FROM dbo.rv_legacy ORDER BY id')
  await record('cross-table versions', `SELECT 'a' AS table_name,id,rv FROM dbo.rv_a UNION ALL SELECT 'b',id,rv FROM dbo.rv_b UNION ALL SELECT 'nullable',id,rv FROM dbo.rv_nullable ORDER BY table_name,id`)
  await record('explicit insert rejected', 'INSERT dbo.rv_a(id,v,rv) VALUES(2,20,0x0000000000000001)')
  await record('explicit update rejected', 'UPDATE dbo.rv_a SET rv=0x0000000000000001 WHERE id=1')
  await record('dbts after rejected writes', 'SELECT @@DBTS AS dbts; SELECT id,v,rv FROM dbo.rv_a ORDER BY id')
  await record('rollback allocated value', 'BEGIN TRANSACTION; INSERT dbo.rv_a(id,v) VALUES(3,30); SELECT id,rv,@@DBTS AS dbts FROM dbo.rv_a WHERE id=3; ROLLBACK TRANSACTION; SELECT @@DBTS AS dbts')
  await record('after rollback', 'SELECT id,v,rv FROM dbo.rv_a ORDER BY id; SELECT @@DBTS AS dbts')
  await record('insert after rollback', 'INSERT dbo.rv_a(id,v) VALUES(4,40); SELECT id,v,rv,@@DBTS AS dbts FROM dbo.rv_a ORDER BY id')
  await record('rpc read first', 'SELECT id,rv,@@DBTS AS dbts FROM dbo.rv_a WHERE id=@id', { id: 1 })
  await record('rpc no-op update', 'UPDATE dbo.rv_a SET v=v WHERE id=@id; SELECT id,v,rv,@@DBTS AS dbts FROM dbo.rv_a WHERE id=@id', { id: 1 })
  await record('rpc read after update', 'SELECT id,rv,@@DBTS AS dbts FROM dbo.rv_a WHERE id=@id', { id: 1 })
  await record('select into rowversion', 'SELECT id,v,rv INTO dbo.rv_copy FROM dbo.rv_a; SELECT id,v,rv FROM dbo.rv_copy ORDER BY id')
  await record('select into column catalog', `SELECT c.name,t.name AS type_name,c.max_length,c.is_nullable FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('dbo.rv_copy') ORDER BY c.column_id`)
  await record('dbts after select into', 'SELECT @@DBTS AS dbts')
  await record('second rowversion rejected', 'CREATE TABLE dbo.rv_bad_double(id INT, a ROWVERSION, b ROWVERSION)')
  await record('rowversion default rejected', 'CREATE TABLE dbo.rv_bad_default(id INT, rv ROWVERSION DEFAULT 0x0000000000000001)')
  await record('rowversion identity rejected', 'CREATE TABLE dbo.rv_bad_identity(id INT, rv ROWVERSION IDENTITY)')
  await record('rowversion cast', 'SELECT CAST(0x00000000000007D1 AS ROWVERSION) AS cast_value,@@DBTS AS dbts')
  await record('final reuse', 'SELECT @@TRANCOUNT AS tran_count,XACT_STATE() AS xact_state,@@DBTS AS dbts; SELECT 1 AS reusable')
  return records
}

function validate(run) {
  assert.equal(run.length, 31)
  assert.equal(new Set(run.map(item => item.name)).size, run.length)
  for (const { name, result } of run) assert(result.done.length > 0, `${name}: missing completion`)
  const get = name => run.find(item => item.name === name)?.result
  assert.equal(get('create rowversion table').errors.length, 0)
  assert.equal(get('create timestamp table').errors.length, 0)
  assert.equal(get('insert first').errors.length, 0)
  assert.equal(get('no-op update').errors.length, 0)
  assert.equal(get('after rollback').errors.length, 0)
  assert.equal(get('final reuse').errors.length, 0)
  assert.deepEqual(get('final reuse').sets.at(-1)?.rows, [[1]])
  const version = value => {
    assert.equal(value?.kind, 'binary')
    assert.match(value.value, /^[0-9a-f]{16}$/)
    return BigInt('0x' + value.value)
  }
  const initial = version(get('initial dbts').sets[0].rows[0][0])
  const last = name => version(get(name).sets.at(-1).rows.at(-1).at(-1))
  assert.equal(last('dbts after DDL'), initial)
  assert.equal(last('insert first'), initial + 1n)
  assert.equal(last('insert second table'), initial + 2n)
  assert.equal(last('no-op update'), initial + 3n)
  assert.equal(last('zero-row update'), initial + 3n)
  assert.equal(last('insert nullable declaration'), initial + 4n)
  assert.equal(last('insert timestamp synonym'), initial + 5n)
  assert.equal(version(get('dbts after rejected writes').sets[0].rows[0][0]), initial + 5n)
  assert.equal(last('rollback allocated value'), initial + 6n)
  assert.equal(last('after rollback'), initial + 6n)
  assert.equal(last('insert after rollback'), initial + 7n)
  assert.equal(last('rpc no-op update'), initial + 8n)
  assert.equal(last('dbts after select into'), initial + 8n)
  assert.equal(get('after rollback').sets[0].rows.length, 1)
  assert.equal(get('select into rowversion').sets[0].rows[0][2].value, get('rpc no-op update').sets[0].rows[0][2].value)
  assert.equal(get('select into rowversion').sets[0].columns[2].type, 'Binary')
  assert.equal(get('insert first').sets[0].columns[2].type, 'Binary')
  assert.equal(get('insert first').sets[0].columns[2].length, 8)
  assert.equal(get('insert first').sets[0].columns[3].type, 'VarBinary')
  assert.deepEqual(get('explicit insert rejected').errors.map(error => [error.number,error.state]), [[273,1]])
  assert.deepEqual(get('explicit update rejected').errors.map(error => [error.number,error.state]), [[272,1]])
  assert.deepEqual(get('second rowversion rejected').errors.map(error => [error.number,error.state]), [[2738,2]])
  assert.deepEqual(get('rowversion default rejected').errors.map(error => [error.number,error.state]), [[1755,0],[1750,0]])
  assert.deepEqual(get('rowversion identity rejected').errors.map(error => [error.number,error.state]), [[2749,2]])
}

await rejectFixtureOutput()
if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
  const entry = await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let db = 0; db < (oneDatabase ? 1 : 2); db++) {
      const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, observe)
      runs.push(run)
    }
    return { image: container.image, runs }
  })
  containers.push(entry)
  // Keep every completed raw capture even if a later database disagrees.
  await writeFile(output, JSON.stringify({ containers }) + '\n')
}
const actual = { containers }
for (const container of containers) {
  for (const run of container.runs) validate(run)
  if (container.runs.length === 2) assertSameCapture(container.runs[0], container.runs[1], 'rowversion differs across fresh databases')
}
if (containers.length === 2) assertSameCapture(containers[0].runs[0], containers[1].runs[0], 'rowversion differs across fresh containers')
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (retained) {
  if (oneDatabase) assertSameCapture(actual.containers[0].runs[0], retained.containers[0].runs[0], 'rowversion differs from retained fixture')
  else assertSameCapture(actual, retained, 'rowversion differs from retained fixture')
}
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${containers[0].runs[0].length} rowversion observations in ${containers.length * containers[0].runs.length} fresh databases${retained ? ' and matched retained fixture' : ''}`)
