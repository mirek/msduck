import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile, mkdir, mkdtemp, writeFile, symlink, link, rm, access} from 'node:fs/promises'
import {spawnSync} from 'node:child_process'
import {resolve, join} from 'node:path'
import {isDeepStrictEqual} from 'node:util'
import {compareFixture, validateFixture} from '../scripts/capture-bulk-load-wire.mjs'
import {describeFirstDifference} from '../scripts/lib/reference.mjs'

const fixturePath = new URL('../reference/bulk-load-wire.json', import.meta.url)
const originalBytes = await readFile(fixturePath)
const original = JSON.parse(originalBytes)
const script = resolve('scripts/capture-bulk-load-wire.mjs')
const clone = () => structuredClone(original)
function refresh(value) {
  value.comparisons = value.runs.slice(1).map((run, index) => {
    const equal = isDeepStrictEqual(run, value.runs[0])
    return {run:index + 1, exactMatch:equal, firstDifference:equal ? null : describeFirstDifference(run, value.runs[0])}
  })
}
function replacePayload(value, run, observation, message, change) {
  const m = value.runs[run].observations[observation].messages[message]
  assert.equal(m.packets.length, 1)
  const payload = Buffer.from(m.payloadHex, 'hex')
  change(payload)
  m.payloadHex = payload.toString('hex')
  m.packets[0].payloadHex = m.payloadHex
  const raw = Buffer.from(m.packets[0].rawHex, 'hex')
  payload.copy(raw, 8)
  m.packets[0].rawHex = raw.toString('hex')
  refresh(value)
}

test('retained four-run raw fixture replays unchanged', async () => {
  validateFixture(original)
  assert.deepEqual(compareFixture(original, original), {exactMatch:true, permittedDifferences:[]})
  assert.deepEqual(await readFile(fixturePath), originalBytes)
})

test('only parsed zero-row ERROR hostname bytes may vary; originals remain intact', () => {
  const fresh = clone()
  for (let run = 0; run < 4; ++run) {
    replacePayload(fresh, run, 0, 3, payload => {
      // Independently locate the name preceding empty procedure, line and DONE.
      const start = payload.length - 13 - 4 - 1 - 24
      assert.equal(payload[start - 1], 12)
      Buffer.from('abcdef123456', 'utf16le').copy(payload, start)
    })
  }
  const before = JSON.stringify(fresh)
  const result = compareFixture(fresh, original)
  assert.equal(result.exactMatch, false)
  assert.equal(result.permittedDifferences.length, 12)
  assert(result.permittedDifferences.every(d => d.currentServer === 'abcdef123456' && d.currentHex.length === 48))
  assert.equal(JSON.stringify(fresh), before)
  assert.deepEqual(original, JSON.parse(originalBytes))
})

test('hostname differences cannot hide state, class, text, DONE or packet header drift', () => {
  for (const offset of [7, 8, 11, -13, -1]) {
    const value = clone()
    replacePayload(value, 0, 0, 3, payload => {
      Buffer.from('abcdef123456', 'utf16le').copy(payload, payload.length - 42)
      payload[offset < 0 ? payload.length + offset : offset] ^= 1
    })
    assert.throws(() => compareFixture(value, original), undefined, `changed byte ${offset}`)
  }
  const header = clone()
  const packet = header.runs[0].observations[0].messages[3].packets[0]
  const raw = Buffer.from(packet.rawHex, 'hex'); raw[5] ^= 1; packet.rawHex = raw.toString('hex'); refresh(header)
  assert.throws(() => compareFixture(header, original), /unexpected raw drift/)
})

test('metadata, input, callbacks, readback, requests and container identity remain exact', () => {
  const mutations = [
    v => {v.runs[0].observations[1].readback.sets[0].columns[1].flags ^= 1},
    v => {v.runs[0].observations[0].callback.errors[0].state = 3},
    v => {v.runs[0].observations[2].input[0].payloadHex = '00ff81'},
    v => {v.runs[0].observations[2].readback.sets[0].rows[0][1] = 'changed'},
    v => {v.containers[0].image = 'different-image'},
    v => replacePayload(v, 0, 1, 2, b => {b[b.length - 1] ^= 1}),
  ]
  for (const change of mutations) {const value = clone(); change(value); refresh(value); assert.throws(() => compareFixture(value, original))}
})

test('invalid ERROR name, layout and retained comparison records fail closed', () => {
  for (const name of ['abcdefghijkl', 'ABCDEF123456']) {
    const value = clone()
    replacePayload(value, 0, 0, 3, b => Buffer.from(name, 'utf16le').copy(b, b.length - 42))
    assert.throws(() => compareFixture(value, original), /Docker hostname/)
  }
  const summary = clone(); summary.comparisons[0].exactMatch = false
  assert.throws(() => compareFixture(summary, original))
  const extra = clone(); extra.runs[0].unexpected = true; refresh(extra)
  assert.throws(() => compareFixture(extra, original), /field drift/)
})

test('output aliases, existing files, symlinks and sidecar conflicts stop before Docker', async () => {
  await mkdir('.tmp', {recursive:true})
  const root = await mkdtemp(resolve('.tmp/bulk-capture-test-'))
  try {
    const bin = join(root, 'bin'); await mkdir(bin)
    const marker = join(root, 'docker-called')
    await writeFile(join(bin, 'docker'), `#!/bin/sh\ntouch '${marker}'\nexit 1\n`, {mode:0o755})
    const existing = join(root, 'existing.json'); await writeFile(existing, 'preserve')
    const alias = join(root, 'alias.json'); await link(fixturePath, alias)
    const dangling = join(root, 'dangling.json'); await symlink(join(root, 'missing'), dangling)
    const parent = join(root, 'parent'); await symlink(root, parent)
    const sidecar = join(root, 'sidecar.json'); await writeFile(`${sidecar}.comparison.json`, 'preserve')
    for (const output of [new URL(fixturePath).pathname, alias, existing, dangling, join(parent, 'output.json'), sidecar, root]) {
      const result = spawnSync(process.execPath, [script, output], {env:{...process.env, PATH:`${bin}:${process.env.PATH}`}, encoding:'utf8', timeout:10000})
      assert.notEqual(result.status, 0, output)
      await assert.rejects(access(marker), {code:'ENOENT'})
    }
    assert.equal(await readFile(existing, 'utf8'), 'preserve')
    assert.equal(await readFile(`${sidecar}.comparison.json`, 'utf8'), 'preserve')
    assert.deepEqual(await readFile(fixturePath), originalBytes)
  } finally {await rm(root, {recursive:true, force:true})}
})
