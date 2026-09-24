import assert from 'node:assert/strict'
import { test } from 'node:test'
import { start, query } from './support/client.mjs'

test('stored JSON validation and escaping preserve isolated Unicode units on the wire', async t => {
  const c = await start(t)
  await query(c, "CREATE TABLE json_wire(id INT,s NVARCHAR(MAX),j NVARCHAR(MAX)); INSERT INTO json_wire VALUES(1,LEFT(N'🦆',1),N'{\"s\":\"'+LEFT(N'🦆',1)+N'\"}'),(2,RIGHT(N'🦆',1),N'{\"s\":\"'+RIGHT(N'🦆',1)+N'\"}'),(3,NULL,NULL)")
  const result = await query(c, "SELECT id,ISJSON(j),STRING_ESCAPE(s,N'json'),CONVERT(VARBINARY(MAX),STRING_ESCAPE(s,N'json')) FROM json_wire ORDER BY id")
  assert.deepEqual(result.rows, [[1,1,'\ud83e',Buffer.from('3ed8','hex')],[2,1,'\udd86',Buffer.from('86dd','hex')],[3,null,null,null]])
  assert.deepEqual((await query(c, "SELECT STRING_ESCAPE(LEFT(N'🦆',1)+NCHAR(0)+N'/'+RIGHT(N'🦆',1),N'json')")).rows, [['\ud83e\\u0000\\/\udd86']])
})
