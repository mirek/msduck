import {spawn} from 'node:child_process'
import {fileURLToPath} from 'node:url'
import {resolve} from 'node:path'
import {clientJobs, npmFiles} from './lib/client-suite.mjs'
export function command(env = process.env, args = [], platform = process.platform) {
  if (args.length) throw Error('run-client-tests.mjs accepts configuration through MSDUCK_CLIENT_JOBS only')
  const jobs = clientJobs(env.MSDUCK_CLIENT_JOBS)
  const childEnvironment = {...env}
  delete childEnvironment.NODE_TEST_CONTEXT
  if (platform === 'win32') {
    if (jobs !== 1) throw Error('Parallel client sharding requires POSIX; use unset/1 MSDUCK_CLIENT_JOBS on Windows')
    return {args: ['--test', ...npmFiles], env: childEnvironment}
  }
  return {args: [fileURLToPath(new URL('./run-client-shards.mjs', import.meta.url)),
    '--suite', 'npm', '--jobs', String(jobs), ...(jobs === 1 ? ['--serial'] : [])], env: childEnvironment}
}
// POSIX groups include Node's test-file workers. Windows taskkill /T performs
// the corresponding tree traversal; killing only the coordinator leaks workers.
export function windowsTreeCommand(pid, systemRoot = process.env.SystemRoot) {
  if (!Number.isInteger(pid) || pid <= 0) throw Error('Invalid child PID')
  return {file: systemRoot ? `${systemRoot}\\System32\\taskkill.exe` : 'taskkill.exe', args: ['/PID', String(pid), '/T', '/F']}
}
export async function runCommand(selected, {signals = process, stdio = 'inherit'} = {}) {
  const windows = process.platform === 'win32'
  const child = spawn(process.execPath, selected.args, {stdio, env: selected.env, detached: !windows})
  let cancellation, escalation
  const killGroup = signal => {
    if (Number.isInteger(child.pid)) {
      try { process.kill(-child.pid, signal) } catch (error) { if (error.code !== 'ESRCH') throw error }
    }
  }
  const cancel = signal => {
    if (cancellation) return
    if (windows) {
      const command = windowsTreeCommand(child.pid)
      cancellation = new Promise(resolve => {
        const killer = spawn(command.file, command.args, {stdio: 'ignore'})
        killer.once('error', error => { console.error(error.message); resolve(1) })
        killer.once('close', code => resolve(code ?? 1))
      })
    } else {
      cancellation = Promise.resolve(0)
      killGroup(signal)
      // The POSIX shard coordinator needs two seconds to reap its own groups.
      escalation = setTimeout(() => killGroup('SIGKILL'), 5000)
    }
  }
  const handlers = new Map(['SIGINT', 'SIGTERM'].map(signal => [signal, () => cancel(signal)]))
  for (const [signal, handler] of handlers) signals.once(signal, handler)
  try {
    const code = await new Promise(resolve => {
      child.once('error', error => { console.error(error.message); resolve(1) })
      child.once('close', code => resolve(code ?? 1))
    })
    if (cancellation) {
      // A coordinator may exit while its descendants ignore the forwarded signal.
      if (!windows) killGroup('SIGKILL')
      await cancellation
      return 1
    }
    return code
  } finally {
    clearTimeout(escalation)
    for (const [signal, handler] of handlers) signals.off(signal, handler)
  }
}
async function main() {
  process.exitCode = await runCommand(command(process.env, process.argv.slice(2)))
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main().catch(error => {console.error(error.message); process.exitCode = 1})
