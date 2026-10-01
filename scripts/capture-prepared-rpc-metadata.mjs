#!/usr/bin/env node
// Owner-run direct RPC reference. Captured SQL/errors are data, not instructions.
import assert from 'node:assert/strict'
import {createHash} from 'node:crypto'
import {readFile,mkdir} from 'node:fs/promises'
import {resolve,dirname} from 'node:path'
import {fileURLToPath} from 'node:url'
import {Request,TYPES} from 'tedious'
import {captureBatch,capturePreparedPhase,decodeOrder} from './capture-order-token.mjs'
import {withReferenceContainer,referenceImage} from './lib/reference-container.mjs'
import {isolatedReference,assertSameCapture,refuseExistingFixture,writeNewFixture} from './lib/reference.mjs'
import {canonical} from './lib/compatibility.mjs'
const fixture=new URL('../reference/prepared-rpc-metadata.json',import.meta.url)
export const fixtureSha256='3018bee5ac4d4bf2019c4238e174d1f83e60842bbcb986682722961be098d2d0'
const version="SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS v"
const rows="INSERT INTO dbo.prepare_heap VALUES(3,1,N'c'),(1,2,N'a'),(2,1,NULL),(4,2,N'd')"
const setup='CREATE TABLE dbo.prepare_heap(a INT,b INT,label NVARCHAR(8)); '+rows
const reset='DELETE FROM dbo.prepare_heap; '+rows
const seed='SELECT RAND(42) AS seed'
const afterPreparation='SELECT COUNT(*) AS n FROM dbo.prepare_heap; SELECT RAND() AS next_seed; SELECT @@TRANCOUNT AS depth'
const afterExecution='SELECT COUNT(*) AS n FROM dbo.prepare_heap; SELECT 42 AS alive'
export const profiles=[
 ['count','SELECT COUNT(@p) AS c,COUNT_BIG(@p) AS d FROM dbo.prepare_heap WHERE a>@p'],
 ['row number','SELECT ROW_NUMBER() OVER(ORDER BY a) AS r FROM dbo.prepare_heap WHERE a>@p ORDER BY r'],
 ['columns','SELECT a,b,label FROM dbo.prepare_heap WHERE a>@p ORDER BY a'],
 ['typed constants','SELECT CAST(@p AS BIGINT) AS n,CAST(NULL AS DECIMAL(12,3)) AS d,CAST(NULL AS NVARCHAR(8)) AS s'],
 ['empty','SELECT a,label FROM dbo.prepare_heap WHERE 1=0'],
 ['insert','INSERT INTO dbo.prepare_heap(a,b,label) VALUES(@p,3,N\'x\')'],
 ['multiple results','SELECT COUNT(*) AS c FROM dbo.prepare_heap; SELECT a FROM dbo.prepare_heap WHERE a>@p ORDER BY a'],
 ['volatile seed','SELECT RAND(777) AS v'],
]
export const variants=['api-default','named-default','named-zero','named-one','named-two','named-null']
export const regressionProfiles=[
 ['numeric conditional','SELECT CASE WHEN @p=1 THEN CAST(a AS BIGINT) ELSE b END AS n FROM dbo.prepare_heap'],
 ['sum CTE','WITH q AS (SELECT CAST(@p AS BIGINT) AS n FROM dbo.prepare_heap) SELECT SUM(n) AS s FROM q'],
 ['statistical','SELECT STDEV(a) AS s,VAR(a) AS v,AVG(a) AS n FROM dbo.prepare_heap'],
 ['GUID','SELECT NEWID() AS g'],
 ['calendar','SELECT EOMONTH(\'2024-01-15\',@p) AS d,DATEFROMPARTS(2024,1,@p) AS f'],
 ['bitwise','SELECT CAST(@p AS BIT)&CAST(1 AS SMALLINT) AS b'],
 ['datetime conditional','SELECT ISNULL(CAST(NULL AS DATETIME2(2)),CAST(\'2024-01-01\' AS DATETIME2(7))) AS d'],
 ['datetime values','SELECT d FROM (VALUES(CAST(\'2024-01-01\' AS DATETIME2(2))),(CAST(NULL AS DATETIME2(7)))) q(d)'],
 ['catalog functions',"SELECT OBJECT_ID(N'dbo.prepare_heap') AS o,SCHEMA_NAME(@p) AS s,TYPE_NAME(@p) AS t,COL_NAME(OBJECT_ID(N'dbo.prepare_heap'),@p) AS c"],
 ['identity functions',"SELECT IDENT_CURRENT(N'dbo.prepare_heap') AS c,IDENT_SEED(N'dbo.prepare_heap') AS s,IDENT_INCR(N'dbo.prepare_heap') AS i"],
 ['JSON presence',"SELECT JSON_PATH_EXISTS(N'{\"a\":1}',N'$.a') AS p"],
 ['NTILE order','SELECT NTILE(@p) OVER(ORDER BY a) AS r FROM dbo.prepare_heap ORDER BY a'],
 ['frame order','SELECT SUM(a) OVER(ORDER BY b ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS s FROM dbo.prepare_heap ORDER BY a'],
 ['named window order','SELECT SUM(a) OVER w AS s FROM dbo.prepare_heap WINDOW w AS (ORDER BY a ROWS UNBOUNDED PRECEDING) ORDER BY s DESC'],
 ['variant order','SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap ORDER BY v'],
 ['variant distinct','SELECT DISTINCT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap ORDER BY v'],
 ['group order','SELECT a,SUM(b) AS s FROM dbo.prepare_heap GROUP BY a ORDER BY s'],
 ['recursive order',"WITH q AS (SELECT 1 AS n UNION ALL SELECT n+1 FROM q WHERE n<3) SELECT n FROM q ORDER BY n"],
 ['CTE delete','WITH q AS (SELECT * FROM dbo.prepare_heap WHERE a>@p) DELETE FROM q'],
 ['insert OUTPUT INTO',"INSERT INTO dbo.prepare_heap(a,b,label) OUTPUT inserted.a,inserted.b,inserted.label INTO dbo.prepare_heap(a,b,label) VALUES(@p,3,N'x')"],
 ['insert OUTPUT',"INSERT INTO dbo.prepare_heap(a,b,label) OUTPUT inserted.a,inserted.label VALUES(@p,3,N'x')"],
 ['bare NULL','SELECT NULL AS n'],
 ['count NULL','SELECT COUNT(NULL) AS n'],
]
// Supplemental original declarations, independent of returned payload values.
export const declarationProfiles=[
 ['variant properties',"SELECT SQL_VARIANT_PROPERTY(CAST(@p AS INT),N'BaseType') AS t,SQL_VARIANT_PROPERTY(CAST(@p AS INT),N'Precision') AS p"],
 ['variant unknown property',"SELECT SQL_VARIANT_PROPERTY(@p,CAST(@p AS NVARCHAR(8))) AS v"],
 ['variant NULL property',"SELECT SQL_VARIANT_PROPERTY(NULL,N'BaseType') AS v"],
 ['variant conditional',"SELECT COALESCE(CAST(NULL AS SQL_VARIANT),CAST(@p AS BIGINT)) AS v"],
 ['variant extrema',"SELECT MIN(CAST(a AS SQL_VARIANT)) AS lo,MAX(CAST(a AS SQL_VARIANT)) AS hi FROM dbo.prepare_heap"],
 ['variant group cast order',"SELECT CAST(a AS INT) AS n,COUNT(*) AS c FROM dbo.prepare_heap GROUP BY a ORDER BY n"],
 ['datetime values','SELECT d FROM (VALUES(CAST(\'2024-01-01\' AS DATETIME2(2))),(CAST(NULL AS DATETIME2(7)))) q(d)'],
 ['datetime COALESCE','SELECT COALESCE(CAST(NULL AS DATETIME2(2)),CAST(NULL AS DATETIME2(7))) AS d'],
 ['datetime mixed offset','SELECT COALESCE(CAST(NULL AS DATETIME2(2)),CAST(NULL AS DATETIMEOFFSET(7))) AS d'],
]
export const catalogDeclarationSetup=setup+'; CREATE TABLE dbo.prepare_identity(id BIGINT IDENTITY(2147483648,3),v INT)'
export const catalogDeclarationProfiles=[
 ['columns empty shape','SELECT * FROM sys.columns WHERE 1=0'],
 ['identity empty shape','SELECT * FROM sys.identity_columns WHERE 1=0'],
 ['columns declared fields',"SELECT name,user_type_id,max_length,column_id FROM sys.columns WHERE object_id=OBJECT_ID(N'dbo.prepare_heap') AND column_id=@p"],
 ['columns TYPE_NAME',"SELECT TYPE_NAME(user_type_id) AS t,max_length,column_id FROM sys.columns WHERE object_id=OBJECT_ID(N'dbo.prepare_heap') AND column_id=@p"],
 ['columns hidden sort',"SELECT name,max_length FROM sys.columns WHERE object_id=OBJECT_ID(N'dbo.prepare_heap') ORDER BY column_id"],
 ['identity variant fields',"SELECT name,seed_value,increment_value,last_value,is_not_for_replication FROM sys.identity_columns WHERE object_id=OBJECT_ID(N'dbo.prepare_identity')"],
 ['identity integer casts',"SELECT CAST(seed_value AS BIGINT) AS b,TRY_CAST(seed_value AS INT) AS i FROM sys.identity_columns WHERE object_id=OBJECT_ID(N'dbo.prepare_identity')"],
 ['identity qualified variant',"SELECT c.seed_value AS s FROM sys.identity_columns c WHERE c.column_id=@p ORDER BY c.column_id"],
]
export const groupingOrderProfiles=[
 ['integral conditional alias','SELECT COALESCE(a,@p) AS k,COUNT(*) AS c FROM dbo.prepare_heap GROUP BY COALESCE(a,@p) ORDER BY k'],
 ['integral conditional expression','SELECT COALESCE(a,@p) AS k,COUNT(*) AS c FROM dbo.prepare_heap GROUP BY COALESCE(a,@P) ORDER BY COALESCE(a,@p)'],
 ['integral repeated parameter','SELECT COALESCE(a,@p) AS k,COUNT(*) AS c,@p AS other FROM dbo.prepare_heap GROUP BY COALESCE(a,@P) ORDER BY COALESCE(a,@p)'],
 ['integral conditional ordinal','SELECT COALESCE(a,@p) AS k FROM dbo.prepare_heap ORDER BY 1 DESC'],
 ['integral ISNULL order','SELECT ISNULL(a,@p) AS k,COUNT(*) AS c FROM dbo.prepare_heap GROUP BY ISNULL(a,@p) ORDER BY k'],
 ['integral CASE order','SELECT CASE WHEN a>@p THEN b ELSE a END AS k,COUNT(*) AS c FROM dbo.prepare_heap GROUP BY CASE WHEN a>@p THEN b ELSE a END ORDER BY k'],
 ['integral qualified CASE identity','SELECT CASE WHEN t.a>@p THEN t.b ELSE t.a END AS k,COUNT(*) AS c FROM dbo.prepare_heap t GROUP BY CASE WHEN a>@p THEN b ELSE a END ORDER BY CASE WHEN a>@p THEN b ELSE a END'],
 ['integral legacy ROLLUP conditional','SELECT COALESCE(a,@p) AS k,COUNT(*) AS c,GROUPING(COALESCE(a,@p)) AS g FROM dbo.prepare_heap GROUP BY COALESCE(a,@p) WITH ROLLUP ORDER BY g,1'],
 ['integral constant conditional','SELECT COALESCE(CAST(NULL AS INT),@p) AS k FROM dbo.prepare_heap ORDER BY k'],
 ['integral folded CASE order','SELECT CASE WHEN a>@p THEN 1 ELSE 1 END AS k,COUNT(*) AS c FROM dbo.prepare_heap GROUP BY CASE WHEN a>@p THEN 1 ELSE 1 END ORDER BY k'],
]
export const rankingDeclarationProfiles=[
 ['floating ranks','SELECT a,PERCENT_RANK() OVER(ORDER BY b) AS p,CUME_DIST() OVER(ORDER BY b) AS c FROM dbo.prepare_heap ORDER BY a'],
 ['floating ranks partition','SELECT a,PERCENT_RANK() OVER(PARTITION BY b ORDER BY a DESC) AS p,CUME_DIST() OVER(PARTITION BY b ORDER BY a DESC) AS c FROM dbo.prepare_heap ORDER BY a'],
 ['floating ranks empty','SELECT PERCENT_RANK() OVER(ORDER BY a) AS p,CUME_DIST() OVER(ORDER BY a) AS c FROM dbo.prepare_heap WHERE 1=0'],
 ['floating ranks derived','WITH q AS (SELECT a,PERCENT_RANK() OVER(ORDER BY b) AS p,CUME_DIST() OVER(ORDER BY b) AS c FROM dbo.prepare_heap) SELECT p,c FROM q ORDER BY a'],
 ['floating ranks conditional','SELECT CASE WHEN @p=1 THEN PERCENT_RANK() OVER(ORDER BY a) ELSE CUME_DIST() OVER(ORDER BY a) END AS n FROM dbo.prepare_heap'],
 ['floating ranks named window','SELECT a,PERCENT_RANK() OVER w AS p,CUME_DIST() OVER w AS c FROM dbo.prepare_heap WINDOW w AS(PARTITION BY b ORDER BY a) ORDER BY a'],
]
export const bitwiseSetup=setup+'; CREATE TABLE dbo.prepare_bits(b BIT,t TINYINT,s SMALLINT,i INT,big BIGINT); INSERT INTO dbo.prepare_bits VALUES(1,170,75,-1,4294967296),(NULL,NULL,NULL,NULL,NULL)'
export const bitwiseDeclarationProfiles=[
 ['BIT pair parameter declarations','SELECT CAST(@p AS BIT)&CAST(@p AS BIT) AS a,CAST(@p AS BIT)|CAST(@p AS BIT) AS o,CAST(@p AS BIT)^CAST(@p AS BIT) AS x'],
 ['BIT mixed parameter declarations','SELECT CAST(@p AS BIT)&CAST(@p AS TINYINT) AS t,CAST(@p AS BIT)|CAST(@p AS SMALLINT) AS s,CAST(@p AS BIT)^CAST(@p AS INT) AS i,CAST(@p AS BIT)&CAST(@p AS BIGINT) AS b'],
 ['BIT complement declaration','SELECT ~CAST(@p AS BIT) AS b'],
 ['BIT column declarations','SELECT b&b AS a,b|b AS o,b^b AS x,b&t AS t,b|s AS s,b^i AS i,b&big AS n FROM dbo.prepare_bits'],
 ['BIT empty column declarations','SELECT b&b AS a,b^s AS s FROM dbo.prepare_bits WHERE 1=0'],
 ['BIT derived declarations','WITH q AS (SELECT b,s FROM dbo.prepare_bits) SELECT b&b AS a,b|s AS s FROM q'],
]
export const orderPropertyProfiles=[
 ['variant cast no order','SELECT CAST(b AS SQL_VARIANT) AS v FROM dbo.prepare_heap'],
 ['variant cast projected order','SELECT CAST(b AS SQL_VARIANT) AS v FROM dbo.prepare_heap ORDER BY v'],
 ['variant cast hidden column','SELECT CAST(b AS SQL_VARIANT) AS v FROM dbo.prepare_heap ORDER BY a'],
 ['variant cast projected and hidden column','SELECT CAST(b AS SQL_VARIANT) AS v FROM dbo.prepare_heap ORDER BY v,a'],
 ['variant cast projected and hidden expression','SELECT CAST(b AS SQL_VARIANT) AS v FROM dbo.prepare_heap ORDER BY v,a+1'],
 ['variant cast only hidden expression','SELECT CAST(b AS SQL_VARIANT) AS v FROM dbo.prepare_heap ORDER BY a+1'],
 ['variant cast projected arithmetic','SELECT CAST(b AS SQL_VARIANT) AS v,a+1 AS n FROM dbo.prepare_heap ORDER BY v,n'],
 ['variant cast duplicate expressions','SELECT CAST(b AS SQL_VARIANT) AS v,a+1 AS n FROM dbo.prepare_heap ORDER BY v,a+1'],
]
export const orderDeclarationProfiles=[
 ['variant cast alias','SELECT CAST(v AS INT) AS n FROM (SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap) q ORDER BY n'],
 ['variant cast ordinal','SELECT CAST(v AS BIGINT) AS n FROM (SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap) q ORDER BY 1 DESC'],
 ['variant grouped cast','SELECT CAST(v AS INT) AS n,COUNT(*) AS c FROM (SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap) q GROUP BY v ORDER BY n'],
 ['variant conditional alias','SELECT a,COALESCE(CAST(b AS SQL_VARIANT),CAST(@p AS SQL_VARIANT)) AS v FROM dbo.prepare_heap ORDER BY v,a'],
 ['variant conditional ordinal','SELECT a,COALESCE(CAST(b AS SQL_VARIANT),CAST(@p AS SQL_VARIANT)) AS v FROM dbo.prepare_heap ORDER BY 2 DESC,1'],
 ['variant hidden arithmetic key','SELECT CAST(b AS SQL_VARIANT) AS v FROM dbo.prepare_heap ORDER BY v,a+1'],
 ['variant distinct conditional','SELECT DISTINCT COALESCE(CAST(b AS SQL_VARIANT),CAST(@p AS SQL_VARIANT)) AS v FROM dbo.prepare_heap ORDER BY v'],
 ['variant union order','SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap UNION SELECT CAST(b AS SQL_VARIANT) FROM dbo.prepare_heap ORDER BY v'],
 ['variant union all order','SELECT CAST(a AS SQL_VARIANT) AS v FROM dbo.prepare_heap UNION ALL SELECT CAST(b AS SQL_VARIANT) FROM dbo.prepare_heap ORDER BY 1'],
]
export const windowDeclarationProfiles=[
 ['variant COUNT partition','SELECT a,COUNT(*) OVER(PARTITION BY COALESCE(CAST(b AS SQL_VARIANT),CAST(@p AS SQL_VARIANT))) AS c FROM dbo.prepare_heap ORDER BY a'],
 ['variant COUNT_BIG partition','SELECT a,COUNT_BIG(*) OVER(PARTITION BY COALESCE(CAST(b AS SQL_VARIANT),CAST(@p AS SQL_VARIANT))) AS c FROM dbo.prepare_heap ORDER BY a'],
 ['variant COUNT value','SELECT COUNT(COALESCE(CAST(b AS SQL_VARIANT),CAST(@p AS SQL_VARIANT))) AS c FROM dbo.prepare_heap'],
 ['variant distinct COUNT value','SELECT COUNT(DISTINCT COALESCE(CAST(b AS SQL_VARIANT),CAST(@p AS SQL_VARIANT))) AS c FROM dbo.prepare_heap'],
 ['variant declared source COUNT partition','SELECT a,COUNT(*) OVER(PARTITION BY COALESCE(v,CAST(@p AS SQL_VARIANT))) AS c FROM (SELECT a,CAST(b AS SQL_VARIANT) AS v FROM dbo.prepare_heap) q ORDER BY a'],
 ['variant nested shadow partition','SELECT a,COUNT(*) OVER(PARTITION BY COALESCE(v,CAST(@p AS SQL_VARIANT))) AS c FROM (SELECT a,CAST(b AS SQL_VARIANT) AS v FROM dbo.prepare_heap) q WHERE a IN (SELECT a FROM dbo.prepare_heap WHERE b>@p) ORDER BY a'],
 ['typed NULL COUNT window','SELECT COUNT(CAST(NULL AS INT)) OVER(PARTITION BY COALESCE(CAST(b AS SQL_VARIANT),CAST(@p AS SQL_VARIANT))) AS c FROM dbo.prepare_heap'],
 ['untyped NULL COUNT window','SELECT COUNT(NULL) OVER(PARTITION BY COALESCE(CAST(b AS SQL_VARIANT),CAST(@p AS SQL_VARIANT))) AS c FROM dbo.prepare_heap'],
 ['variant ROW_NUMBER partition','SELECT a,ROW_NUMBER() OVER(PARTITION BY COALESCE(CAST(b AS SQL_VARIANT),CAST(@p AS SQL_VARIANT)) ORDER BY a) AS n FROM dbo.prepare_heap ORDER BY a'],
 ['variant ROW_NUMBER order','SELECT a,ROW_NUMBER() OVER(ORDER BY COALESCE(CAST(b AS SQL_VARIANT),CAST(@p AS SQL_VARIANT))) AS n FROM dbo.prepare_heap ORDER BY a'],
]
export const temporalSetProfiles=[
 ...['DATETIME2','DATETIMEOFFSET'].flatMap(type=>['UNION','UNION ALL','INTERSECT','EXCEPT'].map(op=>[
  type+' '+op,`SELECT CAST(NULL AS ${type}(2)) AS d ${op} SELECT CAST(NULL AS ${type}(7))`,
 ])),
 ['mixed temporal forward','SELECT CAST(NULL AS DATETIME2(7)) AS d UNION ALL SELECT CAST(NULL AS DATETIMEOFFSET(2))'],
 ['mixed temporal reverse','SELECT CAST(NULL AS DATETIMEOFFSET(2)) AS d UNION ALL SELECT CAST(NULL AS DATETIME2(7))'],
]
export const setPropertySetup=setup+'; CREATE TABLE dbo.prepare_required(n INT NOT NULL); INSERT INTO dbo.prepare_required VALUES(1),(2)'
export const setPropertyProfiles=[
 ...[
  ['nullable integer','SELECT CAST(NULL AS INT) AS d','SELECT CAST(NULL AS INT)'],
  ['nonnull integer','SELECT CAST(1 AS INT) AS d','SELECT CAST(2 AS INT)'],
  ['nonnull then nullable','SELECT CAST(1 AS INT) AS d','SELECT CAST(NULL AS INT)'],
  ['nullable then nonnull','SELECT CAST(NULL AS INT) AS d','SELECT CAST(1 AS INT)'],
  ['nonnull stored','SELECT n AS d FROM dbo.prepare_required','SELECT n FROM dbo.prepare_required'],
  ['nonnull stored then nullable stored','SELECT n AS d FROM dbo.prepare_required','SELECT a FROM dbo.prepare_heap'],
  ['nullable stored then nonnull stored','SELECT a AS d FROM dbo.prepare_heap','SELECT n FROM dbo.prepare_required'],
  ['nonnull stored then expression','SELECT n AS d FROM dbo.prepare_required','SELECT CAST(NULL AS INT)'],
  ['stored then expression','SELECT a AS d FROM dbo.prepare_heap','SELECT CAST(NULL AS INT)'],
  ['expression then stored','SELECT CAST(NULL AS INT) AS d','SELECT a FROM dbo.prepare_heap'],
  ['temporal nonnull then nullable',"SELECT CAST('2024-01-01' AS DATETIME2(2)) AS d",'SELECT CAST(NULL AS DATETIME2(7))'],
  ['temporal nullable then nonnull','SELECT CAST(NULL AS DATETIME2(2)) AS d',"SELECT CAST('2024-01-01' AS DATETIME2(7))"],
 ].flatMap(([name,left,right])=>['UNION','UNION ALL','INTERSECT','EXCEPT'].map(op=>[name+' '+op,left+' '+op+' '+right])),
 ['nested intersect union','(SELECT CAST(NULL AS INT) AS d INTERSECT SELECT CAST(1 AS INT)) UNION ALL SELECT CAST(2 AS INT)'],
 ['nested union except','(SELECT CAST(1 AS INT) AS d UNION ALL SELECT CAST(NULL AS INT)) EXCEPT SELECT CAST(2 AS INT)'],
 ['nested except intersect','SELECT CAST(NULL AS INT) AS d EXCEPT (SELECT CAST(1 AS INT) INTERSECT SELECT CAST(NULL AS INT))'],
]
export const cteDeleteProfiles=[
 ['CTE delete reuse','WITH q AS (SELECT * FROM dbo.prepare_heap WHERE a>@p) DELETE FROM q'],
 ['qualified CTE delete reuse','WITH q AS (SELECT src.* FROM dbo.prepare_heap AS src WHERE src.a>@p) DELETE FROM q'],
 ['CTE delete rollback','WITH q AS (SELECT * FROM dbo.prepare_heap WHERE a>@p) DELETE FROM q'],
 ['qualified CTE delete rollback','WITH q AS (SELECT src.* FROM dbo.prepare_heap AS src WHERE src.a>@p) DELETE FROM q'],
]
export const cteDeleteOptions={profilePlan:cteDeleteProfiles,variantPlan:['api-default','named-one'],executionValues:[null,9,1,0],rollbackProfiles:cteDeleteProfiles.filter(([name])=>name.endsWith('rollback')).map(([name])=>name)}
const deleteState='SELECT a,b,label FROM dbo.prepare_heap ORDER BY a; SELECT @@TRANCOUNT AS depth'
export const values=[null,0,9,1]
export async function observe(connection,{verifyVersion=true,profilePlan=profiles,variantPlan=variants,execute=true,executionValues=values,rollbackProfiles=[],setupSql=setup}={}){
 const records=[]
 for(const [name,sql] of [['version',version],['setup',setupSql]]){
  const result=canonical(await captureBatch(connection,sql));if(name!=='version'||verifyVersion)assert.deepEqual(result.errors,[]);records.push({name,sql,result})
 }
 for(const [name,sql] of profilePlan)for(const variant of variantPlan){
  const resetResult=canonical(await captureBatch(connection,reset));assert.deepEqual(resetResult.errors,[])
  const seeded=canonical(await captureBatch(connection,seed));assert.deepEqual(seeded.errors,[])
  let complete=()=>{}
  const request=new Request(variant==='api-default'?sql:'sys.sp_prepare',(error,rowCount)=>complete(error,rowCount))
  if(variant==='api-default')request.addParameter('p',TYPES.Int,undefined)
  else{
   request.addOutputParameter('handle',TYPES.Int)
   request.addParameter('params',TYPES.NVarChar,'@p INT')
   request.addParameter('stmt',TYPES.NVarChar,sql)
   if(variant!=='named-default')request.addParameter('options',TYPES.Int,{'named-zero':0,'named-one':1,'named-two':2,'named-null':null}[variant])
  }
  const preparation=canonical(await capturePreparedPhase(connection,request,()=>variant==='api-default'?connection.prepare(request):connection.callProcedure(request),callback=>{complete=callback},variant==='api-default'))
  const after=canonical(await captureBatch(connection,afterPreparation));assert.deepEqual(after.errors,[])
  let executionRequest=request
  if(variant!=='api-default'){
   executionRequest=new Request(sql,(error,rowCount)=>complete(error,rowCount));executionRequest.addParameter('p',TYPES.Int,undefined)
   executionRequest.handle=preparation.returnValues.find(value=>value.name==='handle')?.value
  }
  const executions=[];let unpreparation=null
  if(!preparation.errors.length&&Number.isInteger(executionRequest.handle)&&executionRequest.handle>0)try{
   if(execute)for(const value of executionValues){
    const execution={value}
    if(rollbackProfiles.includes(name))execution.begin=canonical(await captureBatch(connection,'BEGIN TRANSACTION'))
    try{
     execution.result=canonical(await capturePreparedPhase(connection,executionRequest,()=>connection.execute(executionRequest,{p:value}),callback=>{complete=callback}))
     if(rollbackProfiles.includes(name))execution.beforeRollback=canonical(await captureBatch(connection,deleteState))
    }finally{
     if(rollbackProfiles.includes(name)){
      execution.rollback=canonical(await captureBatch(connection,'ROLLBACK TRANSACTION'))
      execution.afterRollback=canonical(await captureBatch(connection,deleteState))
     }
    }
    executions.push(execution)
   }
  }finally{
   unpreparation=canonical(await capturePreparedPhase(connection,executionRequest,()=>connection.unprepare(executionRequest),callback=>{complete=callback}))
  }
  const end=canonical(await captureBatch(connection,afterExecution));assert.deepEqual(end.errors,[])
  records.push({name,sql,variant,reset:{sql:reset,result:resetResult},seed:{sql:seed,result:seeded},preparation,afterPreparation:{sql:afterPreparation,result:after},executions,unpreparation,afterExecution:{sql:afterExecution,result:end}})
  console.log('prepared metadata',name,variant,'errors',preparation.errors.map(e=>e.number),'sets',preparation.sets.length,'executions',executions.length)
 }
 return records
}
function phase(result){
 assert(result.done.length>0);assert.equal(result.done.length,result.doneTokens.length)
 for(const token of result.doneTokens){
  assert.equal(token.more,Boolean(token.status&1));assert.equal(token.sqlError,Boolean(token.status&2));assert.equal(token.attention,Boolean(token.status&32));assert.equal(token.serverError,Boolean(token.status&256));assert.equal(token.rowCount===null,!(token.status&16))
 }
 for(const set of result.sets)for(const row of set.rows)assert.equal(row.length,set.columns.length)
 for(const event of result.events)if(event.kind==='ORDER')assertSameCapture(decodeOrder(Buffer.from(event.hex,'hex')),{hex:event.hex,length:event.length,ordinals:event.ordinals},'prepared RPC ORDER bytes')
}
export function validate(run){
 assert.equal(run.length,2+profiles.length*variants.length)
 assertSameCapture(run.slice(0,2).map(({name,sql})=>({name,sql})),[{name:'version',sql:version},{name:'setup',sql:setup}],'prepared RPC setup plan')
 assertSameCapture(run.slice(2).map(({name,sql,variant})=>({name,sql,variant})),profiles.flatMap(([name,sql])=>variants.map(variant=>({name,sql,variant}))),'prepared RPC request plan')
 assertSameCapture(run[0].result.sets[0].rows,[['17.0.4065.4']],'prepared RPC version')
 for(const record of run){
  if(record.result){phase(record.result);continue}
  for(const [key,sql] of [['reset',reset],['seed',seed],['afterPreparation',afterPreparation],['afterExecution',afterExecution]]){
   assert.equal(record[key]?.sql,sql);assert.deepEqual(record[key].result.errors,[]);phase(record[key].result)
  }
  phase(record.preparation)
  if(record.preparation.errors.length){assert.deepEqual(record.executions,[]);assert.equal(record.unpreparation,null)}
  else {assert.deepEqual(record.executions.map(e=>e.value),values);assert(record.unpreparation);record.executions.forEach(e=>phase(e.result));phase(record.unpreparation)}
 }
}
export async function retained(){
 const bytes=await readFile(fixture);assert.equal(createHash('sha256').update(bytes).digest('hex'),fixtureSha256)
 const result=JSON.parse(bytes);assert.equal(result.image,referenceImage);assert.equal(result.runs.length,2);result.runs.forEach(validate);assertSameCapture(result.runs[0],result.runs[1],'prepared RPC independent captures');return result
}
export async function retainedCteDelete(){
 const bytes=await readFile(new URL('../reference/prepared-cte-delete.json',import.meta.url))
 assert.equal(createHash('sha256').update(bytes).digest('hex'),'82b301242462bc0ce227173523a96d4b06b15ddf85ec1c8c2fdd9c4ad0120a81')
 const result=JSON.parse(bytes);assert.equal(result.image,referenceImage);assert.equal(result.runs.length,2)
 assertSameCapture(result.runs[0],result.runs[1],'prepared CTE DELETE independent captures')
 return result
}
export function validateTemporalSets(run){
 assert.equal(run.length,22)
 assertSameCapture(run.slice(0,2).map(({name,sql})=>({name,sql})),[{name:'version',sql:version},{name:'setup',sql:setup}],'temporal set setup plan')
 assertSameCapture(run[0].result.sets[0].rows,[['17.0.4065.4']],'temporal set version')
 assertSameCapture(run.slice(2).map(({name,sql,variant})=>({name,sql,variant})),temporalSetProfiles.flatMap(([name,sql])=>['api-default','named-one'].map(variant=>({name,sql,variant}))),'temporal set request plan')
 for(const record of run.slice(2)){
  phase(record.preparation);assert.deepEqual(record.preparation.errors,[])
  assert.equal(record.preparation.sets.length,1);assert.deepEqual(record.preparation.sets[0].rows,[])
  assert.deepEqual(record.executions,[]);phase(record.unpreparation)
  assert.deepEqual(record.unpreparation.errors,[])
  for(const [key,sql] of [['reset',reset],['seed',seed],['afterPreparation',afterPreparation],['afterExecution',afterExecution]]){
   assert.equal(record[key]?.sql,sql);phase(record[key].result);assert.deepEqual(record[key].result.errors,[])
  }
  assertSameCapture(record.afterPreparation.result.sets.map(s=>s.rows),[[[4]],[[0.041009986028273604]],[[0]]],'temporal set nonexecution')
  assertSameCapture(record.afterExecution.result.sets.map(s=>s.rows),[[[4]],[[42]]],'temporal set final state')
 }
}
export async function retainedTemporalSets(){
 const bytes=await readFile(new URL('../reference/prepared-temporal-sets.json',import.meta.url))
 assert.equal(createHash('sha256').update(bytes).digest('hex'),'9b000b8cc9402b1ca396bf8b5cc4c830b15bba6ba562bb71f7d510cee43fd1c1')
 const result=JSON.parse(bytes);assert.equal(result.image,referenceImage);assert.equal(result.runs.length,2)
 result.runs.forEach(validateTemporalSets)
 assertSameCapture(result.runs[0],result.runs[1],'temporal set independent captures')
 return result
}
export function validateSetProperties(run){
 assert.equal(run.length,2+setPropertyProfiles.length*2)
 assertSameCapture(run.slice(0,2).map(({name,sql})=>({name,sql})),[{name:'version',sql:version},{name:'setup',sql:setPropertySetup}],'set property setup plan')
 assertSameCapture(run[0].result.sets[0].rows,[['17.0.4065.4']],'set property version')
 assertSameCapture(run.slice(2).map(({name,sql,variant})=>({name,sql,variant})),setPropertyProfiles.flatMap(([name,sql])=>['api-default','named-one'].map(variant=>({name,sql,variant}))),'set property request plan')
 for(const record of run.slice(2)){
  phase(record.preparation);assert.deepEqual(record.preparation.errors,[])
  assert.equal(record.preparation.sets.length,1);assert.deepEqual(record.preparation.sets[0].rows,[])
  assert.deepEqual(record.executions,[]);phase(record.unpreparation)
  assert.deepEqual(record.unpreparation.errors,[])
  for(const [key,sql] of [['reset',reset],['seed',seed],['afterPreparation',afterPreparation],['afterExecution',afterExecution]]){
   assert.equal(record[key]?.sql,sql);phase(record[key].result);assert.deepEqual(record[key].result.errors,[])
  }
  assertSameCapture(record.afterPreparation.result.sets.map(s=>s.rows),[[[4]],[[0.041009986028273604]],[[0]]],'set property nonexecution')
  assertSameCapture(record.afterExecution.result.sets.map(s=>s.rows),[[[4]],[[42]]],'set property final state')
 }
}
export async function retainedSetProperties(){
 const bytes=await readFile(new URL('../reference/prepared-set-properties.json',import.meta.url))
 assert.equal(createHash('sha256').update(bytes).digest('hex'),'fc4d1ee4fe991cca987be3d261f01da58e0c59032a2a35825c28fe2993aa9e50')
 const result=JSON.parse(bytes);assert.equal(result.image,referenceImage);assert.equal(result.runs.length,2)
 result.runs.forEach(validateSetProperties)
 assertSameCapture(result.runs[0],result.runs[1],'set property independent captures')
 return result
}
async function main(){
 const args=process.argv.slice(2);const mode=args[0]?.startsWith('--')?args.shift():undefined
 if(![undefined,'--check','--write-fixture','--regressions','--declarations','--window-declarations','--order-declarations','--order-properties','--bitwise-declarations','--ranking-declarations','--grouping-order','--catalog-declarations','--temporal-sets','--set-properties','--cte-delete'].includes(mode)||args.length>1)throw Error('usage: capture-prepared-rpc-metadata.mjs [--check | --write-fixture | --regressions | --declarations | --window-declarations | --order-declarations | --order-properties | --bitwise-declarations | --ranking-declarations | --grouping-order | --catalog-declarations | --temporal-sets | --set-properties | --cte-delete] [output]')
 if(mode==='--check'){const r=await retained();console.log('Checked prepared RPC records',r.runs[0].length);return}
 if(mode==='--write-fixture')await refuseExistingFixture(fixture)
 const output=resolve(args[0]??'artifacts/prepared-rpc-metadata/capture.json');assert.notEqual(output,fileURLToPath(fixture))
 const regression=mode==='--catalog-declarations'||mode==='--grouping-order'||mode==='--ranking-declarations'||mode==='--bitwise-declarations'||mode==='--order-properties'||mode==='--order-declarations'||mode==='--window-declarations'||mode==='--regressions'||mode==='--declarations'||mode==='--temporal-sets'||mode==='--set-properties';const cteDelete=mode==='--cte-delete';const preparationProfiles=mode==='--catalog-declarations'?catalogDeclarationProfiles:mode==='--grouping-order'?groupingOrderProfiles:mode==='--ranking-declarations'?rankingDeclarationProfiles:mode==='--bitwise-declarations'?bitwiseDeclarationProfiles:mode==='--order-properties'?orderPropertyProfiles:mode==='--order-declarations'?orderDeclarationProfiles:mode==='--window-declarations'?windowDeclarationProfiles:mode==='--set-properties'?setPropertyProfiles:mode==='--temporal-sets'?temporalSetProfiles:mode==='--declarations'?declarationProfiles:regressionProfiles
 const runs=[];for(let i=0;i<2;i++)runs.push(await withReferenceContainer(config=>isolatedReference(config,connection=>observe(connection,cteDelete?cteDeleteOptions:regression?{profilePlan:preparationProfiles,variantPlan:['api-default','named-one'],execute:false,...(mode==='--catalog-declarations'?{setupSql:catalogDeclarationSetup}:mode==='--set-properties'?{setupSql:setPropertySetup}:mode==='--bitwise-declarations'?{setupSql:bitwiseSetup}:{})}:{}))))
 if(cteDelete)for(const run of runs){
  assert.equal(run.length,10)
  for(const record of run.slice(2)){
   phase(record.preparation);assert.deepEqual(record.preparation.errors,[])
   assertSameCapture(record.afterPreparation.result.sets.map(s=>s.rows),[[[4]],[[0.041009986028273604]],[[0]]],'CTE DELETE preparation does not execute')
   assert.deepEqual(record.executions.map(e=>e.value),cteDeleteOptions.executionValues)
   for(const execution of record.executions){
    phase(execution.result);assert.deepEqual(execution.result.errors,[])
    if(record.name.endsWith('rollback')){
     assert.deepEqual(execution.begin.errors,[]);assert.deepEqual(execution.rollback.errors,[])
     assertSameCapture(execution.afterRollback.sets.map(s=>s.rows),[[[1,2,'a'],[2,1,null],[3,1,'c'],[4,2,'d']],[[0]]],'CTE DELETE rollback restores every row')
    }
   }
   assertSameCapture(record.afterExecution.result.sets.map(s=>s.rows),[[[record.name.endsWith('rollback')?4:0]],[[42]]],'CTE DELETE final state')
  }
 }
 else if(!regression)runs.forEach(validate)
 else for(const run of runs){
  assertSameCapture(run.slice(2).map(({name,sql,variant})=>({name,sql,variant})),preparationProfiles.flatMap(([name,sql])=>['api-default','named-one'].map(variant=>({name,sql,variant}))),'regression preparation plan')
  for(const record of run.slice(2)){
   phase(record.preparation);assert.deepEqual(record.executions,[])
   assertSameCapture(record.afterPreparation.result.sets.map(s=>s.rows),[[[4]],[[0.041009986028273604]],[[0]]],'regression preparation does not execute')
  }
 }
 if(mode==='--set-properties')runs.forEach(validateSetProperties)
 if(mode==='--temporal-sets')runs.forEach(validateTemporalSets)
 assertSameCapture(runs[0],runs[1],'prepared RPC independent captures')
 const result={image:referenceImage,runs};await mkdir(dirname(output),{recursive:true});await writeNewFixture(output,result)
 if(mode==='--write-fixture')await writeNewFixture(fixture,result)
 console.log('Captured prepared RPC records',runs[0].length)
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await main()
