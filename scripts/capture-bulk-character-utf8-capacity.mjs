#!/usr/bin/env node
// UTF8-bounded-target-specific probe construction; bounded exchange/retention primitives
// reuse the owner-reviewed task895 capture and task899 envelopes (owner
// PR902 checkpoint edbdd67a), immutable at their pinned source hashes.
import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {dirname, resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {createHash} from 'node:crypto'
import {createRequire} from 'node:module'
import {isDeepStrictEqual} from 'node:util'
import {TYPES} from 'tedious'
import {capture} from './lib/compatibility.mjs'
import {connect, isolatedReference} from './lib/reference.mjs'
import {withReferenceContainer, referenceImage} from './lib/reference-container.mjs'
import {CAPTURE_LIMIT, profiles, jsonSize, budget, guardOutput, exchange, compare, responseSignature, readCaptureFile} from './capture-bulk-character-conversion.mjs'
export {CAPTURE_LIMIT, jsonSize, budget, guardOutput, exchange, compare, readCaptureFile}
const require = createRequire(import.meta.url)
const {Collation} = require('tedious/lib/collation')
const CLIENT_VERSION = require('tedious/package.json').version
const SOURCE = fileURLToPath(import.meta.url)
const SOURCE_SHA = createHash('sha256').update(await readFile(SOURCE)).digest('hex')
const HELPER_SHA = createHash('sha256').update(await readFile(new URL('./capture-bulk-character-conversion.mjs', import.meta.url))).digest('hex')
const EXPECTED_HELPERS = {"capture-bulk-character-conversion.mjs":"9dd773a8170ea2c255d51bf40574fa75474563902c9211a2bceae7f5382e8868","lib/compatibility.mjs":"62f032fce72cf9a1fa0cb14bb2c7f1b5a2c396fdf4e8fcd5486d8db96aa0cf4d","lib/reference.mjs":"419a96611c0251e66b1f0783c9413151b4dacb5ee005ad89383402ff8c738df8","lib/reference-container.mjs":"d5b8d66c920330cbb4554cd16f1525f005eed9e3bdc69d5b00e421791545120c","capture-bulk-character-capacity.mjs":"2cbb4172e902e2a4817812de570d09e375533da8a3efaf539cd08f9408975e87","capture-bulk-character-cp1252.mjs":"7a1e2540ff6d25eae530b26ab96ea114f26f13a28b81de46d0e3c58e7d956d5f","capture-bulk-character-trailing-space.mjs":"805bebbe2a22373e1745dc17786d1763cd295097e24aa2be83a5cfb8ce5211aa","capture-bulk-character-utf8-boundary.mjs":"e85213c75f9c4a1cea93a5034e82d4b4d2481dae624d37b87f952dc384932c4a","capture-bulk-character-utf8-source-form.mjs":"a321c5461e262352a013d3b5a7952932e8f04eb1bc696aa1e5ef96a28067e30d","capture-bulk-character-utf8-bounded-target.mjs":"7040f1ade58bc18083618184de28d3351cc282675631727c47e5576b618bd53b"}
const HELPERS = Object.fromEntries(await Promise.all(Object.keys(EXPECTED_HELPERS).map(async name => [name,createHash('sha256').update(await readFile(new URL(name,import.meta.url))).digest('hex')])))
const EXPECTED_HELPER_SHA = '9dd773a8170ea2c255d51bf40574fa75474563902c9211a2bceae7f5382e8868'
const fixture = fileURLToPath(new URL('../reference/bulk-character-utf8-capacity.json', import.meta.url))
const LIMIT = 2 * 1024 * 1024
const MAX_PACKETS = 1024
const GOLD = null
const RETAINED_SHA256 = null
const REVIEWED_RECAPTURE_SOURCES = []
// Original native bytes and explicit target capacities; no platform decoder oracle.
// width counts target bytes (ANSI) or declared UTF16 units (Unicode), not characters.
export const vectors = [
  ['empty','',2,2], ['ascii-exact','4141',2,2], ['ascii-over','414141',2,2],
  ['two-byte-exact','c2a9',2,1], ['two-byte-over','c2a9',1,1],
  ['supplementary-exact','f0908080',4,2], ['supplementary-over','f0908080',1,1],
  ['space-over','412020',1,1], ['nul-over','4100',1,1], ['nbsp-over','41c2a0',1,1],
  ['incomplete','c2',1,1], ['continuation','80',1,1],
  ['invalid-range','e08042',2,2], ['incomplete-suffix','41c2',1,1]
]
export const cases = []
function add(vector,sourceWidth,target,targetFamily) {
  const [category,hex,byteWidth,unitWidth]=vector
  cases.push({declared:'utf8',wire:'utf8',target,sourceFamily:'varchar',sourceWidth,wireWidth:sourceWidth,
    targetFamily,targetWidth:target==='unicode'?unitWidth:byteWidth,category,
    name:`utf8-${category}-source${sourceWidth}-to-${target}-${targetFamily}`,values:[null,hex]})
}
for(const vector of vectors)for(const sourceWidth of [64,'max'])
  for(const [target,family] of [['utf8','varchar'],['utf8','char'],['unicode','nvarchar'],['unicode','nchar']])add(vector,sourceWidth,target,family)
for(const category of ['ascii-over','two-byte-exact','supplementary-exact','space-over'])
  for(const sourceWidth of [64,'max'])for(const target of ['cp1251','cp1252'])add(vectors.find(v=>v[0]===category),sourceWidth,target,'varchar')
assert.equal(vectors.length,14);assert.equal(cases.length,128)
assert.equal(new Set(cases.map(c=>c.name)).size,128)
export function rowsFor(c) {
  assert.equal(c.values.length,2)
  for(const value of c.values)assert.ok(value===null||typeof value==='string'&&/^(?:[0-9a-f]{2})*$/.test(value)&&value.length<=8)
  return c.values.map((valueHex,index)=>({id:index+1,valueHex}))
}
function failure(error) {return {name:error.name??'Error',code:error.code??null,message:String(error.message).slice(0,1024)}}
function requireCapture(record) {
  assert.ok(!record.result.traceFailure && !record.result.captureBudgetFailure, 'controlled capture interrupted; partial exchange retained')
}
async function sql(connection, text, retained) {
  return {sql: text, ...await exchange(connection, () => capture(connection, text), retained)}
}
function runBulk(connection, c, collation, rows) {
  return new Promise(resolveResult => {
    const errors = [], info = []
    let settled = false
    const finish = result => {if (settled) return; settled = true; cleanup(); resolveResult(result)}
    const fields = e => ({number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber,
      message: e.message, serverName: e.serverName, procName: e.procName})
    const onError = e => errors.push(fields(e)), onInfo = e => info.push(fields(e))
    const cleanup = () => {connection.off('errorMessage', onError); connection.off('infoMessage', onInfo); clearTimeout(timer)}
    const timer = setTimeout(() => {finish({errors, info, rowCount: null, error: {number: null, code: 'CAPTURE_TIMEOUT', message: 'Controlled bulk callback exceeded 15000ms'}}); connection.close()}, 15000)
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    try {
      const load = connection.newBulkLoad('dbo.utf8_capacity_probe', {keepNulls: true}, (error, rowCount) => {
        finish({errors, info, rowCount: rowCount ?? null, error: error ? {number: error.number ?? null, code: error.code ?? null, message: error.message} : null})
      })
      load.addColumn('id', TYPES.Int, {nullable: false})
      const type = c.sourceFamily === 'char' ? TYPES.Char : TYPES.VarChar
      const raw = {...type, validate: value => {assert.ok(value === null || Buffer.isBuffer(value)); return value}}
      if (c.wire === 'omitted') raw.generateTypeInfo = (parameter, options) => type.generateTypeInfo(parameter, options).subarray(0, 3)
      load.addColumn('value', raw, {length: c.wireWidth === 'max' ? Infinity : c.wireWidth, nullable: true})
      load.columns[1].collation = collation
      load.getBulkInsertSql = () => `INSERT BULK dbo.utf8_capacity_probe ([id] int, [value] ${c.sourceFamily}(${c.sourceWidth}) COLLATE ${profiles[c.declared]}) WITH (KEEP_NULLS)`
      connection.execBulkLoad(load, rows.map(row => ({id: row.id, value: row.valueHex === null ? null : Buffer.from(row.valueHex, 'hex')})))
    } catch (error) {finish({errors, info, rowCount: null, error: {number: error.number ?? null, code: error.code ?? null, message: error.message}})}
  })
}
const readbackSql = `SELECT @@ERROR AS last_error, @@ROWCOUNT AS last_rowcount, XACT_STATE() AS transaction_state, @@TRANCOUNT AS transaction_count;
SELECT id, value, CAST(value AS varbinary(max)) AS native_bytes, CAST(value AS nvarchar(max)) AS unicode_value,
CAST(CAST(value AS nvarchar(max)) AS varbinary(max)) AS unicode_units FROM dbo.utf8_capacity_probe ORDER BY id;`
async function observe(config, initial, run, retained) {
  run.version = await sql(initial, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)) AS version, DB_NAME() AS database_name, CAST(SERVERPROPERTY('ServerName') AS nvarchar(128)) AS server_name", retained)
  requireCapture(run.version)
  assert.equal(run.version.result.errors.length, 0)
  const database = run.version.result.sets[0].rows[0][1]
  const caseConfig = {...config, options: {...config.options, database, packetSize: 512, requestTimeout: 10000}}
  run.observations = []
  for (const c of cases) {
    const o = {case: c, input: rowsFor(c)}
    retained.take(jsonSize(o, LIMIT))
    run.observations.push(o)
    let connection = await connect(caseConfig)
    try {
      const wireProfile = profiles[c.wire] ?? profiles.cp1252
      o.metadata = await sql(connection, `SELECT CAST('' AS ${c.sourceFamily}(${c.wireWidth})) COLLATE ${wireProfile} AS sample`, retained)
      requireCapture(o.metadata)
      assert.equal(o.metadata.result.errors.length, 0)
      const descriptor = o.metadata.result.sets[0].columns[0].collation
      const collation = new Collation(descriptor.lcid, descriptor.flags, descriptor.version, descriptor.sortId)
      if (c.wire === 'zero') collation.toBuffer = () => Buffer.alloc(5)
      o.wireCollationHex = c.wire === 'omitted' ? null : collation.toBuffer().toString('hex')
      const targetProfile = profiles[c.target] ?? profiles.cp1252
      const targetFamily = c.target === 'unicode' && c.targetFamily === 'varchar' ? 'nvarchar' : c.targetFamily
      o.setup = await sql(connection, `CREATE TABLE dbo.utf8_capacity_probe (id int NOT NULL, value ${targetFamily}(${c.targetWidth}) COLLATE ${targetProfile} NULL)`, retained)
      requireCapture(o.setup)
      assert.equal(o.setup.result.errors.length, 0)
      o.execution = await exchange(connection, () => runBulk(connection, c, collation, o.input), retained)
      requireCapture(o.execution)
      o.readback = {session: 'original', ...await sql(connection, readbackSql, retained)}
      requireCapture(o.readback)
      if (connection.closed || o.readback.result.transportError || o.readback.result.errors?.some(e => e.number === null)) {
        connection.close()
        connection = await connect(caseConfig)
        // A replacement connection cannot establish the failed session's counters.
        o.recoveryReadback = {session: 'replacement', ...await sql(connection, readbackSql, retained)}
        requireCapture(o.recoveryReadback)
      }
      o.cleanup = await sql(connection, 'DROP TABLE dbo.utf8_capacity_probe', retained)
      requireCapture(o.cleanup)
      assert.equal(o.cleanup.result.errors.length, 0)
      console.log(JSON.stringify({case: c.name, errors: o.execution.result.errors?.map(e => e.number), callback: o.execution.result.error?.code ?? null, count: o.execution.result.rowCount}))
    } finally {connection.close()}
  }
}
export function validateRequests(o) {
  const c = o.case
  const targetFamily = c.target === 'unicode' && c.targetFamily === 'varchar' ? 'nvarchar' : c.targetFamily
  assert.equal(o.metadata.sql, `SELECT CAST('' AS ${c.sourceFamily}(${c.wireWidth})) COLLATE ${profiles[c.wire] ?? profiles.cp1252} AS sample`, 'fixed metadata SQL')
  assert.equal(o.setup.sql, `CREATE TABLE dbo.utf8_capacity_probe (id int NOT NULL, value ${targetFamily}(${c.targetWidth}) COLLATE ${profiles[c.target] ?? profiles.cp1252} NULL)`, 'fixed setup SQL')
  assert.equal(o.readback.sql, readbackSql, 'fixed readback SQL')
  if (o.recoveryReadback) assert.equal(o.recoveryReadback.sql, readbackSql, 'fixed recovery SQL')
  assert.equal(o.cleanup.sql, 'DROP TABLE dbo.utf8_capacity_probe', 'fixed cleanup SQL')
  const expectedCollation = {cp1251: '1904002200', cp1252: '0904d00034', utf8: '0904002600', zero: '0000000000', omitted: null}[c.wire]
  assert.equal(o.wireCollationHex, expectedCollation, 'fixed supplied wire descriptor')
  const width = Buffer.alloc(2); width.writeUInt16LE(c.wireWidth === 'max' ? 65535 : c.wireWidth)
  const metadata = '81020000000000040026040269006400000000000500'
    + (c.sourceFamily === 'char' ? 'af' : 'a7') + width.toString('hex')
    + (expectedCollation ?? '') + '05760061006c0075006500'
  const rows = rowsFor(c).map(row => {
    const id = Buffer.alloc(4); id.writeInt32LE(row.id)
    let payload
    if (c.wireWidth !== 'max') {
      const length = Buffer.alloc(2); length.writeUInt16LE(row.valueHex === null ? 65535 : row.valueHex.length / 2)
      payload = length.toString('hex') + (row.valueHex ?? '')
    } else if (row.valueHex === null) payload = 'ffffffffffffffff'
    else {
      const length = Buffer.alloc(4); length.writeUInt32LE(row.valueHex.length / 2)
      payload = 'feffffffffffffff' + (row.valueHex.length ? length.toString('hex') + row.valueHex : '') + '00000000'
    }
    return 'd104' + id.toString('hex') + payload
  }).join('')
  const actual = o.execution.packets.filter(p => p.direction === 'out' && p.rawHex.startsWith('07')).map(p => p.rawHex.slice(16)).join('')
  assert.ok(actual === metadata + rows + 'fd000000000000000000000000', 'fixed original BulkLoad TYPE_INFO/ROW/PLP/DONE bytes')
}
export function requestSignature(record) {
  return createHash('sha256').update(record.packets.filter(p => p.direction === 'out').map(p => p.rawHex.slice(16)).join('')).digest('hex')
}
export function fragmentationSignature(record) {
  const headers = record.packets.map(p => ({direction: p.direction, header: p.rawHex.slice(0,8) + p.rawHex.slice(12,16)}))
  return createHash('sha256').update(JSON.stringify(headers)).digest('hex')
}
function validatePackets(record, expectedSpid, bulk = false) {
  assert.ok(record.packets.length <= MAX_PACKETS)
  const expectedMessages = bulk ? [['out',1],['in',4],['out',7],['in',4]] : [['out',1],['in',4]]
  let size = 0, message = null, inboundSpid = expectedSpid, messages = 0
  for (const p of record.packets) {
    assert.ok(['in','out'].includes(p.direction) && /^(?:[0-9a-f]{2})+$/.test(p.rawHex) && p.rawHex.length <= 65534)
    size += p.rawHex.length / 2
    assert.ok(size <= LIMIT)
    const bytes = Buffer.from(p.rawHex, 'hex')
    assert.ok(bytes.length >= 8 && bytes.readUInt16BE(2) === bytes.length)
    assert.ok(p.direction === 'in' ? bytes[0] === 4 : [1,6,7].includes(bytes[0]))
    assert.ok(bytes[1] === 0 || bytes[1] === 1, 'fixed controlled packet status')
    const spid = bytes.readUInt16BE(4)
    if (p.direction === 'in') {
      if (inboundSpid === undefined) inboundSpid = spid
      assert.equal(spid, inboundSpid, 'consistent inbound connection SPID')
    } else assert.equal(spid, 0, 'fixed outbound SPID')
    assert.equal(bytes[7], 0)
    if (message) {assert.equal(message.direction, p.direction); assert.equal(message.type, bytes[0])}
    else {
      assert.deepEqual([p.direction,bytes[0]], expectedMessages[messages], 'fixed phase message direction/type')
      message = {direction: p.direction, type: bytes[0], packetId: 1}
    }
    assert.equal(bytes[6], message.packetId)
    message.packetId = (message.packetId + 1) & 255
    if (bytes[1] & 1) {message = null; messages++}
  }
  assert.equal(message, null, 'complete retained EOM')
  assert.equal(messages, expectedMessages.length, 'complete fixed phase messages')
  return inboundSpid
}
export function semantic(o, database, serverName) {
  // Independent semantic digest only; original full packets/fields/differences
  // remain untouched. Ephemeral container identity is checked, not substituted.
  const value = structuredClone({metadata: o.metadata.result, setup: o.setup.result, execution: o.execution.result,
    readback: o.readback.result, recoveryReadback: o.recoveryReadback?.result ?? null, cleanup: o.cleanup.result})
  for (const result of Object.values(value)) if (result) for (const e of [...(result.errors ?? []), ...(result.info ?? [])]) {
    if (Object.hasOwn(e, 'serverName')) {assert.equal(e.serverName, serverName); delete e.serverName}
    if (e.number === 2628) {assert.ok(e.message.startsWith("String or binary data would be truncated in table '" + database + ".dbo.utf8_capacity_probe', column 'value'. Truncated value: '")); e.message = e.message.replace(database, '@DATABASE@')}
  }
  if (value.execution.error?.number === 2628) {assert.ok(value.execution.error.message.includes(database + '.dbo.utf8_capacity_probe')); value.execution.error.message = value.execution.error.message.replace(database, '@DATABASE@')}
  return createHash('sha256').update(JSON.stringify(value)).digest('hex')
}
export function validate(actual) {
  jsonSize(actual)
  assert.equal(actual.format, 1)
  assert.equal(actual.containers.length, 2)
  assert.equal(actual.runs.length, 4)
  for (const c of actual.containers) assert.equal(c.image, referenceImage)
  assert.equal(actual.provenance.clientVersion, '20.0.0')
  assert.equal(actual.provenance.packetSize, 512)
  assert.equal(actual.provenance.helperSha, EXPECTED_HELPER_SHA, 'fixed reviewed helper provenance')
  assert.ok(isDeepStrictEqual(actual.provenance.helpers, EXPECTED_HELPERS), 'fixed complete helper provenance')
  assert.match(actual.provenance.sourceSha, /^[0-9a-f]{64}$/)
  assert.ok(GOLD, 'actual capture has not yet been independently pinned')
  assert.equal(actual.provenance.nodeVersion, GOLD.nodeVersion, 'fixed actual Node provenance')
  assert.ok([GOLD.sourceSha, ...REVIEWED_RECAPTURE_SOURCES, SOURCE_SHA].includes(actual.provenance.sourceSha), 'fixed source provenance')
  const identities = actual.runs.map(run => run.version.result.sets[0].rows[0])
  assert.equal(new Set(identities.map(row => row[1])).size, 4, 'four independent database identities')
  assert.equal(identities[0][2], identities[1][2], 'first container server identity')
  assert.equal(identities[2][2], identities[3][2], 'second container server identity')
  assert.notEqual(identities[0][2], identities[2][2], 'two independent container identities')
  for (const run of actual.runs) {
    assert.equal(run.version.sql, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)) AS version, DB_NAME() AS database_name, CAST(SERVERPROPERTY('ServerName') AS nvarchar(128)) AS server_name", 'fixed version SQL')
    assert.equal(run.version.result.sets[0].rows[0][0], '17.0.4065.4')
    assert.equal(run.version.result.sets[0].rows[0].length, 3, 'fixed version identity row width')
    const [version, database, serverName] = run.version.result.sets[0].rows[0]
    assert.match(database, /^msduck_audit_[0-9a-f]{32}$/)
    assert.match(serverName, /^[0-9a-f]{12}$/)
    const versionResult = structuredClone(run.version.result)
    versionResult.sets[0].rows[0] = [version, '@DATABASE@', '@SERVER@']
    assert.equal(createHash('sha256').update(JSON.stringify(versionResult)).digest('hex'), GOLD.version, 'fixed complete version result')
    validatePackets(run.version)
    assert.equal(requestSignature(run.version), GOLD.versionRequest, 'fixed version request payload')
    assert.equal(responseSignature(run.version, database, serverName), GOLD.versionResponse, 'fixed version response payload')
    assert.equal(run.observations.length, cases.length)
    for (const [i, o] of run.observations.entries()) {
      assert.ok(isDeepStrictEqual(o.case, cases[i]), 'fixed probe specification')
      assert.ok(isDeepStrictEqual(o.input, rowsFor(cases[i])), 'fixed supplied bytes')
      assert.equal(o.readback.session, 'original', 'fixed readback session')
      if (o.recoveryReadback) assert.equal(o.recoveryReadback.session, 'replacement', 'fixed recovery session')
      validateRequests(o)
      assert.equal(semantic(o, database, serverName), GOLD.semantics[o.case.name], 'fixed independent semantic digest: ' + o.case.name)
      const originalSpid = validatePackets(o.metadata)
      const replacementSpid = o.recoveryReadback ? validatePackets(o.recoveryReadback) : originalSpid
      for (const phase of ['metadata','setup','execution','readback','recoveryReadback','cleanup']) if (o[phase]) {
        validatePackets(o[phase], phase === 'recoveryReadback' || phase === 'cleanup' ? replacementSpid : originalSpid, phase === 'execution')
        assert.equal(fragmentationSignature(o[phase]), GOLD.fragmentation[o.case.name][phase], 'fixed complete packet framing: ' + o.case.name + '/' + phase)
        assert.equal(requestSignature(o[phase]), GOLD.requests[o.case.name][phase], 'fixed request payload: ' + o.case.name + '/' + phase)
        assert.equal(responseSignature(o[phase], database, serverName), GOLD.responses[o.case.name][phase], 'fixed response payload: ' + o.case.name + '/' + phase)
      }
    }
  }
  assert.ok(isDeepStrictEqual(actual.comparisons, actual.runs.slice(1).map(run => compare(run, actual.runs[0]))), 'exact unnormalized comparisons')
}
export function validateRetained(bytes) {
  assert.ok(bytes.length <= CAPTURE_LIMIT && RETAINED_SHA256, 'bounded pinned retained artifact')
  assert.equal(createHash('sha256').update(bytes).digest('hex'), RETAINED_SHA256, 'fixed retained bytes')
  const value = JSON.parse(bytes.toString()); validate(value); return value
}
async function saveCapture(actual, output, captureError) {
  await guardOutput(output); await guardOutput(`${output}.comparison.json`)
  await mkdir(dirname(output), {recursive: true})
  const size = jsonSize(actual)
  const bytes = JSON.stringify(actual) + (size < CAPTURE_LIMIT ? '\n' : '')
  await writeFile(output, bytes, {flag: 'wx'})
  let expected, retainedBytes
  try {retainedBytes = await readCaptureFile(fixture); expected = JSON.parse(retainedBytes.toString())} catch (error) {if (error.code !== 'ENOENT') throw error}
  let comparison, comparisonError
  try {
    comparison = {retained: Boolean(expected), differences: expected ? compare(actual, expected) : [],
      ...(captureError ? {captureFailure: failure(captureError)} : {})}
    jsonSize(comparison)
  } catch (error) {
    comparisonError = error
    comparison = {retained: Boolean(expected), differencesOmitted: true, failure: failure(error),
      ...(captureError ? {captureFailure: failure(captureError)} : {})}
    jsonSize(comparison)
  }
  const comparisonSize = jsonSize(comparison)
  await writeFile(`${output}.comparison.json`, JSON.stringify(comparison) + (comparisonSize < CAPTURE_LIMIT ? '\n' : ''), {flag: 'wx'})
  if (comparisonError) throw comparisonError
  if (retainedBytes) validateRetained(retainedBytes)
  return {cases: cases.length, runs: actual.runs.length, output, sha256: createHash('sha256').update(bytes).digest('hex')}
}
export async function persistCapture(actual, output) {
  const result = await saveCapture(actual, output)
  validate(actual)
  return result
}
export async function finalizeCapture(raw, output) {
  jsonSize(raw)
  let actual
  try {
    actual = {...raw, comparisons: raw.runs.slice(1).map(run => compare(run, raw.runs[0]))}
    jsonSize(actual)
  } catch (error) {
    // Cross-run differences can multiply retained values. Preserve the bounded
    // original runs even when their derived comparisons exceed the envelope.
    // Failure metadata lives in the sidecar: adding it to near-limit raw data
    // must not make the original evidence exceed its previously checked bound.
    await saveCapture(raw, output, error)
    throw error
  }
  return persistCapture(actual, output)
}
export async function main(args = process.argv.slice(2)) {
  assert.ok(args.filter(a => a.startsWith('--')).every(a => a === '--replay-fixture'), 'known flags')
  const positional = args.filter(a => !a.startsWith('--'))
  assert.ok(positional.length <= 1)
  if (args.includes('--replay-fixture')) {validateRetained(await readCaptureFile(fixture)); console.log('Validated retained UTF8-bounded-target capture'); return}
  const output = resolve(positional[0] ?? 'artifacts/compatibility/bulk-character-utf8-capacity/capture.json')
  assert.notEqual(output, fixture, 'separate raw output')
  await guardOutput(output); await guardOutput(`${output}.comparison.json`)
  const containers = [], runs = [], retained = budget()
  assert.equal(HELPER_SHA, EXPECTED_HELPER_SHA, 'reviewed helper unchanged before Docker')
  assert.ok(isDeepStrictEqual(HELPERS, EXPECTED_HELPERS), 'reviewed helpers unchanged before Docker')
  const provenance = {clientVersion: CLIENT_VERSION, packetSize: 512, nodeVersion: process.versions.node, sourceSha: SOURCE_SHA, helperSha: HELPER_SHA, helpers: HELPERS}
  try {
    for (let i = 0; i < 2; i++) await withReferenceContainer(async (config, container) => {
      containers.push({image: container.image})
      for (let db = 0; db < 2; db++) {
        const run = {}; runs.push(run)
        await isolatedReference(config, initial => observe(config, initial, run, retained))
      }
    }, {image:referenceImage})
  } catch (error) {
    const partial = {format: 1, provenance, containers, runs, failure: failure(error)}
    await saveCapture(partial, output)
    throw error
  }
  console.log(JSON.stringify(await finalizeCapture({format: 1, provenance, containers, runs}, output)))
}
if (process.argv[1] && resolve(process.argv[1]) === SOURCE) await main()
