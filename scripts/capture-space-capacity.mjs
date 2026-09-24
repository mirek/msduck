// Run with Node 24 on a Docker-capable host; output preserves raw UTF-16 JSON.
import assert from 'node:assert/strict'
import { writeFile } from 'node:fs/promises'
import { Request, TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, command } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const output = process.argv[2] ?? 'reference/space-capacity.json'
const query = "SELECT SPACE(@n),N'a'+SPACE(@n)+N'b'"
const programs = [
 ['signed integer boundary', 'SELECT SPACE(-2147483648) AS n'],
 ['zero and signs', 'SELECT SPACE(0) AS a,SPACE(-1) AS b,SPACE(+3) AS c,SPACE(-(-3)) AS d'],
 ['decimal counts', 'SELECT SPACE(2.9) AS a,SPACE(-0.9) AS b,SPACE(-2.9) AS c'],
 ['null and cap', 'SELECT SPACE(NULL) AS a,SPACE(9000) AS b'],
 ['dependent width', "SELECT ISNULL(CASE WHEN 1=0 THEN SPACE(0) END,CHAR(65)) AS n,SPACE(0)+'x' AS s"],
]

await withReferenceContainer(async (config, container) => {
  const c = await connect(config)
  let tokens = []
  const debug = c.debug.token.bind(c.debug)
  c.debug.token = token => { if (token.name.startsWith('DONE')) tokens.push({ ...token }); debug(token) }
  try {
    const version = await command(c, 'SELECT @@VERSION AS version')
    let result
    let complete = () => {}
    const request = new Request(query, (...args) => complete(...args))
    request.addParameter('n', TYPES.Int, undefined)
    const errorFields = e => ({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
    const onError = e => result?.errors.push(errorFields(e))
    const onInfo = e => result?.info.push(errorFields(e))
    c.on('errorMessage', onError)
    c.on('infoMessage', onInfo)
    request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(x => ({ name: x.colName, type: x.type.name, length: x.dataLength ?? null, precision: x.precision ?? null, scale: x.scale ?? null, flags: x.flags, collation: canonical(x.collation ?? null) })), rows: [] }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(x => x.value)))
    for (const kind of ['done','doneInProc','doneProc']) request.on(kind, (rowCount, more) => result?.done.push({ kind, rowCount: rowCount ?? null, more }))
    request.on('doneProc', (_count, _more, status) => { if (result) result.returnStatus = status })
    await new Promise((resolve, reject) => {
      complete = error => { if (error) reject(error) }
      request.once('prepared', resolve)
      request.once('error', reject)
      c.prepare(request)
    })
    const results = []
    for (const n of [null,-1,0,2,3999,4000,8000,2147483647]) {
      result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
      tokens = []
      await new Promise((resolve, reject) => {
        complete = (error, rowCount) => { result.rowCount = rowCount; error ? reject(error) : resolve() }
        request.error = undefined
        c.execute(request, { n })
      })
      assert.equal(result.errors.length, 0)
      assert.equal(result.sets.length, 1)
      assert.equal(result.sets[0].rows.length, 1)
      results.push({ name: `prepared SPACE ${n}`, query, parameters: { n }, reference: canonical(result), tokens: canonical(tokens) })
    }
    result = undefined
    await new Promise((resolve, reject) => { complete = error => error ? reject(error) : resolve(); c.unprepare(request) })
    c.off('errorMessage', onError)
    c.off('infoMessage', onInfo)
    for (const [name, query] of programs) {
      tokens = []
      const reference = canonical(await capture(c, query))
      assert.equal(reference.errors.length, 0, name)
      assert.equal(reference.sets.length, 1, name)
      // Empty and multi-row metadata probes are intentional.
      results.push({ name, query, reference, tokens: canonical(tokens) })
    }
    const reuse = await command(c, 'SELECT 1 AS reusable')
    await writeFile(output, JSON.stringify({ image: container.image, version, preparedProtocol: 'tedious.prepare/execute/unprepare; same handle reused for all SPACE inputs', results, reuse }, null, 2)+'\n')
    for (const x of results) console.log(JSON.stringify({ name: x.name, columns: x.reference.sets[0].columns.map(c => ({ type: c.type, length: c.length })), lengths: (x.reference.sets[0].rows[0] ?? []).map(v => v?.length ?? null), suffixUnits: (x.reference.sets[0].rows[0] ?? []).map(v => typeof v === 'string' ? Array.from({length:Math.min(3,v.length)}, (_,i) => v.charCodeAt(v.length-Math.min(3,v.length)+i)) : null) }))
  } finally { c.close() }
})
