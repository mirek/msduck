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
const fixture=new URL('../reference/order-token-expanded.json',import.meta.url);
export const fixtureSha256='d91c7525cc15bf5797c31786733a5ce1e92362e395f7624868f1e55d258eba0b';
export const queries=[
  [
    "union same",
    "SELECT 1 AS a UNION ALL SELECT 1 ORDER BY a"
  ],
  [
    "union distinct same",
    "SELECT 1 AS a UNION SELECT 1 ORDER BY a"
  ],
  [
    "union NULL",
    "SELECT CAST(NULL AS INT) AS a UNION ALL SELECT CAST(NULL AS INT) ORDER BY a"
  ],
  [
    "union mixed",
    "SELECT CAST(NULL AS INT) AS a UNION ALL SELECT 1 ORDER BY a"
  ],
  [
    "join equivalent",
    "SELECT h.a FROM dbo.order_heap h JOIN dbo.order_clustered c ON h.a=c.a ORDER BY c.a"
  ],
  [
    "join own",
    "SELECT h.a FROM dbo.order_heap h JOIN dbo.order_clustered c ON h.a=c.a ORDER BY h.a"
  ],
  [
    "join both",
    "SELECT h.a,c.a FROM dbo.order_heap h JOIN dbo.order_clustered c ON h.a=c.a ORDER BY c.a"
  ],
  [
    "join inequality",
    "SELECT h.a FROM dbo.order_heap h JOIN dbo.order_clustered c ON h.a<c.a ORDER BY c.a"
  ],
  [
    "join left",
    "SELECT h.a FROM dbo.order_heap h LEFT JOIN dbo.order_clustered c ON h.a=c.a ORDER BY c.a"
  ],
  [
    "fixed key",
    "SELECT a FROM dbo.order_heap WHERE a=1 ORDER BY a"
  ],
  [
    "TOP one",
    "SELECT TOP(1) a FROM dbo.order_heap ORDER BY a"
  ],
  [
    "NULL empty",
    "SELECT CAST(NULL AS INT) AS k FROM dbo.order_heap WHERE 1=0 ORDER BY k"
  ],
  [
    "NULL TOP zero",
    "SELECT TOP(0) CAST(NULL AS INT) AS k FROM dbo.order_heap ORDER BY k"
  ],
  [
    "NULL scalar",
    "SELECT CAST(NULL AS INT) AS k ORDER BY k"
  ],
  [
    "NULL mixed",
    "SELECT a,CAST(NULL AS INT) AS k FROM dbo.order_heap ORDER BY k"
  ],
  [
    "row number partition",
    "SELECT a,ROW_NUMBER() OVER(PARTITION BY b ORDER BY a) AS n FROM dbo.order_heap ORDER BY n"
  ],
  [
    "row number empty",
    "SELECT a,ROW_NUMBER() OVER(PARTITION BY b ORDER BY b,a) AS n FROM dbo.order_heap WHERE 1=0 ORDER BY n"
  ],
  [
    "duplicate bare",
    "SELECT a,a FROM dbo.order_heap ORDER BY a"
  ],
  [
    "wildcard",
    "SELECT * FROM dbo.order_heap ORDER BY b,a"
  ],
  [
    "qualified wildcard",
    "SELECT h.* FROM dbo.order_heap h ORDER BY h.b,h.a"
  ],
  [
    "mixed wildcard",
    "SELECT b,* FROM dbo.order_heap ORDER BY 2"
  ],
  [
    "joined wildcard",
    "SELECT h.*,c.b AS cb FROM dbo.order_heap h JOIN dbo.order_clustered c ON h.a=c.a ORDER BY c.b,h.a"
  ],
  [
    "derived wildcard",
    "SELECT * FROM (SELECT b,a FROM dbo.order_heap)q ORDER BY a"
  ],
  [
    "plus two",
    "SELECT a+2 AS k FROM dbo.order_heap ORDER BY k"
  ],
  [
    "multiply",
    "SELECT a*2 AS k FROM dbo.order_heap ORDER BY a*2"
  ],
  [
    "cast",
    "SELECT CAST(a AS BIGINT) AS k FROM dbo.order_heap ORDER BY k"
  ],
  [
    "aggregate alias",
    "SELECT b,COUNT(*) AS n FROM dbo.order_heap GROUP BY b ORDER BY n"
  ],
  [
    "sum alias",
    "SELECT b,SUM(a) AS n FROM dbo.order_heap GROUP BY b ORDER BY n"
  ],
  [
    "NULL numeric ordinal",
    "SELECT CAST(NULL AS INT) AS k FROM dbo.order_heap ORDER BY 1"
  ],
  [
    "constant alias",
    "SELECT 1 AS k FROM dbo.order_heap ORDER BY k"
  ],
  [
    "constant numeric ordinal",
    "SELECT 1 AS k FROM dbo.order_heap ORDER BY 1"
  ],
  [
    "NULL then column",
    "SELECT a,CAST(NULL AS INT) AS k FROM dbo.order_heap ORDER BY k,a"
  ],
  [
    "column then NULL",
    "SELECT a,CAST(NULL AS INT) AS k FROM dbo.order_heap ORDER BY a,k"
  ],
  [
    "two NULL keys",
    "SELECT CAST(NULL AS INT) AS x,CAST(NULL AS INT) AS y FROM dbo.order_heap ORDER BY x,y"
  ],
  [
    "literal key",
    "SELECT a FROM dbo.order_heap ORDER BY 1+0"
  ],
  [
    "scalar folded arithmetic",
    "SELECT 1+2 AS k FROM dbo.order_heap ORDER BY k"
  ],
  [
    "CASE constant",
    "SELECT CASE WHEN a>0 THEN 1 ELSE 1 END AS k FROM dbo.order_heap ORDER BY k"
  ],
  [
    "parameter key",
    "DECLARE @k INT=1; SELECT a FROM dbo.order_heap ORDER BY @k"
  ],
  [
    "empty arithmetic",
    "SELECT a+2 AS k FROM dbo.order_heap WHERE 1=0 ORDER BY k"
  ],
  [
    "distinct wildcard",
    "SELECT DISTINCT * FROM dbo.order_heap ORDER BY b,a"
  ],
  [
    "CTE wildcard",
    "WITH q AS(SELECT b,a FROM dbo.order_heap) SELECT * FROM q ORDER BY a"
  ],
  [
    "UNION wildcard",
    "SELECT * FROM dbo.order_heap UNION ALL SELECT * FROM dbo.order_heap ORDER BY b,a"
  ],
  [
    "hidden aggregate",
    "SELECT b FROM dbo.order_heap GROUP BY b ORDER BY COUNT(*)"
  ],
  [
    "projected collate",
    "SELECT label COLLATE Latin1_General_100_BIN2 AS k FROM dbo.order_heap ORDER BY k"
  ],
  [
    "hidden collate",
    "SELECT a FROM dbo.order_heap ORDER BY label COLLATE Latin1_General_100_BIN2"
  ],
  [
    "row_number constant",
    "SELECT ROW_NUMBER() OVER(ORDER BY (SELECT NULL)) AS n FROM dbo.order_heap ORDER BY n"
  ],
  [
    "prepared projected expression",
    "DECLARE @h INT; EXEC sys.sp_prepare @h OUTPUT,N'@b INT',N'SELECT a+@b AS k,b FROM dbo.order_heap WHERE a>@b ORDER BY k',1; EXEC sys.sp_execute @h,0; EXEC sys.sp_execute @h,9; EXEC sys.sp_execute @h,1; EXEC sys.sp_unprepare @h"
  ],
  [
    "prepared hidden expression",
    "DECLARE @h INT; EXEC sys.sp_prepare @h OUTPUT,N'@b INT',N'SELECT b FROM dbo.order_heap WHERE a>@b ORDER BY a+@b',1; EXEC sys.sp_execute @h,0; EXEC sys.sp_execute @h,9; EXEC sys.sp_execute @h,1; EXEC sys.sp_unprepare @h"
  ]
];

export async function expanded(connection) {
 const baseline=await observe(connection);validateBaseline(baseline);
 const records=baseline.slice(0,2);
 for(const [name,sql] of queries)for(const mode of ['batch','rpc']) {
  const result=canonical(await(mode==='batch'?captureBatch(connection,sql):captureRpc(connection,sql)));
  records.push({name,sql,mode,result});
  console.log('expanded',name,mode,JSON.stringify(result.events.filter(e=>e.kind==='ORDER').map(e=>e.ordinals)),JSON.stringify(result.errors.map(e=>e.number)));
 }
 return records;
}
export function validate(run) {
 assert.equal(run.length,2+2*queries.length);
 assertSameCapture(run.slice(2).map(({name,sql,mode})=>({name,sql,mode})),queries.flatMap(([name,sql])=>['batch','rpc'].map(mode=>({name,sql,mode}))),'expanded request plan');
 assertSameCapture(run[0].result.sets[0].rows,[['17.0.4065.4']],'reference version');
 assert.equal(run[1].result.errors.length,0,'setup');
 for(const {result} of run){
  assert(result.done.length>0);assert.equal(result.done.length,result.doneTokens.length);
  for(const token of result.doneTokens){
   assert.equal(token.more,Boolean(token.status&1));assert.equal(token.sqlError,Boolean(token.status&2));
   assert.equal(token.attention,Boolean(token.status&32));assert.equal(token.serverError,Boolean(token.status&256));assert.equal(token.rowCount===null,!(token.status&16));
  }
  for(const event of result.events)if(event.kind==='ORDER')assertSameCapture(decodeOrder(Buffer.from(event.hex,'hex')),{hex:event.hex,length:event.length,ordinals:event.ordinals},'expanded raw ORDER');
 }
}
export async function retained(){
 const bytes=await readFile(fixture);assert.equal(createHash('sha256').update(bytes).digest('hex'),fixtureSha256);
 const result=JSON.parse(bytes);assert.equal(result.image,referenceImage);assert.equal(result.runs.length,2);
 result.runs.forEach(validate);assertSameCapture(result.runs[0],result.runs[1],'expanded fresh runs');return result;
}
async function main(){
 const args=process.argv.slice(2);const mode=args[0]?.startsWith('--')?args.shift():undefined;
 if(![undefined,'--check','--write-fixture'].includes(mode)||args.length>1)throw Error('usage: capture-order-token-expanded.mjs [--check | --write-fixture] [output]');
 if(mode==='--check'){const r=await retained();console.log('Checked expanded ORDER records',r.runs[0].length);return;}
 if(mode==='--write-fixture')await refuseExistingFixture(fixture);
 const output=resolve(args[0]??'artifacts/order-token-expanded/capture.json');assert.notEqual(output,fileURLToPath(fixture));
 const runs=[];for(let i=0;i<2;i++)runs.push(await withReferenceContainer(config=>isolatedReference(config,expanded)));
 runs.forEach(validate);assertSameCapture(runs[0],runs[1],'expanded fresh runs');
 const result={image:referenceImage,runs};await mkdir(dirname(output),{recursive:true});await writeNewFixture(output,result);
 if(mode==='--write-fixture')await writeNewFixture(fixture,result);
 console.log('Captured expanded ORDER records',runs[0].length);
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await main();
