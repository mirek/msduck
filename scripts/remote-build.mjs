// Optional Linux build/test runner. Configuration is data, never sourced as shell.
import { spawn } from 'node:child_process'
import { existsSync, mkdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { resolve } from 'node:path'
import { createInterface } from 'node:readline'

// A changed file must arrive with a new receiver mtime for Cargo's fingerprint
// checks. Checksums avoid retransferring unchanged files when their mtimes differ.
export const sourceSyncOptions = ['-azc', '--no-times', '--delete', '--exclude=/.git',
  '--exclude=/target', '--exclude=/node_modules', '--exclude=/artifacts',
  '--exclude=/.msduck', '--exclude=.env', '--exclude=.env.*',
  '--exclude=*.duckdb', '--exclude=*.duckdb.wal']

async function main() {
const root = fileURLToPath(new URL('../', import.meta.url))
process.chdir(root)
if (existsSync('.env')) process.loadEnvFile('.env')
const actions = {
  build: 'cargo build --workspace --all-targets --locked',
  fast: 'cargo test -p msduck-core -p msduck-sql -p msduck-tds --locked',
  rust: 'cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked',
  test: 'npm test',
  audit: 'npm run audit:local',
  verify: 'cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked && npm test && npm run audit:local',
}
const action = process.argv[2] ?? 'build'
if (process.argv.length > 3 || !Object.hasOwn(actions, action)) {
  console.error('Usage: node scripts/remote-build.mjs [build|fast|rust|test|audit|verify]')
  process.exit(2)
}
const host = process.env.MSDUCK_BUILD_HOST
const directory = process.env.MSDUCK_BUILD_DIR
const jobs = process.env.MSDUCK_BUILD_JOBS ?? '16'
const toolchain = process.env.MSDUCK_BUILD_TOOLCHAIN ?? ''
if (!host || !/^[a-zA-Z0-9][a-zA-Z0-9._@-]*$/.test(host)
    || !directory || !/^\/[a-zA-Z0-9_./-]+$/.test(directory)
    || directory.split('/').includes('..') || directory === '/'
    || !/^[1-9][0-9]*$/.test(jobs)
    || (toolchain && !/^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(toolchain))) {
  console.error('Set MSDUCK_BUILD_HOST, an absolute MSDUCK_BUILD_DIR, and positive MSDUCK_BUILD_JOBS in .env (see .env.example).')
  process.exit(2)
}
const quote = value => `'${value.replaceAll("'", "'\\''")}'`
const sshOptions = ['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=15']
const marker = '__MSDUCK_REMOTE_READY__'
// The remote lock spans both rsync and execution, so concurrent invocations
// cannot replace source files while a build/test run is reading them.
const script = `set -eu
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_BUILD_JOBS=${quote(jobs)}
${toolchain ? `export RUSTUP_TOOLCHAIN=${quote(toolchain)}` : ""}
root=${quote(directory)}
mkdir -p "$root"
exec 9>"$root/runner.lock"
flock -n 9 || { echo 'Another remote msduck run owns this workspace.' >&2; exit 1; }
if [ ! -f "$root/.msduck-build-workspace" ]; then
  if [ -n "$(find "$root" -mindepth 1 -maxdepth 1 ! -name runner.lock -print -quit)" ]; then
    echo 'Refusing to synchronize into an existing unowned directory.' >&2
    exit 1
  fi
  touch "$root/.msduck-build-workspace"
fi
mkdir -p "$root/source"
printf '%s\\n' ${quote(marker)}
IFS= read -r proceed
cd "$root/source"
if [ ! -f "$root/.msduck-content-sync-v1" ]; then
  if [ -d target ]; then
    echo 'Resetting previous timestamp-based Cargo cache once.'
    cargo clean
  fi
  touch "$root/.msduck-content-sync-v1"
fi
if [ ${quote(action)} = test ] || [ ${quote(action)} = audit ] || [ ${quote(action)} = verify ]; then
  digest=$(sha256sum package-lock.json)
  if [ ! -d node_modules ] || [ ! -f "$root/npm-lock.sha256" ] || [ "$(cat "$root/npm-lock.sha256")" != "$digest" ]; then
    npm ci --no-audit --no-fund
    printf '%s\\n' "$digest" > "$root/npm-lock.sha256"
  fi
fi
${actions[action]}
`
const remote = spawn('ssh', [...sshOptions, host, `bash -c ${quote(script)}`], { stdio: ['pipe', 'pipe', 'inherit'] })
const ended = new Promise(resolve => {
  remote.once('error', error => { console.error(error.message); resolve(1) })
  remote.once('exit', code => resolve(code ?? 1))
})
let readyResolve
const ready = new Promise(resolve => { readyResolve = resolve })
const lines = createInterface({ input: remote.stdout })
lines.on('line', line => { if (line === marker) readyResolve(true); else console.log(line) })
let activeSync
const run = (command, args) => new Promise((resolve, reject) => {
  activeSync = spawn(command, args, { stdio: 'inherit' })
  activeSync.once('error', reject)
  activeSync.once('exit', code => code === 0 ? resolve() : reject(new Error(`${command} exited ${code}`)))
})
for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => {
  activeSync?.kill(signal)
  remote.kill(signal)
  process.exitCode = signal === 'SIGINT' ? 130 : 143
})
try {
  if (!await Promise.race([ready, ended.then(() => false)])) throw new Error('Remote workspace initialization failed')
  console.log(`Syncing working tree to ${host}:${directory}/source (${jobs} build jobs)`)
  await run('rsync', [...sourceSyncOptions,
    '-e', 'ssh -o BatchMode=yes -o ConnectTimeout=15', './', `${host}:${directory}/source/`])
  remote.stdin.end('\n')
  const status = await ended
  if (['audit', 'verify'].includes(action) && status === 0) {
    const destination = `artifacts/remote/${host}/compatibility`
    mkdirSync(destination, { recursive: true })
    await run('rsync', ['-az', '-e', 'ssh -o BatchMode=yes -o ConnectTimeout=15', `${host}:${directory}/source/artifacts/compatibility/`, `${destination}/`])
    console.log(`Remote audit saved separately in ${destination}`)
  }
  process.exitCode = process.exitCode || status
} catch (error) {
  console.error(error.message)
  remote.stdin.destroy()
  remote.kill()
  await ended
  process.exitCode = 1
}
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main()
