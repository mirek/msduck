#!/usr/bin/env node
// Owner-run SQL Server reference; SQL/errors/token records are data.
import assert from 'node:assert/strict'
import {createHash} from 'node:crypto'
import {readFile,mkdir} from 'node:fs/promises'
import {resolve,dirname} from 'node:path'
import {fileURLToPath} from 'node:url'
import {captureBatch,captureRpc,decodeOrder} from './capture-order-token.mjs'
import {withReferenceContainer,referenceImage} from './lib/reference-container.mjs'
import {isolatedReference,assertSameCapture,refuseExistingFixture,writeNewFixture} from './lib/reference.mjs'
import {canonical} from './lib/compatibility.mjs'
const fixture=new URL('../reference/count-null-compilation.json',import.meta.url)
export const fixtureSha256='d08c691dab34e91de2d49aae575d91976419cf666a394b0509fb47ee14e5f530'
const version="SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS v"
const rows="INSERT INTO dbo.count_null_heap VALUES(3,1,N'c'),(1,2,N'a'),(2,1,NULL),(4,2,N'd')"
const setup='CREATE TABLE dbo.count_null_heap(a INT,b INT,label NVARCHAR(8)); '+rows
const reset='DELETE FROM dbo.count_null_heap; '+rows
export const followup='SELECT COUNT(*) AS n,COUNT(label) AS labels FROM dbo.count_null_heap; SELECT @@TRANCOUNT AS depth; SELECT 42 AS alive'
const heap='FROM dbo.count_null_heap'
export const queries=[
 ['count NULL',`SELECT COUNT(NULL) AS c ${heap}`],
 ['count big NULL',`SELECT COUNT_BIG(NULL) AS c ${heap}`],
 ['count parenthesized NULL',`SELECT COUNT(((NULL))) AS c ${heap}`],
 ['count distinct NULL',`SELECT COUNT(DISTINCT NULL) AS c ${heap}`],
 ['count big distinct NULL',`SELECT COUNT_BIG(DISTINCT NULL) AS c ${heap}`],
 ['count NULL no source','SELECT COUNT(NULL) AS c'],
 ['count NULL empty',`SELECT COUNT(NULL) AS c ${heap} WHERE 1=0`],
 ['count NULL window',`SELECT a,COUNT(NULL) OVER(PARTITION BY b) AS c ${heap} ORDER BY a`],
 ['count big NULL frame',`SELECT a,COUNT_BIG(NULL) OVER(ORDER BY a ROWS UNBOUNDED PRECEDING) AS c ${heap} ORDER BY a`],
 ['count typed NULL',`SELECT COUNT(CAST(NULL AS INT)) AS c ${heap}`],
 ['count big typed NULL',`SELECT COUNT_BIG(CAST(NULL AS BIGINT)) AS c ${heap}`],
 ['count converted NULL',`SELECT COUNT(CONVERT(INT,NULL)) AS c ${heap}`],
 ['count character NULL',`SELECT COUNT(CAST(NULL AS NVARCHAR(8))) AS c ${heap}`],
 ['count NULL plus one',`SELECT COUNT(NULL+1) AS c ${heap}`],
 ['count unary NULL',`SELECT COUNT(+NULL) AS c ${heap}`],
 ['count all NULL CASE',`SELECT COUNT(CASE WHEN a=1 THEN NULL ELSE NULL END) AS c ${heap}`],
 ['count scalar subquery NULL',`SELECT COUNT((SELECT NULL)) AS c ${heap}`],
 ['count nested aggregate NULL',`SELECT COUNT(SUM(NULL)) AS c ${heap}`],
 ['sum nested count NULL',`SELECT SUM(COUNT(NULL)) AS c ${heap}`],
 ['count NULL missing table','SELECT COUNT(NULL) AS c FROM count_null_missing'],
 ['count NULL missing predicate',`SELECT COUNT(NULL) AS c ${heap} WHERE missing=1`],
 ['count declared INT NULL',`DECLARE @p INT=NULL; SELECT COUNT(@p) AS c,COUNT_BIG(@p) AS d ${heap}`],
 ['count declared BIGINT NULL',`DECLARE @p BIGINT=NULL; SELECT COUNT(@p) AS c,COUNT_BIG(@p) AS d ${heap}`],
 ['count declared character NULL',`DECLARE @p NVARCHAR(8)=NULL; SELECT COUNT(@p) AS c,COUNT_BIG(@p) AS d ${heap}`],
 ['count NULL prior result',`SELECT 7 AS before_value; SELECT COUNT(NULL) AS c ${heap}; SELECT 9 AS after_value`],
 ['count NULL prior write',`INSERT INTO dbo.count_null_heap VALUES(5,3,NULL); SELECT COUNT(NULL) AS c ${heap}`],
 ['typed NULL prior write',`INSERT INTO dbo.count_null_heap VALUES(5,3,NULL); SELECT COUNT(CAST(NULL AS INT)) AS c ${heap}`],
 ['count NULL unreachable',`IF 1=0 SELECT COUNT(NULL) AS c ${heap}; SELECT 9 AS alive`],
 ['count NULL after RETURN',`RETURN; SELECT COUNT(NULL) AS c ${heap}`],
 ['count NULL same scope catch',`BEGIN TRY SELECT COUNT(NULL) AS c ${heap}; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n,ERROR_STATE() AS s,ERROR_SEVERITY() AS severity,ERROR_MESSAGE() AS message; END CATCH`],
 ['count NULL dynamic catch',`BEGIN TRY EXEC sys.sp_executesql N'SELECT COUNT(NULL) AS c ${heap}'; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n,ERROR_STATE() AS s,ERROR_SEVERITY() AS severity,ERROR_MESSAGE() AS message; END CATCH`],
 ['count NULL update aggregate','UPDATE dbo.count_null_heap SET a=COUNT(NULL)'],
 ['prepare count NULL',`DECLARE @h INT; EXEC sys.sp_prepare @h OUTPUT,N'',N'SELECT COUNT(NULL) AS c ${heap}',1; SELECT @h AS handle`],
 ['prepare count big NULL',`DECLARE @h INT; EXEC sys.sp_prepare @h OUTPUT,N'',N'SELECT COUNT_BIG(NULL) AS c ${heap}',1; SELECT @h AS handle`],
 ['prepare declared NULL',`DECLARE @h INT; EXEC sys.sp_prepare @h OUTPUT,N'@p INT',N'SELECT COUNT(@p) AS c,COUNT_BIG(@p) AS d ${heap}',1; EXEC sys.sp_execute @h,NULL; EXEC sys.sp_execute @h,0; EXEC sys.sp_unprepare @h`],
]
export async function observe(connection){
 const records=[]
 for(const [name,sql] of [['version',version],['setup',setup]]){
  const result=canonical(await captureBatch(connection,sql));assert.deepEqual(result.errors,[])
  records.push({name,mode:'batch',sql,result})
 }
 for(const [name,sql] of queries)for(const mode of ['batch','rpc']){
  const resetResult=canonical(await captureBatch(connection,reset));assert.deepEqual(resetResult.errors,[])
  const result=canonical(await(mode==='batch'?captureBatch(connection,sql):captureRpc(connection,sql)))
  const after=canonical(await captureBatch(connection,followup));assert.deepEqual(after.errors,[])
  records.push({name,mode,sql,reset:{sql:reset,result:resetResult},result,followup:{sql:followup,result:after}})
  console.log('COUNT NULL',name,mode,'errors',result.errors.map(e=>e.number),'sets',result.sets.length,'after',after.sets[0].rows)
 }
 return records
}
function validatePhase(result){
 assert(result.done.length>0);assert.equal(result.done.length,result.doneTokens.length)
 for(const token of result.doneTokens){
  assert.equal(token.more,Boolean(token.status&1));assert.equal(token.sqlError,Boolean(token.status&2))
  assert.equal(token.attention,Boolean(token.status&32));assert.equal(token.serverError,Boolean(token.status&256));assert.equal(token.rowCount===null,!(token.status&16))
 }
 for(const set of result.sets)for(const row of set.rows)assert.equal(row.length,set.columns.length)
 for(const event of result.events)if(event.kind==='ORDER')assertSameCapture(decodeOrder(Buffer.from(event.hex,'hex')),{hex:event.hex,length:event.length,ordinals:event.ordinals},'COUNT NULL ORDER wire')
}
export function validate(run){
 assert.equal(run.length,2+2*queries.length)
 assertSameCapture(run.slice(0,2).map(({name,mode,sql})=>({name,mode,sql})),[{name:'version',mode:'batch',sql:version},{name:'setup',mode:'batch',sql:setup}],'COUNT NULL setup plan')
 assertSameCapture(run.slice(2).map(({name,mode,sql})=>({name,mode,sql})),queries.flatMap(([name,sql])=>['batch','rpc'].map(mode=>({name,mode,sql}))),'COUNT NULL query plan')
 assertSameCapture(run[0].result.sets[0].rows,[['17.0.4065.4']],'COUNT NULL reference version')
 for(const [index,record] of run.entries()){
  if(index>=2)assert(record.reset&&record.followup,'missing COUNT NULL reset/follow-up phase')
  validatePhase(record.result)
  if(record.reset){assert.equal(record.reset.sql,reset);assert.deepEqual(record.reset.result.errors,[]);validatePhase(record.reset.result)}
  if(record.followup){assert.equal(record.followup.sql,followup);assert.deepEqual(record.followup.result.errors,[]);validatePhase(record.followup.result)}
 }
}
export async function retained(){
 const bytes=await readFile(fixture);assert.equal(createHash('sha256').update(bytes).digest('hex'),fixtureSha256)
 const result=JSON.parse(bytes);assert.equal(result.image,referenceImage);assert.equal(result.runs.length,2)
 result.runs.forEach(validate);assertSameCapture(result.runs[0],result.runs[1],'COUNT NULL independent runs');return result
}
async function main(){
 const args=process.argv.slice(2);const mode=args[0]?.startsWith('--')?args.shift():undefined
 if(![undefined,'--check','--write-fixture'].includes(mode)||args.length>1)throw Error('usage: capture-count-null-compilation.mjs [--check | --write-fixture] [output]')
 if(mode==='--check'){const result=await retained();console.log('Checked COUNT NULL records',result.runs[0].length);return}
 if(mode==='--write-fixture')await refuseExistingFixture(fixture)
 const output=resolve(args[0]??'artifacts/count-null-compilation/capture.json');assert.notEqual(output,fileURLToPath(fixture))
 const runs=[];for(let i=0;i<2;i++)runs.push(await withReferenceContainer(config=>isolatedReference(config,observe)))
 runs.forEach(validate);assertSameCapture(runs[0],runs[1],'COUNT NULL independent runs')
 const result={image:referenceImage,runs};await mkdir(dirname(output),{recursive:true});await writeNewFixture(output,result)
 if(mode==='--write-fixture')await writeNewFixture(fixture,result)
 console.log('Captured COUNT NULL records',runs[0].length)
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await main()
