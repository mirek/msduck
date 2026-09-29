#!/usr/bin/env node
// Capture SQL Server's percentile window values, types and diagnostics.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { Request, TYPES } from 'tedious'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/percentile-reference.json', import.meta.url)
const fixtureSha256 = '57f1a731bd6f0713d5a985d17cd57f0e565db08cfda6be5d88c4bb5fdaf444cc'
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-percentile-reference.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/percentile-reference/capture.json')

async function canonicalOutput(path) {
  try { return await realpath(path) }
  catch (error) {
    if (error.code !== 'ENOENT') throw error
    return join(await realpath(dirname(path)), basename(path))
  }
}

async function existingFile(path, inspect = stat) {
  try { return await inspect(path) }
  catch (error) {
    if (error.code === 'ENOENT') return null
    throw error
  }
}

const ordered = (functionName, fraction, expression, source) =>
  `SELECT id,${functionName}(${fraction}) WITHIN GROUP (ORDER BY ${expression}) OVER () AS p FROM ${source} ORDER BY id`
const integerRows = '(VALUES (1,1),(2,2),(3,3),(4,4)) AS sample(id,n)'
const orderedInteger = (functionName, fraction = '.5', expression = 'n') =>
  ordered(functionName, fraction, expression, integerRows)
const plan = [
  { name: 'server version', sql: "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version" },
  { name: 'integer continuous median', sql: orderedInteger('PERCENTILE_CONT') },
  { name: 'integer discrete median', sql: orderedInteger('PERCENTILE_DISC') },
  { name: 'integer continuous quarter', sql: orderedInteger('PERCENTILE_CONT', '.25') },
  { name: 'decimal continuous median', sql: ordered('PERCENTILE_CONT', '.5', 'n',
    '(VALUES (1,CAST(1.00 AS DECIMAL(10,2))),(2,CAST(1.01 AS DECIMAL(10,2)))) AS sample(id,n)') },
  { name: 'decimal discrete median', sql: ordered('PERCENTILE_DISC', '.5', 'n',
    '(VALUES (1,CAST(1.00 AS DECIMAL(10,2))),(2,CAST(1.01 AS DECIMAL(10,2)))) AS sample(id,n)') },
  { name: 'float continuous median', sql: ordered('PERCENTILE_CONT', '.5', 'n',
    '(VALUES (1,CAST(0.1 AS FLOAT)),(2,CAST(0.2 AS FLOAT))) AS sample(id,n)') },
  ...['CONT', 'DISC'].flatMap(kind => [
    { name: `bit ${kind.toLowerCase()}`, sql: ordered(`PERCENTILE_${kind}`, '.5', 'n',
      '(VALUES (1,CAST(0 AS BIT)),(2,CAST(1 AS BIT))) AS sample(id,n)') },
    { name: `text ${kind.toLowerCase()}`, sql: ordered(`PERCENTILE_${kind}`, '.5', 'n',
      "(VALUES (1,CAST('a' AS VARCHAR(8))),(2,CAST('b' AS VARCHAR(8)))) AS sample(id,n)") },
    { name: `date ${kind.toLowerCase()}`, sql: ordered(`PERCENTILE_${kind}`, '.5', 'n',
      "(VALUES (1,CAST('2024-01-01' AS DATE)),(2,CAST('2024-01-02' AS DATE))) AS sample(id,n)") },
    { name: `ties NULL ${kind.toLowerCase()}`, sql: ordered(`PERCENTILE_${kind}`, '.5', 'n',
      '(VALUES (1,1),(2,1),(3,3),(4,NULL)) AS sample(id,n)') },
    { name: `descending zero ${kind.toLowerCase()}`, sql: ordered(`PERCENTILE_${kind}`, '0', 'n DESC', integerRows) },
    { name: `descending median ${kind.toLowerCase()}`, sql: ordered(`PERCENTILE_${kind}`, '.5', 'n DESC', integerRows) },
    { name: `empty ${kind.toLowerCase()}`, sql: `SELECT PERCENTILE_${kind}(.5) WITHIN GROUP (ORDER BY n) OVER () AS p FROM ${integerRows} WHERE 1=0` },
  ]),
  { name: 'partitioned continuous', sql: 'SELECT id,PERCENTILE_CONT(.5) WITHIN GROUP (ORDER BY n) OVER (PARTITION BY g) AS p FROM (VALUES (1,1,1),(2,1,2),(3,2,10),(4,2,NULL)) sample(id,g,n) ORDER BY id' },
  { name: 'partitioned discrete', sql: 'SELECT id,PERCENTILE_DISC(.5) WITHIN GROUP (ORDER BY n) OVER (PARTITION BY g) AS p FROM (VALUES (1,1,1),(2,1,2),(3,2,10),(4,2,NULL)) sample(id,g,n) ORDER BY id' },
  ...[
    ['negative fraction', '-0.1'], ['above one fraction', '1.1'],
    ['NULL fraction', 'NULL'], ['character fraction', "'0.5'"],
  ].map(([name, fraction]) => ({ name, sql: orderedInteger('PERCENTILE_CONT', fraction) })),
  { name: 'empty text continuous', sql: `SELECT PERCENTILE_CONT(.5) WITHIN GROUP (ORDER BY CAST(n AS VARCHAR(8))) OVER () AS p FROM ${integerRows} WHERE 1=0` },
  { name: 'session reusable', sql: 'SELECT 1 AS reusable' },
]
const preparedPlan = [
  { name: 'prepared integer continuous', type: TYPES.Int, values: [2, null, 4, 2],
    sql: ordered('PERCENTILE_CONT', '.5', '@b', integerRows) },
  { name: 'prepared text continuous', type: TYPES.NVarChar, options: { length: 8 }, values: ['a', null, 'b'],
    sql: ordered('PERCENTILE_CONT', '.5', '@b', integerRows) },
  { name: 'prepared text discrete', type: TYPES.NVarChar, options: { length: 8 }, values: ['a', null, 'b'],
    sql: ordered('PERCENTILE_DISC', '.5', '@b', integerRows) },
]

async function captureTokens(connection, action) {
  const doneTokens = []
  const raw = []
  const events = []
  const createParser = connection.createTokenStreamParser
  const readToken = StreamParser.prototype.readToken
  const recordRaw = (parser, type) => {
    assert(parser.options.tdsVersion >= '7_2', 'unexpected TDS version')
    const at = parser.position
    if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordRaw(parser, type))
    raw.push({ kind: doneKinds.get(type), status: parser.buffer.readUInt16LE(at), command: parser.buffer.readUInt16LE(at + 2) })
    return readToken.call(parser, type)
  }
  StreamParser.prototype.readToken = function (type) {
    return doneKinds.has(type) ? recordRaw(this, type) : readToken.call(this, type)
  }
  connection.createTokenStreamParser = function (message, handler) {
    const parser = createParser.call(this, message, handler)
    assert.equal(typeof parser.parser?.prependListener, 'function', 'Tedious token stream unavailable')
    parser.parser.prependListener('data', token => {
      if (typeof token.name === 'string') events.push({ kind: token.name })
      if (['DONE', 'DONEINPROC', 'DONEPROC'].includes(token.name)) doneTokens.push({
        kind: token.name, more: token.more, sqlError: token.sqlError,
        attention: token.attention, serverError: token.serverError,
        rowCount: token.rowCount ?? null, command: token.curCmd,
      })
    })
    return parser
  }
  try {
    const result = await action()
    assert.equal(doneTokens.length, result.done.length, 'incomplete decoded DONE tokens')
    assert.equal(raw.length, doneTokens.length, 'incomplete raw DONE words')
    for (let index = 0; index < doneTokens.length; index++) {
      assert.equal(raw[index].kind, doneTokens[index].kind, 'raw and decoded DONE kinds differ')
      assert.equal(raw[index].command, doneTokens[index].command, 'raw and decoded DONE commands differ')
      doneTokens[index].status = raw[index].status
    }
    return { ...result, doneTokens, events }
  } finally {
    connection.createTokenStreamParser = createParser
    StreamParser.prototype.readToken = readToken
  }
}

async function capturePreparedPhase(connection, request, issue, setComplete, prepared = false) {
  const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
  const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
  const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
  const onMetadata = metadata => result.sets.push({ columns: metadata.map(c => ({ name: c.colName, type: c.type.name,
    length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null,
    flags: c.flags, collation: canonical(c.collation ?? null) })), rows: [] })
  const onRow = row => result.sets.at(-1).rows.push(row.map(c => c.value))
  const done = Object.fromEntries(['done', 'doneInProc', 'doneProc'].map(kind => [kind, (rowCount, more, status) => {
    result.done.push({ kind, rowCount: rowCount ?? null, more })
    if (kind === 'doneProc') result.returnStatus = status
  }]))
  connection.on('errorMessage', onError); connection.on('infoMessage', onInfo)
  request.on('columnMetadata', onMetadata); request.on('row', onRow)
  for (const [kind, listener] of Object.entries(done)) request.on(kind, listener)
  try {
    return await captureTokens(connection, async () => {
      if (prepared) {
        await new Promise(resolve => {
          const success = () => { request.off('error', failure); resolve() }
          const failure = error => {
            request.off('prepared', success)
            if (!result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          }
          request.once('prepared', success)
          request.once('error', failure)
          issue()
        })
      } else {
        await new Promise(resolve => {
          request.error = undefined
          setComplete((error, rowCount) => {
            result.rowCount = rowCount
            if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
            resolve()
          })
          issue()
        })
      }
      return result
    })
  } finally {
    connection.off('errorMessage', onError); connection.off('infoMessage', onInfo)
    request.off('columnMetadata', onMetadata); request.off('row', onRow)
    for (const [kind, listener] of Object.entries(done)) request.off(kind, listener)
  }
}

async function observe(connection) {
  const records = []
  for (const { name, sql } of plan) {
    const result = canonical(await captureTokens(connection, () => capture(connection, sql)))
    records.push({ name, sql, mode: 'batch', result })
    console.log(name)
  }
  for (const { name, sql, type, options, values } of preparedPlan) {
    let complete = () => {}
    const setComplete = callback => { complete = callback }
    const request = new Request(sql, (error, rowCount) => complete(error, rowCount))
    request.addParameter('b', type, undefined, options)
    const preparation = canonical(await capturePreparedPhase(connection, request, () => connection.prepare(request), setComplete, true))
    const executions = []
    let unpreparation = null
    if (!preparation.errors.length) {
      try {
        for (const value of values) {
          const result = canonical(await capturePreparedPhase(connection, request,
            () => connection.execute(request, { b: value }), setComplete))
          executions.push({ value, result })
        }
      } finally {
        unpreparation = canonical(await capturePreparedPhase(connection, request,
          () => connection.unprepare(request), setComplete))
      }
    }
    records.push({ name, sql, mode: 'prepared', type: type.name, options: options ?? null,
      values, preparation, executions, unpreparation })
    console.log(name)
  }
  return records
}

function validate(run) {
  assertSameCapture(run.map(({ name, sql, mode, type, options, values }) =>
    ({ name, sql, mode, ...(mode === 'prepared' ? { type, options, values } : {}) })),
  [...plan.map(({ name, sql }) => ({ name, sql, mode: 'batch' })),
    ...preparedPlan.map(({ name, sql, type, options, values }) =>
      ({ name, sql, mode: 'prepared', type: type.name, options: options ?? null, values }))],
  'capture plan changed')
  const get = name => {
    const record = run.find(item => item.name === name)
    assert(record, `missing ${name}`)
    return record.result
  }
  const validateResult = (name, result) => {
    for (const key of ['sets', 'done', 'doneTokens', 'events', 'errors', 'info']) assert(Array.isArray(result[key]), `${name}: missing ${key}`)
    assert(result.done.length > 0, `${name}: missing completion`)
    assert.equal(result.doneTokens.length, result.done.length, `${name}: incomplete completion`)
    for (const token of result.doneTokens) assert(Number.isInteger(token.status), `${name}: missing DONE status`)
    for (const set of result.sets) {
      assert(Array.isArray(set.columns) && Array.isArray(set.rows), `${name}: incomplete result set`)
      for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
    }
  }
  for (const record of run) {
    if (record.mode === 'batch') validateResult(record.name, record.result)
    else {
      validateResult(`${record.name} prepare`, record.preparation)
      for (const [index, execution] of record.executions.entries()) {
        assertSameCapture(execution.value, record.values[index], `${record.name}: rebound value changed`)
        validateResult(`${record.name} execute ${index}`, execution.result)
      }
      assert.equal(record.executions.length, record.preparation.errors.length ? 0 : record.values.length,
        `${record.name}: missing prepared execution`)
      if (record.unpreparation) validateResult(`${record.name} unprepare`, record.unpreparation)
      else assert(record.preparation.errors.length, `${record.name}: missing unprepare`)
    }
  }
  assertSameCapture(get('session reusable').sets.map(set => set.rows), [[[1]]], 'session not reusable')
  assertSameCapture(get('server version').sets[0].rows, [['17.0.4065.4']], 'server version changed')
  const rows = result => result.sets.map(set => set.rows)
  const shape = result => result.sets.map(set => set.columns.map(({ type, length, precision, scale, flags }) =>
    [type, length, precision, scale, flags]))
  const diagnostics = result => result.errors.map(({ number, state, class: severity, message }) =>
    [number, state, severity, message])
  const events = result => result.events.map(event => event.kind)
  const done = result => result.doneTokens.map(({ kind, status, command }) => [kind, status, command])
  const checkRows = (name, expected) => assertSameCapture(rows(get(name)), expected, `${name}: rows changed`)
  const checkType = (name, type) => assert.equal(get(name).sets[0].columns.at(-1).type, type, `${name}: type changed`)
  for (const [name, value, count] of [
    ['integer continuous median', 2.5, 4], ['integer continuous quarter', 1.75, 4],
    ['decimal continuous median', 1.005, 2], ['float continuous median', 0.15000000000000002, 2],
    ['bit cont', 0.5, 2], ['descending zero cont', 4, 4], ['descending median cont', 2.5, 4],
  ]) {
    checkRows(name, [[1, 2, 3, 4].slice(0, count).map(id => [id, value])])
    checkType(name, 'FloatN')
  }
  for (const [name, value, type, count] of [
    ['integer discrete median', 2, 'Int', 4], ['decimal discrete median', 1, 'DecimalN', 2],
    ['bit disc', false, 'BitN', 2], ['text disc', 'a', 'VarChar', 2],
    ['descending zero disc', 4, 'Int', 4], ['descending median disc', 3, 'Int', 4],
  ]) {
    checkRows(name, [[1, 2, 3, 4].slice(0, count).map(id => [id, value])])
    checkType(name, type)
  }
  checkRows('partitioned continuous', [[[1, 1.5], [2, 1.5], [3, 10], [4, 10]]])
  checkRows('partitioned discrete', [[[1, 1], [2, 1], [3, 10], [4, 10]]])
  checkRows('ties NULL cont', [[[1, 1], [2, 1], [3, 1], [4, 1]]])
  checkRows('ties NULL disc', [[[1, 1], [2, 1], [3, 1], [4, 1]]])
  checkRows('empty cont', [[]]); checkType('empty cont', 'FloatN')
  checkRows('empty disc', [[]]); checkType('empty disc', 'Int')
  assertSameCapture(shape(get('decimal discrete median'))[0][1], ['DecimalN', 17, 10, 2, 1], 'decimal descriptor changed')
  assertSameCapture(shape(get('ties NULL disc'))[0][1], ['IntN', 4, null, null, 1], 'nullable descriptor changed')
  for (const [name, datatype] of [['text cont', 'varchar'], ['date cont', 'date'], ['empty text continuous', 'varchar']]) {
    const result = get(name)
    assertSameCapture(diagnostics(result), [[402, 1, 16,
      `The data types numeric and ${datatype} are incompatible in the percentile_cont operator.`]], `${name}: diagnostic changed`)
    assertSameCapture(shape(result), [], `${name}: unexpected metadata`)
    assertSameCapture(events(result), ['ERROR', 'DONE'], `${name}: events changed`)
    assertSameCapture(done(result), [['DONE', 2, 253]], `${name}: completion changed`)
  }
  for (const name of ['negative fraction', 'above one fraction', 'NULL fraction']) {
    const result = get(name)
    assertSameCapture(diagnostics(result), [[8727, 1, 16,
      'Input parameter of percentile function is outside of range [0, 1].']], `${name}: diagnostic changed`)
    assertSameCapture(shape(result)[0][1], ['FloatN', 8, null, null, 1], `${name}: descriptor changed`)
    assertSameCapture(rows(result), [[]], `${name}: unexpected rows`)
    assertSameCapture(events(result), ['COLMETADATA', 'ORDER', 'ERROR', 'DONE'], `${name}: events changed`)
    assertSameCapture(done(result), [['DONE', 2, 193]], `${name}: completion changed`)
  }
  checkRows('character fraction', [[[1, 2.5], [2, 2.5], [3, 2.5], [4, 2.5]]])
  const integer = run.find(record => record.name === 'prepared integer continuous')
  assertSameCapture(shape(integer.preparation)[0][1], ['FloatN', 8, null, null, 1], 'INT prepare descriptor changed')
  assertSameCapture(integer.executions.map(({ result }) => rows(result)),
    [2, null, 4, 2].map(value => [[1, 2, 3, 4].map(id => [id, value])]), 'INT rebinding changed')
  const textContinuous = run.find(record => record.name === 'prepared text continuous')
  assertSameCapture(diagnostics(textContinuous.preparation), [
    [402, 1, 16, 'The data types numeric and nvarchar are incompatible in the percentile_cont operator.'],
    [8180, 1, 16, 'Statement(s) could not be prepared.'],
  ], 'text continuous preparation changed')
  assertSameCapture(events(textContinuous.preparation),
    ['ERROR', 'ERROR', 'RETURNSTATUS', 'RETURNVALUE', 'DONEPROC'], 'text continuous events changed')
  assertSameCapture(done(textContinuous.preparation), [['DONEPROC', 2, 224]], 'text continuous completion changed')
  const textDiscrete = run.find(record => record.name === 'prepared text discrete')
  assertSameCapture(shape(textDiscrete.preparation)[0][1], ['NVarChar', 16, null, null, 1], 'text discrete descriptor changed')
  assertSameCapture(textDiscrete.executions.map(({ result }) => rows(result)),
    ['a', null, 'b'].map(value => [[1, 2, 3, 4].map(id => [id, value])]), 'text discrete rebinding changed')
  assertSameCapture(events(textDiscrete.executions[1].result),
    ['COLMETADATA', 'ORDER', 'NBCROW', 'NBCROW', 'NBCROW', 'NBCROW', 'DONEINPROC', 'RETURNSTATUS', 'DONEPROC'],
    'text NULL row encoding changed')
  for (const record of [integer, textDiscrete]) {
    assertSameCapture(events(record.preparation),
      ['COLMETADATA', 'ORDER', 'DONEINPROC', 'RETURNSTATUS', 'RETURNVALUE', 'DONEPROC'],
      `${record.name}: prepare events changed`)
    assertSameCapture(done(record.preparation), [['DONEINPROC', 17, 193], ['DONEPROC', 0, 224]],
      `${record.name}: prepare completion changed`)
    for (const { result } of record.executions) {
      assertSameCapture(done(result), [['DONEINPROC', 17, 193], ['DONEPROC', 0, 224]],
        `${record.name}: execution completion changed`)
    }
    assertSameCapture(events(record.unpreparation), ['RETURNSTATUS', 'DONEPROC'],
      `${record.name}: unprepare events changed`)
    assertSameCapture(done(record.unpreparation), [['DONEPROC', 0, 224]],
      `${record.name}: unprepare completion changed`)
  }
}

async function checkFixture() {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'reference repetitions missing')
  for (const run of retained.runs) validate(run)
  assertSameCapture(retained.runs[0], retained.runs[1], 'reference runs differ')
  console.log(`Checked ${retained.runs[0].length} percentile observations in two retained runs`)
}

if (check) await checkFixture()
else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if ((await existingFile(output, lstat))?.isSymbolicLink()) {
    throw Error('capture output must not be a symbolic link')
  }
  const fixturePath = fileURLToPath(fixture)
  if (await canonicalOutput(output) === await canonicalOutput(fixturePath)) {
    throw Error('capture output must not be the retained fixture')
  }
  const [outputFile, retainedFile] = await Promise.all([existingFile(output), existingFile(fixturePath)])
  if (outputFile && retainedFile && outputFile.dev === retainedFile.dev && outputFile.ino === retainedFile.ino) {
    throw Error('capture output must not be a hard link to the retained fixture')
  }
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) {
    throw Error('reference image must be pinned')
  }
  await withReferenceContainer(async (config, container) => {
    assert.equal(container.image, referenceImage, 'reference image must be pinned')
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      const run = await isolatedReference(config, observe)
      runs.push(run)
      await writeFile(output, JSON.stringify({ image: container.image, runs }) + '\n')
      validate(run)
    }
    assertSameCapture(runs[0], runs[1], 'fresh-database captures differ')
    const actual = { image: container.image, runs }
    await writeFile(output, JSON.stringify(actual) + '\n')
    let retained
    try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    if (retained) {
      assert.equal(actual.image, retained.image, 'reference image changed')
      assertSameCapture(actual.runs, retained.runs, 'live capture differs from retained fixture')
    }
    if (writeFixture) await writeNewFixture(fixture, actual)
    console.log(`Captured ${runs[0].length} percentile observations in two fresh databases`)
  })
}
