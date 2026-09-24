import assert from 'node:assert/strict';
import {readFile,mkdir,writeFile} from 'node:fs/promises';
import {test} from 'node:test';
import {start,query} from './support/client.mjs';
import {capture,canonical,differences} from '../scripts/lib/compatibility.mjs';
const fixture=JSON.parse(await readFile(new URL('./reference/checked-projection.json',import.meta.url),'utf8'));
test('checked SELECT projections retain exact reference metadata rows diagnostics and transaction state',async t=>{
 const records=[];
 for(const probe of fixture.cases)await t.test(probe.id,async t=>{
  const c=await start(t,{options:{requestTimeout:15000}});
  await query(c,probe.setup);
  const actual=canonical(await capture(c,probe.sql));
  const followup=canonical(await capture(c,probe.followup.sql));
  const record={id:probe.id,sql:probe.sql,expected:probe.result,actual,followup:{sql:probe.followup.sql,expected:probe.followup.result,actual:followup},differences:[...differences(actual,probe.result),...differences(followup,probe.followup.result,'/followup')]};
  records.push(record);
  assert.equal(record.differences.length,0,`${probe.id}: exact reference differences remain`);
 });
 await mkdir('artifacts/compatibility',{recursive:true});
 await writeFile('artifacts/compatibility/checked-projection.json',JSON.stringify(records,null,2)+'\n');
});
