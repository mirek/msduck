#!/usr/bin/env node
// Owner-controlled SQL Server evidence for temporary-table lifetime and visibility.
import assert from 'node:assert/strict'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/temp-table-scope.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/temp-table-scope/capture.json')

async function canonicalTarget(path) {
  let current = path
  const missing = []
  while (true) {
    try { return resolve(await realpath(current), ...missing.reverse()) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    const parent = dirname(current)
    if (parent === current) throw Error('cannot resolve capture output path')
    missing.push(basename(current))
    current = parent
  }
}

async function assertSeparateOutput() {
  for (let part = output; ; part = dirname(part)) {
    let info
    try { info = await lstat(part) } catch (error) { if (error.code !== 'ENOENT') throw error }
    assert.ok(!info?.isSymbolicLink(), 'capture output path contains a symlink')
    if (dirname(part) === part) break
  }
  const retained = fileURLToPath(fixture)
  assert.notEqual(await canonicalTarget(output), await canonicalTarget(retained), 'capture output aliases retained fixture')
  let outputFile, retainedFile
  try { outputFile = await stat(output) } catch (error) { if (error.code !== 'ENOENT') throw error }
  try { retainedFile = await stat(retained) } catch (error) { if (error.code !== 'ENOENT') throw error }
  assert.ok(!outputFile || !retainedFile || outputFile.dev !== retainedFile.dev || outputFile.ino !== retainedFile.ino,
    'capture output hard-links retained fixture')
  assert.ok(!outputFile, 'refusing to overwrite capture output')
}

async function close(connection) {
  if (connection.closed) return
  await new Promise(resolve => { connection.once('end', resolve); connection.close() })
}

async function observe(config, primary) {
  const run = []
  const version = canonical(await capture(primary, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"))
  assert.equal(version.errors.length, 0)
  run.push({ name: 'server version', session: 'A', mode: 'batch', result: version })
  const database = canonical(await capture(primary, 'SELECT DB_NAME() AS database_name'))
  assert.equal(database.errors.length, 0)
  const databaseName = database.sets[0].rows[0][0]
  const secondary = await connect({ ...config, options: { ...config.options, database: databaseName } })
  let replacement
  async function record(name, session, sql, mode = 'batch') {
    const connection = session === 'A' ? primary : session === 'B' ? secondary : replacement
    const transport = mode === 'rpc' ? {
      on: (...arguments_) => connection.on(...arguments_),
      off: (...arguments_) => connection.off(...arguments_),
      execSqlBatch: request => connection.execSql(request),
    } : connection
    const result = canonical(await capture(transport, sql))
    run.push({ name, session, mode, sql, result })
    return result
  }
  try {
    await record('local create and initial rows', 'A', "CREATE TABLE #scope_local(id INT NOT NULL, label NVARCHAR(20) NULL, bytes VARBINARY(4) NULL); INSERT #scope_local VALUES(1,N'first',0x00ff); SELECT id,label,bytes FROM #scope_local ORDER BY id")
    await record('local invisible to second session', 'B', "SELECT OBJECT_ID('tempdb..#scope_local') AS object_id; SELECT id,label,bytes FROM #scope_local")
    await record('local survives outer batch', 'A', 'SELECT id,label,bytes FROM #scope_local ORDER BY id')
    await record('nested SQL reads and writes outer local', 'A', "EXEC sp_executesql N'INSERT #scope_local VALUES(2,N''nested'',0x); SELECT id,label,bytes FROM #scope_local ORDER BY id'")
    await record('outer sees nested write', 'A', 'SELECT id,label,bytes FROM #scope_local ORDER BY id')
    await record('nested SQL creates inner local', 'A', "EXEC sp_executesql N'CREATE TABLE #scope_inner(id INT); INSERT #scope_inner VALUES(10); SELECT id FROM #scope_inner'")
    await record('inner local absent after nested SQL', 'A', "SELECT OBJECT_ID('tempdb..#scope_inner') AS object_id; SELECT id FROM #scope_inner")
    await record('RPC reads outer local', 'A', 'SELECT id,label,bytes FROM #scope_local ORDER BY id', 'rpc')
    await record('RPC creates scoped local', 'A', 'CREATE TABLE #scope_rpc(id INT); INSERT #scope_rpc VALUES(20); SELECT id FROM #scope_rpc', 'rpc')
    await record('RPC local absent afterward', 'A', "SELECT OBJECT_ID('tempdb..#scope_rpc') AS object_id; SELECT id FROM #scope_rpc")
    await record('transaction creates local and rolls back', 'A', 'BEGIN TRANSACTION; CREATE TABLE #scope_tx(id INT); INSERT #scope_tx VALUES(30); ROLLBACK TRANSACTION')
    await record('rolled-back local absent', 'A', "SELECT OBJECT_ID('tempdb..#scope_tx') AS object_id; SELECT id FROM #scope_tx")
    await record('transaction updates outer local and rolls back', 'A', "BEGIN TRANSACTION; INSERT #scope_local VALUES(3,N'rolled back',0x01); ROLLBACK TRANSACTION; SELECT id,label,bytes FROM #scope_local ORDER BY id")
    await record('global create', 'A', 'CREATE TABLE ##msduck_scope_probe(id INT NOT NULL); INSERT ##msduck_scope_probe VALUES(40)')
    await record('global visible to second session', 'B', 'SELECT id FROM ##msduck_scope_probe ORDER BY id; INSERT ##msduck_scope_probe VALUES(41)')
    await record('global sees second session write', 'A', 'SELECT id FROM ##msduck_scope_probe ORDER BY id')
    await record('global drop', 'A', 'DROP TABLE ##msduck_scope_probe')
    await record('global absent in second session', 'B', "SELECT OBJECT_ID('tempdb..##msduck_scope_probe') AS object_id; SELECT id FROM ##msduck_scope_probe")
    await record('local drop', 'A', 'DROP TABLE #scope_local')
    await record('local absent after explicit drop', 'A', "SELECT OBJECT_ID('tempdb..#scope_local') AS object_id; SELECT id FROM #scope_local")
    await record('local create before disconnect', 'A', 'CREATE TABLE #scope_disconnect(id INT); INSERT #scope_disconnect VALUES(50)')
    await close(primary)
    replacement = await connect({ ...config, options: { ...config.options, database: databaseName } })
    await record('local absent after reconnect', 'C', "SELECT OBJECT_ID('tempdb..#scope_disconnect') AS object_id; SELECT id FROM #scope_disconnect")
    await record('connection reusable after scope errors', 'C', 'SELECT 1 AS reusable')
    return run
  } finally {
    await close(secondary)
    if (replacement) await close(replacement)
  }
}

function validate(run) {
  assert.equal(run.length, 24, 'complete scenario set')
  const byName = new Map(run.map(entry => [entry.name, entry.result]))
  const absent = new Set([
    'local invisible to second session', 'inner local absent after nested SQL',
    'RPC local absent afterward', 'rolled-back local absent',
    'global absent in second session', 'local absent after explicit drop',
    'local absent after reconnect',
  ])
  for (const entry of run) {
    if (absent.has(entry.name)) {
      assert.equal(entry.result.errors.length, 1, entry.name)
      assert.equal(entry.result.errors[0].number, 208, entry.name)
      assertSameCapture(entry.result.sets[0].rows, [[null]], `${entry.name}: OBJECT_ID`)
    } else assert.equal(entry.result.errors.length, 0, entry.name)
  }
  const rows = name => byName.get(name).sets[0].rows
  assertSameCapture(rows('outer sees nested write'), [[1, 'first', { kind: 'binary', value: '00ff' }], [2, 'nested', { kind: 'binary', value: '' }]], 'nested SQL must update outer local')
  assertSameCapture(rows('RPC reads outer local'), rows('outer sees nested write'), 'RPC must read outer local')
  assertSameCapture(rows('transaction updates outer local and rolls back'), rows('outer sees nested write'), 'rollback must preserve outer local')
  assertSameCapture(rows('global visible to second session'), [[40]], 'global table must cross sessions')
  assertSameCapture(rows('global sees second session write'), [[40], [41]], 'global table must share writes')
  assertSameCapture(rows('connection reusable after scope errors'), [[1]], 'connection remains reusable')
  const columns = byName.get('local create and initial rows').sets[0].columns
  assertSameCapture(columns.map(({ type, length }) => [type, length]), [['Int', null], ['NVarChar', 40], ['VarBinary', 4]], 'typed temp-table descriptors')
}

await assertSeparateOutput()
if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(dirname(output), { recursive: true })
const captures = []
let image
for (let containerNumber = 0; containerNumber < 2; containerNumber++) {
  await withReferenceContainer(async (config, container) => {
    image ??= container.image
    assert.equal(container.image, image)
    for (let databaseNumber = 0; databaseNumber < 2; databaseNumber++) {
      const run = await isolatedReference(config, primary => observe(config, primary))
      validate(run)
      if (captures.length) assertSameCapture(run, captures[0], 'independent temporary-table captures differ')
      captures.push(run)
    }
  })
}
const actual = { image, freshDatabases: captures.length, independentContainers: 2, results: captures[0] }
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (retained) assertSameCapture(actual, retained, 'fresh capture differs from retained fixture')
await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${actual.results.length} temporary-table observations in ${captures.length} fresh databases and two containers${retained ? '; matched retained fixture' : ''}`)
