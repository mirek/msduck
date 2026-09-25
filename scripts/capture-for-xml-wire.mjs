#!/usr/bin/env node
// Record only post-login TDS packets; TLS/login bytes and credentials are excluded.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/for-xml-wire.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/for-xml-wire/capture.json')
const cases = [
  { name: 'text', query: "SELECT 1 AS [value] FOR XML PATH('row')" },
  { name: 'empty', query: "SELECT 1 AS [value] WHERE 1=0 FOR XML PATH('row')" },
  { name: 'type', query: "SELECT 1 AS [value] FOR XML PATH('row'), TYPE" },
  { name: 'nested type', query: "SELECT (SELECT 1 AS [value] FOR XML PATH('item'), TYPE) AS payload" },
  { name: 'large text', query: "SELECT REPLICATE(CONVERT(NVARCHAR(MAX),N'x'),5000) AS [value] FOR XML PATH('row')" },
  { name: 'attribute order error', query: "SELECT 1 AS [@id], 2 AS [value], 3 AS [@late] FOR XML PATH('row')", expectError: true }
]

function tracePackets(connection) {
  const packets = []
  const pending = { in: Buffer.alloc(0), out: Buffer.alloc(0) }
  let total = 0
  const observe = direction => chunk => {
    total += chunk.length
    assert.ok(total <= 4 * 1024 * 1024, 'bounded FOR XML wire capture')
    pending[direction] = Buffer.concat([pending[direction], chunk])
    while (pending[direction].length >= 8) {
      const length = pending[direction].readUInt16BE(2)
      assert.ok(length >= 8 && length <= 32767, 'valid TDS packet length')
      if (pending[direction].length < length) break
      packets.push({ direction, hex: pending[direction].subarray(0, length).toString('hex') })
      pending[direction] = pending[direction].subarray(length)
    }
  }
  const incoming = connection.messageIo.securePair?.cleartext ?? connection.messageIo.socket
  const outgoing = connection.messageIo.outgoingMessageStream
  const onIn = observe('in')
  const onOut = observe('out')
  incoming.on('data', onIn)
  outgoing.on('data', onOut)
  return () => {
    incoming.off('data', onIn)
    outgoing.off('data', onOut)
    assert.equal(pending.in.length + pending.out.length, 0, 'complete packet capture')
    return packets
  }
}

function messages(packets, direction) {
  const result = []
  let parts = []
  for (const entry of packets.filter(packet => packet.direction === direction)) {
    const packet = Buffer.from(entry.hex, 'hex')
    parts.push(packet.subarray(8))
    if (packet[1] & 1) {
      result.push({ type: packet[0], payloadHex: Buffer.concat(parts).toString('hex') })
      parts = []
    }
  }
  assert.equal(parts.length, 0, 'all TDS messages terminate')
  return result
}

class Reader {
  constructor(bytes) { this.bytes = bytes; this.at = 0 }
  take(length) {
    assert.ok(Number.isSafeInteger(length) && length >= 0 && this.at + length <= this.bytes.length, 'bounded token field')
    const value = this.bytes.subarray(this.at, this.at + length)
    this.at += length
    return value
  }
  u8() { return this.take(1)[0] }
  u16() { return this.take(2).readUInt16LE() }
  u32() { return this.take(4).readUInt32LE() }
  u64() { return this.take(8).readBigUInt64LE() }
  utf16(units) { return this.take(units * 2).toString('utf16le') }
}

function decodeResponse(hex) {
  const bytes = Buffer.from(hex, 'hex')
  const reader = new Reader(bytes)
  const tokens = []
  let type
  while (reader.at < bytes.length) {
    const start = reader.at
    const token = reader.u8()
    if (token === 0x81) {
      const count = reader.u16()
      assert.equal(count, 1, 'one FOR XML result column')
      const userType = reader.u32()
      const flags = reader.u16()
      const typeStart = reader.at
      type = reader.u8()
      let typeInfo
      if (type === 0x63) {
        const maxLength = reader.u32()
        const collation = reader.take(5).toString('hex')
        const tableParts = reader.u8()
        const table = []
        for (let i = 0; i < tableParts; i++) table.push(reader.utf16(reader.u16()))
        typeInfo = { maxLength, collation, table }
      } else if (type === 0xf1) {
        const schemaPresent = reader.u8()
        assert.equal(schemaPresent, 0, 'untyped XML schema info')
        typeInfo = { schemaPresent }
      } else throw new Error(`unexpected FOR XML TYPE_INFO 0x${type.toString(16)}`)
      const typeInfoHex = bytes.subarray(typeStart, reader.at).toString('hex')
      const name = reader.utf16(reader.u8())
      tokens.push({ token: 'COLMETADATA', offset: start, end: reader.at, rawHex: bytes.subarray(start, reader.at).toString('hex'), userType, flags, typeId: type, typeInfoHex, typeInfo, name })
    } else if (token === 0xd1) {
      assert.ok(type === 0x63 || type === 0xf1, 'ROW follows COLMETADATA')
      let framing
      let value
      if (type === 0x63) {
        const pointerLength = reader.u8()
        const pointerHex = reader.take(pointerLength).toString('hex')
        const timestampHex = reader.take(8).toString('hex')
        const length = reader.u32()
        value = reader.take(length)
        framing = { kind: 'ntext', pointerLength, pointerHex, timestampHex, length }
      } else {
        const totalLength = reader.u64()
        const chunks = []
        const values = []
        while (true) {
          const length = reader.u32()
          if (length === 0) break
          chunks.push(length)
          values.push(reader.take(length))
        }
        value = Buffer.concat(values)
        assert.ok(totalLength === 0xfffffffffffffffen || totalLength === BigInt(value.length), 'valid XML PLP total length')
        framing = { kind: 'xml-plp', totalLength: totalLength.toString(), chunks }
      }
      assert.equal(value.length % 2, 0, 'UTF-16 value length')
      tokens.push({ token: 'ROW', offset: start, end: reader.at, framing, valueBytes: value.length, valueSha256: createHash('sha256').update(value).digest('hex'), value: value.toString('utf16le') })
    } else if (token === 0xfd || token === 0xfe || token === 0xff) {
      tokens.push({ token: { 253: 'DONE', 254: 'DONEPROC', 255: 'DONEINPROC' }[token], offset: start, end: start + 13, status: reader.u16(), command: reader.u16(), count: reader.u64().toString(), rawHex: bytes.subarray(start, reader.at).toString('hex') })
    } else if (token === 0xaa || token === 0xab) {
      const length = reader.u16()
      const body = new Reader(reader.take(length))
      const number = body.u32()
      const state = body.u8()
      const severity = body.u8()
      const message = body.utf16(body.u16())
      const server = body.utf16(body.u8())
      const procedure = body.utf16(body.u8())
      const line = body.u32()
      assert.equal(body.at, body.bytes.length, 'complete ERROR/INFO token')
      tokens.push({ token: token === 0xaa ? 'ERROR' : 'INFO', offset: start, end: reader.at, rawHex: bytes.subarray(start, reader.at).toString('hex'), number, state, severity, message, server, procedure, line })
    } else throw new Error(`unexpected TDS token 0x${token.toString(16)} at ${start}`)
  }
  return tokens
}

function headers(packets) {
  return packets.map(({ direction, hex }) => {
    const bytes = Buffer.from(hex, 'hex')
    return { direction, type: bytes[0], status: bytes[1], length: bytes.readUInt16BE(2), spid: bytes.readUInt16BE(4), packetId: bytes[6], window: bytes[7] }
  })
}

function stable(observation) {
  return observation.map(({ name, query, result, tokens }) => {
    const metadata = tokens.filter(token => token.token === 'COLMETADATA').map(({ userType, flags, typeId, typeInfo, name }) => ({ userType, flags, typeId, typeInfo, name }))
    const rows = tokens.filter(token => token.token === 'ROW')
    const errors = tokens.filter(token => token.token === 'ERROR').map(({ number, state, severity, message }) => ({ number, state, severity, message }))
    const done = tokens.filter(token => token.token.startsWith('DONE')).map(({ token, status, command, count }) => ({ token, status, command, count }))
    const semanticResult = name === 'large text'
      ? { ...result, sets: result.sets.map(set => ({ ...set, rows: [[set.rows.map(row => row[0]).join('')]] })) }
      : result
    return { name, query, result: semanticResult, metadata, value: rows.map(row => row.value).join(''), errors, done }
  })
}

async function runDatabase(config) {
  return isolatedReference(config, async connection => {
    const observations = []
    for (const scenario of cases) {
      const stop = tracePackets(connection)
      let packets
      let result
      try {
        try { result = canonical(await capture(connection, scenario.query)) }
        finally { packets = stop() }
        assert.equal(Boolean(result.errors.length), Boolean(scenario.expectError), scenario.name)
        assert.equal(messages(packets, 'out').length, 1, `${scenario.name}: one SQL batch`)
        const responses = messages(packets, 'in')
        assert.equal(responses.length, 1, `${scenario.name}: one tabular response`)
        assert.equal(responses[0].type, 4, `${scenario.name}: tabular result`)
        const tokens = decodeResponse(responses[0].payloadHex)
      assert.equal(tokens.at(-1)?.token, 'DONE', `${scenario.name}: final DONE`)
      assert.equal(tokens.filter(token => token.token === 'ROW').length, result.sets[0]?.rows.length ?? 0, `${scenario.name}: row tokens`)
      assert.deepEqual(tokens.filter(token => token.token === 'ROW').map(token => token.value), result.sets[0]?.rows.map(row => row[0]) ?? [], `${scenario.name}: row values`)
      const metadata = tokens.find(token => token.token === 'COLMETADATA')
      if (scenario.expectError) {
        assert.equal(metadata, undefined, 'invalid shape has no metadata')
        assert.equal(tokens.find(token => token.token === 'ERROR')?.number, 6852)
        assert.equal(tokens.at(-1).status, 0x0002)
      } else {
        assert.equal(metadata.typeId, scenario.name.includes('type') ? 0xf1 : 0x63, `${scenario.name}: TYPE_INFO ID`)
        assert.equal(metadata.name, scenario.name === 'nested type' ? 'payload' : scenario.name === 'type' ? '' : 'XML_F52E2B61-18A1-11d1-B105-00805F49916B')
      }
      if (scenario.name === 'empty') assert.equal(tokens.filter(token => token.token === 'ROW').length, 0)
      if (scenario.name === 'large text') {
        const value = tokens.filter(token => token.token === 'ROW').map(token => token.value).join('')
        assert.equal(value, `<row><value>${'x'.repeat(5000)}</value></row>`)
        assert.ok(Buffer.byteLength(value, 'utf16le') > 8192, 'large result exceeds 8 KiB')
      }
        observations.push({ name: scenario.name, query: scenario.query, result, packets, headers: headers(packets), responses, tokens })
      } catch (error) {
        error.capture = { previous: observations, scenario, result, packets }
        throw error
      }
    }
    return observations
  })
}

await mkdir(dirname(output), { recursive: true })
try {
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (writeFixture) assert.equal(retained, undefined, 'refusing to overwrite retained fixture')
  const captureContainer = () => withReferenceContainer(async (config, container) => {
    const runs = [await runDatabase(config), await runDatabase(config)]
    assert.deepEqual(stable(runs[0]), stable(runs[1]), 'fresh databases differ')
    return { image: container.image, runs }
  })
  const first = await captureContainer()
  await writeFile(output, JSON.stringify(first) + '\n')
  const independent = await captureContainer()
  assert.equal(independent.image, first.image, 'same pinned image')
  for (const run of independent.runs) assert.deepEqual(stable(run), stable(first.runs[0]), 'independent container differs')
  const actual = { ...first, independentRuns: independent.runs }
  await writeFile(output, JSON.stringify(actual) + '\n')
  if (retained) assert.deepEqual(stable(actual.runs[0]), stable(retained.runs[0]), 'retained wire observations differ')
  if (writeFixture) {
    await writeFile(fixture, JSON.stringify(actual) + '\n')
  }
  console.log(`Captured ${cases.length} FOR XML wire cases in two fresh databases and an independent fresh container`)
} catch (error) {
  const failedCapture = error.capture ?? null
  await writeFile(resolve(dirname(output), 'capture-failed.json'), JSON.stringify({ error: error.message, capture: failedCapture }) + '\n')
  delete error.capture
  throw error
}
