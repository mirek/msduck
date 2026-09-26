#!/usr/bin/env node
// First-party post-login BulkLoad evidence. Never record LOGIN7 or setup traffic.
import assert from 'node:assert/strict'
import {lstat, mkdir, readFile, realpath, stat, writeFile} from 'node:fs/promises'
import {basename, dirname, resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {isDeepStrictEqual} from 'node:util'
import {TYPES} from 'tedious'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference, refuseExistingFixture, writeNewFixture, describeFirstDifference} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

const fixture = new URL('../reference/bulk-load-wire.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const replayFixture = args.includes('--replay-fixture')
const positional = args.filter(arg => arg !== '--write-fixture' && arg !== '--replay-fixture')
if (writeFixture && replayFixture) throw Error('choose fixture writing or offline replay')
if (positional.length > 1) throw Error('expected at most one diagnostic output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/bulk-load-wire/capture.json')
const MAX_PACKET = 32767
const MAX_CASE_BYTES = 512 * 1024
const MAX_CASE_PACKETS = 128

async function canonicalTarget(path) {
  let current = path
  const missing = []
  for (;;) {
    try { return resolve(await realpath(current), ...missing.reverse()) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    const parent = dirname(current)
    if (parent === current) throw Error('cannot resolve diagnostic output')
    missing.push(basename(current))
    current = parent
  }
}

async function assertSeparateOutput() {
  for (let part = output;; part = dirname(part)) {
    let info
    try { info = await lstat(part) } catch (error) { if (error.code !== 'ENOENT') throw error }
    assert.ok(!info?.isSymbolicLink(), 'diagnostic output path contains a symlink')
    if (dirname(part) === part) break
  }
  const retained = fileURLToPath(fixture)
  assert.notEqual(await canonicalTarget(output), await canonicalTarget(retained), 'diagnostic output aliases retained fixture')
  let outputInfo, retainedInfo
  try { outputInfo = await stat(output) } catch (error) { if (error.code !== 'ENOENT') throw error }
  try { retainedInfo = await stat(retained) } catch (error) { if (error.code !== 'ENOENT') throw error }
  assert.ok(!outputInfo || !retainedInfo || outputInfo.dev !== retainedInfo.dev || outputInfo.ino !== retainedInfo.ino,
    'diagnostic output hard-links retained fixture')
}

function frame(packet) {
  assert.ok(packet.length >= 8 && packet.length <= MAX_PACKET, 'bounded TDS packet')
  assert.equal(packet.readUInt16BE(2), packet.length, 'complete TDS packet')
  return {type: packet[0], status: packet[1], packetId: packet[6], length: packet.length,
    rawHex: packet.toString('hex'), payloadHex: packet.subarray(8).toString('hex')}
}

function traceExchange(connection) {
  const outgoing = connection.messageIo.outgoingMessageStream
  const incoming = connection.messageIo.securePair?.cleartext ?? connection.messageIo.socket
  const packets = []
  let pending = Buffer.alloc(0)
  let total = 0
  const retain = (direction, packet) => {
    total += packet.length
    assert.ok(total <= MAX_CASE_BYTES && packets.length < MAX_CASE_PACKETS, 'bounded BulkLoad capture')
    const entry = frame(packet)
    assert.ok(direction === 'out' ? [1, 7].includes(entry.type) : entry.type === 4,
      'only controlled SQL Batch, BulkLoad and response packets')
    packets.push({direction, ...entry})
  }
  const onOutgoing = packet => retain('out', packet)
  const onIncoming = chunk => {
    assert.ok(chunk.length <= MAX_CASE_BYTES && pending.length + chunk.length <= MAX_CASE_BYTES, 'bounded incoming buffer')
    pending = Buffer.concat([pending, chunk])
    while (pending.length >= 8) {
      const length = pending.readUInt16BE(2)
      assert.ok(length >= 8 && length <= MAX_PACKET, 'bounded incoming TDS packet length')
      if (pending.length < length) break
      retain('in', pending.subarray(0, length))
      pending = pending.subarray(length)
    }
  }
  outgoing.on('data', onOutgoing)
  incoming.on('data', onIncoming)
  return () => {
    outgoing.off('data', onOutgoing)
    incoming.off('data', onIncoming)
    assert.equal(pending.length, 0, 'complete incoming packets')
    const messages = []
    let current
    for (const packet of packets) {
      if (!current) current = {direction: packet.direction, type: packet.type, packets: []}
      assert.equal(packet.direction, current.direction, 'no cross-direction interleaving within a message')
      assert.equal(packet.type, current.type, 'uniform packet type per message')
      current.packets.push(packet)
      if (packet.status & 1) {
        current.payloadHex = current.packets.map(part => part.payloadHex).join('')
        messages.push(current)
        current = undefined
      }
    }
    assert.equal(current, undefined, 'complete TDS messages')
    assert.deepEqual(messages.map(message => [message.direction, message.type]),
      [['out', 1], ['in', 4], ['out', 7], ['in', 4]], 'two complete BulkLoad request-response rounds')
    return messages
  }
}

const cases = [
  {name: 'zero rows', rows: [], expectedRows: []},
  {name: 'NULL row', rows: [{id: 1, label: null, payload: null}], expectedRows: [[1, null, null]]},
  {name: 'Unicode and binary', rows: [{id: 2, label: 'é😀', payload: Buffer.from([0, 255, 128])}],
    expectedRows: [[2, 'é😀', {kind: 'binary', value: '00ff80'}]]},
  {name: 'multirow', rows: [
    {id: 3, label: 'alpha', payload: Buffer.from([1, 2])},
    {id: 4, label: null, payload: Buffer.alloc(0)},
    {id: 5, label: 'Ω', payload: null}
  ], expectedRows: [[3, 'alpha', {kind: 'binary', value: '0102'}],
    [4, null, {kind: 'binary', value: ''}], [5, 'Ω', null]]}
]

function runBulk(connection, rows) {
  return new Promise(resolve => {
    const errors = []
    const onError = error => errors.push({number: error.number, state: error.state, class: error.class, message: error.message})
    connection.on('errorMessage', onError)
    const load = connection.newBulkLoad('dbo.BulkWireProbe', (error, rowCount) => {
      connection.off('errorMessage', onError)
      resolve({rowCount: rowCount ?? null, errors, error: error ?
        {number: error.number ?? null, code: error.code ?? null, message: error.message} : null})
    })
    load.addColumn('id', TYPES.Int, {nullable: false})
    load.addColumn('label', TYPES.NVarChar, {length: 16, nullable: true})
    load.addColumn('payload', TYPES.VarBinary, {length: 16, nullable: true})
    connection.execBulkLoad(load, rows)
  })
}

async function observe(connection) {
  const version = canonical(await capture(connection, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version"))
  assert.deepEqual(version.errors, [])
  const observations = []
  for (const entry of cases) {
    const setup = await capture(connection, 'CREATE TABLE dbo.BulkWireProbe (id INT NOT NULL, label NVARCHAR(16) NULL, payload VARBINARY(16) NULL)')
    assert.deepEqual(setup.errors, [])
    const stop = traceExchange(connection)
    let callback, messages
    try { callback = await runBulk(connection, entry.rows) }
    finally { messages = stop() }
    const readback = canonical(await capture(connection, 'SELECT id, label, payload FROM dbo.BulkWireProbe ORDER BY id'))
    assert.deepEqual(readback.errors, [])
    assert.deepEqual(readback.sets[0].rows, entry.expectedRows)
    assert.ok(Buffer.from(messages[0].payloadHex, 'hex').toString('utf16le').toLowerCase().includes('insert bulk'),
      'generated INSERT BULK SQL Batch')
    if (entry.rows.length === 0) {
      assert.equal(messages[2].payloadHex, 'fd000000000000000000000000', 'tedious DONE-only empty stream')
      assert.equal(callback.error?.number, 4804)
      assert.equal(callback.errors[0]?.number, 4804)
    } else {
      assert.equal(Buffer.from(messages[2].payloadHex, 'hex')[0], 0x81, 'BulkLoad COLMETADATA')
      assert.equal(callback.error, null)
      assert.deepEqual(callback.errors, [])
      assert.equal(callback.rowCount, entry.rows.length)
    }
    observations.push({name: entry.name, input: entry.rows.map(row => ({id: row.id, label: row.label,
      payloadHex: row.payload?.toString('hex') ?? null})), messages, callback, readback})
    const drop = await capture(connection, 'DROP TABLE dbo.BulkWireProbe')
    assert.deepEqual(drop.errors, [])
  }
  return {version, observations}
}

function validateFixture(value) {
  assert.equal(value.format, 1)
  assert.equal(value.containers.length, 2)
  assert.equal(value.runs.length, 4)
  for (const run of value.runs) {
    assert.equal(run.observations.length, cases.length)
    for (const [index, observation] of run.observations.entries()) {
      assert.equal(observation.name, cases[index].name)
      assert.deepEqual(observation.readback.sets[0].rows, cases[index].expectedRows)
      assert.equal(observation.callback.error?.number ?? null, index === 0 ? 4804 : null)
      if (index === 0) assert.equal(observation.messages[2].payloadHex, 'fd000000000000000000000000')
      else assert.equal(observation.callback.rowCount, cases[index].rows.length)
      const messages = observation.messages
      assert.deepEqual(messages.map(message => [message.direction, message.type]),
        [['out', 1], ['in', 4], ['out', 7], ['in', 4]])
      let totalBytes = 0
      for (const message of messages) {
        assert.ok(message.packets.length > 0 && message.packets.length <= MAX_CASE_PACKETS)
        assert.equal(message.payloadHex, message.packets.map(packet => packet.payloadHex).join(''))
        for (const [packetIndex, packet] of message.packets.entries()) {
          assert.ok(packet.rawHex.length <= MAX_PACKET * 2, 'bounded retained packet')
          const bytes = Buffer.from(packet.rawHex, 'hex')
          assert.deepEqual(frame(bytes), (({direction, ...rest}) => rest)(packet))
          totalBytes += bytes.length
          assert.ok(totalBytes <= MAX_CASE_BYTES, 'bounded retained exchange')
          assert.equal(packet.direction, message.direction)
          assert.equal(packet.type, message.type)
          assert.equal(Boolean(packet.status & 1), packetIndex === message.packets.length - 1, 'EOM only on final packet')
        }
      }
    }
  }
  assert.equal(value.comparisons.length, 3)
  for (let i = 1; i < 4; i++) {
    const equal = isDeepStrictEqual(value.runs[0], value.runs[i])
    assert.equal(value.comparisons[i - 1].exactMatch, equal)
    assert.equal(value.comparisons[i - 1].firstDifference,
      equal ? null : describeFirstDifference(value.runs[i], value.runs[0]))
  }
}

await assertSeparateOutput()
if (writeFixture) await refuseExistingFixture(fixture)
if (replayFixture) {
  const retained = JSON.parse(await readFile(fixture, 'utf8'))
  validateFixture(retained)
  console.log('Replayed exact retained BulkLoad packet bytes and four-run comparison')
  process.exit(0)
}
await mkdir(dirname(output), {recursive: true})
const containers = []
const runs = []
for (let i = 0; i < 2; i++) {
  const result = await withReferenceContainer(async (config, container) => ({
    image: container.image,
    runs: [await isolatedReference(config, observe), await isolatedReference(config, observe)]
  }))
  containers.push({image: result.image})
  runs.push(...result.runs)
}
assert.equal(containers[0].image, containers[1].image)
const comparisons = runs.slice(1).map((run, index) => {
  const equal = isDeepStrictEqual(runs[0], run)
  return {run: index + 1, exactMatch: equal, firstDifference: equal ? null : describeFirstDifference(run, runs[0])}
})
const actual = {format: 1, containers, runs, comparisons}
validateFixture(actual)
await writeFile(output, JSON.stringify(actual) + '\n')
let retained
try { retained = JSON.parse(await readFile(fixture, 'utf8')) } catch (error) { if (error.code !== 'ENOENT') throw error }
let matchedRetained = false
if (retained) {
  validateFixture(retained)
  matchedRetained = isDeepStrictEqual(actual, retained)
  if (!matchedRetained) {
    console.log(`Fresh SQL Server wire differs from retained fixture: ${describeFirstDifference(actual, retained)}`)
  }
}
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${cases.length} BulkLoad cases in four fresh databases across two containers${matchedRetained ? ' and matched fixture' : ''}`)
