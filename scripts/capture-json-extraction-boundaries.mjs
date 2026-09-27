#!/usr/bin/env node
// Owner-run SQL Server evidence for JSON extraction validation order.
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'

const fixture = new URL('../reference/json-extraction-boundaries.json', import.meta.url)
const writeFixture = process.argv.includes('--write-fixture')
const positional = process.argv.slice(2).filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one scratch output path')
const output = resolve(positional[0] ?? 'artifacts/json-extraction-boundaries/capture.json')
async function canonicalPath(path) {
  const absolute = resolve(path)
  try { return await realpath(absolute) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    const parent = dirname(absolute)
    return parent === absolute ? absolute : resolve(await canonicalPath(parent), basename(absolute))
  }
}
async function identity(path) {
  try { const value = await stat(path); return `${value.dev}:${value.ino}` }
  catch (error) { if (error.code === 'ENOENT') return null; throw error }
}
const fixturePath = fileURLToPath(fixture)
let outputIsSymlink = false
try { outputIsSymlink = (await lstat(output)).isSymbolicLink() }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (outputIsSymlink ||
    await canonicalPath(output) === await canonicalPath(fixturePath) ||
    (await identity(fixturePath) !== null && await identity(output) === await identity(fixturePath))) {
  throw new Error('refusing to overwrite retained fixture ' + fixturePath)
}
if (writeFixture) await refuseExistingFixture(fixture)

const cases = [
  ['valid object scalar', `N'{"a":1}'`, `N'$.a'`],
  ['valid object fragment', `N'{"a":{"x":1}}'`, `N'$.a'`],
  ['valid array fragment', `N'{"a":[1,2]}'`, `N'$.a'`],
  ['valid root object', `N'{"a":1}'`, `N'$'`],
  ['valid root scalar', `N'1'`, `N'$'`],
  ['valid root string', `N'"text"'`, `N'$'`],
  ['valid root null', `N'null'`, `N'$'`],
  ['valid duplicate', `N'{"a":1,"a":2}'`, `N'$.a'`],
  ['missing property', `N'{"a":1}'`, `N'$.missing'`],
  ['wrong kind property', `N'{"a":1}'`, `N'$.a.x'`],
  ['wrong kind selected scalar', `N'{"a":"text"}'`, `N'$.a'`],
  ['wrong kind selected object', `N'{"a":{"x":1}}'`, `N'$.a'`],
  ['invalid prefix', `N'{"a":x,"b":1}'`, `N'$.b'`],
  ['invalid suffix after match', `N'{"a":1,"b":x}'`, `N'$.a'`],
  ['invalid suffix after wrong kind', `N'{"a":{},"b":x}'`, `N'$.a'`],
  ['invalid suffix after missing path', `N'{"a":1,"b":x}'`, `N'$.missing'`],
  ['truncated selected object', `N'{"a":{"x":1'`, `N'$.a'`],
  ['truncated parent after scalar', `N'{"a":1'`, `N'$.a'`],
  ['truncated parent after object', `N'{"a":{}'`, `N'$.a'`],
  ['trailing text after root', `N'{"a":1} x'`, `N'$.a'`],
  ['array malformed later member', `N'[1,x]'`, `N'$[0]'`],
  ['array malformed earlier member', `N'[x,1]'`, `N'$[1]'`],
  ['array truncated after first', `N'[1'`, `N'$[0]'`],
  ['array missing index', `N'[1,2]'`, `N'$[2]'`],
  ['empty document', `N''`, `N'$'`],
  ['null document', `CAST(NULL AS NVARCHAR(MAX))`, `N'$.a'`],
  ['null path', `N'{"a":1}'`, `CAST(NULL AS NVARCHAR(100))`],
  ['strict missing', `N'{"a":1}'`, `N'strict $.missing'`],
  ['strict wrong kind property', `N'{"a":1}'`, `N'strict $.a.x'`],
  ['strict wrong kind selected scalar', `N'{"a":"text"}'`, `N'strict $.a'`],
  ['strict wrong kind selected object', `N'{"a":{"x":1}}'`, `N'strict $.a'`],
  ['strict wrong kind with invalid suffix', `N'{"a":{},"b":x}'`, `N'strict $.a'`],
  ['strict missing with invalid suffix', `N'{"a":1,"b":x}'`, `N'strict $.missing'`],
  ['strict truncated parent after scalar', `N'{"a":1'`, `N'strict $.a'`],
  ['strict root scalar', `N'1'`, `N'strict $'`],
  ['invalid path missing bracket', `N'{"a":[1]}'`, `N'$.a[0'`],
  ['wildcard path', `N'{"a":[1]}'`, `N'$.a[*]'`],
  ['long scalar exact limit', `N'{"a":"' + REPLICATE(CAST(N'x' AS NVARCHAR(MAX)),4000) + N'"}'`, `N'$.a'`],
  ['long scalar over limit', `N'{"a":"' + REPLICATE(CAST(N'x' AS NVARCHAR(MAX)),4001) + N'"}'`, `N'$.a'`],
  ['long scalar with invalid suffix', `N'{"a":"' + REPLICATE(CAST(N'x' AS NVARCHAR(MAX)),4001) + N'","b":x}'`, `N'$.a'`],
]
const programs = cases.flatMap(([name, document, path]) =>
  ['JSON_VALUE', 'JSON_QUERY'].map(fn => ({ name, fn, sql: `SELECT ${fn}(${document},${path}) AS value` })))

function captureOrdered(connection, sql) {
  return new Promise(resolve => {
    const result = { sets: [], done: [], errors: [], info: [], events: [], returnStatus: null }
    const request = new Request(sql, (error, rowCount) => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.rowCount = rowCount
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      resolve(result)
    })
    const onError = error => {
      result.events.push({ kind: 'ERROR', number: error.number })
      result.errors.push({ number: error.number, state: error.state, class: error.class, lineNumber: error.lineNumber, message: error.message })
    }
    const onInfo = info => {
      result.events.push({ kind: 'INFO', number: info.number })
      result.info.push({ number: info.number, state: info.state, class: info.class, lineNumber: info.lineNumber, message: info.message })
    }
    connection.on('infoMessage', onInfo)
    connection.on('errorMessage', onError)
    request.on('columnMetadata', metadata => {
      result.events.push({ kind: 'COLMETADATA', columns: metadata.length })
      result.sets.push({
        columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })),
        rows: [],
      })
    })
    request.on('row', row => {
      result.events.push({ kind: 'ROW' })
      result.sets.at(-1).rows.push(row.map(c => c.value))
    })
    for (const name of ['done', 'doneInProc', 'doneProc']) request.on(name, (rowCount, more) => {
      result.events.push({ kind: name.toUpperCase(), rowCount: rowCount ?? null, more })
      result.done.push({ kind: name, rowCount: rowCount ?? null, more })
    })
    request.on('doneProc', (_count, _more, status) => { result.returnStatus = status })
    try { connection.execSqlBatch(request) }
    catch (error) {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.errors.push({ message: error.message, number: error.number ?? null })
      resolve(result)
    }
  })
}

async function observe(connection) {
  const records = []
  for (const { name, fn, sql } of programs) {
    const result = canonical(await captureOrdered(connection, sql))
    if (result.sets.length > 1 || result.sets.some(set => set.rows.length > 1) ||
        result.errors.length > 4 || result.done.length > 8 || result.events.length > 16 ||
        JSON.stringify(result).length > 20000) throw new Error(`unbounded capture: ${name}/${fn}`)
    records.push({ name, fn, sql, result })
  }
  return records
}

const containers = []
await withReferenceContainer(async (config, container) => {
  const runs = [await isolatedReference(config, observe), await isolatedReference(config, observe)]
  assertSameCapture(runs[0], runs[1], 'JSON extraction observations differ across fresh databases')
  containers.push({ image: container.image, runs })
})
const actual = { containers }
const retained = writeFixture ? null : JSON.parse(await readFile(fixture, 'utf8'))
if (retained) assertSameCapture(actual, retained, 'JSON extraction observations differ from retained fixture')
await mkdir(dirname(output), { recursive: true })
await writeFile(output, JSON.stringify(actual) + '\n')
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${programs.length} JSON extraction observations twice${retained ? ' and matched retained fixture' : ''}`)
