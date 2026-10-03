import test from 'node:test'
import assert from 'node:assert/strict'
import {spawnSync} from 'node:child_process'
import {mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, utimesSync, writeFileSync, existsSync, lstatSync, symlinkSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
import {sourceSyncOptions, receiverCacheMigration, npmCacheSetup} from '../scripts/remote-build.mjs'

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
