#!/usr/bin/env node
// First-party post-login byte probes; never retain login packets or credentials.
import assert from 'node:assert/strict'
import {lstat, mkdir, readFile, writeFile} from 'node:fs/promises'
import {dirname, resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {isDeepStrictEqual} from 'node:util'
import {createHash} from 'node:crypto'
import {createRequire} from 'node:module'
import {TYPES} from 'tedious'
import {capture, canonical, differences} from './lib/compatibility.mjs'
import {isolatedReference} from './lib/reference.mjs'
import {withReferenceContainer, referenceImage} from './lib/reference-container.mjs'
const {Collation} = createRequire(import.meta.url)('tedious/lib/collation')
const fixture = fileURLToPath(new URL('../reference/bulk-character-encoding.json', import.meta.url))
const LIMIT = 2 * 1024 * 1024
const MAX_PACKETS = 1024
const GOLD_VERSION = '17.0.4065.4'
// Filled from the first independently inspected capture before publication.
const RETAINED_SHA256 = '0d2cff08bc7f59d271955311ac540f95a37f31b456bc008f739117c1a09d3a83'
export const profiles = [
  {name: 'SQL_Latin1_General_CP1_CI_AS', codepage: 'CP1252', sampleHex: '80e98c'},
  {name: 'Latin1_General_100_BIN2_UTF8', codepage: 'utf-8', sampleHex: Buffer.from('éΩ🦆').toString('hex')},
  {name: 'Cyrillic_General_100_BIN2', codepage: 'CP1251', sampleHex: 'cff0e8e2e5f2'}
]
export const cases = [
  ...profiles.flatMap(profile => ['same', 'cp1252', 'unicode'].flatMap(target => [false, true].map(max => ({
    name: `${profile.codepage}-${target}-${max ? 'max' : 'bounded'}`, source: profile.name, target, max
  })))),
  ...['80', 'c328', 'c0af', 'eda080', 'f09f92'].flatMap(invalidHex => ['same', 'cp1252', 'unicode'].map(target => ({
    name: `utf-8-invalid-${invalidHex}-${target}`, source: 'Latin1_General_100_BIN2_UTF8', target, max: false, invalidHex
  })))
]
export function rowsFor(entry) {
  const profile = profiles.find(p => p.name === entry.source)
  if (entry.invalidHex) return [{id: 1, valueHex: '4173636969'}, {id: 2, valueHex: entry.invalidHex}]
  const values = [null, '', '4173636969', profile.sampleHex]
  if (profile.codepage !== 'utf-8') values.push('818d90ff', 'c328')
  if (entry.max) values.push(profile.sampleHex.repeat(3000))
  return values.map((hex, index) => ({id: index + 1, valueHex: hex}))
}

export async function guardOutput(path) {
  for (let part = resolve(path);; part = dirname(part)) {
    let info
    try { info = await lstat(part) } catch (error) { if (error.code !== 'ENOENT') throw error }
    assert.ok(!info?.isSymbolicLink(), 'output path contains a symlink')
    if (part === resolve(path)) assert.ok(!info, 'refusing existing output')
    if (dirname(part) === part) break
  }
}

function trace(connection) {
  const outgoing = connection.messageIo.outgoingMessageStream
  const incoming = connection.messageIo.securePair?.cleartext ?? connection.messageIo.socket
  let pending = Buffer.alloc(0), bytes = 0
  const packets = []
  const retain = (direction, packet) => {
    bytes += packet.length
    assert.ok(bytes <= LIMIT && packets.length < MAX_PACKETS, 'bounded post-login exchange')
    assert.ok(packet.length >= 8 && packet.length <= 32767, 'bounded packet length')
    assert.equal(packet.readUInt16BE(2), packet.length, 'complete packet')
    assert.ok(direction === 'out' ? [1, 7].includes(packet[0]) : packet[0] === 4, 'controlled post-login packet type')
    packets.push({direction, rawHex: packet.toString('hex')})
  }
  const onOut = packet => retain('out', packet)
  const onIn = chunk => {
    assert.ok(chunk.length + pending.length <= LIMIT, 'bounded incoming buffer')
    pending = Buffer.concat([pending, chunk])
    while (pending.length >= 8) {
      const size = pending.readUInt16BE(2)
      assert.ok(size >= 8 && size <= 32767, 'bounded incoming frame')
      if (pending.length < size) break
      retain('in', pending.subarray(0, size))
      pending = pending.subarray(size)
    }
  }
  outgoing.on('data', onOut)
  incoming.on('data', onIn)
  return () => {
    outgoing.off('data', onOut)
    incoming.off('data', onIn)
    assert.equal(pending.length, 0, 'no unfinished captured packet')
    return packets
  }
}

async function exchange(connection, action) {
  const stop = trace(connection)
  let result, packets
  try { result = canonical(await action()) } finally { packets = stop() }
  return {result, packets}
}

async function sql(connection, text) {
  return {sql: text, ...await exchange(connection, () => capture(connection, text))}
}

function runBulk(connection, entry, collation, input) {
  return new Promise((resolveResult, reject) => {
    const errors = [], info = []
    const fields = e => ({number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber,
      message: e.message, serverName: e.serverName, procName: e.procName})
    const onError = e => errors.push(fields(e)), onInfo = e => info.push(fields(e))
    const clean = () => {connection.off('errorMessage', onError); connection.off('infoMessage', onInfo)}
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    try {
      const load = connection.newBulkLoad('dbo.encoding_probe', {keepNulls: true}, (error, rowCount) => {
        clean()
        resolveResult({errors, info, rowCount: rowCount ?? null,
          error: error ? {number: error.number ?? null, code: error.code ?? null, message: error.message} : null})
      })
      load.addColumn('id', TYPES.Int, {nullable: false})
      // Keep actual supplied bytes. This controlled type override changes only
      // client validation/encoding; ordinary tedious writes TYPE_INFO/ROW/PLP.
      const rawVarchar = {...TYPES.VarChar, validate: value => {
        assert.ok(value === null || Buffer.isBuffer(value), 'explicit supplied bytes only')
        return value
      }}
      load.addColumn('value', rawVarchar, {length: entry.max ? Infinity : 64, nullable: true})
      load.columns[1].collation = collation
      const statement = `INSERT BULK dbo.encoding_probe ([id] int, [value] varchar(${entry.max ? 'max' : '64'}) COLLATE ${entry.source}) WITH (KEEP_NULLS)`
      load.getBulkInsertSql = () => statement
      connection.execBulkLoad(load, input.map(row => ({id: row.id, value: row.valueHex === null ? null : Buffer.from(row.valueHex, 'hex')})))
    } catch (error) {clean(); reject(error)}
  })
}

async function observe(connection, run) {
  const version = await sql(connection, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)) AS version, DB_NAME() AS database_name")
  run.version = version
  const observations = run.observations = []
  assert.equal(version.result.errors.length, 0, 'version succeeds')
  for (const entry of cases) {
    const observation = {case: entry, input: rowsFor(entry)}
    observations.push(observation)
    const metadata = await sql(connection, `SELECT CAST('' AS varchar(64)) COLLATE ${entry.source} AS sample`)
    observation.metadata = metadata
    assert.equal(metadata.result.errors.length, 0, 'source collation metadata succeeds')
    const c = metadata.result.sets[0].columns[0].collation
    const collation = new Collation(c.lcid, c.flags, c.version, c.sortId)
    observation.wireCollationHex = collation.toBuffer().toString('hex')
    assert.equal(collation.codepage, profiles.find(p => p.name === entry.source).codepage, 'actual source code page')
    const target = entry.target === 'same' ? entry.source : 'SQL_Latin1_General_CP1_CI_AS'
    const kind = entry.target === 'unicode' ? 'nvarchar' : 'varchar'
    const setup = await sql(connection, `CREATE TABLE dbo.encoding_probe (id int NOT NULL, value ${kind}(${entry.max ? 'max' : '64'}) COLLATE ${target} NULL)`)
    observation.setup = setup
    assert.equal(setup.result.errors.length, 0, 'setup succeeds')
    const input = rowsFor(entry)
    const execution = await exchange(connection, () => runBulk(connection, entry, collation, input))
    observation.execution = execution
    const readback = await sql(connection, `SELECT @@ERROR AS last_error, @@ROWCOUNT AS last_rowcount, XACT_STATE() AS transaction_state, @@TRANCOUNT AS transaction_count;
SELECT id, value, CAST(value AS varbinary(max)) AS native_bytes, CAST(value AS nvarchar(max)) AS unicode_value FROM dbo.encoding_probe ORDER BY id;`)
    observation.readback = readback
    const cleanup = await sql(connection, 'DROP TABLE dbo.encoding_probe')
    observation.cleanup = cleanup
    assert.equal(cleanup.result.errors.length, 0, 'cleanup succeeds')
    console.log(JSON.stringify({case: entry.name, errorNumbers: execution.result.errors.map(e => e.number), rowCount: execution.result.rowCount}))
  }
  return {version, observations}
}

export function compare(actual, expected) {
  return differences(actual, expected).map(d => ({...d, category: /\/packets\/\d+\/rawHex$/.test(d.path) ? 'raw-packet' : 'observed-field'}))
}

function exact(actual, expected, label) {
  if (!isDeepStrictEqual(actual, expected)) throw Error(label)
}
function validateQuery(result, rows, descriptorSHA, label) {
  assert.equal(createHash('sha256').update(JSON.stringify(result.sets.map(s => s.columns))).digest('hex'), descriptorSHA, `fixed ${label} descriptors`)
  exact(result.sets.map(s => s.rows), rows, `fixed ${label} rows`)
  exact(result.done, rows.map((r, i) => ({kind: 'done', rowCount: r.length, more: i < rows.length - 1})), `fixed ${label} completions`)
  exact(result.errors, [], `fixed ${label} errors`)
  exact(result.info, [], `fixed ${label} informational diagnostics`)
  assert.equal(result.returnStatus, null, `fixed ${label} status`)
  assert.equal(result.rowCount, rows.reduce((n, r) => n + r.length, 0), `fixed ${label} callback`)
}
function statement(result, label) {
  exact(result, {sets: [], done: [{kind: 'done', rowCount: null, more: false}], errors: [], info: [], returnStatus: null, rowCount: 0}, `fixed ${label}`)
}
export function expectedRows(entry) {
  const source = profiles.find(p => p.name === entry.source).codepage
  if (['80', 'c0af', 'f09f92'].includes(entry.invalidHex)) return []
  const sample = {'CP1252': '€éŒ', 'CP1251': 'Привет', 'utf-8': 'éΩ🦆'}[source]
  let texts = entry.invalidHex ? ['Ascii', entry.invalidHex === 'c328' ? '�(' : '��'] : [null, '', 'Ascii', sample]
  if (!entry.invalidHex && source !== 'utf-8') texts.push(source === 'CP1252' ? '\u0081\u008d\u0090ÿ' : 'ЃЌђя', source === 'CP1252' ? 'Ã(' : 'Г(')
  if (entry.max) texts.push(sample.repeat(3000))
  if (entry.target === 'cp1252') {
    const substitutions = source === 'CP1251' ? {'Привет': '??????', 'ЃЌђя': '????', 'Г(': '?('}
      : source === 'utf-8' ? {'éΩ🦆': 'éO??', '�(': '?(', '��': '??'} : {}
    texts = texts.map(text => entry.max && text === sample.repeat(3000) ? (substitutions[sample] ?? sample).repeat(3000) : substitutions[text] ?? text)
  }
  return texts.map((text, i) => {
    if (text === null) return [i + 1, null, null, null]
    let bytes, publicText = text
    if (entry.target === 'unicode') bytes = Buffer.from(text, 'utf16le').toString('hex')
    else if (entry.target === 'same' || source === 'CP1252') {
      bytes = rowsFor(entry)[i].valueHex
      if (source === 'CP1252') publicText = text.replace(/[\u0081\u008d\u0090]/g, '�')
      if (entry.invalidHex === 'eda080' && i === 1) publicText = '���'
    } else {
      // Only these measured conversion templates, not a general best-fit map.
      bytes = Array.from(text, ch => ch === 'é' ? 'e9' : ch.charCodeAt(0).toString(16).padStart(2, '0')).join('')
    }
    return [i + 1, publicText, {kind: 'binary', value: bytes}, text]
  })
}
function validateSemantics(o) {
  const entry = o.case, rows = expectedRows(entry)
  const failure = entry.invalidHex === 'f09f92' ? 7339 : ['80', 'c0af'].includes(entry.invalidHex) ? 9833 : 0
  validateQuery(o.metadata.result, [[['']]], DESCRIPTOR_SHA.metadata[entry.source], 'source metadata')
  statement(o.setup.result, 'setup result')
  statement(o.cleanup.result, 'cleanup result')
  const wireHex = {'CP1252': '0904d00034', 'utf-8': '0904002600', 'CP1251': '1904002200'}[profiles.find(p => p.name === entry.source).codepage]
  assert.equal(o.wireCollationHex, wireHex, 'fixed supplied collation bytes')
  if (failure) {
    const serverName = o.execution.result.errors[0]?.serverName
    assert.match(serverName, /^[0-9a-f]{12}$/)
    const message = failure === 9833 ? 'Invalid data for UTF8-encoded characters'
      : "OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."
    exact(o.execution.result, {
      errors: [{number: failure, state: failure === 9833 ? 2 : 1, class: 16, lineNumber: 1, message, serverName, procName: ''}],
      info: failure === 9833 ? [{number: 3621, state: 0, class: 0, lineNumber: 1, message: 'The statement has been terminated.', serverName, procName: ''}] : [],
      rowCount: 0, error: {number: failure, code: 'EREQUEST', message}
    }, 'fixed failed-load diagnostics')
  } else exact(o.execution.result, {errors: [], info: [], rowCount: rows.length, error: null}, 'fixed successful-load callback')
  validateQuery(o.readback.result, [[[failure, rows.length, 0, 0]], rows], DESCRIPTOR_SHA.readback[entry.name], 'readback')
  const metadata = '81020000000000040026040269006400000000000500a7'
    + (entry.max ? 'ffff' : '4000') + wireHex + '05760061006c0075006500'
  const encoded = o.input.map(row => {
    const id = Buffer.alloc(4); id.writeInt32LE(row.id)
    const value = row.valueHex === null ? null : Buffer.from(row.valueHex, 'hex')
    let payload
    if (!entry.max) {
      const length = Buffer.alloc(2); length.writeUInt16LE(value === null ? 65535 : value.length)
      payload = length.toString('hex') + (value?.toString('hex') ?? '')
    } else if (value === null) payload = 'ffffffffffffffff'
    else {
      const length = Buffer.alloc(4); length.writeUInt32LE(value.length)
      payload = 'feffffffffffffff' + (value.length ? length.toString('hex') + value.toString('hex') : '') + '00000000'
    }
    return 'd104' + id.toString('hex') + payload
  }).join('')
  const actual = o.execution.packets.filter(p => p.direction === 'out' && p.rawHex.startsWith('07')).map(p => p.rawHex.slice(16)).join('')
  exact(actual, metadata + encoded + 'fd000000000000000000000000', 'fixed actual BulkLoad metadata/ROW/PLP/DONE bytes')
}


const DESCRIPTOR_SHA = {"metadata":{"SQL_Latin1_General_CP1_CI_AS":"a72a321ad913fb12430fe85934ece6604f9a0c4fbe0dc342fa8b36af6d7b9e09","Latin1_General_100_BIN2_UTF8":"6a8208336b4c4df9cd22ac4731365fc668ee0b06a369a78088dc8a89a22197c5","Cyrillic_General_100_BIN2":"1e1ac4e8b1e209a5d6a3308548f39a0c59cd694fc1c7724a827dcd02617d2f3b"},"readback":{"CP1252-same-bounded":"9b0c91a7b2ba9638dd5603c729c106ee77586ee6f50ceb4f1c6cf390780183e3","CP1252-same-max":"d870f35885efd86a3bbb8e66dc39bfe11e284301592b98cce81a75f5c57d71af","CP1252-cp1252-bounded":"9b0c91a7b2ba9638dd5603c729c106ee77586ee6f50ceb4f1c6cf390780183e3","CP1252-cp1252-max":"d870f35885efd86a3bbb8e66dc39bfe11e284301592b98cce81a75f5c57d71af","CP1252-unicode-bounded":"53175b2ea794a1c91e084047fff52deee0ed1302f9d3989dbd0a880c7dbf1394","CP1252-unicode-max":"e9676f1f9d1fd4cbd1bcd9810fbfe8a432d1a7b4126bb33cb1dcf90df319651e","utf-8-same-bounded":"4aaa70b820c3446aa8a651b7ad93e31408b3fad1e5b24c42fd15cf3ed5dd9160","utf-8-same-max":"80f9c47758099dccffe2a3aa50360f04e27916593b7efd0a61415065c9559959","utf-8-cp1252-bounded":"9b0c91a7b2ba9638dd5603c729c106ee77586ee6f50ceb4f1c6cf390780183e3","utf-8-cp1252-max":"d870f35885efd86a3bbb8e66dc39bfe11e284301592b98cce81a75f5c57d71af","utf-8-unicode-bounded":"53175b2ea794a1c91e084047fff52deee0ed1302f9d3989dbd0a880c7dbf1394","utf-8-unicode-max":"e9676f1f9d1fd4cbd1bcd9810fbfe8a432d1a7b4126bb33cb1dcf90df319651e","CP1251-same-bounded":"361d111dc3234c4f60e4457481dc077798f0c2f819cc0d68a40a8a7f735a60ce","CP1251-same-max":"fd2f12086cc4f47f9ad1c51ef964384e2839bd9ef48f475d03557ef1cb30b3cf","CP1251-cp1252-bounded":"9b0c91a7b2ba9638dd5603c729c106ee77586ee6f50ceb4f1c6cf390780183e3","CP1251-cp1252-max":"d870f35885efd86a3bbb8e66dc39bfe11e284301592b98cce81a75f5c57d71af","CP1251-unicode-bounded":"53175b2ea794a1c91e084047fff52deee0ed1302f9d3989dbd0a880c7dbf1394","CP1251-unicode-max":"e9676f1f9d1fd4cbd1bcd9810fbfe8a432d1a7b4126bb33cb1dcf90df319651e","utf-8-invalid-80-same":"4aaa70b820c3446aa8a651b7ad93e31408b3fad1e5b24c42fd15cf3ed5dd9160","utf-8-invalid-80-cp1252":"9b0c91a7b2ba9638dd5603c729c106ee77586ee6f50ceb4f1c6cf390780183e3","utf-8-invalid-80-unicode":"53175b2ea794a1c91e084047fff52deee0ed1302f9d3989dbd0a880c7dbf1394","utf-8-invalid-c328-same":"4aaa70b820c3446aa8a651b7ad93e31408b3fad1e5b24c42fd15cf3ed5dd9160","utf-8-invalid-c328-cp1252":"9b0c91a7b2ba9638dd5603c729c106ee77586ee6f50ceb4f1c6cf390780183e3","utf-8-invalid-c328-unicode":"53175b2ea794a1c91e084047fff52deee0ed1302f9d3989dbd0a880c7dbf1394","utf-8-invalid-c0af-same":"4aaa70b820c3446aa8a651b7ad93e31408b3fad1e5b24c42fd15cf3ed5dd9160","utf-8-invalid-c0af-cp1252":"9b0c91a7b2ba9638dd5603c729c106ee77586ee6f50ceb4f1c6cf390780183e3","utf-8-invalid-c0af-unicode":"53175b2ea794a1c91e084047fff52deee0ed1302f9d3989dbd0a880c7dbf1394","utf-8-invalid-eda080-same":"4aaa70b820c3446aa8a651b7ad93e31408b3fad1e5b24c42fd15cf3ed5dd9160","utf-8-invalid-eda080-cp1252":"9b0c91a7b2ba9638dd5603c729c106ee77586ee6f50ceb4f1c6cf390780183e3","utf-8-invalid-eda080-unicode":"53175b2ea794a1c91e084047fff52deee0ed1302f9d3989dbd0a880c7dbf1394","utf-8-invalid-f09f92-same":"4aaa70b820c3446aa8a651b7ad93e31408b3fad1e5b24c42fd15cf3ed5dd9160","utf-8-invalid-f09f92-cp1252":"9b0c91a7b2ba9638dd5603c729c106ee77586ee6f50ceb4f1c6cf390780183e3","utf-8-invalid-f09f92-unicode":"53175b2ea794a1c91e084047fff52deee0ed1302f9d3989dbd0a880c7dbf1394"},"version":"6dac9402560ab85e0497e6b77403e59427dbd53778fbc132310d29f3dd1f20b8"}

export function validate(value) {
  assert.equal(value.format, 1)
  assert.equal(value.containers.length, 2)
  assert.equal(value.runs.length, 4)
  for (const container of value.containers) assert.equal(container.image, referenceImage, 'fixed image')
  for (const run of value.runs) {
    assert.equal(run.version.result.sets[0].rows[0][0], GOLD_VERSION, 'fixed version')
    const database = run.version.result.sets[0].rows[0][1]
    assert.match(database, /^msduck_audit_[0-9a-f]{32}$/)
    validateQuery(run.version.result, [[[GOLD_VERSION, database]]], DESCRIPTOR_SHA.version, 'version')
    assert.equal(run.observations.length, cases.length, 'complete cases')
    for (const [index, o] of run.observations.entries()) {
      assert.ok(isDeepStrictEqual(o.case, cases[index]), 'exact case manifest')
      assert.ok(isDeepStrictEqual(o.input, rowsFor(cases[index])), 'exact supplied bytes')
      assert.match(o.wireCollationHex, /^[0-9a-f]{10}$/)
      for (const step of [run.version, o.metadata, o.setup, o.execution, o.readback, o.cleanup]) {
        assert.ok(step.packets.length > 0 && step.packets.length <= MAX_PACKETS, 'bounded packet count')
        let bytes = 0, message = null
        for (const packet of step.packets) {
          assert.ok(typeof packet.rawHex === 'string' && packet.rawHex.length >= 16 && packet.rawHex.length <= 65534
            && packet.rawHex.length % 2 === 0 && bytes + packet.rawHex.length / 2 <= LIMIT, 'bounded encoded packet')
          assert.match(packet.rawHex, /^(?:[0-9a-f]{2})+$/)
          const raw = Buffer.from(packet.rawHex, 'hex')
          bytes += raw.length
          assert.equal(raw.readUInt16BE(2), raw.length, 'complete packet')
          assert.ok(packet.direction === 'out' ? [1, 7].includes(raw[0]) : packet.direction === 'in' && raw[0] === 4, 'controlled post-login packet')
          if (message) assert.ok(message.direction === packet.direction && message.type === raw[0], 'uniform message')
          else message = {direction: packet.direction, type: raw[0]}
          if (raw[1] & 1) message = null
        }
        assert.equal(message, null, 'complete EOM')
      }
      validateSemantics(o)
    }
  }
  assert.ok(isDeepStrictEqual(value.comparisons, value.runs.slice(1).map(run => compare(run, value.runs[0]))), 'exact raw difference summaries')
}

export function validateRetained(bytes) {
  assert.equal(createHash('sha256').update(bytes).digest('hex'), RETAINED_SHA256, 'fixed retained bytes')
  const value = JSON.parse(bytes.toString())
  validate(value)
  return value
}

export async function persistCapture(actual, output) {
  await guardOutput(output)
  await guardOutput(`${output}.comparison.json`)
  await mkdir(dirname(output), {recursive: true})
  const bytes = JSON.stringify(actual) + '\n'
  await writeFile(output, bytes, {flag: 'wx'})
  let retainedBytes
  try {retainedBytes = await readFile(fixture)} catch (error) {if (error.code !== 'ENOENT') throw error}
  const comparison = retainedBytes ? compare(actual, JSON.parse(retainedBytes.toString())) : []
  await writeFile(`${output}.comparison.json`, JSON.stringify({retained: Boolean(retainedBytes), differences: comparison}) + '\n', {flag: 'wx'})
  validate(actual)
  if (retainedBytes) validateRetained(retainedBytes)
  return {cases: cases.length, runs: 4, output, sha256: createHash('sha256').update(bytes).digest('hex')}
}

export async function main(args = process.argv.slice(2)) {
  const flags = new Set(['--replay-fixture'])
  assert.ok(args.filter(a => a.startsWith('--')).every(a => flags.has(a)), 'known flags')
  const positional = args.filter(a => !a.startsWith('--'))
  assert.ok(positional.length <= 1, 'one output')
  const replay = args.includes('--replay-fixture')
  if (replay) {validateRetained(await readFile(fixture)); console.log('Validated retained character encoding capture'); return}
  const output = resolve(positional[0] ?? 'artifacts/compatibility/bulk-character-encoding/capture.json')
  await guardOutput(output)
  await guardOutput(`${output}.comparison.json`)
  assert.notEqual(output, fixture, 'separate raw output')
  const containers = [], runs = []
  try {
    for (let index = 0; index < 2; index++) {
      await withReferenceContainer(async (config, container) => {
        containers.push({image: container.image})
        for (let database = 0; database < 2; database++) {
          const run = {}
          runs.push(run)
          await isolatedReference(config, connection => observe(connection, run))
        }
      })
    }
  } catch (error) {
    await mkdir(dirname(output), {recursive: true})
    await writeFile(output, JSON.stringify({format: 1, containers, runs, failure: String(error.message)}) + '\n', {flag: 'wx'})
    await writeFile(`${output}.comparison.json`, JSON.stringify({failedCapture: true, failure: String(error.message)}) + '\n', {flag: 'wx'})
    throw error
  }
  const actual = {format: 1, containers, runs, comparisons: runs.slice(1).map(run => compare(run, runs[0]))}
  console.log(JSON.stringify(await persistCapture(actual, output)))
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main()
