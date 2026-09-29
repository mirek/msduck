#!/usr/bin/env node
// Independent SQL Server evidence for the sys.databases columns msduck exposes.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/sys-databases.json', import.meta.url)
const fixtureSha256 = 'e7da8332bd616f55370339602200cf06b75cf1f06839ba1bc481e7d8249c3705'
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-sys-databases.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/sys-databases-reference/capture.json')
const published = [
  'name', 'database_id', 'source_database_id', 'create_date',
  'compatibility_level', 'collation_name', 'user_access', 'user_access_desc',
  'is_read_only', 'state', 'state_desc', 'recovery_model', 'recovery_model_desc',
]
const columns = published.join(',')
const quoted = published.map(name => `N'${name}'`).join(',')
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['declarations', `SELECT name,column_id,system_type_id,user_type_id,max_length,precision,scale,collation_name,is_nullable,is_computed FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.databases') AND name IN (${quoted}) ORDER BY column_id`],
  ['published empty', `SELECT ${columns} FROM sys.databases WHERE database_id=-1`],
  ['published master', `SELECT ${columns} FROM sys.databases WHERE database_id=1`],
  ['complete empty', 'SELECT * FROM sys.databases WHERE database_id=-1'],
  ['session reusable', 'SELECT 1 AS reusable'],
]

async function existingFile(path, inspect = stat) {
  try { return await inspect(path) } catch (error) { if (error.code === 'ENOENT') return null; throw error }
}
async function canonicalOutput(path) {
  try { return await realpath(path) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return join(await realpath(dirname(path)), basename(path))
  }
}
function validate(run) {
  assertSameCapture(run.map(({ name, sql }) => [name, sql]), plan, 'capture plan changed')
  for (const { name, result } of run) {
    assert.equal(result.errors.length, 0, `${name}: SQL Server error`)
    assert.equal(result.sets.length, 1, `${name}: expected one result set`)
    assert(result.done.length > 0, `${name}: completion missing`)
    assert(Array.isArray(result.info), `${name}: information tokens missing`)
    const { columns, rows } = result.sets[0]
    assert(Array.isArray(columns) && Array.isArray(rows), `${name}: incomplete result`)
    for (const column of columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete TDS descriptor`)
  }
  const get = name => run.find(item => item.name === name).result.sets[0]
  assertSameCapture(get('server version').rows, [['17.0.4065.4']], 'reference image version changed')
  assertSameCapture(get('session reusable').rows, [[1]], 'session unusable')
  assertSameCapture(get('declarations').rows.map(row => row[0]), published, 'published catalog column order changed')
  for (const name of ['published empty', 'published master']) {
    assertSameCapture(get(name).columns.map(column => column.name), published, `${name}: projection changed`)
  }
  assert.equal(get('published empty').rows.length, 0, 'empty filter returned rows')
  assert.equal(get('published master').rows.length, 1, 'master row missing')
  assertSameCapture(get('published master').rows[0].slice(0, 2), ['master', 1], 'master identity changed')
  assert.equal(get('published master').rows[0][4], 170, 'server compatibility level changed')
  assertSameCapture(get('published empty').columns.map(({ type, length, flags }) => [type, length, flags]), [
    ['NVarChar', 256, 8], ['Int', null, 8], ['IntN', 4, 9],
    ['DateTime', null, 8], ['TinyInt', null, 8], ['NVarChar', 256, 33],
    ['IntN', 1, 33], ['NVarChar', 120, 33], ['BitN', 1, 33],
    ['IntN', 1, 33], ['NVarChar', 120, 33], ['IntN', 1, 33],
    ['NVarChar', 120, 33],
  ], 'published descriptor controls changed')
  assert.equal(get('complete empty').rows.length, 0, 'complete empty filter returned rows')
  assert.equal(get('complete empty').columns.length, 98, 'complete SQL Server schema changed')
}
async function observe(connection) {
  const run = []
  for (const [name, sql] of plan) {
    run.push({ name, sql, result: canonical(await capture(connection, sql)) })
    console.log(name)
  }
  validate(run)
  return run
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
  console.log(`Checked ${retained.runs[0].length} sys.databases observations in two retained runs`)
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if ((await existingFile(output, lstat))?.isSymbolicLink()) throw Error('capture output must not be a symbolic link')
  const fixturePath = fileURLToPath(fixture)
  if (await canonicalOutput(output) === await canonicalOutput(fixturePath)) throw Error('capture output must not be the retained fixture')
  const [outputFile, retainedFile] = await Promise.all([existingFile(output), existingFile(fixturePath)])
  if (outputFile && retainedFile && outputFile.dev === retainedFile.dev && outputFile.ino === retainedFile.ino) throw Error('capture output must not be a hard link to the retained fixture')
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) throw Error('reference image must be pinned')
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    await withReferenceContainer(async (config, container) => {
      assert.equal(container.image, referenceImage, 'reference image must be pinned')
      const run = await isolatedReference(config, observe)
      if (runs.length) assertSameCapture(run, runs[0], 'independent reference containers differ')
      runs.push(run)
    })
  }
  const actual = { image: referenceImage, captureSha256: createHash('sha256').update(JSON.stringify(runs[0])).digest('hex'), runs }
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) } catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'fresh capture differs from retained fixture')
  await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} sys.databases observations in two fresh containers${retained ? '; matched fixture' : ''}`)
}
