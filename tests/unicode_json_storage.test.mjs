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

test('Unicode JSON extraction preserves raw units escaped spelling and stored property paths', async t => {
  const c = await start(t)
  await query(c, "CREATE TABLE json_extract_wire(id INT,j NVARCHAR(MAX)); INSERT INTO json_extract_wire VALUES(1,N'{\"s\":\"'+LEFT(N'🦆',1)+N'\",\"a\":[\"'+LEFT(N'🦆',1)+N'\"]}')")
  await query(c, String.raw`INSERT INTO json_extract_wire VALUES(2,N'{"s":"\ud800","a":["\ud800"]}')`)
  assert.deepEqual((await query(c, "SELECT JSON_VALUE(j,'$.s'),JSON_QUERY(j,'$.a'),JSON_PATH_EXISTS(j,'$.a[*]') FROM json_extract_wire ORDER BY id")).rows, [['\ud83e','["\ud83e"]',1],['\ud800',String.raw`["\ud800"]`,1]])
  await query(c, "CREATE TABLE json_path_wire(j NVARCHAR(MAX),p NVARCHAR(30)); INSERT INTO json_path_wire VALUES(N'{\"'+LEFT(N'🦆',1)+N'\":7}',N'$.\"'+LEFT(N'🦆',1)+N'\"')")
  assert.deepEqual((await query(c, 'SELECT JSON_VALUE(j,p),JSON_PATH_EXISTS(j,p) FROM json_path_wire')).rows, [['7',1]])
})

test('JSON NULL path diagnostics match captured source-null and path-type cases', async t => {
  const { readFile } = await import('node:fs/promises')
  const fixture = JSON.parse(await readFile(new URL('../reference/unicode-json-storage.json',import.meta.url),'utf8'))
  const c = await start(t)
  await query(c, 'CREATE TABLE dbo.unicode_json_source(j NVARCHAR(MAX),p NVARCHAR(100)); INSERT INTO dbo.unicode_json_source VALUES(NULL,NULL)')
  for (const item of fixture.nullPaths) {
    await query(c,item.setup)
    const expected = item.result.errors[0]
    assert.ok(expected)
    await assert.rejects(query(c,item.sql),error => error.number === expected.number && error.state === expected.state && error.message === expected.message, `${item.source} ${item.path} ${item.operation}`)
  }
})
