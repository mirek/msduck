import assert from 'node:assert/strict'
import {readFile,mkdir,writeFile} from 'node:fs/promises'
import {test} from 'node:test'
import {start,query} from './support/client.mjs'
import {capture,canonical,differences} from '../scripts/lib/compatibility.mjs'
const fixture=JSON.parse(await readFile(new URL('./reference/storage-diagnostic.json',import.meta.url),'utf8'))
test('storage truncation preserves reference diagnostics atomicity and batch continuation',async t=>{
 const c=await start(t);const records=[]
 async function replay(connection,probe){
  if(probe.setup)await query(connection,probe.setup)
  const actual=canonical(await capture(connection,probe.sql))
  const record={id:probe.id,sql:probe.sql,expected:probe.result,actual,differences:differences(actual,probe.result)}
  if(probe.followup){
   record.followup={sql:probe.followup.sql,expected:probe.followup.result,actual:canonical(await capture(connection,probe.followup.sql))}
   record.differences.push(...differences(record.followup.actual,record.followup.expected,'/followup'))
  }
  records.push(record)
  return record
 }
 for(const probe of fixture.cases){
  if(probe.isolated)await t.test(probe.id,async isolated=>{
   const record=await replay(await start(isolated),probe)
   assert.equal(record.differences.length,0,`${probe.id}: exact reference differences remain`)
  })
  else await replay(c,probe)
 }
 await mkdir('artifacts/compatibility',{recursive:true})
 await writeFile('artifacts/compatibility/storage-diagnostic.json',JSON.stringify(records,null,2)+'\n')
 assert.equal(records.reduce((n,r)=>n+r.differences.length,0),0,'exact storage differences remain; see storage-diagnostic.json')
})
