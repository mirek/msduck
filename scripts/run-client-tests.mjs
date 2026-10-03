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
// The portable serial fallback forwards the same signals as the POSIX adapter.
// Keep lifecycle handling in one place, and remove handlers after terminal exit.
export async function runCommand(selected, {signals = process, stdio = 'inherit'} = {}) {
  const child = spawn(process.execPath, selected.args, {stdio, env: selected.env})
  const handlers = new Map(['SIGINT', 'SIGTERM'].map(signal => [signal, () => child.kill(signal)]))
  for (const [signal, handler] of handlers) signals.once(signal, handler)
  try {
    return await new Promise(resolve => {
      child.once('error', error => { console.error(error.message); resolve(1) })
      child.once('close', code => resolve(code ?? 1))
    })
  } finally {
    for (const [signal, handler] of handlers) signals.off(signal, handler)
  }
}
async function main() {
  process.exitCode = await runCommand(command(process.env, process.argv.slice(2)))
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main().catch(error => {console.error(error.message); process.exitCode = 1})
