#!/usr/bin/env node
// Pinned SQL Server evidence for character-to-UNIQUEIDENTIFIER conversion.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/guid-character-conversion.json', import.meta.url)
const fixtureSha256 = 'cbf75c6230252f12fec7519993425d069f5a28c228424b3c2d37c857554ddd41'
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-guid-character-conversion.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/guid-character-conversion/capture.json')

const good = '00112233-4455-6677-8899-aabbccddeeff'
const samples = [
  ['canonical', good],
  ['uppercase', good.toUpperCase()],
  ['braced', `{${good}}`],
  ['braced extra suffix', `{${good}}EXTRA`],
  ['braced trailing spaces', `{${good}}   `],
  ['braced missing close', `{${good}`],
  ['braced wrong close', `{${good})`],
  ['parenthesized', `(${good})`],
  ['hyphenless', good.replaceAll('-', '')],
  ['short', good.slice(0, -1)],
  ['extra suffix', `${good}EXTRA`],
  ['suffix closing brace', `${good}}`],
  ['suffix non ASCII', `${good}é`],
  ['trailing spaces', `${good}   `],
  ['leading space', ` ${good}`],
  ['internal space', `${good.slice(0, 8)} ${good.slice(9)}`],
  ['bad hex', `x${good.slice(1)}`],
  ['bad hyphen', `${good.slice(0, 8)}_${good.slice(9)}`],
  ['empty', ''],
  ['non ASCII', `é${good.slice(1)}`],
  ['all zero', '00000000-0000-0000-0000-000000000000'],
  ['all one', 'ffffffff-ffff-ffff-ffff-ffffffffffff'],
  ['typed null', null],
]
const accepted = new Map([
  ...['canonical', 'uppercase', 'braced', 'braced extra suffix', 'braced trailing spaces',
    'extra suffix', 'trailing spaces', 'suffix closing brace', 'suffix non ASCII']
    .map(name => [name, good.toUpperCase()]),
  ['all zero', '00000000-0000-0000-0000-000000000000'],
  ['all one', 'FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF'],
  ['typed null', null],
])
const recoverySql = 'SELECT 1 AS reusable'
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ['canonical bytes', `SELECT CONVERT(binary(16),CONVERT(uniqueidentifier,'${good}')) AS bytes`],
]
for (const [label, value] of samples) {
  for (const family of ['varchar', 'nvarchar']) {
    const quoted = value === null ? 'NULL' : `${family === 'nvarchar' ? 'N' : ''}'${value.replaceAll("'", "''")}'`
    const source = `CAST(${quoted} AS ${family}(100))`
    plan.push([`${label} ${family} CAST`, `SELECT CAST(${source} AS uniqueidentifier) AS g`, value, family, 'CAST'])
    plan.push([`${label} ${family} TRY_CONVERT`, `SELECT TRY_CONVERT(uniqueidentifier,${source}) AS g`, value, family, 'TRY_CONVERT'])
  }
}
plan.push(['session reusable', recoverySql])

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
  assertSameCapture(run.map(({ name, sql }) => [name, sql]), plan.map(entry => entry.slice(0, 2)), 'capture plan changed')
  for (const { name, result, recovery, input, family, mode } of run) {
    const planned = plan.find(entry => entry[0] === name)
    if (planned[3]) assertSameCapture([input, family, mode], planned.slice(2), `${name}: input changed`)
    assert(result.done.length > 0, `${name}: completion missing`)
    assert(Array.isArray(result.info), `${name}: information tokens missing`)
    if (name.includes('TRY_CONVERT') || name === 'server version' || name === 'canonical bytes' || name === 'session reusable') {
      assert.equal(result.errors.length, 0, `${name}: unexpected SQL Server error`)
    }
    if (result.errors.length) {
      assert(recovery, `${name}: recovery missing`)
      assert.equal(recovery.errors.length, 0, `${name}: recovery error`)
      assertSameCapture(recovery.sets[0].rows, [[1]], `${name}: session unusable`)
    } else assert.equal(recovery, undefined, `${name}: unexpected recovery`)
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete result`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
  }
  const get = name => run.find(item => item.name === name).result.sets[0]
  assertSameCapture(get('server version').rows, [['17.0.4065.4']], 'reference image version changed')
  assertSameCapture(get('session reusable').rows, [[1]], 'session unusable')
  assertSameCapture(get('canonical bytes').rows, [[{ kind: 'binary', value: '33221100554477668899aabbccddeeff' }]], 'mixed-endian bytes changed')
  for (const [label] of samples) {
    for (const family of ['varchar', 'nvarchar']) {
      for (const mode of ['CAST', 'TRY_CONVERT']) {
        const name = `${label} ${family} ${mode}`
        const result = run.find(item => item.name === name).result
        assertSameCapture(result.sets[0].columns.map(c => [c.type, c.length, c.flags]), [['UniqueIdentifier', 16, 33]], `${name}: descriptor changed`)
        if (accepted.has(label)) {
          assert.equal(result.errors.length, 0, `${name}: unexpected conversion error`)
          assertSameCapture(result.sets[0].rows, [[accepted.get(label)]], `${name}: value changed`)
        } else if (mode === 'CAST') {
          assertSameCapture(result.errors.map(e => [e.number, e.state, e.class]), [[8169, 2, 16]], `${name}: diagnostic changed`)
          assert.equal(result.sets[0].rows.length, 0, `${name}: row after error`)
        } else {
          assert.equal(result.errors.length, 0, `${name}: TRY_CONVERT error`)
          assertSameCapture(result.sets[0].rows, [[null]], `${name}: TRY_CONVERT value changed`)
        }
      }
    }
  }
}
async function observe(connection) {
  const run = []
  for (const [name, sql, input, family, mode] of plan) {
    const result = canonical(await capture(connection, sql))
    const item = { name, sql, ...(family ? { input, family, mode } : {}), result }
    if (result.errors.length) item.recovery = canonical(await capture(connection, recoverySql))
    run.push(item)
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
  console.log(`Checked ${retained.runs[0].length} GUID conversion observations in two retained runs`)
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
  console.log(`Captured ${runs[0].length} GUID conversion observations in two fresh containers${retained ? '; matched fixture' : ''}`)
}
