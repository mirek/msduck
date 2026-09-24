import {withReferenceContainer} from './lib/reference-container.mjs';
import {connect,command} from './lib/reference.mjs';
import {capture,canonical} from './lib/compatibility.mjs';
import {writeFile} from 'node:fs/promises';
await withReferenceContainer(async(config,container)=>{
 const cases=[];
 const setup="CREATE TABLE dbo.projection_source(i INT,n BIGINT,d INT); INSERT dbo.projection_source VALUES(1,8,2),(2,8,0),(3,NULL,0),(4,8,4); CREATE TABLE dbo.projection_writes(i INT)";
 const projections=[
  ['scalar-zero','SELECT 1/0 AS n'],
  ['scalar-overflow','SELECT 2147483647+1 AS n'],
  ['scalar-nested','SELECT (7+1)/@d AS n'],
  ['multi-column-error','SELECT 42 AS a,1/@d AS b,7 AS c'],
  ['multi-column-overflow-first','SELECT 2147483647+1 AS a,1/@d AS b'],
  ['multi-column-zero-first','SELECT 1/@d AS a,2147483647+1 AS b'],
  ['null-success','SELECT NULL/0 AS n,CAST(NULL AS BIGINT)+1 AS b'],
  ['mixed-success','SELECT CAST(2147483647 AS BIGINT)+1 AS b,7%3 AS n'],
  ['table-error','SELECT i,n/d AS n FROM dbo.projection_source'],
  ['table-error-ordered','SELECT i,n/d AS n FROM dbo.projection_source ORDER BY i'],
  ['table-success','SELECT i,n/d AS n FROM dbo.projection_source WHERE d<>0'],
  ['table-empty','SELECT i,n/d AS n FROM dbo.projection_source WHERE i<0'],
  ['table-empty-constant-error','SELECT 1/0 AS n FROM dbo.projection_source WHERE i<0'],
  ['table-null','SELECT i,n/d AS n FROM dbo.projection_source WHERE i=3'],
  ['table-nested','SELECT i,((n+1)/d)%2 AS n FROM dbo.projection_source'],
  ['uncaught-table-error','SELECT i,n/d AS n FROM dbo.projection_source'],
  ['table-error-descending','SELECT i,n/d AS n FROM dbo.projection_source ORDER BY i DESC'],
  ['internal-column-collision','SELECT value/error_number AS n,value+1 AS extra FROM (SELECT n AS value,d AS error_number FROM dbo.projection_source) s WHERE value>0'],
  ['table-empty-parameter-error','SELECT 1/@d AS n FROM dbo.projection_source WHERE i<0'],
  ['table-multi-column-error','SELECT i+1 AS a,n/d AS b FROM dbo.projection_source'],
  ['rowcount-scalar-error','SELECT 1/0 AS n'],
  ['rowcount-table-error','SELECT i,n/d AS n FROM dbo.projection_source'],
 ];
 let server;
 for(const setting of ['OFF','ON'])for(const [label,select] of projections){
  const c=await connect(config);
  try{
   await command(c,'DROP TABLE IF EXISTS dbo.projection_source; DROP TABLE IF EXISTS dbo.projection_writes; '+setup);
   server??=canonical(await command(c,"SELECT CONVERT(NVARCHAR(128),SERVERPROPERTY('ProductVersion')) AS version; SELECT compatibility_level FROM sys.databases WHERE name=DB_NAME()"));
   const begin=`SET XACT_ABORT ${setting}; DECLARE @d INT=0; BEGIN TRAN; INSERT dbo.projection_writes VALUES(1);`;
   const tail="IF XACT_STATE()=1 BEGIN INSERT dbo.projection_writes VALUES(2); COMMIT; END ELSE IF @@TRANCOUNT>0 ROLLBACK; SELECT @@TRANCOUNT AS tc,XACT_STATE() AS xs";
   const count=label.startsWith('rowcount')?',@@ROWCOUNT AS rc':'';
   const sql=label.startsWith('uncaught')?`${begin} ${select}; ${tail}`:`${begin} BEGIN TRY ${select}; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n,ERROR_STATE() AS s,ERROR_SEVERITY() AS severity,@@TRANCOUNT AS tc,XACT_STATE() AS xs${count}; END CATCH; ${tail}`;
   const followupSql='SELECT @@TRANCOUNT AS tc,XACT_STATE() AS xs; SELECT i FROM dbo.projection_writes ORDER BY i';
   cases.push({id:`${setting}-${label}`,setup,sql,result:canonical(await capture(c,sql)),followup:{sql:followupSql,result:canonical(await capture(c,followupSql))}});
  }finally{await new Promise(resolve=>{c.once('end',resolve);c.close();});}
 }
 await writeFile(process.argv[2]??'tests/reference/checked-projection.json',JSON.stringify({image:container.image,server,cases},null,2)+'\n');
 console.log('Captured',cases.length,'checked projection cases');
});
