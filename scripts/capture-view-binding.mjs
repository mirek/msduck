// First-party SQL Server ground truth; errors and warning streams are evidence.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const output = resolve(process.argv[2] ?? 'artifacts/compatibility/view-binding-reference')
const suites = await Promise.all(['properties', 'execution', 'invalid-binding'].map(async name => ({
  name,
  fixture: JSON.parse(await readFile(new URL(`../reference/view-${name}.json`, import.meta.url), 'utf8')),
})))
const image = suites[0].fixture.image
for (const { fixture } of suites) assert.equal(fixture.image, image)
await mkdir(output, { recursive: true })
const differences = []
await withReferenceContainer(async (config, container) => {
  for (const { name, fixture } of suites) {
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      runs.push(await isolatedReference(config, async connection => {
        const results = []
        for (const { id, sql } of fixture.results) {
          results.push({ id, sql, result: canonical(await capture(connection, sql)) })
        }
        return results
      }))
    }
    // Preserve both raw executions before checking determinism or old evidence.
    await writeFile(resolve(output, `view-${name}-runs.json`), JSON.stringify(runs, null, 2) + '\n')
    assert.deepEqual(runs[0], runs[1], `${name}: fresh captures differ`)
    const actual = { image: container.image, identicalFreshCaptures: 2, results: runs[0] }
    await writeFile(resolve(output, `view-${name}.json`), JSON.stringify(actual, null, 2) + '\n')
    try { assert.deepEqual(actual, fixture) } catch { differences.push(name) }
    console.log(`Captured ${name}: ${actual.results.length} cases, two identical fresh databases`)
  }
}, { image })
assert.deepEqual(differences, [], `Retained reference differs: ${differences.join(', ')}; inspect raw captures in ${output}`)
