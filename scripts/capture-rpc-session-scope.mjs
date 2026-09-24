import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {Request, TYPES} from 'tedious'
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

export const preparedCases = [
  {name: 'prepared DATEFIRST values and recovery', sql: "SET DATEFIRST @first; SELECT @@DATEFIRST AS first_day; IF @fail=1 THROW 51000,'scope probe',1", parameters: ['first', 'fail'], values: [
    {first: 3, fail: 0}, {first: 5, fail: 1}, {first: 0, fail: 0}, {first: null, fail: 0}, {first: 1, fail: 0},
  ]},
  {name: 'prepared ANSI_WARNINGS and recovery', sql: `SET ANSI_WARNINGS OFF; ${probe}; IF @fail=1 THROW 51000,'scope probe',1`, parameters: ['fail'], values: [{fail: 0}, {fail: 1}, {fail: 0}]},
]

export async function runPreparedCase(connection, entry) {
  const setup = await captureStep(connection, {mode: 'batch', sql: baseline})
  assert.deepEqual(setup.errors, [])
  const observe = () => captureStep(connection, {mode: 'rpc', sql: probe})
  const initial = await observe()
  let complete = () => {}
  const request = new Request(entry.sql, (...args) => complete(...args))
  for (const name of entry.parameters) request.addParameter(name, TYPES.Int)
  await new Promise((resolve, reject) => {
    request.once('prepared', resolve)
    request.once('error', reject)
    connection.prepare(request)
  })
  const afterPrepare = await observe()
  const executions = []
  try {
    for (const values of entry.values) {
      const result = {sets: [], done: [], errors: [], info: [], returnStatus: null}
      const onError = e => result.errors.push({number:e.number,state:e.state,class:e.class,lineNumber:e.lineNumber,message:e.message})
      const onInfo = e => result.info.push({number:e.number,state:e.state,class:e.class,lineNumber:e.lineNumber,message:e.message})
      const onMetadata = metadata => result.sets.push({columns:metadata.map(c=>({name:c.colName,type:c.type.name,length:c.dataLength??null,precision:c.precision??null,scale:c.scale??null,flags:c.flags,collation:canonical(c.collation??null)})),rows:[]})
      const onRow = row => result.sets.at(-1).rows.push(row.map(c=>c.value))
      const done = Object.fromEntries(['done','doneInProc','doneProc'].map(kind=>[kind,(rowCount,more,status)=>{
        result.done.push({kind,rowCount:rowCount??null,more})
        if(kind==='doneProc') result.returnStatus=status
      }]))
      connection.on('errorMessage',onError);connection.on('infoMessage',onInfo)
      request.on('columnMetadata',onMetadata);request.on('row',onRow)
      for(const [kind,listener] of Object.entries(done)) request.on(kind,listener)
      try {
        await new Promise(resolve=>{
          complete=(error,rowCount)=>{
            result.rowCount=rowCount
            if(error&&!result.errors.length) result.errors.push({message:error.message,number:error.number??null})
            resolve()
          }
          request.error=undefined
          connection.execute(request,values)
        })
      } finally {
        connection.off('errorMessage',onError);connection.off('infoMessage',onInfo)
        request.off('columnMetadata',onMetadata);request.off('row',onRow)
        for(const [kind,listener] of Object.entries(done)) request.off(kind,listener)
      }
      executions.push({values,result:canonical(result),reuse:await observe()})
    }
  } finally {
    await new Promise((resolve,reject)=>{
      complete=error=>error?reject(error):resolve()
      request.error=undefined
      connection.unprepare(request)
    })
  }
  return {setup,initial,afterPrepare,executions,afterUnprepare:await observe()}
}

export async function runPreparedCases(connection) {
  const results=[]
  for(const entry of preparedCases) results.push({...entry,result:await runPreparedCase(connection,entry)})
  return results
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const output = resolve(process.argv[2] ?? 'artifacts/compatibility/rpc-session-scope-reference')
  await mkdir(output, {recursive: true})
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) runs.push(await isolatedReference(config, async connection => ({
      results: await runCases(connection), prepared: await runPreparedCases(connection),
    })))
    assert.deepEqual(runs[0], runs[1], 'Fresh session scope captures differ')
    const actual = {image: container.image, identicalFreshCaptures: 2, ...runs[0]}
    await writeFile(resolve(output, 'runs.json'), JSON.stringify(runs, null, 2) + '\n')
    await writeFile(resolve(output, 'rpc-session-scope.json'), JSON.stringify(actual, null, 2) + '\n')
    let fixture
    try { fixture = JSON.parse(await readFile(new URL('../reference/rpc-session-scope.json', import.meta.url), 'utf8')) }
    catch (error) { if (error.code !== 'ENOENT') throw error }
    if (fixture) assert.deepEqual(actual, fixture, 'Retained session scope capture differs')
    console.log(`Captured ${actual.results.length} session scenarios and ${actual.prepared.length} prepared scenarios twice identically${fixture ? ' and matched retained fixture' : ''}`)
  })
}
