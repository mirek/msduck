import test from 'node:test'
import assert from 'node:assert/strict'
import {spawnSync} from 'node:child_process'
import {mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, utimesSync, writeFileSync, existsSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
import {sourceSyncOptions} from '../scripts/remote-build.mjs'

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
      '[package]\nname = "remote_fingerprint_probe"\nversion = "0.1.0"\nedition = "2024"\n')
    const program = join(source, 'src/main.rs')
    writeFileSync(program, 'fn main() { println!("OLD"); }\n')
    writeFileSync(join(source, '.env'), 'SECRET=excluded\n')
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
