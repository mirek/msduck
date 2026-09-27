#!/usr/bin/env node
// Owner-run SQL Server evidence for JSON extraction ranges and advanced path diagnostics.
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'

const fixture = new URL('../reference/json-advanced-path.json', import.meta.url)
const writeFixture = process.argv.includes('--write-fixture')
const positional = process.argv.slice(2).filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one scratch output path')
const output = resolve(positional[0] ?? 'artifacts/json-advanced-path/capture.json')
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
  ['range first singleton', `N'[1,2,3]'`, `N'$[0 to 0]'`],
  ['range second singleton', `N'[1,2,3]'`, `N'$[1 to 1]'`],
  ['range third singleton', `N'[1,2,3]'`, `N'$[2 to 2]'`],
  ['range first two', `N'[1,2,3]'`, `N'$[0 to 1]'`],
  ['range last two', `N'[1,2,3]'`, `N'$[1 to 2]'`],
  ['range all three', `N'[1,2,3]'`, `N'$[0 to 2]'`],
  ['range beyond end singleton', `N'[1,2,3]'`, `N'$[9 to 9]'`],
  ['range partial beyond end', `N'[1,2,3]'`, `N'$[2 to 9]'`],
  ['range entirely beyond end', `N'[1,2,3]'`, `N'$[9 to 10]'`],
  ['range reversed', `N'[1,2,3]'`, `N'$[2 to 0]'`],
  ['range reverse adjacent', `N'[1,2,3]'`, `N'$[1 to 0]'`],
  ['range empty array', `N'[]'`, `N'$[0 to 0]'`],
  ['range one array element', `N'[1]'`, `N'$[0 to 1]'`],
  ['range one object', `N'[{"a":1},{"a":2}]'`, `N'$[0 to 0]'`],
  ['range two objects', `N'[{"a":1},{"a":2}]'`, `N'$[0 to 1]'`],
  ['range one nested array', `N'[[1,2],[3,4]]'`, `N'$[0 to 0]'`],
  ['range null singleton', `N'[null,2]'`, `N'$[0 to 0]'`],
  ['range null plus number', `N'[null,2]'`, `N'$[0 to 1]'`],
  ['range number plus null', `N'[1,null]'`, `N'$[0 to 1]'`],
  ['range property singleton', `N'{"a":[1,2]}'`, `N'$.a[1 to 1]'`],
  ['range property multiple', `N'{"a":[1,2]}'`, `N'$.a[0 to 1]'`],
  ['range property missing', `N'{"b":[1,2]}'`, `N'$.a[0 to 0]'`],
  ['range on scalar property', `N'{"a":1}'`, `N'$.a[0 to 0]'`],
  ['range nested key first', `N'[{"a":1},{"a":2}]'`, `N'$[0 to 0].a'`],
  ['range nested key multiple', `N'[{"a":1},{"a":2}]'`, `N'$[0 to 1].a'`],
  ['range nested first absent', `N'[{"b":0},{"a":2}]'`, `N'$[0 to 1].a'`],
  ['range after wildcard', `N'[[1,2],[3,4]]'`, `N'$[*][0 to 0]'`],
  ['range then wildcard', `N'[[1,2],[3,4]]'`, `N'$[0 to 0][*]'`],
  ['strict singleton', `N'[1,2]'`, `N'strict $[0 to 0]'`],
  ['strict multiple', `N'[1,2]'`, `N'strict $[0 to 1]'`],
  ['strict missing', `N'[1,2]'`, `N'strict $[9 to 9]'`],
  ['strict one object', `N'[{"a":1}]'`, `N'strict $[0 to 0]'`],
  ['strict partial range scalar', `N'[1]'`, `N'strict $[0 to 2]'`],
  ['strict partial zero to one', `N'[1]'`, `N'strict $[0 to 1]'`],
  ['strict partial zero to three', `N'[1]'`, `N'strict $[0 to 3]'`],
  ['strict partial two element zero to two', `N'[1,2]'`, `N'strict $[0 to 2]'`],
  ['strict partial two element one to two', `N'[1,2]'`, `N'strict $[1 to 2]'`],
  ['strict partial three element zero to three', `N'[1,2,3]'`, `N'strict $[0 to 3]'`],
  ['strict partial three element one to three', `N'[1,2,3]'`, `N'strict $[1 to 3]'`],
  ['strict partial three element two to four', `N'[1,2,3]'`, `N'strict $[2 to 4]'`],
  ['strict empty wider range', `N'[]'`, `N'strict $[0 to 1]'`],
  ['strict partial range object', `N'[{"a":1}]'`, `N'strict $[0 to 2]'`],
  ['strict partial range null', `N'[null]'`, `N'strict $[0 to 2]'`],
  ['strict null singleton', `N'[null,2]'`, `N'strict $[0 to 0]'`],
  ['strict null plus number', `N'[null,2]'`, `N'strict $[0 to 1]'`],
  ['strict reversed', `N'[1,2]'`, `N'strict $[1 to 0]'`],
  ['malformed before range', `N'[x,1]'`, `N'$[0 to 1]'`],
  ['malformed before singleton range', `N'[x,1]'`, `N'$[1 to 1]'`],
  ['malformed after range', `N'[1,x]'`, `N'$[0 to 0]'`],
  ['malformed inside range', `N'[1,x]'`, `N'$[0 to 1]'`],
  ['malformed after two range matches', `N'[1,2,x]'`, `N'$[0 to 1]'`],
  ['malformed after two strict matches', `N'[1,2,x]'`, `N'strict $[0 to 1]'`],
  ['missing range before malformed tail', `N'[1,x]'`, `N'$[9 to 9]'`],
  ['reversed range with malformed input', `N'[x]'`, `N'$[1 to 0]'`],
  ['last with malformed input', `N'[x]'`, `N'$[last]'`],
  ['list with malformed input', `N'[x]'`, `N'$[0,1]'`],
  ['truncated after range', `N'[1'`, `N'$[0 to 0]'`],
  ['uppercase TO', `N'[1,2]'`, `N'$[0 TO 0]'`],
  ['no range spaces', `N'[1,2]'`, `N'$[0to0]'`],
  ['space before bracket close', `N'[1,2]'`, `N'$[0 to 0 ]'`],
  ['space after bracket open', `N'[1,2]'`, `N'$[ 0 to 0]'`],
  ['negative start', `N'[1,2]'`, `N'$[-1 to 0]'`],
  ['negative end', `N'[1,2]'`, `N'$[0 to -1]'`],
  ['large endpoint', `N'[1,2]'`, `N'$[0 to 999999999999999999999]'`],
  ['last singleton', `N'[1,2]'`, `N'$[last]'`],
  ['last on empty', `N'[]'`, `N'$[last]'`],
  ['last with offset', `N'[1,2]'`, `N'$[last-1]'`],
  ['range to last', `N'[1,2]'`, `N'$[0 to last]'`],
  ['list two indexes', `N'[1,2]'`, `N'$[0,1]'`],
  ['list one index', `N'[1,2]'`, `N'$[0,0]'`],
  ['list spaces', `N'[1,2]'`, `N'$[0, 1]'`],
]
function literal(expression) {
  if (!expression.startsWith("N'") || !expression.endsWith("'") || expression.slice(2, -1).includes("'")) {
    throw new Error(`expected one unescaped Unicode literal: ${expression}`)
  }
  return expression.slice(2, -1)
}
const programs = cases.flatMap(([name, document, path]) =>
  ['JSON_VALUE', 'JSON_QUERY'].map(fn => ({
    name, fn, source: literal(document), path: literal(path),
    sql: `SELECT ${fn}(${document},${path}) AS value`,
  })))

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
  for (const { name, fn, source, path, sql } of programs) {
    const result = canonical(await captureOrdered(connection, sql))
    if (result.sets.length > 1 || result.sets.some(set => set.rows.length > 1) ||
        result.errors.length > 4 || result.done.length > 8 || result.events.length > 16 ||
        JSON.stringify(result).length > 20000) throw new Error(`unbounded capture: ${name}/${fn}`)
    records.push({ name, fn, source, path, sql, result })
  }
  return records
}

const containers = []
await withReferenceContainer(async (config, container) => {
  const runs = [await isolatedReference(config, observe), await isolatedReference(config, observe)]
  assertSameCapture(runs[0], runs[1], 'JSON advanced-path observations differ across fresh databases')
  containers.push({ image: container.image, runs })
})
const actual = { containers }
const retained = writeFixture ? null : JSON.parse(await readFile(fixture, 'utf8'))
if (retained) assertSameCapture(actual, retained, 'JSON advanced-path observations differ from retained fixture')
await mkdir(dirname(output), { recursive: true })
await writeFile(output, JSON.stringify(actual) + '\n')
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${programs.length} JSON advanced-path observations twice${retained ? ' and matched retained fixture' : ''}`)
