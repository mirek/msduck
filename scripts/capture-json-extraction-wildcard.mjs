#!/usr/bin/env node
// Owner-run SQL Server evidence for JSON extraction wildcard selection.
import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'

const fixture = new URL('../reference/json-extraction-wildcard.json', import.meta.url)
const writeFixture = process.argv.includes('--write-fixture')
const positional = process.argv.slice(2).filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one scratch output path')
const output = resolve(positional[0] ?? 'artifacts/json-extraction-wildcard/capture.json')
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
  ['root empty', `N'[]'`, `N'$[*]'`],
  ['root one number', `N'[1]'`, `N'$[*]'`],
  ['root one string', `N'["one"]'`, `N'$[*]'`],
  ['root one true', `N'[true]'`, `N'$[*]'`],
  ['root one null', `N'[null]'`, `N'$[*]'`],
  ['root one object', `N'[{"x":1}]'`, `N'$[*]'`],
  ['root one array', `N'[[1,2]]'`, `N'$[*]'`],
  ['root two numbers', `N'[1,2]'`, `N'$[*]'`],
  ['root two strings', `N'["one","two"]'`, `N'$[*]'`],
  ['root null then number', `N'[null,2]'`, `N'$[*]'`],
  ['root number then null', `N'[1,null]'`, `N'$[*]'`],
  ['root object then number', `N'[{"x":1},2]'`, `N'$[*]'`],
  ['root number then object', `N'[1,{"x":2}]'`, `N'$[*]'`],
  ['property one number', `N'{"a":[1]}'`, `N'$.a[*]'`],
  ['property two numbers', `N'{"a":[1,2]}'`, `N'$.a[*]'`],
  ['property empty array', `N'{"a":[]}'`, `N'$.a[*]'`],
  ['property scalar', `N'{"a":1}'`, `N'$.a[*]'`],
  ['property object', `N'{"a":{"x":1}}'`, `N'$.a[*]'`],
  ['property absent', `N'{"b":[1]}'`, `N'$.a[*]'`],
  ['nested first property', `N'[{"a":1},{"a":2}]'`, `N'$[*].a'`],
  ['nested first absent', `N'[{"b":0},{"a":2}]'`, `N'$[*].a'`],
  ['nested first null', `N'[{"a":null},{"a":2}]'`, `N'$[*].a'`],
  ['nested first wrong kind', `N'[1,{"a":2}]'`, `N'$[*].a'`],
  ['nested inner index', `N'[[1,2],[3,4]]'`, `N'$[*][0]'`],
  ['nested two wildcards', `N'[[1,2],[3,4]]'`, `N'$[*][*]'`],
  ['strict empty', `N'[]'`, `N'strict $[*]'`],
  ['strict one number', `N'[1]'`, `N'strict $[*]'`],
  ['strict one string', `N'["one"]'`, `N'strict $[*]'`],
  ['strict one null', `N'[null]'`, `N'strict $[*]'`],
  ['strict two numbers', `N'[1,2]'`, `N'strict $[*]'`],
  ['strict one object', `N'[{"x":1}]'`, `N'strict $[*]'`],
  ['strict one array', `N'[[1,2]]'`, `N'strict $[*]'`],
  ['strict two objects', `N'[{"x":1},{"x":2}]'`, `N'strict $[*]'`],
  ['strict object then number', `N'[{"x":1},2]'`, `N'strict $[*]'`],
  ['strict null then number', `N'[null,2]'`, `N'strict $[*]'`],
  ['strict nested first absent', `N'[{"b":0},{"a":2}]'`, `N'strict $[*].a'`],
  ['strict nested two matches', `N'[{"a":1},{"a":2}]'`, `N'strict $[*].a'`],
  ['strict nested first null', `N'[{"a":null},{"a":2}]'`, `N'strict $[*].a'`],
  ['strict property absent', `N'{"b":[1]}'`, `N'strict $.a[*]'`],
  ['strict property scalar', `N'{"a":1}'`, `N'strict $.a[*]'`],
  ['explicit lax two', `N'[1,2]'`, `N'lax $[*]'`],
  ['malformed before match', `N'[x,1]'`, `N'$[*]'`],
  ['malformed after first', `N'[1,x]'`, `N'$[*]'`],
  ['malformed after first object', `N'[{"a":1},x]'`, `N'$[*]'`],
  ['malformed after nested match', `N'[{"a":1},x]'`, `N'$[*].a'`],
  ['truncated after first', `N'[1'`, `N'$[*]'`],
  ['wrong root object', `N'{"a":1}'`, `N'$[*]'`],
  ['range path', `N'[1,2]'`, `N'$[0 to 1]'`],
  ['single range path', `N'[1,2]'`, `N'$[0 to 0]'`],
  ['quoted star key', `N'{"*":1}'`, `N'$."*"'`],
  ['last path', `N'[1,2]'`, `N'$[last]'`],
  ['list path', `N'[1,2]'`, `N'$[0,1]'`],
  ['wildcard invalid suffix', `N'[1,2]'`, `N'$[*].'`],
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
  assertSameCapture(runs[0], runs[1], 'JSON wildcard extraction observations differ across fresh databases')
  containers.push({ image: container.image, runs })
})
const actual = { containers }
const retained = writeFixture ? null : JSON.parse(await readFile(fixture, 'utf8'))
if (retained) assertSameCapture(actual, retained, 'JSON wildcard extraction observations differ from retained fixture')
await mkdir(dirname(output), { recursive: true })
await writeFile(output, JSON.stringify(actual) + '\n')
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${programs.length} JSON wildcard extraction observations twice${retained ? ' and matched retained fixture' : ''}`)
