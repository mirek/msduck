import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { start, query } from './support/client.mjs'

test('stored Unicode integer casts match captured values and exact UTF16 diagnostics', async t => {
  const c = await start(t)
  const fixture=JSON.parse(await readFile(new URL('../reference/unicode-integer.json',import.meta.url),'utf8'))
  await query(c,'CREATE TABLE dbo.unicode_integer_source(s NVARCHAR(MAX))')
  for (const sample of fixture.results) {
    const source=sample.source.result.sets[0].rows[0][0]
    await query(c,'DELETE FROM dbo.unicode_integer_source; INSERT INTO dbo.unicode_integer_source VALUES(@s)',[['s',TYPES.NVarChar,source,{length:Infinity}]])
    for (const operation of sample.operations) {
      const expected=operation.result
      if (expected.errors.length) {
        const e=expected.errors[0]
        await assert.rejects(query(c,operation.sql),actual => {
          assert.deepEqual({number:actual.number,state:actual.state,class:actual.class,message:actual.message},{number:e.number,state:e.state,class:e.class,message:e.message},`${sample.sample}: ${operation.sql}`)
          return true
        })
      } else {
        const actual=await query(c,operation.sql)
        assert.deepEqual(actual.rows,expected.sets.flatMap(s=>s.rows),`${sample.sample}: ${operation.sql}`)
        assert.equal(actual.columns[0][0].dataLength,expected.sets[0].columns[0].length)
      }
    }
  }
  assert.deepEqual((await query(c,"SELECT JSON_VALUE(N'{\"n\":\"7\"}','$.n')+1 AS n")).rows,[[8]])
})
