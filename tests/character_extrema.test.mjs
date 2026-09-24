import assert from 'node:assert/strict'
import { readFile, mkdir, writeFile } from 'node:fs/promises'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { start, query } from './support/client.mjs'
import { capture, canonical, differences } from '../scripts/lib/compatibility.mjs'

const fixture = JSON.parse(await readFile(new URL('../reference/character-extrema.json', import.meta.url), 'utf8'))
for (const sample of fixture.results.filter(s => s.collation === 'Latin1_General_100_BIN2')) {
  test(`BIN2 character extrema preserve captured ${sample.declaration} values and metadata`, async t => {
    const c = await start(t)
    assert.deepEqual((await capture(c, sample.setup)).errors, [])
    const results=[]
    for (const operation of sample.operations) {
      const actual = canonical(await capture(c, operation.sql))
      results.push({operation:operation.operation,sql:operation.sql,actual,expected:operation.result,differences:differences(actual,operation.result)})
    }
    await mkdir('artifacts/compatibility',{recursive:true})
    await writeFile(`artifacts/compatibility/character-extrema-${sample.declaration.replaceAll(/[()]/g,'-')}.json`,JSON.stringify(results,null,2)+'\n')
    assert.deepEqual(results.flatMap(r=>r.differences.map(d=>({operation:r.operation,...d}))),[])
  })
}

test('BIN2 extrema keep scoped metadata, casts, prepared values and validation', async t => {
  const c = await start(t)
  await query(c, 'CREATE TABLE dbo.extrema(s NVARCHAR(8) COLLATE Latin1_General_100_BIN2); INSERT INTO dbo.extrema VALUES (@a),(@b)', [
    ['a', TYPES.NVarChar, 'ÿ'], ['b', TYPES.NVarChar, 'Ā'],
  ])
  for (const sql of [
    'SELECT MIN(s),MAX(s) FROM dbo.extrema',
    'WITH q(v) AS (SELECT s FROM dbo.extrema) SELECT MIN(v),MAX(v) FROM q',
    'SELECT MIN(q.v),MAX(q.v) FROM (SELECT s AS v FROM dbo.extrema) q',
    'SELECT (SELECT MIN(s) FROM dbo.extrema),(SELECT MAX(s) FROM dbo.extrema)',
    'SELECT CAST(MIN(s) AS NVARCHAR(1)),CAST(MAX(s) AS NVARCHAR(1)) FROM dbo.extrema',
  ]) assert.deepEqual((await query(c, sql)).rows, [['ÿ', 'Ā']], sql)
  assert.deepEqual((await query(c, 'SELECT MAX(@p COLLATE Latin1_General_100_BIN2)', [['p', TYPES.NVarChar, '\ud83e']])).rows, [['\ud83e']])
  await assert.rejects(query(c, 'SELECT MAX(DISTINCT s) OVER() FROM dbo.extrema'), e => e.number === 10759)
  await assert.rejects(query(c, 'SELECT MAX(MIN(s)) FROM dbo.extrema'), e => e.number === 130)
  assert.deepEqual((await query(c, 'SELECT MIN(s),MAX(s) FROM dbo.extrema WHERE 1=0')).rows, [[null,null]])
})

test('BIN2 ANSI extrema compare CP1252 bytes and preserve fixed padding', async t => {
  const c=await start(t)
  for(const declaration of ['VARCHAR(8)','CHAR(8)']) {
    await query(c,`DROP TABLE IF EXISTS dbo.ansi_extrema; CREATE TABLE dbo.ansi_extrema(s ${declaration} COLLATE Latin1_General_100_BIN2); INSERT INTO dbo.ansi_extrema VALUES(N'€'),(N' '),(NULL)`)
    const pad=declaration.startsWith('CHAR') ? ' '.repeat(7) : ''
    assert.deepEqual((await query(c,'SELECT MIN(s),MAX(s),CAST(MIN(s) AS NVARCHAR(8)) FROM dbo.ansi_extrema')).rows,[['€'+pad,' '+pad,'€'+pad]])
    assert.deepEqual((await query(c,'SELECT MIN(s),MAX(s) FROM dbo.ansi_extrema WHERE 1=0')).rows,[[null,null]])
  }
})
