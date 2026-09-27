// Smoke-test a built msduck image the way clients use the SQL Server image:
//   node scripts/docker-smoke.mjs IMAGE
// Credentials stay in child environments, never in command arguments.
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { randomBytes, randomUUID } from 'node:crypto'
import { setTimeout as delay } from 'node:timers/promises'
import { Connection, Request } from 'tedious'

const image = process.argv[2]
if (!image) throw new Error('Usage: node scripts/docker-smoke.mjs IMAGE')
const exec = promisify(execFile)
const docker = async (args, env = {}) =>
  (await exec('docker', args, { env: { ...process.env, ...env }, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()
const password = `Duck!9${randomBytes(16).toString('hex')}`
const run = `msduck-smoke-${randomUUID()}`
const volume = `${run}-data`
const containers = []

async function start(name, env, { port = 1433 } = {}) {
  containers.push(name)
  await docker(['run', '--detach', '--name', name, '--volume', `${volume}:/var/opt/mssql`,
    '--publish', `127.0.0.1::${port}`, ...Object.keys(env).flatMap(key => ['--env', key]), image], env)
  const deadline = Date.now() + 60000
  while (!(await logs(name)).includes('SQL Server is now ready for client connections')) {
    if (await docker(['inspect', '--format', '{{.State.Running}}', name]) !== 'true') {
      throw new Error(`container exited before readiness:\n${await logs(name)}`)
    }
    if (Date.now() > deadline) throw new Error(`container readiness timed out:\n${await logs(name)}`)
    await delay(250)
  }
  const binding = await docker(['port', name, `${port}/tcp`])
  return Number(/^127\.0\.0\.1:(\d+)$/m.exec(binding)[1])
}
const logs = async name => {
  const { stdout, stderr } = await exec('docker', ['logs', name])
  return stdout + stderr
}

function connect(port, secret, options = {}) {
  const connection = new Connection({
    server: 'localhost',
    authentication: { type: 'default', options: { userName: 'sa', password: secret } },
    options: { port, database: 'master', encrypt: true, trustServerCertificate: true,
      connectTimeout: 10000, requestTimeout: 10000, ...options }
  })
  connection.on('error', () => {})
  return new Promise((resolve, reject) =>
    connection.connect(error => error ? reject(error) : resolve(connection)))
}
function query(connection, sql) {
  return new Promise((resolve, reject) => {
    const rows = []
    const request = new Request(sql, error => error ? reject(error) : resolve(rows))
    request.on('row', cells => rows.push(cells.map(cell => cell.value)))
    connection.execSql(request)
  })
}
async function exitsWith(env, pattern) {
  const name = `${run}-rejected-${containers.length}`
  containers.push(name)
  await docker(['run', '--name', name, ...Object.keys(env).flatMap(key => ['--env', key]), image], env)
    .then(() => assert.fail('container accepted invalid configuration'), () => {})
  const output = await logs(name)
  assert.match(output, pattern)
  assert.doesNotMatch(output, /listening on/)
}

try {
  await exitsWith({ ACCEPT_EULA: 'Y' }, /MSSQL_SA_PASSWORD/)
  await exitsWith({ ACCEPT_EULA: 'Y', MSSQL_SA_PASSWORD: 'duckduck' }, /password policy/)

  const first = `${run}-first`
  const port = await start(first, { ACCEPT_EULA: 'Y', MSSQL_PID: 'Developer', MSSQL_SA_PASSWORD: password })
  const output = await logs(first)
  assert.match(output, /accepted ACCEPT_EULA/)
  assert.match(output, /accepted MSSQL_PID/)
  assert.match(output, /generated self-signed TLS certificate/)
  assert.ok(!output.includes(password), 'logs never contain the password')
  const connection = await connect(port, password)
  await query(connection, 'CREATE TABLE dbo.ducks (id INT PRIMARY KEY, name NVARCHAR(50))')
  await query(connection, "INSERT INTO dbo.ducks VALUES (1, N'Mallard')")
  assert.deepEqual(await query(connection, 'SELECT id, name FROM dbo.ducks'), [[1, 'Mallard']])
  connection.close()
  await assert.rejects(connect(port, `${password}x`), error => /Login failed/.test(error.message))
  const certificate = await docker(['exec', first, 'cat', '/var/opt/mssql/secrets/msduck-cert.pem'])
  await docker(['rm', '--force', first])

  // The volume keeps data and certificate; SA_PASSWORD and MSSQL_TCP_PORT are honoured.
  const second = `${run}-second`
  const secondPort = await start(second, { ACCEPT_EULA: 'Y', SA_PASSWORD: password, MSSQL_TCP_PORT: '14330' }, { port: 14330 })
  assert.doesNotMatch(await logs(second), /generated self-signed/)
  assert.equal(await docker(['exec', second, 'cat', '/var/opt/mssql/secrets/msduck-cert.pem']), certificate)
  const reopened = await connect(secondPort, password)
  assert.deepEqual(await query(reopened, 'SELECT name FROM dbo.ducks'), [['Mallard']])
  reopened.close()
  await docker(['rm', '--force', second])

  // An interrupted start can leave only the key; the next start regenerates the pair.
  await docker(['run', '--rm', '--volume', `${volume}:/var/opt/mssql`, image, 'rm', '/var/opt/mssql/secrets/msduck-cert.pem'])
  const third = `${run}-third`
  const thirdPort = await start(third, { ACCEPT_EULA: 'Y', MSSQL_SA_PASSWORD: password })
  assert.match(await logs(third), /generated self-signed TLS certificate/)
  const recovered = await connect(thirdPort, password)
  assert.deepEqual(await query(recovered, 'SELECT name FROM dbo.ducks'), [['Mallard']])
  recovered.close()
  await docker(['rm', '--force', third])

  // Likewise when only the certificate survived.
  await docker(['run', '--rm', '--volume', `${volume}:/var/opt/mssql`, image, 'rm', '/var/opt/mssql/secrets/msduck-key.pem'])
  const fourth = `${run}-fourth`
  const fourthPort = await start(fourth, { ACCEPT_EULA: 'Y', MSSQL_SA_PASSWORD: password })
  assert.match(await logs(fourth), /generated self-signed TLS certificate/)
  ;(await connect(fourthPort, password)).close()
  console.log(`docker smoke test passed for ${image}`)
} finally {
  for (const name of containers) await docker(['rm', '--force', name]).catch(() => {})
  await docker(['volume', 'rm', '--force', volume]).catch(() => {})
}
