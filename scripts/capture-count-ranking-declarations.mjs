#!/usr/bin/env node
// SQL/reference errors are data. Reuses first-party bounded raw wire capture.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFile,mkdir} from 'node:fs/promises';
import {resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import {captureBatch,captureRpc,observe,validate as validateBaseline,decodeOrder} from './capture-order-token.mjs';
import {withReferenceContainer,referenceImage} from './lib/reference-container.mjs';
import {isolatedReference,assertSameCapture,refuseExistingFixture,writeNewFixture} from './lib/reference.mjs';
import {canonical} from './lib/compatibility.mjs';
const fixture=new URL('../reference/count-ranking-declarations.json',import.meta.url);
export const fixtureSha256='90e58f096efc179289404dfb29af02323ca6f44c1ba6745b704da59629fe2738';
export const queries=[
  [
    "count star",
    "SELECT COUNT(*) AS c FROM dbo.order_heap"
  ],
  [
    "count column",
    "SELECT COUNT(label) AS c FROM dbo.order_heap"
  ],
  [
    "count big star",
    "SELECT COUNT_BIG(*) AS c FROM dbo.order_heap"
  ],
  [
    "count big column",
    "SELECT COUNT_BIG(label) AS c FROM dbo.order_heap"
  ],
  [
    "count distinct",
    "SELECT COUNT(DISTINCT b) AS c FROM dbo.order_heap"
  ],
  [
    "count big distinct",
    "SELECT COUNT_BIG(DISTINCT b) AS c FROM dbo.order_heap"
  ],
  [
    "count literal",
    "SELECT COUNT(1) AS c FROM dbo.order_heap"
  ],
  [
    "count NULL",
    "SELECT COUNT(NULL) AS c FROM dbo.order_heap"
  ],
  [
    "count typed NULL",
    "SELECT COUNT(CAST(NULL AS INT)) AS c FROM dbo.order_heap"
  ],
  [
    "count missing",
    "SELECT COUNT(missing) AS c FROM dbo.order_heap"
  ],
  [
    "count missing table",
    "SELECT COUNT(*) AS c FROM missing_table"
  ],
  [
    "count empty",
    "SELECT COUNT(*) AS c,COUNT_BIG(label) AS d FROM dbo.order_heap WHERE 1=0"
  ],
  [
    "count grouped",
    "SELECT b,COUNT(*) AS c,COUNT_BIG(label) AS d FROM dbo.order_heap GROUP BY b ORDER BY b"
  ],
  [
    "count grouped empty",
    "SELECT b,COUNT(*) AS c FROM dbo.order_heap WHERE 1=0 GROUP BY b ORDER BY b"
  ],
  [
    "count window",
    "SELECT a,COUNT(*) OVER(PARTITION BY b) AS c,COUNT_BIG(label) OVER(PARTITION BY b) AS d FROM dbo.order_heap ORDER BY a"
  ],
  [
    "count derived",
    "SELECT c,c+1 AS next FROM(SELECT COUNT(*) AS c FROM dbo.order_heap)q"
  ],
  [
    "count big CTE",
    "WITH q AS(SELECT COUNT_BIG(*) AS c FROM dbo.order_heap) SELECT c,c+1 AS next FROM q"
  ],
  [
    "count division",
    "SELECT c/CAST(3 AS DECIMAL(4,2)) AS d FROM(SELECT COUNT(*) AS c FROM dbo.order_heap)q"
  ],
  [
    "count big division",
    "SELECT c/CAST(3 AS DECIMAL(4,2)) AS d FROM(SELECT COUNT_BIG(*) AS c FROM dbo.order_heap)q"
  ],
  [
    "row number",
    "SELECT a,ROW_NUMBER() OVER(ORDER BY a) AS r FROM dbo.order_heap ORDER BY a"
  ],
  [
    "row number empty",
    "SELECT a,ROW_NUMBER() OVER(ORDER BY a) AS r FROM dbo.order_heap WHERE 1=0 ORDER BY a"
  ],
  [
    "row number partition",
    "SELECT a,ROW_NUMBER() OVER(PARTITION BY b ORDER BY a) AS r FROM dbo.order_heap ORDER BY a"
  ],
  [
    "row number derived",
    "SELECT r,r+1 AS next FROM(SELECT ROW_NUMBER() OVER(ORDER BY a) AS r FROM dbo.order_heap)q ORDER BY r"
  ],
  [
    "row number CTE",
    "WITH q AS(SELECT ROW_NUMBER() OVER(ORDER BY a) AS r FROM dbo.order_heap) SELECT r FROM q ORDER BY r"
  ],
  [
    "row number missing",
    "SELECT ROW_NUMBER() OVER(ORDER BY missing) AS r FROM dbo.order_heap"
  ],
  [
    "row number no order",
    "SELECT ROW_NUMBER() OVER() AS r FROM dbo.order_heap"
  ],
  [
    "row number args",
    "SELECT ROW_NUMBER(a) OVER(ORDER BY a) AS r FROM dbo.order_heap"
  ],
  [
    "row number constant",
    "SELECT ROW_NUMBER() OVER(ORDER BY (SELECT NULL)) AS r FROM dbo.order_heap ORDER BY r"
  ],
  [
    "prepared count",
    "DECLARE @h INT; EXEC sys.sp_prepare @h OUTPUT,N'@p INT',N'SELECT COUNT(@p) AS c,COUNT_BIG(@p) AS d FROM dbo.order_heap WHERE a>@p',1; EXEC sys.sp_execute @h,0; EXEC sys.sp_execute @h,9; EXEC sys.sp_execute @h,1; EXEC sys.sp_unprepare @h"
  ],
  [
    "prepared row number",
    "DECLARE @h INT; EXEC sys.sp_prepare @h OUTPUT,N'@p INT',N'SELECT ROW_NUMBER() OVER(ORDER BY a) AS r FROM dbo.order_heap WHERE a>@p ORDER BY r',1; EXEC sys.sp_execute @h,0; EXEC sys.sp_execute @h,9; EXEC sys.sp_execute @h,1; EXEC sys.sp_unprepare @h"
  ]
];
export async function observeDeclarations(connection) {
 const baseline=await observe(connection);validateBaseline(baseline);
 const records=baseline.slice(0,2);
 for(const [name,sql] of queries)for(const mode of ['batch','rpc']) {
  const result=canonical(await(mode==='batch'?captureBatch(connection,sql):captureRpc(connection,sql)));
  records.push({name,sql,mode,result});
  console.log('declarations',name,mode,JSON.stringify(result.events.filter(e=>e.kind==='ORDER').map(e=>e.ordinals)),JSON.stringify(result.errors.map(e=>e.number)));
 }
 return records;
}
export function validate(run) {
 assert.equal(run.length,2+2*queries.length);
 assertSameCapture(run.slice(2).map(({name,sql,mode})=>({name,sql,mode})),queries.flatMap(([name,sql])=>['batch','rpc'].map(mode=>({name,sql,mode}))),'COUNT/ranking request plan');
 assertSameCapture(run[0].result.sets[0].rows,[['17.0.4065.4']],'reference version');
 assert.equal(run[1].result.errors.length,0,'setup');
 for(const {result} of run){
  assert(result.done.length>0);assert.equal(result.done.length,result.doneTokens.length);
  for(const token of result.doneTokens){
   assert.equal(token.more,Boolean(token.status&1));assert.equal(token.sqlError,Boolean(token.status&2));
   assert.equal(token.attention,Boolean(token.status&32));assert.equal(token.serverError,Boolean(token.status&256));assert.equal(token.rowCount===null,!(token.status&16));
  }
  for(const event of result.events)if(event.kind==='ORDER')assertSameCapture(decodeOrder(Buffer.from(event.hex,'hex')),{hex:event.hex,length:event.length,ordinals:event.ordinals},'COUNT/ranking raw ORDER');
 }
}
export async function retained(){
 const bytes=await readFile(fixture);assert.equal(createHash('sha256').update(bytes).digest('hex'),fixtureSha256);
 const result=JSON.parse(bytes);assert.equal(result.image,referenceImage);assert.equal(result.runs.length,2);
 result.runs.forEach(validate);assertSameCapture(result.runs[0],result.runs[1],'COUNT/ranking fresh runs');return result;
}
// Supplemental capture for the metadata option omitted by tedious.prepare().
export async function observeDefaultPreparation(connection) {
 const setup="CREATE TABLE dbo.order_heap(a INT,b INT,label NVARCHAR(8)); INSERT INTO dbo.order_heap VALUES(3,1,N'c'),(1,2,N'a'),(2,1,NULL),(4,2,N'd')";
 assert.deepEqual((await captureBatch(connection,setup)).errors,[]);
 const records=[];
 for(const [name,sql] of [
  ['prepared count','SELECT COUNT(@p) AS c,COUNT_BIG(@p) AS d FROM dbo.order_heap WHERE a>@p'],
  ['prepared row number','SELECT ROW_NUMBER() OVER(ORDER BY a) AS r FROM dbo.order_heap WHERE a>@p ORDER BY r'],
 ])for(const option of ['',',0'])for(const mode of ['batch','rpc']){
  const batch=`DECLARE @h INT; EXEC sys.sp_prepare @h OUTPUT,N'@p INT',N'${sql}'${option}; EXEC sys.sp_execute @h,0; EXEC sys.sp_execute @h,9; EXEC sys.sp_execute @h,1; EXEC sys.sp_unprepare @h`;
  const result=canonical(await(mode==='batch'?captureBatch(connection,batch):captureRpc(connection,batch)));
  assert.deepEqual(result.errors,[]);assert.equal(result.sets.length,3);
  records.push({name,option:option?'zero':'omitted',mode,sql:batch,result});
 }
 return records;
}
async function main(){
 const args=process.argv.slice(2);const mode=args[0]?.startsWith('--')?args.shift():undefined;
 if(![undefined,'--check','--write-fixture','--default-prepare'].includes(mode)||args.length>1)throw Error('usage: capture-count-ranking-declarations.mjs [--check | --write-fixture] [output]');
 if(mode==='--check'){const r=await retained();console.log('Checked COUNT/ranking records',r.runs[0].length);return;}
 if(mode==='--write-fixture')await refuseExistingFixture(fixture);
 const output=resolve(args[0]??'artifacts/count-ranking-declarations/capture.json');assert.notEqual(output,fileURLToPath(fixture));
 const observer=mode==='--default-prepare'?observeDefaultPreparation:observeDeclarations;
 const runs=[];for(let i=0;i<2;i++)runs.push(await withReferenceContainer(config=>isolatedReference(config,observer)));
 if(mode!=='--default-prepare')runs.forEach(validate);assertSameCapture(runs[0],runs[1],'COUNT/ranking fresh runs');
 const result={image:referenceImage,runs};await mkdir(dirname(output),{recursive:true});await writeNewFixture(output,result);
 if(mode==='--write-fixture')await writeNewFixture(fixture,result);
 console.log('Captured COUNT/ranking records',runs[0].length);
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await main();
