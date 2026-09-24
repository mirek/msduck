import assert from 'node:assert/strict'
import {readFile,mkdir,writeFile} from 'node:fs/promises'
import {test} from 'node:test'
import {start} from './support/client.mjs'
import {capture,canonical,differences} from '../scripts/lib/compatibility.mjs'

const fixture=JSON.parse(await readFile(new URL('./reference/binary-unicode.json',import.meta.url),'utf8'))
test('binary-to-Unicode conversions retain captured bytes widths styles and diagnostics',async t=>{
 const c=await start(t)
 const records=[]
 for(const probe of fixture.cases) {
  const actual=canonical(await capture(c,probe.sql))
  records.push({sql:probe.sql,expected:probe.result,actual,differences:differences(actual,probe.result)})
 }
 await mkdir('artifacts/compatibility',{recursive:true})
 await writeFile('artifacts/compatibility/binary-unicode.json',JSON.stringify(records,null,2)+'\n')
 assert.equal(records.reduce((n,r)=>n+r.differences.length,0),0,'exact differences remain; see binary-unicode.json')
})
