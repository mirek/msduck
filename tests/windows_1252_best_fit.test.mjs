import assert from 'node:assert/strict'
import { readFile, mkdir, writeFile } from 'node:fs/promises'
import { test } from 'node:test'
import { TYPES } from 'tedious'
import { start, query } from './support/client.mjs'
import { capture, canonical, differences } from '../scripts/lib/compatibility.mjs'

const fixture=JSON.parse(await readFile(new URL('../reference/windows-1252-best-fit.json',import.meta.url),'utf8'))
test('SQL best-fit casts and assignments preserve captured characters and check original widths',async t=>{
  const c=await start(t)
  assert.deepEqual((await query(c,'SELECT CAST(@s AS VARCHAR(4)),TRY_CAST(@s AS CHAR(4))',[['s',TYPES.NVarChar,'Ā🦆']])).rows,[['A??','A?? ']])
  await query(c,'CREATE TABLE dbo.best_fit_text(v VARCHAR(3)); INSERT INTO dbo.best_fit_text VALUES(@s)',[['s',TYPES.NVarChar,'Ā🦆']])
  assert.deepEqual((await query(c,'SELECT v FROM dbo.best_fit_text')).rows,[['A??']])
  await query(c,'CREATE TABLE dbo.best_fit_width(v VARCHAR(1))')
  await assert.rejects(query(c,'INSERT INTO dbo.best_fit_width VALUES(@s)',[['s',TYPES.NVarChar,'a\u2000\u2000']]),/truncated/)
  assert.deepEqual((await query(c,'SELECT COUNT(*) FROM dbo.best_fit_width')).rows,[[0]])
  await query(c,'INSERT INTO dbo.best_fit_width VALUES(@s)',[['s',TYPES.NVarChar,'a  ']])
  assert.deepEqual((await query(c,'SELECT v FROM dbo.best_fit_width')).rows,[['a']])
})

test('SQL CP1252 conversion covers every UTF16 code unit through parameters and storage',async t=>{
  const c=await start(t)
  const units=Buffer.alloc(65536*2)
  for(let u=0;u<65536;u++) units.writeUInt16LE(u,u*2)
  const source=units.toString('utf16le')
  const expected=Buffer.from(fixture.maps[0].hex,'hex')
  for(const map of fixture.maps) assert.equal(map.hex,fixture.maps[0].hex)
  const result=await query(c,'SELECT CONVERT(VARBINARY(MAX),CAST(@s AS VARCHAR(MAX)))',[['s',TYPES.NVarChar,source,{length:Infinity}]])
  await mkdir('artifacts/compatibility',{recursive:true})
  await writeFile('artifacts/compatibility/windows-1252-all-units.json',JSON.stringify({actual:canonical(result.rows),expected:canonical([[expected]])},null,2)+'\n')
  assert.ok(Buffer.isBuffer(result.rows[0][0]) && result.rows[0][0].equals(expected),'complete conversion differs; see raw all-unit artifact')
  await query(c,'CREATE TABLE dbo.best_fit_units(s VARCHAR(MAX)); INSERT INTO dbo.best_fit_units VALUES(@s)',[['s',TYPES.NVarChar,source,{length:Infinity}]])
  assert.ok((await query(c,'SELECT CONVERT(VARBINARY(MAX),s) FROM dbo.best_fit_units')).rows[0][0].equals(expected))
})

test('SQL CP1252 CAST and storage boundaries retain complete reference results',async t=>{
  const c=await start(t)
  const records=[]
  for(const probe of fixture.probes) {
    const cast=canonical(await capture(c,probe.cast.sql))
    await query(c,`DROP TABLE IF EXISTS dbo.best_fit_target; CREATE TABLE dbo.best_fit_target(s ${probe.declaration})`)
    const storage=canonical(await capture(c,probe.storage.sql))
    records.push({sourceHex:probe.sourceHex,declaration:probe.declaration,cast,storage,
      differences:[...differences(cast,probe.cast.result,'/cast'),...differences(storage,probe.storage.result,'/storage')]})
  }
  await mkdir('artifacts/compatibility',{recursive:true})
  await writeFile('artifacts/compatibility/windows-1252-boundaries.json',JSON.stringify(records,null,2)+'\n')
  assert.equal(records.reduce((n,r)=>n+r.differences.length,0),0,'complete reference differences remain; see windows-1252-boundaries.json')
})
