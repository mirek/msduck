#!/usr/bin/env node
// First-party SQL Server 2025 evidence for session DATEFORMAT conversion rules.
import assert from 'node:assert/strict'
import { access, mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/dateformat.json', import.meta.url)
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-dateformat.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/dateformat/capture.json')

const orders = [
  ['mdy', '04/05/2024'],
  ['dmy', '05/04/2024'],
  ['ymd', '2024/04/05'],
  ['ydm', '2024/05/04'],
  ['myd', '04/2024/05'],
  ['dym', '05/2024/04'],
]
const types = ['DATE', 'DATETIME', 'DATETIME2(7)', 'DATETIMEOFFSET(7)']
const conversions = value => `SELECT ${types.map((type, index) => `TRY_CAST(N'${value}' AS ${type}) AS value_${index}`).join(',')}`
const isoSql = "SELECT CAST(N'2024-03-04T05:06:07.1234567' AS DATETIME2(7)) AS dt2,CAST(N'2024-03-04T05:06:07.1234567+02:00' AS DATETIMEOFFSET(7)) AS dto"

async function observe(primary, config) {
  const databaseName = (await capture(primary, 'SELECT DB_NAME() AS name')).sets[0].rows[0][0]
  const secondary = await connect({ ...config, options: { ...config.options, database: databaseName } })
  const run = []
  async function record(name, sql, session = 'A') {
    const result = canonical(await capture(session === 'A' ? primary : secondary, sql))
    run.push({ name, sql, session, result })
    console.log(name)
    return result
  }
  try {
    await record('server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version")
    for (const [format, sameDate] of orders) {
      await record(`set ${format}`, `SET DATEFORMAT ${format}`)
      await record(`${format} ambiguous`, conversions('03/04/2024'))
      await record(`${format} same date`, conversions(sameDate))
    }
    await record('set ydm for strict conversion', 'SET DATEFORMAT ydm')
    await record('ydm strict date failure', "SELECT CAST(N'2024/31/12' AS DATE) AS value")
    await record('ydm strict datetime', "SELECT CAST(N'2024/31/12' AS DATETIME) AS value")
    await record('set dmy for ISO probe', 'SET DATEFORMAT dmy')
    await record('ISO timestamp under dmy', isoSql)
    await record('set ydm for ISO probe', 'SET DATEFORMAT ydm')
    await record('ISO timestamp under ydm', isoSql)
    await record('set mdy for ISO probe', 'SET DATEFORMAT mdy')
    await record('ISO timestamp under mdy', isoSql)
    await record('runtime changes in one batch', "SET DATEFORMAT dmy; SELECT TRY_CAST(N'03/04/2024' AS DATE) AS dmy_value; SET DATEFORMAT mdy; SELECT TRY_CAST(N'03/04/2024' AS DATE) AS mdy_value")
    await record('set dmy for independent session', 'SET DATEFORMAT dmy')
    await record('A retains dmy', "SELECT TRY_CAST(N'03/04/2024' AS DATE) AS value")
    await record('B retains default mdy', "SELECT TRY_CAST(N'03/04/2024' AS DATE) AS value", 'B')
    await record('B set ymd', 'SET DATEFORMAT ymd', 'B')
    await record('A unchanged by B', "SELECT TRY_CAST(N'03/04/2024' AS DATE) AS value")
    await record('B own setting', "SELECT TRY_CAST(N'03/04/2024' AS DATE) AS value", 'B')
    await record('language resets format', "SET LANGUAGE us_english; SELECT TRY_CAST(N'03/04/2024' AS DATE) AS value")
    await record('format overrides language', "SET DATEFORMAT dmy; SELECT TRY_CAST(N'03/04/2024' AS DATE) AS value")
    await record('invalid format', 'SET DATEFORMAT xyz')
    await record('after invalid format', "SELECT TRY_CAST(N'03/04/2024' AS DATE) AS value")
    await record('format from variable', "DECLARE @format SYSNAME=N'mdy'; SET DATEFORMAT @format; SELECT TRY_CAST(N'03/04/2024' AS DATE) AS value")
    await record('prepared execute-time setting', `DECLARE @handle INT;
      SET DATEFORMAT mdy;
      EXEC sys.sp_prepare @handle OUTPUT, N'@d NVARCHAR(30)', N'SELECT TRY_CAST(@d AS DATE) AS value';
      SET DATEFORMAT dmy; EXEC sys.sp_execute @handle, N'03/04/2024';
      SET DATEFORMAT mdy; EXEC sys.sp_execute @handle, N'03/04/2024';
      EXEC sys.sp_unprepare @handle`)
    await record('session reusable', 'SELECT 1 AS reusable')
    try { validate(run) }
    catch (error) {
      await mkdir(dirname(output), { recursive: true })
      await writeFile(resolve(dirname(output), `failed-${Date.now()}.json`), JSON.stringify(run) + '\n', { flag: 'wx' })
      throw error
    }
    return run
  } finally {
    if (!secondary.closed) await new Promise(resolve => { secondary.once('end', resolve); secondary.close() })
  }
}

function validate(run) {
  assert.equal(run.length, 42, 'case count')
  const get = name => {
    const entry = run.find(item => item.name === name)
    assert(entry, `missing ${name}`)
    return entry.result
  }
  for (const { name, result } of run) {
    for (const key of ['sets', 'done', 'errors', 'info']) assert(Array.isArray(result[key]), `${name}: missing ${key}`)
    assert(result.done.length > 0, `${name}: missing completion`)
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete result set`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
  }
  const value = name => get(name).sets[0].rows[0][0]
  assert.notDeepEqual(value('mdy ambiguous'), value('dmy ambiguous'), 'ambiguous input should follow order')
  assert.deepEqual(value('A retains dmy'), value('A unchanged by B'), 'other session changed A')
  assert.deepEqual(value('B retains default mdy'), value('language resets format'), 'language did not reset mdy')
  assert.deepEqual(value('A retains dmy'), value('format overrides language'), 'DATEFORMAT did not override language')
  assert.deepEqual(value('format overrides language'), value('after invalid format'), 'invalid format changed A')
  assert.deepEqual(value('B retains default mdy'), value('format from variable'), 'variable format failed')
  assertSameCapture(get('ISO timestamp under dmy').sets, get('ISO timestamp under ydm').sets, 'ISO changed under ydm')
  assertSameCapture(get('ISO timestamp under dmy').sets, get('ISO timestamp under mdy').sets, 'ISO changed under mdy')
  assert.equal(get('ydm strict date failure').errors.length, 1)
  assert.equal(get('invalid format').errors.length, 1)
  assert.equal(get('prepared execute-time setting').errors.length, 0)
  assert.equal(get('prepared execute-time setting').sets.length, 3)
  assert.deepEqual(get('prepared execute-time setting').sets[0].rows, [], 'sp_prepare emits metadata without rows')
  assert.notDeepEqual(get('prepared execute-time setting').sets[1].rows, get('prepared execute-time setting').sets[2].rows)
  assert.deepEqual(get('session reusable').sets[0].rows, [[1]])
}

if (check) {
  const retained = JSON.parse(await readFile(fixture, 'utf8'))
  assert.equal(retained.image, referenceImage)
  assert.equal(retained.independentContainers, 2)
  validate(retained.results)
  console.log(`checked ${retained.results.length} DATEFORMAT observations`)
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  try { await access(output); throw Error(`refusing to overwrite ${output}`) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  await mkdir(dirname(output), { recursive: true })
  const runs = []
  let image
  for (let index = 0; index < 2; index++) {
    await withReferenceContainer(async (config, container) => {
      assert.equal(container.image, referenceImage, 'unexpected SQL Server image')
      image ??= container.image
      assert.equal(container.image, image)
      const run = await isolatedReference(config, primary => observe(primary, config))
      if (runs.length) assertSameCapture(run, runs[0], 'independent DATEFORMAT captures differ')
      runs.push(run)
    })
  }
  const actual = { image, independentContainers: 2, results: runs[0] }
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'fresh DATEFORMAT capture differs from retained fixture')
  await writeFile(output, JSON.stringify(actual) + '\n')
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${actual.results.length} DATEFORMAT observations in two independent containers${retained ? ' and matched the retained fixture' : ''}`)
}
