import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Request, TYPES } from 'tedious'
import { start, query } from './support/client.mjs'

function batch(connection, sql) {
  return new Promise((resolve, reject) => {
    const rows = []
    const request = new Request(sql, error => error ? reject(error) : resolve(rows))
    request.on('row', cells => rows.push(cells.map(cell => cell.value)))
    connection.execSqlBatch(request)
  })
}
const setup = 'CREATE TABLE dbo.quoted_width(id INT NOT NULL, [@p] INT NULL); INSERT INTO dbo.quoted_width VALUES(1,NULL),(2,7)'
const sql = quoted => `SELECT DATALENGTH(${quoted}) AS width, ${quoted} AS value, @p AS parameter FROM dbo.quoted_width ORDER BY id`

test('quoted columns retain their NULL checks and values with conflicting RPC declarations', async t => {
  const c = await start(t)
  await batch(c, setup)
  for (const quoted of ['[@p]', '"@p"']) for (const value of [42, null]) {
    const result = await query(c, sql(quoted), [['p', TYPES.BigInt, value]])
    assert.deepEqual(result.rows, [[null, null, value === null ? null : '42'], [4, 7, value === null ? null : '42']])
    assert.equal(result.columns[0][0].type.name, 'IntN')
    assert.equal(result.columns[0][0].dataLength, 4)
  }
})

test('quoted counter columns survive batch and RPC lowering', async t => {
  const c = await start(t)
  await batch(c, 'CREATE TABLE dbo.quoted_counter([@@ROWCOUNT] INT NULL, [@@ERROR] INT NULL); INSERT INTO dbo.quoted_counter VALUES(NULL,17),(9,NULL)')
  const select = 'SELECT [@@ROWCOUNT] AS r, "@@ERROR" AS e FROM dbo.quoted_counter ORDER BY [@@ROWCOUNT]'
  assert.deepEqual(await batch(c, select), [[null, 17], [9, null]])
  assert.deepEqual((await query(c, select)).rows, [[null, 17], [9, null]])
  assert.equal((await batch(c, 'SELECT @@ERROR AS e'))[0][0], 0)
})

test('prepared quoted columns retain NULL checks across parameter changes', async t => {
  const c = await start(t)
  await batch(c, setup)
  let complete = () => {}
  const request = new Request(sql('[@p]'), (...args) => complete(...args))
  request.addParameter('p', TYPES.BigInt)
  await new Promise((resolve, reject) => {
    complete = error => { if (error) reject(error) }
    request.once('prepared', resolve)
    c.prepare(request)
  })
  try {
    for (const value of [42, null]) {
      const rows = []
      const onRow = cells => rows.push(cells.map(cell => cell.value))
      request.on('row', onRow)
      try {
        await new Promise((resolve, reject) => {
          complete = error => error ? reject(error) : resolve()
          request.error = undefined
          c.execute(request, { p: value })
        })
      } finally { request.off('row', onRow) }
      assert.deepEqual(rows, [[null, null, value === null ? null : '42'], [4, 7, value === null ? null : '42']])
    }
  } finally {
    await new Promise((resolve, reject) => {
      complete = error => error ? reject(error) : resolve()
      request.error = undefined
      c.unprepare(request)
    })
  }
})
