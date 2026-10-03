import {spawn} from 'node:child_process'
import {fileURLToPath} from 'node:url'
import {resolve} from 'node:path'
import {clientJobs} from './lib/client-suite.mjs'
export function command(env = process.env, args = []) {
  if (args.length) throw Error('run-client-tests.mjs accepts configuration through MSDUCK_CLIENT_JOBS only')
  const jobs = clientJobs(env.MSDUCK_CLIENT_JOBS)
  const childEnvironment = {...env}
  delete childEnvironment.NODE_TEST_CONTEXT
  return {args: [fileURLToPath(new URL('./run-client-shards.mjs', import.meta.url)),
    '--suite', 'npm', '--jobs', String(jobs), ...(jobs === 1 ? ['--serial'] : [])], env: childEnvironment}
}
async function main() {
  const selected = command(process.env, process.argv.slice(2))
  const child = spawn(process.execPath, selected.args, {stdio: 'inherit', env: selected.env})
  for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => child.kill(signal))
  process.exitCode = await new Promise(resolve => {
    child.once('error', error => { console.error(error.message); resolve(1) })
    child.once('close', code => resolve(code ?? 1))
  })
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main().catch(error => {console.error(error.message); process.exitCode = 1})
