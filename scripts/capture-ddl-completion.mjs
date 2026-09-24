// First-party SQL Server DDL completion evidence, including internal DONE fields.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = JSON.parse(await readFile(new URL('../reference/ddl-completion.json', import.meta.url), 'utf8'))
const output = resolve(process.argv[2] ?? 'artifacts/compatibility/ddl-completion-reference')
await mkdir(output, { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    const modes = []
    for (const suite of fixture.results) {
      assert.ok(['batch', 'rpc'].includes(suite.mode))
      modes.push(await isolatedReference(config, async connection => {
        if (suite.mode === 'rpc') connection.execSqlBatch = request => connection.execSql(request)
        let tokens = []
        const debugToken = connection.debug.token.bind(connection.debug)
        connection.debug.token = token => {
          if (token.name.startsWith('DONE')) {
            // Match the fixture's JSON representation of optional token fields.
            tokens.push(JSON.parse(JSON.stringify(token)))
          }
          debugToken(token)
        }
        const results = []
        for (const { id, sql } of suite.results) {
          tokens = []
          const result = canonical(await capture(connection, sql))
          const completion = [...tokens]
          const state = canonical(await capture(connection, 'SELECT @@ROWCOUNT AS r,@@ERROR AS e'))
          results.push({ id, sql, result, completion, state })
        }
        return { mode: suite.mode, results }
      }))
    }
    runs.push(modes)
  }
  await writeFile(resolve(output, 'runs.json'), JSON.stringify(runs, null, 2) + '\n')
  assert.deepEqual(runs[0], runs[1], 'Fresh DDL completion captures differ')
  const actual = { image: container.image, identicalFreshCaptures: 2, results: runs[0] }
  await writeFile(resolve(output, 'ddl-completion.json'), JSON.stringify(actual, null, 2) + '\n')
  assert.deepEqual(actual, fixture, `Retained reference differs; inspect raw captures in ${output}`)
  console.log(`Captured ${runs[0].reduce((n, suite) => n + suite.results.length, 0)} batch/RPC observations twice identically`)
}, { image: fixture.image })
