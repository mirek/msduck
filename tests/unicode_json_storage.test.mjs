import assert from 'node:assert/strict'
import { test } from 'node:test'
import { start, query } from './support/client.mjs'
import { TYPES } from 'tedious'

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

test('OPENJSON default and explicit rows preserve captured Unicode values and binary projections', async t => {
  const { readFile } = await import('node:fs/promises')
  const fixture = JSON.parse(await readFile(new URL('../reference/unicode-json-storage.json',import.meta.url),'utf8'))
  const c = await start(t)
  const decode = value => value?.kind === 'binary' ? Buffer.from(value.value,'hex') : value
  const compareRows = async item => {
    assert.equal(item.result.errors.length,0)
    const actual = await query(c,item.sql)
    const expected = item.result.sets.flatMap(set => set.rows.map(row => row.map(decode)))
    assert.deepEqual(actual.rows,expected,item.sql)
  }
  for (const item of fixture.results) {
    // Load captured source units directly so JSON row replay does not depend on
    // the separate document-construction UPDATE regression below.
    const captured = operation => item.operations.find(op => op.operation === operation).result.sets[0].rows[0][0]
    await query(c,`DROP TABLE IF EXISTS dbo.unicode_json_source; CREATE TABLE dbo.unicode_json_source(s ${item.type},j NVARCHAR(MAX),p NVARCHAR(100),f NVARCHAR(10))`)
    await query(c,"INSERT INTO dbo.unicode_json_source VALUES(@s,@j,N'$.s',N'json')",[
      ['s',TYPES.NVarChar,captured('source'),{length:4000}],
      ['j',TYPES.NVarChar,captured('document'),{length:4000}],
    ])
    for (const operation of item.operations.filter(op => ['OPENJSON','OPENJSON WITH'].includes(op.operation))) {
      await compareRows(operation)
    }
  }
  for (const item of fixture.escaped.filter(item => item.sample !== 'invalid trailing')) {
    await query(c,item.setup)
    for (const operation of item.operations.filter(op => ['OPENJSON','OPENJSON WITH'].includes(op.operation))) {
      await compareRows(operation)
    }
  }
  for (const item of fixture.keys) {
    const key = item.operations.find(op => op.operation === 'key source').result.sets[0].rows[0][0]
    const escaped = text => '"'+text.replace(/["\\\x00-\x1f]/g,c => JSON.stringify(c).slice(1,-1))+'"'
    await query(c,'UPDATE dbo.unicode_json_source SET j=@j',[
      ['j',TYPES.NVarChar,key === null ? null : `{${escaped(key)}:7}`,{length:4000}],
    ])
    await compareRows(item.operations.find(op => op.operation === 'OPENJSON key'))
  }
})

test('JSON document construction from stored Unicode values completes inside UPDATE', async t => {
  const { readFile } = await import('node:fs/promises')
  const fixture = JSON.parse(await readFile(new URL('../reference/unicode-json-storage.json',import.meta.url),'utf8'))
  const c = await start(t)
  for (const item of fixture.results) {
    await query(c,item.setup)
    const document = item.operations.find(op => op.operation === 'document')
    const expected = document.result.sets[0].rows.map(row => row.map(value =>
      value?.kind === 'binary' ? Buffer.from(value.value,'hex') : value))
    assert.deepEqual((await query(c,document.sql)).rows,expected,`${item.type}: ${item.sample}`)
  }
})
