// LOGIN7 naming a database that does not exist, compared token for token with
// the SQL Server 2025 capture in reference/login-database-error.json
// (see docs/login-database-error.md).
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { start } from './support/client.mjs'
import { assertSameCapture } from '../scripts/lib/reference.mjs'
import { observeLogin, comparable } from '../scripts/lib/login-response.mjs'

const fixture = JSON.parse(readFileSync(new URL('../reference/login-database-error.json', import.meta.url), 'utf8'))
const reference = name => {
  const record = fixture.runs[0].find(item => item.name === name)
  assert.ok(record, `missing reference case ${name}`)
  return record
}

function tlsServer(t) {
  const directory = mkdtempSync(join(tmpdir(), 'msduck-login-database-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const cert = join(directory, 'cert.pem'), key = join(directory, 'key.pem')
  execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
    '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1',
    '-keyout', key, '-out', cert], { stdio: 'ignore' })
  return { directory, serverArgs: ['--tls-cert', cert, '--tls-key', key] }
}

// The owner's probe shape: encryption on, a trusted development certificate.
const options = { encrypt: true, trustServerCertificate: true }

// msduck names itself `msduck` where SQL Server sends @@SERVERNAME, and writes
// SPID 0 in packet headers; both are documented environment mappings.
function compare(observation, expected) {
  const actual = comparable(observation, 'msduck')
  const strip = record => ({ ...record, packets: record.packets.map(({ spidNonZero, ...packet }) => packet) })
  const { name, database, userName, ...wanted } = strip(expected)
  assertSameCapture(strip(actual), wanted, `${name} differs from SQL Server`)
  assert.equal(actual.packets.every(packet => !packet.spidNonZero), true)
}

test('LOGIN7 with an unknown database matches SQL Server token for token', { timeout: 60000 }, async t => {
  const { serverArgs } = tlsServer(t)
  const c = await start(t, { serverArgs, options })
  const config = c.config
  for (const name of ['nonexistent database', 'nonexistent quoted name', 'nonexistent unicode name']) {
    const { database } = reference(name)
    compare(await observeLogin(config, { database }), reference(name))
  }
  // Without an administrator file the supplied name is the login name.
  const quoted = reference('nonexistent database, login name with quote')
  compare(await observeLogin(config, { database: quoted.database, userName: quoted.userName }), quoted)
  // The owner's control: master logs in and SELECT 42 returns one row.
  const master = await observeLogin(config, { database: 'master', batch: 'select 42' })
  assert.deepEqual(master.rows, reference('master').rows)
  assert.equal(master.closedByServer, false)
})

test('LOGIN7 unknown-database failure names the canonical administrator', { timeout: 60000 }, async t => {
  const { directory, serverArgs } = tlsServer(t)
  const path = join(directory, 'admin.json')
  const password = 'development password'
  writeFileSync(path, execFileSync(process.execPath, ['scripts/create-admin.mjs', 'sa'], { input: password }), { mode: 0o600 })
  const c = await start(t, { serverArgs: [...serverArgs, '--admin-credentials', path],
    authentication: { type: 'default', options: { userName: 'sa', password } }, options })
  const expected = reference('nonexistent database, differently cased login name')
  compare(await observeLogin(c.config, { database: expected.database, userName: 'SA' }), expected)
})
