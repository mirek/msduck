// Container lifecycle adapted from mirek/mssqlite's differential harness.
// Keep credentials in child environments, never command arguments or artifacts.
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { randomBytes, randomUUID } from 'node:crypto'
import { setTimeout as delay } from 'node:timers/promises'
import { connect } from './reference.mjs'

export const referenceImage = 'mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a'
const exec = promisify(execFile)
const docker = async (args, env) => (await exec('docker', args, { env: { ...process.env, ...env }, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()

export async function withReferenceContainer(work, operations = {}) {
  const run = operations.docker ?? docker
  const login = operations.connect ?? connect
  const sleep = operations.delay ?? delay
  const now = operations.now ?? Date.now
  const timeout = operations.timeout ?? 180000
  const signal = operations.signal
  const name = `msduck-reference-${randomUUID()}`
  const password = `Msduck!9${randomBytes(24).toString('hex')}`
  const image = operations.image ?? process.env.MSSQL_REFERENCE_IMAGE ?? referenceImage
  let started = false
  try {
    // Labels let a worker identify orphans it left behind after a crash
    // without guessing from timing; parallel workers share one Docker daemon.
    await run(['run', '--detach', '--rm', '--name', name,
      '--label', 'msduck.reference=1', '--label', `msduck.owner=${process.cwd()}:${process.pid}`,
      '--platform', 'linux/amd64',
      '--env', 'ACCEPT_EULA=Y', '--env', 'MSSQL_PID=Developer', '--env', 'TZ=UTC',
      '--env', 'MSSQL_SA_PASSWORD', '--publish', '127.0.0.1::1433', image], { MSSQL_SA_PASSWORD: password })
    started = true
    signal?.throwIfAborted()
    const binding = await run(['port', name, '1433/tcp'])
    const match = /^127\.0\.0\.1:(\d+)$/.exec(binding)
    const port = match ? Number(match[1]) : NaN
    if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('SQL Server container did not expose a valid loopback port')
    const config = {
      server: 'localhost',
      authentication: { type: 'default', options: { userName: 'sa', password } },
      options: { port, database: 'master', encrypt: true, trustServerCertificate: true, connectTimeout: 2000, requestTimeout: 30000 }
    }
    const deadline = now() + timeout
    while (true) {
      signal?.throwIfAborted()
      if (await run(['inspect', '--format', '{{.State.Running}}', name]) !== 'true') throw new Error('SQL Server reference container exited before login readiness')
      try {
        const connection = await login(config)
        connection.close()
        break
      } catch {
        if (now() >= deadline) throw new Error('SQL Server reference login readiness timed out')
        await sleep(1000)
      }
    }
    signal?.throwIfAborted()
    return await work(config, { name, image })
  } finally {
    // The name is fresh and owned by this invocation, including a partially
    // successful docker run. Never stop or remove a caller-supplied container.
    try { await run(['rm', '--force', name]) }
    catch (error) {
      if (started && !await removed(run, sleep, name)) throw new Error(`Could not remove owned reference container ${name}`, { cause: error })
    }
  }
}

// `--rm` auto-removal can race a forced removal ("removal already in progress").
// Succeed only once the owned container no longer exists, within a bound.
async function removed(run, sleep, name, attempts = 30) {
  for (let attempt = 0; attempt < attempts; ++attempt) {
    try {
      if (await run(['ps', '--all', '--quiet', '--filter', `name=^/${name}$`]) === '') return true
    } catch { return false }
    await sleep(1000)
  }
  return false
}
