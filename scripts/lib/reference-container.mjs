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

// Startup failures that a fresh container usually fixes under parallel load:
// SQL Server exiting before login readiness, or the container vanishing.
class TransientStartup extends Error {}

export async function withReferenceContainer(work, operations = {}) {
  const run = operations.docker ?? docker
  const sleep = operations.delay ?? delay
  const attempts = operations.startupAttempts ?? 3
  const failures = []
  for (let attempt = 1; ; ++attempt) {
    const name = `msduck-reference-${randomUUID()}`
    let started = false
    let ready
    try {
      ready = await start(name, run, sleep, operations, () => { started = true })
    } catch (error) {
      await remove(run, sleep, name, started)
      if (!(error instanceof TransientStartup) || attempt >= attempts) {
        if (failures.length) error.message += ` (after ${attempt} startup attempts: ${[...failures, error.message].join('; ')})`
        throw error
      }
      failures.push(error.message)
      continue
    }
    // The workload runs once; failures after readiness are never retried.
    try { return await work(ready.config, { name, image: ready.image, startupAttempts: attempt }) }
    finally { await remove(run, sleep, name, true) }
  }
}

async function start(name, run, sleep, operations, onStarted) {
  const login = operations.connect ?? connect
  const now = operations.now ?? Date.now
  const timeout = operations.timeout ?? 180000
  const signal = operations.signal
  const password = `Msduck!9${randomBytes(24).toString('hex')}`
  const image = operations.image ?? process.env.MSSQL_REFERENCE_IMAGE ?? referenceImage
  // Labels let a worker identify orphans it left behind after a crash
  // without guessing from timing; parallel workers share one Docker daemon.
  await run(['run', '--detach', '--rm', '--name', name,
    '--label', 'msduck.reference=1', '--label', `msduck.owner=${process.cwd()}:${process.pid}`,
    '--platform', 'linux/amd64',
    '--env', 'ACCEPT_EULA=Y', '--env', 'MSSQL_PID=Developer', '--env', 'TZ=UTC',
    '--env', 'MSSQL_SA_PASSWORD', '--publish', '127.0.0.1::1433', image], { MSSQL_SA_PASSWORD: password })
  onStarted()
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
    let running
    try { running = await run(['inspect', '--format', '{{.State.Running}}', name]) }
    catch { throw new TransientStartup('SQL Server reference container disappeared before login readiness') }
    if (running !== 'true') throw new TransientStartup('SQL Server reference container exited before login readiness')
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
  return { config, image }
}

// The name is fresh and owned by this invocation, including a partially
// successful docker run. Never stop or remove a caller-supplied container.
async function remove(run, sleep, name, started) {
  try { await run(['rm', '--force', name]) }
  catch (error) {
    if (started && !await removed(run, sleep, name)) throw new Error(`Could not remove owned reference container ${name}`, { cause: error })
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
