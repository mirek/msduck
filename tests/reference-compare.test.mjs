import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import { pathToFileURL } from 'node:url'
import { assertSameCapture, describeFirstDifference, refuseExistingFixture, writeNewFixture } from '../scripts/lib/reference.mjs'

const library = new URL('../scripts/lib/reference.mjs', import.meta.url).href

test('a large mismatching capture fails quickly with bounded memory and message', () => {
  // Two independently built ~40 MB captures differing in one late record. The
  // child has a 512 MB heap; inspecting the operands (node:assert behavior)
  // would exhaust it or run far past the timeout.
  const program = `
    import { assertSameCapture } from ${JSON.stringify(library)}
    const build = mutate => ({ image: 'reference', runs: [Array.from({ length: 200000 }, (_, i) => ({
      name: 'case ' + i,
      result: { sets: [{ columns: [{ name: 'value', type: 'NVarChar' }], rows: [[i, (mutate && i === 199990 ? 'y' : 'x').repeat(120)]] }], done: [{ rowCount: 1 }], errors: [] },
    }))] })
    const actual = build(false), expected = build(true)
    const started = Date.now()
    try { assertSameCapture(actual, expected, 'large capture differs'); process.exit(3) }
    catch (error) {
      if (error.constructor !== Error) process.exit(4)
      process.stdout.write(JSON.stringify({ message: error.message, ms: Date.now() - started, rss: process.memoryUsage().rss }))
    }`
  const child = spawnSync(process.execPath, ['--max-old-space-size=512', '--input-type=module', '-e', program], { encoding: 'utf8', timeout: 60000 })
  assert.equal(child.error, undefined)
  assert.equal(child.status, 0, child.stderr.slice(0, 2000))
  const { message, ms, rss } = JSON.parse(child.stdout)
  assert.ok(message.length <= 900, `message length ${message.length}`)
  assert.match(message, /^large capture differs \(first difference at \.runs\[0\]\[199990\]\("case 199990"\)\.result\.sets\[0\]\.rows\[0\]\[1\]: actual "x{80}"\.\.\.\(120 chars\), expected "y{80}"\.\.\.\(120 chars\)\)$/)
  assert.ok(ms < 20000, `comparison took ${ms} ms`)
  assert.ok(rss < 1024 * 1024 * 1024, `child RSS ${rss}`)
})

test('equal captures pass and differences describe a bounded path', () => {
  assertSameCapture({ runs: [[{ name: 'a', value: 1n }]] }, { runs: [[{ name: 'a', value: 1n }]] }, 'same')
  assert.throws(() => assertSameCapture([1, 2, 3], [1, 2], 'length'), { message: 'length (first difference at [2] (array length 3 vs 2): actual 3, expected undefined)' })
  assert.throws(() => assertSameCapture({ a: 1 }, { a: 1, b: null }, 'key'), { message: 'key (first difference at .b: actual undefined, expected null)' })
  assert.throws(() => assertSameCapture({ a: [1] }, { a: { 0: 1 } }, 'kind'), { message: 'kind (first difference at .a: actual array(1), expected object(1 keys))' })
  const long = 'k'.repeat(10000)
  let deep = { [long]: 'v'.repeat(10000) }, other = { [long]: 'w'.repeat(10000) }
  for (let i = 0; i < 200; i++) { deep = { [long + i]: deep }; other = { [long + i]: other } }
  assert.ok(describeFirstDifference(deep, other).length <= 500)
  assert.throws(() => assertSameCapture(deep, other, 'l'.repeat(10000)), error => error.message.length <= 900)
})

test('refusal checks existence only and never parses the fixture', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'msduck-reference-compare-'))
  try {
    const fixture = join(directory, 'fixture.json')
    await refuseExistingFixture(fixture)
    await refuseExistingFixture(pathToFileURL(fixture))
    // Invalid JSON: parsing would raise SyntaxError instead of the refusal.
    await writeFile(fixture, '{ not json')
    await assert.rejects(refuseExistingFixture(fixture), { constructor: Error, message: `refusing to overwrite retained fixture ${fixture}` })
    await assert.rejects(refuseExistingFixture(pathToFileURL(fixture)), { message: `refusing to overwrite retained fixture ${fixture}` })
  } finally { await rm(directory, { recursive: true, force: true }) }
})

test('new fixtures are written exclusively in compact JSON', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'msduck-reference-compare-'))
  try {
    const fixture = join(directory, 'fixture.json')
    await writeNewFixture(fixture, { image: 'reference', runs: [[1]] })
    assert.equal(await readFile(fixture, 'utf8'), '{"image":"reference","runs":[[1]]}\n')
    await assert.rejects(writeNewFixture(fixture, { replaced: true }), { code: 'EEXIST' })
    assert.equal(await readFile(fixture, 'utf8'), '{"image":"reference","runs":[[1]]}\n')
  } finally { await rm(directory, { recursive: true, force: true }) }
})
