#!/usr/bin/env node
// First-party SQL Server 2025 evidence for session DATEFORMAT conversion rules.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { access, mkdir, readFile, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, resolve } from 'node:path'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/dateformat.json', import.meta.url)
const fixtureSha256 = '59652a3bec0cc75ad008982ac846e70c5d374587bf14bb45f28f47d9953b954a'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const rawDoneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
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
const conversions = value => {
  const offset = `TRY_CAST(N'${value}' AS DATETIMEOFFSET(7))`
  return `SELECT ${types.map((type, index) => `TRY_CAST(N'${value}' AS ${type}) AS value_${index}`).join(',')},CONVERT(NVARCHAR(48),${offset}) AS dto_text,DATEPART(TZOFFSET,${offset}) AS dto_offset_minutes`
}
const isoOffset = "CAST(N'2024-03-04T05:06:07.1234567+02:00' AS DATETIMEOFFSET(7))"
const isoSql = `SELECT CAST(N'2024-03-04T05:06:07.1234567' AS DATETIME2(7)) AS dt2,${isoOffset} AS dto,CONVERT(NVARCHAR(48),${isoOffset}) AS dto_text,DATEPART(TZOFFSET,${isoOffset}) AS dto_offset_minutes`
const ambiguous = "SELECT TRY_CAST(N'03/04/2024' AS DATE) AS value"
const ymdOnly = "SELECT TRY_CAST(N'24/04/05' AS DATE) AS value"
const casePlan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version"],
  ...orders.flatMap(([format, sameDate]) => [
    [`set ${format}`, `SET DATEFORMAT ${format}`],
    [`${format} ambiguous`, conversions('03/04/2024')],
    [`${format} same date`, conversions(sameDate)],
  ]),
  ['set ydm for strict conversion', 'SET DATEFORMAT ydm'],
  ['ydm strict date failure', "SELECT CAST(N'2024/31/12' AS DATE) AS value"],
  ['ydm strict datetime', "SELECT CAST(N'2024/31/12' AS DATETIME) AS value"],
  ['set dmy for ISO probe', 'SET DATEFORMAT dmy'],
  ['ISO timestamp under dmy', isoSql],
  ['set ydm for ISO probe', 'SET DATEFORMAT ydm'],
  ['ISO timestamp under ydm', isoSql],
  ['set mdy for ISO probe', 'SET DATEFORMAT mdy'],
  ['ISO timestamp under mdy', isoSql],
  ['runtime changes in one batch', "SET DATEFORMAT dmy; SELECT TRY_CAST(N'03/04/2024' AS DATE) AS dmy_value; SET DATEFORMAT mdy; SELECT TRY_CAST(N'03/04/2024' AS DATE) AS mdy_value"],
  ['set dmy for independent session', 'SET DATEFORMAT dmy'],
  ['A retains dmy', ambiguous],
  ['B retains default mdy', ambiguous, 'B'],
  ['B ymd spelling under mdy', ymdOnly, 'B'],
  ['B set ymd', 'SET DATEFORMAT ymd', 'B'],
  ['A unchanged by B', ambiguous],
  ['B own setting', ymdOnly, 'B'],
  ['language resets format', `SET LANGUAGE us_english; ${ambiguous}`],
  ['format overrides language', `SET DATEFORMAT dmy; ${ambiguous}`],
  ['invalid format', 'SET DATEFORMAT xyz'],
  ['after invalid format', ambiguous],
  ['format from variable', `DECLARE @format SYSNAME=N'mdy'; SET DATEFORMAT @format; ${ambiguous}`],
  ['prepared execute-time setting', `DECLARE @handle INT;
      SET DATEFORMAT mdy;
      EXEC sys.sp_prepare @handle OUTPUT, N'@d NVARCHAR(30)', N'SELECT TRY_CAST(@d AS DATE) AS value';
      SET DATEFORMAT dmy; EXEC sys.sp_execute @handle, N'03/04/2024';
      SET DATEFORMAT mdy; EXEC sys.sp_execute @handle, N'03/04/2024';
      EXEC sys.sp_unprepare @handle`],
  ['session reusable', 'SELECT 1 AS reusable'],
].map(([name, sql, session = 'A']) => ({ name, sql, session }))

async function captureWithDoneTokens(connection, sql) {
  const doneTokens = []
  const rawDone = []
  const events = []
  const createParser = connection.createTokenStreamParser
  const readToken = StreamParser.prototype.readToken
  const recordRawDone = (parser, type) => {
    assert(parser.options.tdsVersion >= '7_2', 'unexpected TDS version')
    const at = parser.position
    if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordRawDone(parser, type))
    rawDone.push({
      kind: rawDoneKinds.get(type),
      status: parser.buffer.readUInt16LE(at),
      command: parser.buffer.readUInt16LE(at + 2),
    })
    return readToken.call(parser, type)
  }
  StreamParser.prototype.readToken = function (type) {
    return rawDoneKinds.has(type) ? recordRawDone(this, type) : readToken.call(this, type)
  }
  connection.createTokenStreamParser = function (message, handler) {
    const parser = createParser.call(this, message, handler)
    assert.equal(typeof parser.parser?.prependListener, 'function', 'Tedious token stream unavailable')
    parser.parser.prependListener('data', token => {
      if (typeof token.name === 'string') events.push({ kind: token.name })
      if (['DONE', 'DONEINPROC', 'DONEPROC'].includes(token.name)) {
        doneTokens.push({
          kind: token.name,
          more: token.more,
          sqlError: token.sqlError,
          attention: token.attention,
          serverError: token.serverError,
          rowCount: token.rowCount ?? null,
          command: token.curCmd,
        })
      }
    })
    return parser
  }
  try {
    const result = await capture(connection, sql)
    assert.equal(doneTokens.length, result.done.length, 'incomplete decoded DONE tokens')
    assert.equal(rawDone.length, doneTokens.length, 'incomplete raw DONE status')
    for (let index = 0; index < doneTokens.length; index++) {
      assert.equal(rawDone[index].kind, doneTokens[index].kind, 'raw and decoded DONE kinds differ')
      assert.equal(rawDone[index].command, doneTokens[index].command, 'raw and decoded DONE commands differ')
      doneTokens[index].status = rawDone[index].status
    }
    assert.deepEqual(events.filter(event => ['DONE', 'DONEINPROC', 'DONEPROC'].includes(event.kind)).map(event => event.kind),
      doneTokens.map(token => token.kind), 'wire and callback DONE order differ')
    return { ...result, doneTokens, events }
  } finally {
    connection.createTokenStreamParser = createParser
    StreamParser.prototype.readToken = readToken
  }
}

async function observe(primary, config) {
  const databaseName = (await capture(primary, 'SELECT DB_NAME() AS name')).sets[0].rows[0][0]
  const secondary = await connect({ ...config, options: { ...config.options, database: databaseName } })
  const run = []
  async function record(name, sql, session = 'A') {
    const result = canonical(await captureWithDoneTokens(session === 'A' ? primary : secondary, sql))
    run.push({ name, sql, session, result })
    console.log(name)
    return result
  }
  try {
    for (const { name, sql, session } of casePlan) await record(name, sql, session)
    return run
  } finally {
    if (!secondary.closed) await new Promise(resolve => { secondary.once('end', resolve); secondary.close() })
  }
}

function validate(run) {
  assert.deepEqual(run.map(({ name, sql, session }) => ({ name, sql, session })), casePlan,
    'retained cases differ from current capture plan')
  const get = name => {
    const entry = run.find(item => item.name === name)
    assert(entry, `missing ${name}`)
    return entry.result
  }
  for (const { name, result } of run) {
    for (const key of ['sets', 'done', 'doneTokens', 'events', 'errors', 'info']) assert(Array.isArray(result[key]), `${name}: missing ${key}`)
    if (!['ydm strict date failure', 'invalid format'].includes(name)) {
      assert.deepEqual(result.errors, [], `${name}: unexpected SQL Server diagnostic`)
    }
    assert(result.done.length > 0, `${name}: missing completion`)
    assert.equal(result.doneTokens.length, result.done.length, `${name}: missing decoded DONE status`)
    for (const token of result.doneTokens) {
      assert(Number.isInteger(token.status) && token.status >= 0 && token.status <= 0xffff,
        `${name}: missing raw DONE status word`)
    }
    assert(result.events.length > 0, `${name}: missing wire event order`)
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete result set`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
  }
  const value = name => get(name).sets[0].rows[0][0]
  const descriptor = name => get(name).sets[0].columns.map(({ name: column, type, length, precision, scale, flags }) =>
    ({ name: column, type, length, precision, scale, flags }))
  const conversionDescriptors = [
    { name: 'value_0', type: 'Date', length: null, precision: null, scale: null, flags: 33 },
    { name: 'value_1', type: 'DateTimeN', length: 8, precision: null, scale: null, flags: 33 },
    { name: 'value_2', type: 'DateTime2', length: null, precision: null, scale: 7, flags: 33 },
    { name: 'value_3', type: 'DateTimeOffset', length: null, precision: null, scale: 7, flags: 33 },
    { name: 'dto_text', type: 'NVarChar', length: 96, precision: null, scale: null, flags: 33 },
    { name: 'dto_offset_minutes', type: 'IntN', length: 4, precision: null, scale: null, flags: 33 },
  ]
  for (const name of ['mdy ambiguous', 'dmy ambiguous', 'ydm same date']) {
    assert.deepEqual(descriptor(name), conversionDescriptors, `${name}: temporal descriptors changed`)
  }
  assert.deepEqual(descriptor('ydm strict date failure'), [
    { ...conversionDescriptors[0], name: 'value' },
  ], 'failed DATE descriptor changed')
  assert.deepEqual(descriptor('ISO timestamp under mdy'), [
    { ...conversionDescriptors[2], name: 'dt2' },
    { ...conversionDescriptors[3], name: 'dto' },
    conversionDescriptors[4], conversionDescriptors[5],
  ], 'ISO temporal descriptors changed')
  const dateValue = (day, nanos = false) => ({
    kind: 'date', value: `2024-${day}T00:00:00.000Z`,
    ...(nanos ? { nanosecondsDelta: 0 } : {}),
  })
  const completeRow = day => [
    dateValue(day), dateValue(day), dateValue(day, true), dateValue(day, true),
    `2024-${day} 00:00:00.0000000 +00:00`, 0,
  ]
  const ambiguousRows = {
    mdy: completeRow('03-04'),
    dmy: completeRow('04-03'),
    ymd: completeRow('03-04'),
    ydm: [null, dateValue('04-03'), null, null, null, null],
    myd: [null, dateValue('03-04'), null, null, null, null],
    dym: [null, dateValue('04-03'), null, null, null, null],
  }
  for (const [format, expected] of Object.entries(ambiguousRows)) {
    assert.deepEqual(get(`${format} ambiguous`).sets[0].rows, [expected],
      `${format}: ambiguous temporal row changed`)
  }
  const sameDateRows = {
    mdy: completeRow('04-05'),
    dmy: completeRow('04-05'),
    ymd: completeRow('04-05'),
    ydm: [
      dateValue('05-04'), dateValue('04-05'), dateValue('05-04', true),
      dateValue('05-04', true), '2024-05-04 00:00:00.0000000 +00:00', 0,
    ],
    myd: completeRow('04-05'),
    dym: completeRow('04-05'),
  }
  for (const [format, expected] of Object.entries(sameDateRows)) {
    assert.deepEqual(get(`${format} same date`).sets[0].rows, [expected],
      `${format}: order-specific temporal row changed`)
  }
  const singleDateRows = day => [[dateValue(day)]]
  assert.deepEqual(get('runtime changes in one batch').sets.map(set => set.rows),
    [singleDateRows('04-03'), singleDateRows('03-04')], 'same-batch format change lost')
  for (const [name, day] of [
    ['A retains dmy', '04-03'],
    ['A unchanged by B', '04-03'],
    ['B retains default mdy', '03-04'],
    ['B own setting', '04-05'],
    ['language resets format', '03-04'],
    ['format overrides language', '04-03'],
    ['after invalid format', '04-03'],
    ['format from variable', '03-04'],
  ]) assert.deepEqual(get(name).sets[0].rows, singleDateRows(day), `${name}: session outcome changed`)
  assert.deepEqual(get('B ymd spelling under mdy').sets[0].rows, [[null]],
    'B mdy control unexpectedly accepted ymd spelling')
  assert.deepEqual(get('ydm same date').sets[0].rows[0].slice(0, 4), [
    { kind: 'date', value: '2024-05-04T00:00:00.000Z' },
    { kind: 'date', value: '2024-04-05T00:00:00.000Z' },
    { kind: 'date', value: '2024-05-04T00:00:00.000Z', nanosecondsDelta: 0 },
    { kind: 'date', value: '2024-05-04T00:00:00.000Z', nanosecondsDelta: 0 },
  ], 'ydm DATE/DATETIME/DATETIME2/DATETIMEOFFSET type split changed')
  assert.deepEqual(get('ydm strict datetime').sets[0].rows,
    [[{ kind: 'date', value: '2024-12-31T00:00:00.000Z' }]],
    'legacy DATETIME ydm conversion changed')
  assert.deepEqual(value('A retains dmy'), value('A unchanged by B'), 'other session changed A')
  assert.deepEqual(value('B own setting'), get('ymd same date').sets[0].rows[0][0], 'B did not adopt ymd')
  assert.notDeepEqual(value('B own setting'), value('B ymd spelling under mdy'), 'B setting indistinguishable from mdy')
  assert.deepEqual(value('B retains default mdy'), value('language resets format'), 'language did not reset mdy')
  assert.deepEqual(value('A retains dmy'), value('format overrides language'), 'DATEFORMAT did not override language')
  assert.deepEqual(value('format overrides language'), value('after invalid format'), 'invalid format changed A')
  assert.deepEqual(value('B retains default mdy'), value('format from variable'), 'variable format failed')
  assertSameCapture(get('ISO timestamp under dmy').sets, get('ISO timestamp under ydm').sets, 'ISO changed under ydm')
  assertSameCapture(get('ISO timestamp under dmy').sets, get('ISO timestamp under mdy').sets, 'ISO changed under mdy')
  const isoRow = get('ISO timestamp under dmy').sets[0].rows[0]
  assert.deepEqual(isoRow, [
    { kind: 'date', value: '2024-03-04T05:06:07.123Z', nanosecondsDelta: 0.0004567 },
    { kind: 'date', value: '2024-03-04T03:06:07.123Z', nanosecondsDelta: 0.0004567 },
    '2024-03-04 05:06:07.1234567 +02:00', 120,
  ], 'ISO conversion or original offset changed')
  assert.deepEqual(get('ydm strict date failure').errors, [{
    number: 241, state: 1, class: 16, lineNumber: 1,
    message: 'Conversion failed when converting date and/or time from character string.',
  }], 'strict DATE diagnostic changed')
  assert.deepEqual(get('invalid format').errors, [{
    number: 2741, state: 1, class: 16, lineNumber: 1,
    message: "SET DATEFORMAT date order 'xyz' is invalid.",
  }], 'invalid DATEFORMAT diagnostic changed')
  for (const [name, command] of [['ydm strict date failure', 193], ['invalid format', 249]]) {
    assertSameCapture(get(name).doneTokens, [{
      kind: 'DONE', more: false, sqlError: true, attention: false,
      serverError: false, rowCount: null, command, status: 2,
    }], `${name}: decoded DONE_ERROR status and command`)
  }
  assertSameCapture(get('ydm strict date failure').events.map(event => event.kind),
    ['COLMETADATA', 'ERROR', 'DONE'], 'strict DATE metadata, error and DONE order')
  assertSameCapture(get('invalid format').events.map(event => event.kind),
    ['ERROR', 'DONE'], 'invalid DATEFORMAT error and DONE order')
  assert.equal(get('prepared execute-time setting').errors.length, 0)
  assert.equal(get('prepared execute-time setting').sets.length, 3)
  assert.deepEqual(get('prepared execute-time setting').sets.map(set => set.rows), [
    [], singleDateRows('04-03'), singleDateRows('03-04'),
  ], 'sp_prepare metadata or execute-time dmy/mdy rows changed')
  assert.deepEqual(get('session reusable').sets[0].rows, [[1]])
}

if (check) {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256,
    'retained DATEFORMAT fixture changed; recapture and verify before updating the pinned digest')
  const retained = JSON.parse(bytes.toString('utf8'))
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
      runs.push(run)
      try {
        validate(run)
        if (runs.length === 2) assertSameCapture(runs[1], runs[0], 'independent DATEFORMAT captures differ')
      } catch (error) {
        await writeFile(output, JSON.stringify({ image, independentContainers: runs.length, divergentRuns: runs }) + '\n', { flag: 'wx' })
        throw error
      }
    })
  }
  const actual = { image, independentContainers: 2, results: runs[0] }
  await writeFile(output, JSON.stringify(actual) + '\n', { flag: 'wx' })
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'fresh DATEFORMAT capture differs from retained fixture')
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${actual.results.length} DATEFORMAT observations in two independent containers${retained ? ' and matched the retained fixture' : ''}`)
}
