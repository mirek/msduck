import assert from 'node:assert/strict'
import { test } from 'node:test'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync, writeFileSync, renameSync, unlinkSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { Connection, TYPES } from 'tedious'
import { connect } from 'node:net'
import { start, query } from './support/client.mjs'

function certificate(t) {
  const directory = mkdtempSync(join(tmpdir(), 'msduck-tls-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const cert = join(directory, 'cert.pem'), key = join(directory, 'key.pem')
  const names = ['DNS:localhost', 'IP:127.0.0.1', ...Array.from({ length: 140 }, (_, i) => `DNS:fragment-${i}.certificate-handshake.msduck.test`)].join(',')
  execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
    '-subj', '/CN=localhost', '-addext', `subjectAltName=${names}`,
    '-keyout', key, '-out', cert], { stdio: 'ignore' })
  const ca = readFileSync(cert)
  const der = Buffer.from(ca.toString().replace(/-----[^-]+-----|\s/g, ''), 'base64')
  assert.ok(der.length > 4096, 'certificate forces fragmented TLS handshake output')
  return { serverArgs: ['--tls-cert', cert, '--tls-key', key], ca }
}

test('required TDS TLS encrypts login, fragmented RPC, results and transaction traffic', { timeout: 30000 }, async t => {
  const { serverArgs, ca } = certificate(t)
  const c = await start(t, { serverArgs, options: {
    encrypt: true, trustServerCertificate: false, serverName: 'localhost',
    cryptoCredentialsDetails: { ca }, packetSize: 512
  } })
  const text = 'Encrypted Duck 🦆'.repeat(2000)
  assert.deepEqual((await query(c, 'SELECT @s AS text', [['s', TYPES.NVarChar, text]])).rows, [[text]])
  await query(c, 'CREATE TABLE tls_rows(n INT)')
  await query(c, 'BEGIN TRAN; INSERT INTO tls_rows VALUES(42); ROLLBACK')
  assert.deepEqual((await query(c, 'SELECT COUNT(*) FROM tls_rows')).rows, [[0]])
  await assert.rejects(query(c, 'SELECT * FROM missing_tls_table'), e => e.number === 208)
  assert.deepEqual((await query(c, 'SELECT 42')).rows, [[42]])
})

test('required TDS TLS negotiation rejects incapable clients and enforces certificate verification', { timeout: 30000 }, async t => {
  const { serverArgs, ca } = certificate(t)
  const options = { encrypt: true, trustServerCertificate: false, serverName: 'localhost', cryptoCredentialsDetails: { ca } }
  const c = await start(t, { serverArgs, options })
  assert.deepEqual((await query(c, 'SELECT 17')).rows, [[17]])
  // Tedious encrypt:false advertises NOT_SUP, rather than protocol OFF.
  await assert.rejects(start(t, { serverArgs, options: { ...options, encrypt: false } }), e => e.code === 'EENCRYPT')
  const reply = await new Promise((resolve, reject) => {
    const socket = connect(c.config.options.port, '127.0.0.1')
    t.after(() => socket.destroy())
    socket.setTimeout(5000, () => { socket.destroy(); reject(new Error('PRELOGIN timed out')) })
    socket.once('error', reject)
    socket.once('connect', () => socket.write(Buffer.from('1201001a0000010000000b00060100110001ff10000000000000', 'hex')))
    let received = Buffer.alloc(0)
    socket.on('data', data => {
      received = Buffer.concat([received, data])
      if (received.length >= 8 && received.length >= received.readUInt16BE(2)) {
        resolve(received.subarray(8)); socket.destroy()
      }
    })
  })
  assert.equal(reply[22], 3) // Capable OFF is upgraded to ENCRYPT_REQ.
  await assert.rejects(start(t, { serverArgs, options: { ...options, cryptoCredentialsDetails: {} } }),
    e => /certificate|self.signed/i.test(e.message))
  await assert.rejects(start(t, { serverArgs, options: { ...options, serverName: 'wrong.example' } }),
    e => /certificate|hostname|altnames/i.test(e.message))
})

test('TLS configuration rejects missing, malformed and mismatched keys before listening', { timeout: 20000 }, async t => {
  const first = certificate(t), second = certificate(t)
  for (const args of [
    ['--tls-cert', first.serverArgs[1]],
    ['--admin-credentials', 'unused.json'],
    ['--tls-cert', first.serverArgs[1], '--tls-key', first.serverArgs[1]],
    ['--tls-cert', first.serverArgs[1], '--tls-key', second.serverArgs[3]]
  ]) {
    assert.throws(() => execFileSync('target/debug/msduck', ['--listen', '127.0.0.1:0', ...args],
      { stdio: ['ignore', 'pipe', 'pipe'], timeout: 5000 }), error => {
      assert.equal(error.status, 1)
      assert.match(error.stderr.toString(), /TLS|tls-cert/)
      assert.doesNotMatch(error.stderr.toString(), /listening on/)
      return true
    })
  }
})


function credentialFile(path, userName, password) {
  const json = execFileSync(process.execPath, ['scripts/create-admin.mjs', userName], { input: password })
  const value = JSON.parse(json)
  assert.equal(value.userName, userName)
  assert.match(value.passwordHash, /^msduck\$pbkdf2-sha256\$v1\$/)
  writeFileSync(path + '.next', json, { mode: 0o600 })
  renameSync(path + '.next', path)
}

async function loginAs(t, port, ca, userName, password) {
  const errors = []
  const c = new Connection({ server: '127.0.0.1',
    authentication: { type: 'default', options: { userName, password } },
    options: { port, encrypt: true, trustServerCertificate: false, serverName: 'localhost',
      cryptoCredentialsDetails: { ca }, connectTimeout: 5000, requestTimeout: 5000 }
  })
  t.after(() => c.close())
  c.on('error', () => {})
  c.on('errorMessage', e => errors.push([e.number, e.state, e.class, e.message]))
  try {
    await new Promise((resolve, reject) => c.connect(e => e ? reject(e) : resolve()))
    return c
  } catch (error) { error.wireErrors = errors; throw error }
}

test('bootstrap SQL administrator authenticates Unicode passwords and fails closed on rotation errors', { timeout: 30000 }, async t => {
  const { serverArgs, ca } = certificate(t)
  const path = join(serverArgs[1], '..', 'admin.json')
  const password = 'Pässword 🦆 with spaces'
  credentialFile(path, 'Admin🦆', password)
  const c = await start(t, { serverArgs: [...serverArgs, '--admin-credentials', path],
    authentication: { type: 'default', options: { userName: 'admin🦆', password } },
    options: { encrypt: true, trustServerCertificate: false, serverName: 'localhost', cryptoCredentialsDetails: { ca } }
  })
  const rejected = error => {
    assert.equal(error.code, 'ELOGIN')
    assert.deepEqual(error.wireErrors, [[18456, 1, 14, 'Login failed.']])
    return true
  }
  const port = c.config.options.port
  assert.deepEqual((await query(c, 'SELECT ORIGINAL_LOGIN()')).rows, [['Admin🦆']])
  for (const [name, candidate] of [['unknown', password], ['Admin🦆', 'wrong'], ['Admin🦆', ''], ['Admin🦆\0', password]]) {
    await assert.rejects(loginAs(t, port, ca, name, candidate), rejected)
  }
  await query(c, 'CREATE TABLE authenticated_rows(n INT); INSERT INTO authenticated_rows VALUES(1)')
  credentialFile(path, 'Admin🦆', 'replacement 🦆')
  await assert.rejects(loginAs(t, port, ca, 'Admin🦆', password), rejected)
  const rotated = await loginAs(t, port, ca, 'Admin🦆', 'replacement 🦆')
  assert.deepEqual((await query(rotated, 'SELECT n FROM authenticated_rows')).rows, [[1]])
  credentialFile(path, 'Rotated🦆', 'replacement 🦆')
  const renamed = await loginAs(t, port, ca, 'rotated🦆', 'replacement 🦆')
  assert.deepEqual((await query(renamed, 'SELECT ORIGINAL_LOGIN()')).rows, [['Rotated🦆']])
  assert.deepEqual((await query(c, 'SELECT ORIGINAL_LOGIN()')).rows, [['Admin🦆']])
  writeFileSync(path, '{invalid')
  await assert.rejects(loginAs(t, port, ca, 'Admin🦆', 'replacement 🦆'), rejected)
  unlinkSync(path)
  await assert.rejects(loginAs(t, port, ca, 'Admin🦆', 'replacement 🦆'), rejected)
  // Rotation and failed new logins do not destroy already authenticated sessions.
  assert.deepEqual((await query(c, 'SELECT n FROM authenticated_rows')).rows, [[1]])
})
