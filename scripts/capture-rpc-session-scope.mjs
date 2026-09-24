import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

const baseline = 'SET DATEFIRST 7; SET ANSI_WARNINGS ON'
const probe = 'SELECT @@DATEFIRST AS first_day; SELECT SUM(n) AS total FROM (VALUES (CAST(NULL AS INT)),(1)) v(n)'
export const cases = []
for (const mode of ['batch', 'rpc']) {
  for (const setting of ['SET DATEFIRST 3', 'SET ANSI_WARNINGS OFF']) {
    for (const ending of ['', "; RAISERROR('scope probe',16,1)", "; THROW 51000,'scope probe',1"]) {
      cases.push({name: `${mode}: ${setting}: ${ending || 'success'}`, steps: [
        {mode: 'batch', sql: baseline},
        {mode, sql: `${setting}; ${probe}${ending}`},
        {mode: 'rpc', sql: probe},
      ]})
    }
  }
}
for (const mode of ['batch', 'rpc']) {
  cases.push({name: `${mode}: LANGUAGE resets DATEFIRST`, steps: [
    {mode: 'batch', sql: 'SET DATEFIRST 3; SET ANSI_WARNINGS ON'},
    {mode, sql: 'SET LANGUAGE us_english; SELECT @@DATEFIRST AS first_day'},
    {mode: 'rpc', sql: 'SELECT @@DATEFIRST AS first_day'},
  ]})
}

export async function captureStep(connection, {mode, sql}) {
  assert.ok(['batch', 'rpc'].includes(mode))
  const transport = mode === 'batch' ? connection : {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch: request => connection.execSql(request),
  }
  return canonical(await capture(transport, sql))
}

export async function runCases(connection) {
  const results = []
  for (const entry of cases) {
    const steps = []
    for (const step of entry.steps) steps.push({...step, result: await captureStep(connection, step)})
    assert.deepEqual(steps[0].result.errors, [], entry.name)
    assert.deepEqual(steps.at(-1).result.errors, [], `${entry.name}: connection reuse`)
    results.push({name: entry.name, steps})
  }
  return results
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const output = resolve(process.argv[2] ?? 'artifacts/compatibility/rpc-session-scope-reference')
  await mkdir(output, {recursive: true})
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) runs.push(await isolatedReference(config, runCases))
    assert.deepEqual(runs[0], runs[1], 'Fresh session scope captures differ')
    const actual = {image: container.image, identicalFreshCaptures: 2, results: runs[0]}
    await writeFile(resolve(output, 'runs.json'), JSON.stringify(runs, null, 2) + '\n')
    await writeFile(resolve(output, 'rpc-session-scope.json'), JSON.stringify(actual, null, 2) + '\n')
    let fixture
    try { fixture = JSON.parse(await readFile(new URL('../reference/rpc-session-scope.json', import.meta.url), 'utf8')) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    if (fixture) assert.deepEqual(actual, fixture, 'Retained session scope capture differs')
    console.log(`Captured ${actual.results.length} session scenarios twice identically${fixture ? ' and matched retained fixture' : ''}`)
  })
}
