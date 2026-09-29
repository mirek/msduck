#!/usr/bin/env node
// Independent SQL Server evidence for contextual bare integer arithmetic.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, join, resolve } from 'node:path'
import { Request, TYPES } from 'tedious'
import { fileURLToPath } from 'node:url'
import { capture, canonical } from './lib/compatibility.mjs'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/numeric-arithmetic-context.json', import.meta.url)
const fixtureSha256 = '4151c73088b502858b4184df746995242b142c4c4fb71f240e9e14643e37e3ea'
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-numeric-arithmetic-context.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/numeric-arithmetic-context/capture.json')

const show = expression => `SELECT ${expression} AS value,CONVERT(VARCHAR(100),${expression}) AS rendered`
const plan = [
  ['server version', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version"],
  ['bare large over small', show('2147483649/2')],
  ['bare large over leading-zero small', show('2147483649/0002')],
  ['bare large over nine', show('2147483649/9')],
  ['bare large over ten', show('2147483649/10')],
  ['bare large over ninety-nine', show('2147483649/99')],
  ['bare large over hundred', show('2147483649/100')],
  ['bare large over million', show('2147483649/1000000')],
  ['bare large over billion', show('2147483649/1000000000')],
  ['bare large over int maximum', show('2147483649/2147483647')],
  ['int maximum over bare large', show('2147483647/2147483649')],
  ['ten over bare large', show('10/2147483649')],
  ['billion over bare large', show('1000000000/2147483649')],
  ['bare large over next numeric', show('2147483649/2147483648')],
  ['bare large over negative small', show('2147483649/-2')],
  ['bare decimal over small', show('1.0/2')],
  ['small over bare large', show('2/2147483649')],
  ['explicit decimal over bare small', show('CAST(2147483649 AS DECIMAL(10,0))/2')],
  ['explicit numeric over bare small', show('CAST(2147483649 AS NUMERIC(10,0))/2')],
  ['explicit decimal over cast int', show('CAST(2147483649 AS DECIMAL(10,0))/CAST(2 AS INT)')],
  ['explicit numeric over cast int', show('CAST(2147483649 AS NUMERIC(10,0))/CAST(2 AS INT)')],
  ['explicit decimal over leading-zero small', show('CAST(2147483649 AS DECIMAL(10,0))/0002')],
  ['short decimal over bare small', show('CAST(2 AS DECIMAL(5,2))/3')],
  ['short numeric over bare small', show('CAST(2 AS NUMERIC(5,2))/3')],
  ['short decimal over cast int', show('CAST(2 AS DECIMAL(5,2))/CAST(3 AS INT)')],
  ['bare decimal multiplied by small', show('1.0*2')],
  ['explicit decimal multiplied by small', show('CAST(1.0 AS DECIMAL(2,1))*2')],
  ['bare decimal plus small', show('1.0+2')],
  ['explicit decimal plus small', show('CAST(1.0 AS DECIMAL(2,1))+2')],
  ['chained bare division', show('2147483649/2/2')],
  ['nested bare division', show('2147483649/(2/2)')],
  ['empty contextual metadata', `${show('CAST(2147483649 AS DECIMAL(10,0))/2')} WHERE 1=0`],
  ['invalid numeric text conversion', "SELECT CAST('not-a-number' AS NUMERIC(10,0)) AS value"],
  ['recovery after conversion error', 'SELECT 1 AS reusable'],
]
const preparedSql = show('CAST(2147483649 AS DECIMAL(10,0))/@divisor')
const preparedValues = [2, 3, null, 2]

function descriptor(column) {
  return { name: column.colName, type: column.type.name, length: column.dataLength ?? null,
    precision: column.precision ?? null, scale: column.scale ?? null, flags: column.flags,
    collation: canonical(column.collation ?? null) }
}

async function preparedCapture(connection) {
  const runs = []
  let result
  let complete = () => {}
  const request = new Request(preparedSql, (error, rowCount) => complete(error, rowCount))
  request.addParameter('divisor', TYPES.Int, undefined)
  const fields = error => ({ number: error.number, state: error.state, class: error.class,
    lineNumber: error.lineNumber, message: error.message })
  const onError = error => result?.errors.push(fields(error))
  const onInfo = info => result?.info.push(fields(info))
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(descriptor), rows: [] }))
  request.on('row', row => result?.sets.at(-1).rows.push(row.map(cell => cell.value)))
  for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => {
    result?.done.push({ kind, rowCount: rowCount ?? null, more })
  })
  request.on('doneProc', (_count, _more, status) => { if (result) result.returnStatus = status })
  try {
    await new Promise((done, fail) => {
      complete = error => error ? fail(error) : done()
      request.once('prepared', done)
      request.once('error', fail)
      connection.prepare(request)
    })
    for (const value of preparedValues) {
      result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
      await new Promise((done, fail) => {
        complete = (error, rowCount) => { result.rowCount = rowCount; error ? fail(error) : done() }
        request.error = undefined
        connection.execute(request, { divisor: value })
      })
      runs.push({ divisor: value, result: canonical(result) })
    }
    result = undefined
    await new Promise((done, fail) => {
      complete = error => error ? fail(error) : done()
      connection.unprepare(request)
    })
    return runs
  } finally {
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}

function validate(run) {
  assertSameCapture(run.batches.map(({ name, sql }) => [name, sql]), plan, 'capture plan changed')
  assert.equal(run.prepared.sql, preparedSql)
  assertSameCapture(run.prepared.runs.map(({ divisor }) => divisor), preparedValues, 'prepared bindings changed')
  for (const { name, result } of run.batches) {
    assert(Array.isArray(result.sets) && Array.isArray(result.done) && Array.isArray(result.errors) && Array.isArray(result.info), `${name}: incomplete capture`)
    assert(result.done.length > 0, `${name}: missing completion token`)
    assert.equal(result.errors.length, name === 'invalid numeric text conversion' ? 1 : 0, `${name}: unexpected errors`)
    for (const set of result.sets) for (const column of set.columns) assert(typeof column.type === 'string' && typeof column.flags === 'number', `${name}: incomplete descriptor`)
  }
  const get = name => run.batches.find(entry => entry.name === name).result.sets[0]
  assertSameCapture(get('server version').rows, [['17.0.4065.4']], 'reference image version changed')
  assertSameCapture(get('recovery after conversion error').rows, [[1]], 'session did not recover')
  const kind = name => {
    const { type, precision, scale } = get(name).columns[0]
    return [type, precision, scale]
  }
  assert.equal(get('empty contextual metadata').rows.length, 0)
  const error = run.batches.find(entry => entry.name === 'invalid numeric text conversion').result.errors[0]
  assertSameCapture([error.number, error.state, error.class], [8114, 5, 16], 'conversion diagnostic changed')
  for (const name of plan.map(([name]) => name).filter(name => !['server version', 'invalid numeric text conversion', 'recovery after conversion error'].includes(name))) {
    const [type, precision, scale] = kind(name)
    assert(type === 'DecimalN' || type === 'NumericN' || type === 'IntN', `${name}: unexpected type`)
    if (type !== 'IntN') assert(Number.isInteger(precision) && Number.isInteger(scale), `${name}: incomplete numeric shape`)
  }
  for (const [name, expected] of [
    ['bare large over small', ['NumericN', 16, 6]],
    ['bare large over leading-zero small', ['NumericN', 16, 6]],
    ['bare large over ninety-nine', ['NumericN', 16, 6]],
    ['bare large over hundred', ['NumericN', 16, 6]],
    ['bare large over million', ['NumericN', 18, 8]],
    ['bare large over billion', ['NumericN', 21, 11]],
    ['bare large over int maximum', ['NumericN', 21, 11]],
    ['int maximum over bare large', ['NumericN', 21, 11]],
    ['ten over bare large', ['NumericN', 13, 11]],
    ['billion over bare large', ['NumericN', 21, 11]],
    ['bare large over next numeric', ['NumericN', 21, 11]],
    ['bare decimal over small', ['NumericN', 7, 6]],
    ['small over bare large', ['NumericN', 12, 11]],
    ['explicit decimal over bare small', ['DecimalN', 16, 6]],
    ['explicit numeric over bare small', ['NumericN', 16, 6]],
    ['explicit decimal over cast int', ['DecimalN', 21, 11]],
    ['explicit numeric over cast int', ['NumericN', 21, 11]],
    ['short decimal over bare small', ['DecimalN', 9, 6]],
    ['short numeric over bare small', ['NumericN', 9, 6]],
    ['short decimal over cast int', ['DecimalN', 16, 13]],
    ['bare decimal multiplied by small', ['NumericN', 4, 1]],
    ['explicit decimal multiplied by small', ['DecimalN', 4, 1]],
    ['bare decimal plus small', ['NumericN', 3, 1]],
    ['explicit decimal plus small', ['DecimalN', 3, 1]],
    ['chained bare division', ['NumericN', 18, 8]],
    ['nested bare division', ['NumericN', 21, 11]],
    ['empty contextual metadata', ['DecimalN', 16, 6]],
  ]) assertSameCapture(kind(name), expected, `${name}: declaration changed`)
  assertSameCapture(get('explicit decimal over bare small').rows, [[1073741824.5, '1073741824.500000']], 'bare divisor quotient changed')
  assertSameCapture(get('explicit decimal over cast int').rows, [[1073741824.5, '1073741824.50000000000']], 'cast divisor quotient changed')
  for (const { divisor, result } of run.prepared.runs) {
    assert(Array.isArray(result.sets) && Array.isArray(result.done) && Array.isArray(result.errors) && Array.isArray(result.info), `prepared ${divisor}: incomplete capture`)
    assert.equal(result.errors.length, 0, `prepared ${divisor}: unexpected error`)
    assert.equal(result.sets.length, 1, `prepared ${divisor}: missing result`)
    assert.equal(result.sets[0].rows.length, 1, `prepared ${divisor}: missing row`)
    const { type, precision, scale } = result.sets[0].columns[0]
    assertSameCapture([type, precision, scale], ['DecimalN', 21, 11], `prepared ${divisor}: declaration changed`)
  }
}

async function observe(connection) {
  const batches = []
  for (const [name, sql] of plan) {
    batches.push({ name, sql, result: canonical(await capture(connection, sql)) })
    console.log(name)
  }
  const run = { batches, prepared: { sql: preparedSql, runs: await preparedCapture(connection) } }
  validate(run)
  return run
}

async function existingFile(path, inspect = stat) {
  try { return await inspect(path) } catch (error) { if (error.code === 'ENOENT') return null; throw error }
}
async function canonicalOutput(path) {
  try { return await realpath(path) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return join(await realpath(dirname(path)), basename(path))
  }
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
  console.log(`Checked ${retained.runs[0].batches.length} batch and ${retained.runs[0].prepared.runs.length} prepared observations in two retained runs`)
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
  console.log(`Captured ${runs[0].batches.length} batch and ${runs[0].prepared.runs.length} prepared observations in two fresh containers${retained ? '; matched fixture' : ''}`)
}
