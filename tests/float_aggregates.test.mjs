import assert from 'node:assert/strict'
import { readFile, mkdir, writeFile } from 'node:fs/promises'
import { test } from 'node:test'
import { createHash } from 'node:crypto'
import { createRequire } from 'node:module'
import { TYPES } from 'tedious'
import { start, query } from './support/client.mjs'
import { capture, canonical, differences } from '../scripts/lib/compatibility.mjs'

const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneKinds = new Map([[0xFD, 'DONE'], [0xFE, 'DONEPROC'], [0xFF, 'DONEINPROC']])

async function floatCapture(connection, sql) {
  const raw = [], doneTokens = [], events = []
  const createParser = connection.createTokenStreamParser
  const readToken = StreamParser.prototype.readToken
  function recordDone(parser, type) {
    const at = parser.position
    if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordDone(parser, type))
    raw.push({ kind: doneKinds.get(type), status: parser.buffer.readUInt16LE(at), command: parser.buffer.readUInt16LE(at + 2) })
    return readToken.call(parser, type)
  }
  StreamParser.prototype.readToken = function (type) {
    return doneKinds.has(type) ? recordDone(this, type) : readToken.call(this, type)
  }
  connection.createTokenStreamParser = function (message, handler) {
    const parser = createParser.call(this, message, handler)
    parser.parser.prependListener('data', token => {
      if (typeof token.name === 'string') events.push({ kind: token.name })
      if (['DONE', 'DONEINPROC', 'DONEPROC'].includes(token.name)) doneTokens.push({
        kind: token.name, more: token.more, sqlError: token.sqlError,
        attention: token.attention, serverError: token.serverError,
        rowCount: token.rowCount ?? null, command: token.curCmd,
      })
    })
    return parser
  }
  try {
    const result = canonical(await capture(connection, sql))
    assert.equal(raw.length, doneTokens.length)
    assert.equal(doneTokens.length, result.done.length)
    doneTokens.forEach((token, index) => {
      assert.equal(token.kind, raw[index].kind)
      assert.equal(token.command, raw[index].command)
      token.status = raw[index].status
    })
    return { ...result, doneTokens, events }
  } finally {
    connection.createTokenStreamParser = createParser
    StreamParser.prototype.readToken = readToken
  }
}

function bits(value, width = 8) {
  const buffer = Buffer.alloc(width)
  if (width === 4) buffer.writeFloatBE(value); else buffer.writeDoubleBE(value)
  return buffer.toString('hex')
}
function rows(result, retainedBits) {
  return result.sets.map((set, si) => set.rows.map((row, ri) => row.map((value, ci) => {
    const column = set.columns[ci]
    if (!['FloatN', 'Real', 'Float'].includes(column.type) || value === null) return value
    const number = typeof value === 'number' ? value : value?.kind === 'number' ? Number(value.value) : NaN
    return retainedBits ? retainedBits[si][ri][ci] : bits(number, column.type === 'Real' ? 4 : column.type === 'Float' ? 8 : column.length)
  })))
}

test('FLOAT aggregate execution matches stable captured bits widths diagnostics and reuse', async t => {
  const bytes = await readFile(new URL('../reference/float-aggregates.json', import.meta.url))
  assert.equal(createHash('sha256').update(bytes).digest('hex'), 'e19bac07a0d08caf68fc94101a3ee1dee891faf6598002af44d8d93800492ce2')
  const fixture = JSON.parse(bytes)
  assert.equal(JSON.stringify(fixture.runs[0]), JSON.stringify(fixture.runs[1]))
  const connection = await start(t)
  const records = [], failures = []
  for (const sample of fixture.runs[0].filter(sample => sample.name !== 'server version')) {
    const actual = await floatCapture(connection, sample.sql)
    const actualRows = rows(actual), expectedRows = rows(sample.result, sample.bits)
    const bitDifferences = differences(actualRows, expectedRows)
    records.push({ name: sample.name, sql: sample.sql, actual, reference: sample.result, referenceBits: sample.bits, actualRows, bitDifferences, differences: differences(actual, sample.result) })
    let stableActual = actualRows, stableExpected = expectedRows
    if (sample.name === 'FLOAT(53) distinct') {
      // Backend DISTINCT transition order is not fixed. Assert exact possible
      // sequential results, without tolerance or sorting the captured input.
      const possible = [[0.1,0.2,0.3],[0.1,0.3,0.2],[0.2,0.1,0.3],[0.2,0.3,0.1],[0.3,0.1,0.2],[0.3,0.2,0.1]]
        .map(input => { let sum = 0; for (const value of input) sum += value; return [bits(sum), bits(sum/3)] })
      assert(possible.some(pair => pair[0] === actualRows[0]?.[0]?.[0] && pair[1] === actualRows[0]?.[0]?.[1]), 'DISTINCT sum/count must correspond to one exact typed input permutation')
      // Record the raw differences above; exclude only these two order-sensitive
      // cells from the stable-plan assertion, retaining shape and MIN/MAX checks.
      stableActual = actualRows.map(set => set.map(row => row.slice(2)))
      stableExpected = expectedRows.map(set => set.map(row => row.slice(2)))
    }
    for (const difference of differences({ columns: actual.sets.map(set => set.columns), rows: stableActual, errors: actual.errors, info: actual.info },
      { columns: sample.result.sets.map(set => set.columns), rows: stableExpected, errors: sample.result.errors, info: sample.result.info })) failures.push({ name: sample.name, ...difference })
  }
  await mkdir('artifacts/compatibility', { recursive: true })
  await writeFile('artifacts/compatibility/float-aggregate-execution.json', JSON.stringify({ records }, null, 2)+'\n')
  assert.equal(records.length, 45)
  assert.deepEqual(failures, [])
})

test('FLOAT SUM AVG window warnings inspect consumed frames only', async t => {
  const connection = await start(t)
  for (const [direction, warnings] of [['FOLLOWING', []], ['PRECEDING', [8153]]]) {
    const sql = `SELECT SUM(v) OVER(ORDER BY id ROWS BETWEEN 1 ${direction} AND 1 ${direction}) AS s,AVG(v) OVER(ORDER BY id ROWS BETWEEN 1 ${direction} AND 1 ${direction}) AS a FROM (VALUES(1,CAST(NULL AS REAL)),(2,CAST(2 AS REAL))) d(id,v) ORDER BY id`
    const result = await floatCapture(connection, sql)
    assert.deepEqual(result.errors, [])
    assert.deepEqual(result.info.map(message => message.number), warnings)
    assert.deepEqual(result.sets[0].rows, direction === 'FOLLOWING' ? [[2,2],[null,null]] : [[null,null],[null,null]])
  }
})

test('FLOAT aggregates preserve prepared parameters catalog declarations and catchable overflow', async t => {
  const connection = await start(t)
  await query(connection, 'CREATE TABLE float_source(v REAL); INSERT INTO float_source VALUES(0.1),(0.2),(0.3)')
  const result = await query(connection, 'SELECT SUM(v) AS s,AVG(v) AS a FROM float_source')
  assert(result.columns[0].every(column => column.type.name === 'FloatN' && column.dataLength === 8))
  const call = value => query(connection, 'SELECT SUM(@v) AS s,AVG(@v) AS a FROM (VALUES(1),(2)) d(id)', [['v',TYPES.Float,value]])
  assert.deepEqual((await call(1.25)).rows, [[2.5,1.25]])
  await assert.rejects(call(1e308), error => error.number === 8115 && error.state === 2 && error.class === 16)
  assert.deepEqual((await call(2.5)).rows, [[5,2.5]])
  const caught = await query(connection, "BEGIN TRY SELECT SUM(v) FROM (VALUES(CAST('1e308' AS FLOAT)),(CAST('1e308' AS FLOAT))) d(v); END TRY BEGIN CATCH SELECT ERROR_NUMBER(),ERROR_STATE(); END CATCH")
  assert.deepEqual(caught.rows, [[8115,2]])
})

test('FLOAT expression operands use checked aggregates without changing ISNULL precedence', async t => {
  const connection = await start(t)
  for (const operand of ["CAST(v AS FLOAT)+CAST(0 AS FLOAT)", "CASE WHEN id=1 THEN CAST(v AS FLOAT) ELSE CAST(v AS FLOAT) END", "COALESCE(CAST(v AS FLOAT),CAST(0 AS FLOAT))", "ISNULL(CAST(v AS FLOAT),CAST(0 AS FLOAT))"]) {
    await assert.rejects(query(connection, `SELECT SUM(${operand}) FROM (VALUES(1,CAST('1e308' AS FLOAT)),(2,CAST('1e308' AS FLOAT))) d(id,v)`), error => error.number === 8115 && error.state === 2)
  }
  const integer = await query(connection, 'SELECT SUM(ISNULL(CAST(v AS INT),CAST(1.25 AS REAL))) AS s FROM (VALUES(1),(2)) d(v)')
  assert.deepEqual(integer.rows, [[3]])
  assert.equal(integer.columns[0][0].type.name, 'IntN')
})
