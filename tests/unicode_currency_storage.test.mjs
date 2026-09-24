import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Request, TYPES } from 'tedious'
import { start, query } from './support/client.mjs'

test('stored Unicode currency text preserves conversions and failed-write atomicity', async t => {
  const c = await start(t)
  await query(c, "CREATE TABLE unicode_currency(id INT,v NCHAR(20),m MONEY); INSERT INTO unicode_currency VALUES(1,N'£12.34565',0),(2,N'bad',0),(3,NULL,0),(4,N'',0)")
  assert.deepEqual((await query(c, 'SELECT id,TRY_CAST(v AS MONEY) FROM unicode_currency ORDER BY id')).rows,
    [[1,12.3457],[2,null],[3,null],[4,0]])
  await assert.rejects(query(c, 'UPDATE unicode_currency SET m=v'), e => e.number === 235)
  assert.deepEqual((await query(c, 'SELECT m FROM unicode_currency ORDER BY id')).rows, [[0],[0],[0],[0]])
  await query(c, "UPDATE unicode_currency SET v=LEFT(N'🦆',1) WHERE id=2")
  assert.deepEqual((await query(c, 'SELECT TRY_CAST(v AS MONEY) FROM unicode_currency WHERE id=2')).rows, [[null]])
  await assert.rejects(query(c, 'SELECT CAST(v AS MONEY) FROM unicode_currency WHERE id=2'), e => e.number === 235)
  await query(c, "UPDATE unicode_currency SET v=N'214748.3648' WHERE id=2")
  assert.deepEqual((await query(c, 'SELECT TRY_CAST(v AS SMALLMONEY),CAST(v AS MONEY) FROM unicode_currency WHERE id=2')).rows, [[null,214748.3648]])
})

test('prepared currency conversion reads current Unicode column values on each execution', async t => {
  const c = await start(t)
  await query(c, "CREATE TABLE prepared_unicode_currency(id INT,v NVARCHAR(30)); INSERT INTO prepared_unicode_currency VALUES(1,N'€3.12505'),(2,NULL)")
  let done
  const request = new Request('SELECT CAST(v AS MONEY) AS m FROM prepared_unicode_currency WHERE id=@id', (...args) => done(...args))
  request.addParameter('id', TYPES.Int)
  await new Promise((resolve, reject) => {
    request.once('prepared', resolve)
    request.once('error', reject)
    c.prepare(request)
  })
  const run = id => new Promise((resolve, reject) => {
    const rows = []
    const row = cells => rows.push(cells.map(c => c.value))
    request.on('row', row)
    done = error => {
      request.off('row', row)
      error ? reject(error) : resolve(rows)
    }
    c.execute(request, { id })
  })
  assert.deepEqual(await run(1), [[3.1251]])
  assert.deepEqual(await run(2), [[null]])
  await query(c, "UPDATE prepared_unicode_currency SET v=N'£4.50' WHERE id=1")
  assert.deepEqual(await run(1), [[4.5]])
  await new Promise((resolve, reject) => {
    done = error => error ? reject(error) : resolve()
    c.unprepare(request)
  })
})
