#!/usr/bin/env node
// Independent SQL Server evidence for UNIQUEIDENTIFIER ordering and text casts.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/guid-conversion-order.json', import.meta.url)
const fixtureSha256 = 'ffebfcb92b7ee86e221974a6a488d36899f09b5e847bf4a7b15101c332e7bbb8'
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-guid-conversion-order.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/guid-conversion-order/capture.json')

const values = [
  ['zero', '00000000-0000-0000-0000-000000000000'],
  ['first_low', '00000001-0000-0000-0000-000000000000'],
  ['first_high', '01000000-0000-0000-0000-000000000000'],
  ['second_low', '00000000-0001-0000-0000-000000000000'],
  ['second_high', '00000000-0100-0000-0000-000000000000'],
  ['third_low', '00000000-0000-0001-0000-000000000000'],
  ['third_high', '00000000-0000-0100-0000-000000000000'],
  ['fourth_low', '00000000-0000-0000-0001-000000000000'],
  ['fourth_high', '00000000-0000-0000-0100-000000000000'],
  ['last', '00000000-0000-0000-0000-000000000001'],
  ['last_high', '00000000-0000-0000-0000-010000000000'],
  ['max', 'ffffffff-ffff-ffff-ffff-ffffffffffff'],
]
const guid = label => values.find(([name]) => name === label)[1]
const insertValues = values.map(([label, guid]) => `('${label}',CONVERT(uniqueidentifier,'${guid}'))`).join(',')
const good = '00112233-4455-6677-8899-aabbccddeeff'
const upper = good.toUpperCase()
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['create source', 'CREATE TABLE dbo.guid_cases(label varchar(24) NOT NULL,g uniqueidentifier NOT NULL)'],
  ['insert source', `INSERT dbo.guid_cases(label,g) VALUES ${insertValues}`],
  ['guid order', 'SELECT label,g FROM dbo.guid_cases ORDER BY g'],
  ['text order', 'SELECT label,g FROM dbo.guid_cases ORDER BY CONVERT(varchar(36),g)'],
  ['binary order', 'SELECT label,g FROM dbo.guid_cases ORDER BY CONVERT(binary(16),g)'],
  ['guid bytes', 'SELECT label,CONVERT(binary(16),g) AS bytes FROM dbo.guid_cases ORDER BY label'],
  ['guid descending', 'SELECT label,g FROM dbo.guid_cases ORDER BY g DESC'],
  ['guid comparison', `SELECT CASE WHEN CONVERT(uniqueidentifier,'${guid('first_high')}') < CONVERT(uniqueidentifier,'${guid('last')}') THEN 1 ELSE 0 END AS first_less_last,CASE WHEN CONVERT(uniqueidentifier,'${guid('last')}') < CONVERT(uniqueidentifier,'${guid('first_high')}') THEN 1 ELSE 0 END AS last_less_first`],
  ['varchar canonical', `SELECT CAST(CAST('${good}' AS varchar(36)) AS uniqueidentifier) AS g`],
  ['nvarchar uppercase', `SELECT CAST(CAST(N'${upper}' AS nvarchar(36)) AS uniqueidentifier) AS g`],
  ['varchar overlong', `SELECT CAST(CAST('${good}EXTRA' AS varchar(50)) AS uniqueidentifier) AS g`],
  ['nvarchar overlong', `SELECT TRY_CONVERT(uniqueidentifier,CAST(N'${good}EXTRA' AS nvarchar(50))) AS g`],
  ['varchar trailing spaces', `SELECT CAST(CAST('${good}   ' AS varchar(50)) AS uniqueidentifier) AS g`],
  ['nvarchar braces', `SELECT TRY_CONVERT(uniqueidentifier,N'{${good}}') AS g`],
  ['varchar no hyphens', "SELECT TRY_CONVERT(uniqueidentifier,'00112233445566778899aabbccddeeff') AS g"],
  ['varchar short try', "SELECT TRY_CONVERT(uniqueidentifier,'00112233-4455-6677-8899-aabbccddee') AS g"],
  ['nvarchar malformed try', "SELECT TRY_CONVERT(uniqueidentifier,N'not-a-guid') AS g"],
  ['varchar malformed cast', "SELECT CAST('not-a-guid' AS uniqueidentifier) AS g"],
  ['recovery after varchar error', 'SELECT 1 AS reusable'],
  ['nvarchar malformed cast', "SELECT CAST(N'not-a-guid' AS uniqueidentifier) AS g"],
  ['recovery after nvarchar error', 'SELECT 1 AS reusable'],
  ['varchar null', 'SELECT CAST(CAST(NULL AS varchar(36)) AS uniqueidentifier) AS g'],
  ['nvarchar null try', 'SELECT TRY_CONVERT(uniqueidentifier,CAST(NULL AS nvarchar(36))) AS g'],
  ['empty result metadata', 'SELECT g FROM dbo.guid_cases WHERE 1=0'],
]
const errorCases = new Set(['varchar malformed cast', 'nvarchar malformed cast'])

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
    assert(result.done.length > 0, `${name}: completion missing`)
    assert(Array.isArray(result.info), `${name}: information tokens missing`)
    if (errorCases.has(name)) {
      assert(result.errors.length > 0, `${name}: expected conversion error`)
    } else {
      assert.equal(result.errors.length, 0, `${name}: SQL Server error`)
    }
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete result`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
  }
  const get = name => run.find(item => item.name === name).result.sets[0]
  assertSameCapture(get('server version').rows, [['17.0.4065.4']], 'reference image version changed')
  assertSameCapture(get('recovery after varchar error').rows, [[1]], 'varchar recovery failed')
  assertSameCapture(get('recovery after nvarchar error').rows, [[1]], 'nvarchar recovery failed')
  for (const name of ['guid order', 'guid descending', 'text order', 'binary order']) {
    assert.equal(get(name).columns[1].type, 'UniqueIdentifier', `${name}: GUID descriptor changed`)
    assert.equal(get(name).rows.length, values.length, `${name}: missing GUID rows`)
    assertSameCapture(new Set(get(name).rows.map(row => row[0])), new Set(values.map(row => row[0])), `${name}: GUID set changed`)
  }
  assertSameCapture(get('guid order').rows.map(row => row[0]), [
    'zero', 'first_high', 'first_low', 'second_high', 'second_low',
    'third_high', 'third_low', 'fourth_low', 'fourth_high', 'last', 'last_high', 'max',
  ], 'SQL Server GUID order changed')
  assertSameCapture(get('guid comparison').rows, [[1, 0]], 'GUID comparison changed')
  for (const name of ['varchar canonical', 'nvarchar uppercase', 'varchar overlong', 'nvarchar overlong', 'varchar trailing spaces', 'nvarchar braces']) {
    assertSameCapture(get(name).rows, [[good.toUpperCase()]], `${name}: converted value changed`)
  }
  for (const name of ['varchar no hyphens', 'varchar short try', 'nvarchar malformed try', 'varchar null', 'nvarchar null try']) {
    assertSameCapture(get(name).rows, [[null]], `${name}: NULL behavior changed`)
  }
  for (const name of errorCases) {
    const error = run.find(item => item.name === name).result.errors[0]
    assertSameCapture([error.number, error.state, error.class], [8169, 2, 16], `${name}: diagnostic changed`)
  }
  assert.equal(get('empty result metadata').columns[0].type, 'UniqueIdentifier')
  assert.equal(get('empty result metadata').rows.length, 0)
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
  console.log(`Checked ${retained.runs[0].length} GUID observations in two retained runs`)
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
  console.log(`Captured ${runs[0].length} GUID observations in two fresh containers${retained ? '; matched fixture' : ''}`)
}
