import { spawn, spawnSync } from 'node:child_process'
import { withReferenceContainer } from './lib/reference-container.mjs'

const forwarded = process.argv.slice(2)
const listing = forwarded.includes('--list-cases')
const planning = forwarded.includes('--plan')
const preflight = spawnSync(process.execPath, ['scripts/compatibility.mjs',
  ...(listing ? [] : ['--compare']), ...forwarded, ...(listing || planning ? [] : ['--plan'])],
{ encoding: 'utf8' })
if (preflight.error) throw preflight.error
if (preflight.status !== 0) {
  process.stderr.write(preflight.stderr)
  process.exit(preflight.status ?? 1)
}
if (listing || planning) {
  process.stdout.write(preflight.stdout)
  process.exit(0)
}

const controller = new AbortController()
const stop = () => controller.abort(new Error('Reference comparison interrupted'))
process.once('SIGINT', stop)
process.once('SIGTERM', stop)
await withReferenceContainer(async (config, { image }) => {
  console.log(`Reference ready: ${image}`)
  const exitCode = await new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ['scripts/compatibility.mjs', '--compare', ...forwarded], {
      stdio: 'inherit',
      detached: process.platform !== 'win32',
      env: { ...process.env, MSSQL_REFERENCE_HOST: config.server,
        MSSQL_REFERENCE_PORT: String(config.options.port), MSSQL_REFERENCE_USER: 'sa',
        MSSQL_REFERENCE_PASSWORD: config.authentication.options.password,
        MSSQL_REFERENCE_ENCRYPT: 'true', MSSQL_REFERENCE_TRUST_CERTIFICATE: 'true' }
    })
    const stopChild = () => {
      if (!child.pid || child.exitCode !== null || child.signalCode !== null) return
      if (process.platform === 'win32') child.kill('SIGTERM')
      else {
        try { process.kill(-child.pid, 'SIGTERM') }
        catch (error) { if (error.code !== 'ESRCH') throw error }
      }
    }
    controller.signal.addEventListener('abort', stopChild, { once: true })
    child.once('close', () => controller.signal.removeEventListener('abort', stopChild))
    child.once('error', reject)
    child.once('exit', (code, signal) => signal ? reject(new Error(`Comparison terminated by ${signal}`)) : resolve(code))
  })
  process.exitCode = exitCode
}, { signal: controller.signal })
process.removeListener('SIGINT', stop)
process.removeListener('SIGTERM', stop)
