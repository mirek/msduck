import test from 'node:test'
import assert from 'node:assert/strict'
import {spawnSync} from 'node:child_process'
import {mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, utimesSync, writeFileSync, existsSync, lstatSync, symlinkSync, unlinkSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
import {sourceSyncOptions, receiverCacheMigration, npmCacheSetup, remoteTempDirectory, remotePathGuard, remoteTempSetup} from '../scripts/remote-build.mjs'

function run(file, args, cwd, env = process.env) {
  const result = spawnSync(file, args, {cwd, env, encoding: 'utf8', timeout: 120_000})
  assert.equal(result.status, 0, `${file} ${args.join(' ')}\n${result.stderr}`)
  return result
}

test('older-mtime source edits rebuild while unchanged source reuses Cargo targets', {timeout: 120_000}, () => {
  const temporary = mkdtempSync(join(tmpdir(), 'msduck-remote-sync-'))
  try {
    const source = join(temporary, 'source')
    const destination = join(temporary, 'destination')
    mkdirSync(join(source, 'src'), {recursive: true})
    mkdirSync(destination)
    writeFileSync(join(source, 'Cargo.toml'),
      '[package]\nname = "remote_fingerprint_probe"\nversion = "0.1.0"\nedition = "2024"\n\n[workspace]\n')
    const program = join(source, 'src/main.rs')
    writeFileSync(program, 'fn main() { println!("OLD"); }\n')
    writeFileSync(join(source, '.env'), 'SECRET=excluded\n')
    writeFileSync(join(source, '.git'), 'gitdir: /private/mac/worktree\n')
    mkdirSync(join(source, '.msduck', 'claims'), {recursive: true})
    writeFileSync(join(source, '.msduck', 'claims', 'private.json'), '{}\n')
    run('cargo', ['generate-lockfile', '--offline'], source)
    const sync = () => run('rsync', [...sourceSyncOptions, `${source}/`, `${destination}/`], temporary)
    const target = join(destination, 'target')
    const cargo = () => run('cargo', ['build', '--locked', '--offline'], destination,
      {...process.env, CARGO_TARGET_DIR: target, CARGO_TERM_COLOR: 'never'})
    const executable = join(target, 'debug', 'remote_fingerprint_probe')
    sync()
    assert(!existsSync(join(destination, '.env')))
    assert(!existsSync(join(destination, '.git')))
    assert(!existsSync(join(destination, '.msduck')))
    assert.match(cargo().stderr, /Compiling remote_fingerprint_probe/)
    assert.equal(run(executable, [], destination).stdout.trim(), 'OLD')

    // Preserve an old source mtime and the byte length; only content differs.
    writeFileSync(program, 'fn main() { println!("NEW"); }\n')
    utimesSync(program, new Date('2000-01-01T00:00:00Z'), new Date('2000-01-01T00:00:00Z'))
    const oldMtime = statSync(program).mtimeMs
    sync()
    assert.equal(readFileSync(join(destination, 'src/main.rs'), 'utf8'), readFileSync(program, 'utf8'))
    assert(statSync(join(destination, 'src/main.rs')).mtimeMs > oldMtime)
    assert.match(cargo().stderr, /Compiling remote_fingerprint_probe/)
    assert.equal(run(executable, [], destination).stdout.trim(), 'NEW')

    const executableMtime = statSync(executable).mtimeMs
    sync()
    assert.doesNotMatch(cargo().stderr, /Compiling remote_fingerprint_probe/)
    assert.equal(statSync(executable).mtimeMs, executableMtime)
    assert.equal(run(executable, [], destination).stdout.trim(), 'NEW')
  } finally {
    rmSync(temporary, {recursive: true, force: true})
  }
})

test('root cache and private symlinks never replace receiver directories', () => {
  const temporary = mkdtempSync(join(tmpdir(), 'msduck-remote-links-'))
  try {
    const source = join(temporary, 'source')
    const destination = join(temporary, 'destination')
    mkdirSync(source)
    mkdirSync(destination)
    const excluded = ['target', 'node_modules', 'artifacts', '.git', '.msduck']
    for (const name of excluded) {
      const localCache = join(temporary, `local-${name}`)
      mkdirSync(localCache)
      writeFileSync(join(localCache, 'private'), `local ${name}\n`)
      symlinkSync(localCache, join(source, name))
      mkdirSync(join(destination, name))
      writeFileSync(join(destination, name, 'receiver-cache'), `receiver ${name}\n`)
    }
    writeFileSync(join(source, '.env'), 'SECRET=excluded\n')
    writeFileSync(join(source, 'source.txt'), 'first\n')
    writeFileSync(join(destination, 'removed.txt'), 'stale\n')
    const sync = () => run('rsync', [...sourceSyncOptions, `${source}/`, `${destination}/`], temporary)

    sync()
    assert.equal(readFileSync(join(destination, 'source.txt'), 'utf8'), 'first\n')
    assert(!existsSync(join(destination, 'removed.txt')))
    assert(!existsSync(join(destination, '.env')))
    for (const name of excluded) {
      assert(lstatSync(join(destination, name)).isDirectory(), `${name} receiver cache was replaced`)
      assert.equal(readFileSync(join(destination, name, 'receiver-cache'), 'utf8'), `receiver ${name}\n`)
      assert(!existsSync(join(destination, name, 'private')), `${name} local target was copied`)
    }

    writeFileSync(join(source, 'source.txt'), 'second\n')
    sync()
    assert.equal(readFileSync(join(destination, 'source.txt'), 'utf8'), 'second\n')
    for (const name of excluded) {
      assert(lstatSync(join(destination, name)).isDirectory(), `${name} receiver cache was replaced on repeat sync`)
      assert(existsSync(join(destination, name, 'receiver-cache')))
    }
  } finally {
    rmSync(temporary, {recursive: true, force: true})
  }
})

// Run the same bounded shell fragment used under the remote lock, against an
// isolated owned receiver. No SSH, network, native Cargo or shared caches.
function migrate(root, fragment = receiverCacheMigration) {
  return spawnSync('bash', ['-c', `set -eu\n${fragment}`], {
    env: {...process.env, root}, encoding: 'utf8', timeout: 10_000,
  })
}

function fixture(prefix) {
  const root = mkdtempSync(join(tmpdir(), prefix))
  mkdirSync(join(root, 'source'))
  writeFileSync(join(root, '.msduck-build-workspace'), '')
  return root
}

function npmSetup(root, fail = false) {
  const bin = join(root, 'bin')
  mkdirSync(bin, {recursive: true})
  writeFileSync(join(bin, 'sha256sum'), '#!/bin/sh\nprintf "matching digest\\n"\n', {mode: 0o755})
  writeFileSync(join(bin, 'npm'), `#!/bin/sh
printf 'install\\n' >> "$root/installs"
${fail ? 'exit 7' : 'mkdir -p node_modules; printf installed > node_modules/installed'}
`, {mode: 0o755})
  return spawnSync('bash', ['-c', `set -eu\n${npmCacheSetup}`], {
    cwd: join(root, 'source'), env: {...process.env, root, PATH: `${bin}:${process.env.PATH}`},
    encoding: 'utf8', timeout: 10_000,
  })
}

test('stale receiver cache links are unlinked without touching external destinations', () => {
  const temporary = fixture('msduck-receiver-links-')
  try {
    const sender = join(temporary, 'sender')
    mkdirSync(sender)
    writeFileSync(join(sender, 'source.txt'), 'first')
    const cacheNames = ['target', 'node_modules', 'artifacts']
    for (const name of cacheNames) {
      const external = join(temporary, `external-${name}`)
      mkdirSync(external)
      writeFileSync(join(external, 'keep'), name)
      symlinkSync(external, join(temporary, 'source', name))
    }
    writeFileSync(join(temporary, 'npm-lock.sha256'), 'matching digest\n')
    const sync = () => run('rsync', [...sourceSyncOptions, `${sender}/`, `${join(temporary, 'source')}/`], temporary)
    sync()
    // Exclusions alone (the predecessor) preserve stale receiver links.
    for (const name of cacheNames) assert(lstatSync(join(temporary, 'source', name)).isSymbolicLink())
    assert.equal(migrate(temporary).status, 0)
    assert(!existsSync(join(temporary, 'npm-lock.sha256')))
    for (const name of cacheNames) {
      assert(!existsSync(join(temporary, 'source', name)))
      assert.equal(readFileSync(join(temporary, `external-${name}`, 'keep'), 'utf8'), name)
    }
    // A changed source still syncs after migration; npm cannot reuse the old stamp.
    writeFileSync(join(sender, 'source.txt'), 'second')
    sync()
    assert.equal(readFileSync(join(temporary, 'source', 'source.txt'), 'utf8'), 'second')
    assert.equal(npmSetup(temporary).status, 0)
    assert.equal(readFileSync(join(temporary, 'installs'), 'utf8'), 'install\n')
    assert(lstatSync(join(temporary, 'source', 'node_modules')).isDirectory())
    assert.equal(readFileSync(join(temporary, 'npm-lock.sha256'), 'utf8'), 'matching digest\n')
    assert.equal(migrate(temporary).status, 0)
    sync()
    assert.equal(npmSetup(temporary).status, 0)
    assert.equal(readFileSync(join(temporary, 'installs'), 'utf8'), 'install\n')
    assert.equal(readFileSync(join(temporary, 'source', 'node_modules', 'installed'), 'utf8'), 'installed')
  } finally { rmSync(temporary, {recursive: true, force: true}) }
})

test('dangling links migrate while ordinary cache directories and stamps survive', () => {
  const temporary = fixture('msduck-receiver-dangling-')
  try {
    for (const name of ['target', 'node_modules', 'artifacts']) {
      mkdirSync(join(temporary, 'source', name))
      writeFileSync(join(temporary, 'source', name, 'keep'), name)
    }
    writeFileSync(join(temporary, 'npm-lock.sha256'), 'matching digest\n')
    assert.equal(migrate(temporary).status, 0)
    assert.equal(readFileSync(join(temporary, 'npm-lock.sha256'), 'utf8'), 'matching digest\n')
    for (const name of ['target', 'node_modules', 'artifacts']) {
      assert.equal(readFileSync(join(temporary, 'source', name, 'keep'), 'utf8'), name)
      rmSync(join(temporary, 'source', name), {recursive: true})
      symlinkSync(join(temporary, 'missing', name), join(temporary, 'source', name))
    }
    assert.equal(migrate(temporary).status, 0)
    for (const name of ['target', 'node_modules', 'artifacts']) assert.throws(() => lstatSync(join(temporary, 'source', name)), {code: 'ENOENT'})
    assert(!existsSync(join(temporary, 'npm-lock.sha256')))
  } finally { rmSync(temporary, {recursive: true, force: true}) }
})

test('migration refuses unowned receivers and source links before any removal', () => {
  const temporary = fixture('msduck-receiver-guard-')
  try {
    const external = join(temporary, 'external')
    mkdirSync(external)
    writeFileSync(join(external, 'keep'), 'keep')
    symlinkSync(external, join(temporary, 'source', 'target'))
    rmSync(join(temporary, '.msduck-build-workspace'))
    assert.notEqual(migrate(temporary).status, 0)
    assert(lstatSync(join(temporary, 'source', 'target')).isSymbolicLink())
    writeFileSync(join(temporary, '.msduck-build-workspace'), '')
    rmSync(join(temporary, 'source'), {recursive: true})
    symlinkSync(external, join(temporary, 'source'))
    symlinkSync(join(temporary, 'missing'), join(external, 'target'))
    assert.notEqual(migrate(temporary).status, 0)
    assert(lstatSync(join(external, 'target')).isSymbolicLink())
    assert.equal(readFileSync(join(external, 'keep'), 'utf8'), 'keep')
  } finally { rmSync(temporary, {recursive: true, force: true}) }
})

test('migration unlinks a stamp link itself and failed npm does not record success', () => {
  const temporary = fixture('msduck-receiver-npm-')
  try {
    const external = join(temporary, 'external-stamp')
    writeFileSync(external, 'external evidence')
    symlinkSync(external, join(temporary, 'npm-lock.sha256'))
    symlinkSync(join(temporary, 'missing'), join(temporary, 'source', 'node_modules'))
    assert.equal(migrate(temporary).status, 0)
    assert.equal(readFileSync(external, 'utf8'), 'external evidence')
    assert(!existsSync(join(temporary, 'npm-lock.sha256')))
    assert.equal(npmSetup(temporary, true).status, 7)
    assert(!existsSync(join(temporary, 'npm-lock.sha256')))
    assert.equal(readFileSync(external, 'utf8'), 'external evidence')
  } finally { rmSync(temporary, {recursive: true, force: true}) }
})

test('remote client concurrency rejects shell text before SSH and exports a validated quoted value', () => {
  const temporary = fixture('msduck-client-jobs-')
  try {
    const bin = join(temporary,'bin')
    mkdirSync(bin)
    const recorded = join(temporary,'ssh-arguments')
    writeFileSync(join(bin,'ssh'), '#!/bin/sh\nprintf "%s\\n" "$@" > "$MSDUCK_TEST_SSH_RECORD"\nexit 1\n', {mode:0o755})
    const env = {...process.env,PATH:`${bin}:${process.env.PATH}`,MSDUCK_TEST_SSH_RECORD:recorded,
      MSDUCK_BUILD_HOST:'linux.local',MSDUCK_BUILD_DIR:'/owned/cache',MSDUCK_CLIENT_JOBS:'4'}
    const result = spawnSync(process.execPath,['scripts/remote-build.mjs','test'],{env,encoding:'utf8',timeout:10000})
    assert.notEqual(result.status,0)
    assert.match(readFileSync(recorded,'utf8'),/export MSDUCK_CLIENT_JOBS=/)
    assert.match(readFileSync(recorded,'utf8'),/4/)
    rmSync(recorded)
    for (const value of ['0','17','4;touch stolen','$(id)','01','']) {
      const bad = spawnSync(process.execPath,['scripts/remote-build.mjs','test'],{env:{...env,MSDUCK_CLIENT_JOBS:value},encoding:'utf8',timeout:10000})
      assert.notEqual(bad.status,0)
      assert.match(bad.stderr,/integer from 1 to 16/)
      assert(!existsSync(recorded))
    }
  } finally {rmSync(temporary,{recursive:true,force:true})}
})

function temporarySetup(root, temporary, after = '') {
  return spawnSync('bash', ['-c', `set -eu\n${remotePathGuard}\n${remoteTempSetup}\n${after}`], {
    env: {...process.env, root, temporary, TMPDIR: '/deliberately-missing-shared-tmp'},
    encoding: 'utf8', timeout: 10_000,
  })
}

test('owned remote TMPDIR overrides shared temporary storage and retains files', () => {
  const root = fixture('msduck-owned-temp-')
  try {
    for (const temporary of [remoteTempDirectory(root), remoteTempDirectory(root, `${root}/tmp-link-961`)]) {
      const result = temporarySetup(root, temporary,
        'file=$(mktemp "$TMPDIR/probe.XXXXXX"); printf retained > "$file"; printf "%s\\n" "$TMPDIR" "$file"')
      assert.equal(result.status, 0, result.stderr)
      const [effective, file] = result.stdout.trim().split('\n')
      assert.equal(effective, temporary)
      assert.equal(readFileSync(file, 'utf8'), 'retained')
      assert.equal(statSync(temporary).mode & 0o777, 0o700)
      assert.equal(temporarySetup(root, temporary).status, 0)
      assert.equal(readFileSync(file, 'utf8'), 'retained')
    }
  } finally { rmSync(root, {recursive: true, force: true}) }
})

test('temporary setup refuses unowned directories, markers and symlink paths', () => {
  const root = fixture('msduck-temp-guards-')
  try {
    const temporary = join(root, 'tmp')
    rmSync(join(root, '.msduck-build-workspace'))
    assert.notEqual(temporarySetup(root, temporary).status, 0)
    assert(!existsSync(temporary))
    writeFileSync(join(root, '.msduck-build-workspace'), '')
    mkdirSync(temporary)
    writeFileSync(join(temporary, 'keep'), 'unowned')
    assert.notEqual(temporarySetup(root, temporary).status, 0)
    assert.equal(readFileSync(join(temporary, 'keep'), 'utf8'), 'unowned')
    rmSync(temporary, {recursive: true})
    const outside = join(root, 'outside')
    mkdirSync(outside)
    writeFileSync(join(outside, 'keep'), 'external')
    symlinkSync(outside, temporary)
    assert.notEqual(temporarySetup(root, temporary).status, 0)
    assert.equal(readFileSync(join(outside, 'keep'), 'utf8'), 'external')
    unlinkSync(temporary)
    mkdirSync(temporary)
    symlinkSync(join(outside, 'keep'), join(temporary, '.msduck-build-temp'))
    assert.notEqual(temporarySetup(root, temporary).status, 0)
    const alias = join(root, 'alias')
    symlinkSync(root, alias)
    assert.notEqual(temporarySetup(`${alias}/child`, `${alias}/child/tmp`).status, 0)
    assert(!existsSync(join(root, 'child')))
    for (const value of [outside, `${root}/tmp-../../outside`, `${root}/tmp-a/child`]) {
      assert.notEqual(temporarySetup(root, value).status, 0)
    }
  } finally { rmSync(root, {recursive: true, force: true}) }
})

test('temporary path configuration rejects escape and shell text before SSH', () => {
  const root = fixture('msduck-temp-config-')
  try {
    const bin = join(root, 'bin')
    mkdirSync(bin)
    const recorded = join(root, 'ssh-arguments')
    writeFileSync(join(bin, 'ssh'), '#!/bin/sh\nprintf "%s\\n" "$@" > "$MSDUCK_TEST_SSH_RECORD"\nexit 1\n', {mode: 0o755})
    const env = {...process.env, PATH: `${bin}:${process.env.PATH}`, MSDUCK_TEST_SSH_RECORD: recorded,
      MSDUCK_BUILD_HOST: 'linux.local', MSDUCK_BUILD_DIR: root, MSDUCK_CLIENT_JOBS: '4'}
    for (const value of ['/tmp', root, `${root}/source`, `${root}/tmp-../other`, `${root}/tmp-foo/child`,
      `${root}/tmp-$(touch stolen)`, `${root}/tmp-x;touch-stolen`, `${root}/tmp-`, '']) {
      assert.throws(() => remoteTempDirectory(root, value), /MSDUCK_BUILD_TMPDIR/)
      const bad = spawnSync(process.execPath, ['scripts/remote-build.mjs', 'fast'], {
        env: {...env, MSDUCK_BUILD_TMPDIR: value}, encoding: 'utf8', timeout: 10000,
      })
      assert.notEqual(bad.status, 0)
      assert.match(bad.stderr, /MSDUCK_BUILD_TMPDIR/)
      assert(!existsSync(recorded))
    }
    for (const directory of ['/', '///', 'relative', '/owned/../outside']) {
      assert.throws(() => remoteTempDirectory(directory), /MSDUCK_BUILD_DIR/)
    }
    assert.equal(remoteTempDirectory(`${root}/`), `${root}/tmp`)
    const good = spawnSync(process.execPath, ['scripts/remote-build.mjs', 'fast'], {
      env: {...env, MSDUCK_BUILD_TMPDIR: `${root}/tmp-proof`}, encoding: 'utf8', timeout: 10000,
    })
    assert.notEqual(good.status, 0) // fixture SSH intentionally stops before synchronization
    const script = readFileSync(recorded, 'utf8')
    assert(script.indexOf('flock -n 9') < script.indexOf('export TMPDIR='))
    assert(script.indexOf('export TMPDIR=') < script.indexOf('__MSDUCK_REMOTE_READY__'))
    assert(script.indexOf('export TMPDIR=') < script.indexOf('cargo test'))
  } finally { rmSync(root, {recursive: true, force: true}) }
})

test('emitted remote shell owns temporary files only after acquiring the workspace lock', () => {
  const root = fixture('msduck-temp-shell-')
  try {
    const bin = join(root, 'bin')
    mkdirSync(bin)
    const recorded = join(root, 'ssh-arguments')
    writeFileSync(join(bin, 'ssh'), '#!/bin/sh\nprintf "%s\\n" "$@" > "$MSDUCK_TEST_SSH_RECORD"\nexit 1\n', {mode: 0o755})
    writeFileSync(join(bin, 'flock'), '#!/bin/sh\nprintf lock > "$MSDUCK_TEST_LOCK_TRACE"\nexit "$MSDUCK_TEST_LOCK_STATUS"\n', {mode: 0o755})
    writeFileSync(join(bin, 'sha256sum'), '#!/bin/sh\nprintf "digest\\n"\n', {mode: 0o755})
    writeFileSync(join(bin, 'npm'), `#!/bin/sh
if [ "$1" = ci ]; then mkdir -p node_modules; exit 0; fi
[ -f "$MSDUCK_TEST_LOCK_TRACE" ] || exit 90
file=$(mktemp "$TMPDIR/executed.XXXXXX")
printf actual > "$file"
printf '%s\\n' "$TMPDIR" "$file" > "$MSDUCK_TEST_TEMP_RESULT"
`, {mode: 0o755})
    const env = {...process.env, PATH: `${bin}:${process.env.PATH}`, MSDUCK_TEST_SSH_RECORD: recorded,
      MSDUCK_BUILD_HOST: 'linux.local', MSDUCK_BUILD_DIR: root, MSDUCK_CLIENT_JOBS: '4',
      MSDUCK_TEST_LOCK_TRACE: join(root, 'lock-trace'), MSDUCK_TEST_LOCK_STATUS: '1',
      MSDUCK_TEST_TEMP_RESULT: join(root, 'temp-result'), TMPDIR: '/nonexistent-shared-tmp'}
    delete env.MSDUCK_BUILD_TMPDIR
    const capture = spawnSync(process.execPath, ['scripts/remote-build.mjs', 'test'], {env, encoding: 'utf8', timeout: 10000})
    assert.notEqual(capture.status, 0)
    const argumentsText = readFileSync(recorded, 'utf8')
    const emitted = argumentsText.slice(argumentsText.indexOf('bash -c ')).trim()
    const execute = status => spawnSync('bash', ['-c', emitted], {
      env: {...env, MSDUCK_TEST_LOCK_STATUS: status}, input: '\n', encoding: 'utf8', timeout: 10000,
    })
    assert.notEqual(execute('1').status, 0)
    assert(!existsSync(join(root, 'tmp')))
    assert(!existsSync(join(root, 'temp-result')))
    const result = execute('0')
    assert.equal(result.status, 0, result.stderr)
    const [effective, file] = readFileSync(join(root, 'temp-result'), 'utf8').trim().split('\n')
    assert.equal(effective, join(root, 'tmp'))
    assert.equal(readFileSync(file, 'utf8'), 'actual')
    assert.match(result.stdout, /__MSDUCK_REMOTE_READY__/)
    const external = join(root, 'external-lock')
    writeFileSync(external, 'keep')
    unlinkSync(join(root, 'runner.lock'))
    symlinkSync(external, join(root, 'runner.lock'))
    assert.notEqual(execute('0').status, 0)
    assert.equal(readFileSync(external, 'utf8'), 'keep')
  } finally { rmSync(root, {recursive: true, force: true}) }
})
