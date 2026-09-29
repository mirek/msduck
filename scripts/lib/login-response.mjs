// Observe one tedious login and decode the server's LOGIN7 response message.
// Only incoming bytes after the TLS handshake are recorded, so the outgoing
// LOGIN7 (which carries the password) is never retained.
import assert from 'node:assert/strict'
import { Connection, Request } from 'tedious'

// Bounded reader over one response payload.
class Reader {
  constructor(bytes) { this.bytes = bytes; this.at = 0 }
  get done() { return this.at >= this.bytes.length }
  take(length) {
    assert.ok(Number.isSafeInteger(length) && length >= 0 && this.at + length <= this.bytes.length, 'bounded token field')
    const value = this.bytes.subarray(this.at, this.at + length)
    this.at += length
    return value
  }
  u8() { return this.take(1)[0] }
  u16() { return this.take(2).readUInt16LE() }
  u32() { return this.take(4).readUInt32LE() }
  i32() { return this.take(4).readInt32LE() }
  u64() { return this.take(8).readBigUInt64LE().toString() }
  bText() { return this.take(this.u8() * 2).toString('utf16le') }
  usText() { return this.take(this.u16() * 2).toString('utf16le') }
}

const DONE = { 0xfd: 'DONE', 0xfe: 'DONEPROC', 0xff: 'DONEINPROC' }

// Decode every token of a login response. Unknown tokens fail the capture
// rather than being skipped, so the decoded list is the complete stream.
export function decodeTokens(payload) {
  const tokens = []
  const reader = new Reader(payload)
  while (!reader.done) {
    const start = reader.at
    const count = tokens.length
    decodeToken(reader, tokens)
    assert.equal(tokens.length, count + 1, 'one token decoded')
    Object.defineProperty(tokens[count], 'byteLength', { value: reader.at - start })
  }
  return tokens
}

function decodeToken(reader, tokens) {
  const type = reader.u8()
  if (type === 0xaa || type === 0xab) {
    const body = new Reader(reader.take(reader.u16()))
    const token = {
      token: type === 0xaa ? 'ERROR' : 'INFO', number: body.i32(), state: body.u8(), class: body.u8(),
      message: body.usText(), serverName: body.bText(), procName: body.bText(), lineNumber: body.u32()
    }
    assert.ok(body.done, 'diagnostic token fully decoded')
    tokens.push(token)
  } else if (type === 0xe3) {
    const body = new Reader(reader.take(reader.u16()))
    const kind = body.u8()
    if ([1, 2, 4].includes(kind)) tokens.push({ token: 'ENVCHANGE', type: kind, newValue: body.bText(), oldValue: body.bText() })
    else if (kind === 7) tokens.push({ token: 'ENVCHANGE', type: kind, newValue: body.take(body.u8()).toString('hex'), oldValue: body.take(body.u8()).toString('hex') })
    else tokens.push({ token: 'ENVCHANGE', type: kind, bodyHex: body.take(body.bytes.length - body.at).toString('hex') })
    assert.ok(body.done, 'ENVCHANGE fully decoded')
  } else if (type === 0xad) {
    const body = new Reader(reader.take(reader.u16()))
    const token = { token: 'LOGINACK', interface: body.u8(), tdsVersion: body.take(4).toString('hex'), progName: body.bText(), version: [...body.take(4)] }
    assert.ok(body.done, 'LOGINACK fully decoded')
    tokens.push(token)
  } else if (type === 0xae) {
    const features = []
    for (let id = reader.u8(); id !== 0xff; id = reader.u8()) features.push({ id, dataHex: reader.take(reader.u32()).toString('hex') })
    tokens.push({ token: 'FEATUREEXTACK', features })
  } else if (DONE[type]) {
    tokens.push({ token: DONE[type], status: reader.u16(), command: reader.u16(), rowCount: reader.u64() })
  } else {
    throw new Error(`unexpected login response token 0x${type.toString(16)}`)
  }
}

// Split the incoming byte stream into packets and return the first complete
// message: the LOGIN7 response. SPID is environment specific and reported
// separately from the stable header fields.
export function firstMessage(bytes) {
  const packets = []
  const parts = []
  let at = 0
  while (at + 8 <= bytes.length) {
    const length = bytes.readUInt16BE(at + 2)
    assert.ok(length >= 8 && at + length <= bytes.length, 'complete TDS packet')
    const header = bytes.subarray(at, at + 8)
    packets.push({ type: header[0], status: header[1], length, packetId: header[6], window: header[7], spid: header.readUInt16BE(4) })
    parts.push(bytes.subarray(at + 8, at + length))
    at += length
    if (header[1] & 1) return { packets, payload: Buffer.concat(parts) }
  }
  throw new Error('login response message did not complete')
}

// Replace the server's B_VARCHAR name in raw bytes and decoded tokens with a
// marker, and remove its bytes from each diagnostic token's length field.
// Every diagnostic must carry the name exactly once and no other token may,
// so the marker cannot hide any other difference.
export function normalizeServerName(payload, tokens, serverName) {
  const encoded = Buffer.concat([Buffer.from([serverName.length]), Buffer.from(serverName, 'utf16le')])
  const pieces = []
  let at = 0
  for (const token of tokens) {
    const bytes = payload.subarray(at, at + token.byteLength)
    at += token.byteLength
    const occurrences = bytes.toString('hex').split(encoded.toString('hex')).length - 1
    if (token.token !== 'ERROR' && token.token !== 'INFO') {
      assert.equal(occurrences, 0, 'server name appears only in diagnostics')
      pieces.push(bytes.toString('hex'))
      continue
    }
    assert.equal(token.serverName, serverName, 'diagnostic server name')
    // Number, state, class and the US_VARCHAR message precede the name.
    const offset = 3 + 4 + 2 + 2 + 2 * bytes.readUInt16LE(3 + 4 + 2)
    assert.ok(bytes.subarray(offset, offset + encoded.length).equals(encoded), 'server name position')
    const length = Buffer.alloc(2)
    length.writeUInt16LE(bytes.readUInt16LE(1) - 2 * serverName.length)
    pieces.push(bytes.subarray(0, 1).toString('hex') + length.toString('hex') +
      bytes.subarray(3, offset).toString('hex') + '<serverName>' + bytes.subarray(offset + encoded.length).toString('hex'))
  }
  assert.equal(at, payload.length, 'tokens cover the payload')
  return {
    payloadHex: pieces.join(''),
    tokens: tokens.map(token => 'serverName' in token ? { ...token, serverName: '<serverName>' } : { ...token })
  }
}

// Log in once and return tedious events, the decoded response and whether the
// server closed the connection. Tedious' own socket teardown after a failed
// login is deferred briefly so a server-initiated close is observable.
export function observeLogin(config, { database, userName, password, batch, closeWaitMs = 1500 }) {
  const options = { ...config.options, database }
  const authentication = { ...config.authentication, options: { ...config.authentication.options } }
  if (userName !== undefined) authentication.options.userName = userName
  if (password !== undefined) authentication.options.password = password
  return new Promise((resolve, reject) => {
    const events = []
    const incoming = []
    let serverClosed = false
    let received = 0
    const connection = new Connection({ ...config, authentication, options })
    connection.on('secure', cleartext => {
      events.push(['secure'])
      cleartext.on('data', chunk => {
        received += chunk.length
        assert.ok(received <= 1024 * 1024, 'bounded login capture')
        incoming.push(Buffer.from(chunk))
      })
      const socket = connection.socket
      socket.on('end', () => { serverClosed = true })
      const destroy = socket.destroy.bind(socket)
      socket.destroy = (...args) => { setTimeout(() => destroy(...args), closeWaitMs).unref(); return socket }
    })
    for (const name of ['infoMessage', 'errorMessage']) {
      connection.on(name, m => events.push([name, m.number, m.state, m.class, m.message]))
    }
    connection.on('databaseChange', name => events.push(['databaseChange', name]))
    let ended
    const end = new Promise(done => { ended = done })
    connection.on('end', () => { events.push(['end']); ended() })
    connection.on('error', error => events.push(['error', error.code ?? null, error.message]))
    connection.connect(async error => {
      try {
        events.push(['connect', error ? { code: error.code ?? null, message: error.message } : null])
        let rows = null
        if (!error && batch) {
          rows = await new Promise((resolveRows, rejectRows) => {
            const out = []
            const request = new Request(batch, failure => failure ? rejectRows(failure) : resolveRows(out))
            request.on('row', cells => out.push(cells.map(cell => cell.value)))
            connection.execSqlBatch(request)
          })
        }
        // A failed login is already closing; wait for any server FIN first.
        await new Promise(done => setTimeout(done, error ? closeWaitMs - 250 : 0))
        const closedByServer = serverClosed
        if (!error) connection.close()
        await end
        const { packets, payload } = firstMessage(Buffer.concat(incoming))
        resolve({ events, rows, packets, payload, tokens: decodeTokens(payload), closedByServer })
      } catch (failure) { reject(failure) }
    })
  })
}

// The stable, comparable form of an observation: SPID and server name are
// environment specific (see docs/login-database-error.md).
export function comparable(observation, serverName) {
  const { payloadHex, tokens } = normalizeServerName(observation.payload, observation.tokens, serverName)
  // The packet length includes each server name; report it without them.
  assert.equal(observation.packets.length, 1, 'single-packet login response')
  const nameBytes = 2 * serverName.length * tokens.filter(token => 'serverName' in token).length
  return {
    events: observation.events,
    rows: observation.rows,
    packets: observation.packets.map(({ spid, length, ...rest }) => ({ ...rest, lengthWithoutServerNames: length - nameBytes, spidNonZero: spid !== 0 })),
    payloadHex,
    tokens,
    closedByServer: observation.closedByServer
  }
}
