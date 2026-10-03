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
const RETAINED_SHA256 = null
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

export function validate(value) {
  assert.equal(value.format, 1)
  assert.equal(value.containers.length, 2)
  assert.equal(value.runs.length, 4)
  for (const container of value.containers) assert.equal(container.image, referenceImage, 'fixed image')
  for (const run of value.runs) {
    assert.equal(run.version.result.sets[0].rows[0][0], GOLD_VERSION, 'fixed version')
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
  await mkdir(dirname(output), {recursive: true})
  const bytes = JSON.stringify(actual) + '\n'
  await writeFile(output, bytes, {flag: 'wx'})
  let retainedBytes
  try {retainedBytes = await readFile(fixture)} catch (error) {if (error.code !== 'ENOENT') throw error}
  const comparison = retainedBytes ? compare(actual, JSON.parse(retainedBytes.toString())) : []
  await writeFile(`${output}.comparison.json`, JSON.stringify({retained: Boolean(retainedBytes), differences: comparison}) + '\n', {flag: 'wx'})
  validate(actual)
  if (retainedBytes) validateRetained(retainedBytes)
  console.log(JSON.stringify({cases: cases.length, runs: 4, output, sha256: createHash('sha256').update(bytes).digest('hex')}))
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main()
