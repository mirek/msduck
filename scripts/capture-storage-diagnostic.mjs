import {withReferenceContainer} from './lib/reference-container.mjs';
import {connect,command} from './lib/reference.mjs';
import {capture,canonical} from './lib/compatibility.mjs';
import {writeFile} from 'node:fs/promises';
await withReferenceContainer(async(config,container)=>{
 const c=await connect(config);const cases=[];
 try {
  const server=canonical(await command(c,"SELECT CONVERT(NVARCHAR(128),SERVERPROPERTY('ProductVersion')) AS version,CONVERT(NVARCHAR(128),SERVERPROPERTY('ProductLevel')) AS level,CONVERT(NVARCHAR(128),SERVERPROPERTY('Edition')) AS edition; SELECT compatibility_level FROM sys.databases WHERE name=DB_NAME()"));
  for(const declaration of ['VARCHAR(1)','CHAR(3)','NVARCHAR(1)','NCHAR(3)','VARCHAR(120)']) {
   for(const mode of ['insert','multirow','update','caught']) {
    const value=declaration.endsWith('(120)')?"REPLICATE(N'Ā',130)":"N'aĀ🦆z'";
    const setup=`DROP TABLE IF EXISTS dbo.truncation_target; CREATE TABLE dbo.truncation_target(id INT,s ${declaration}); INSERT dbo.truncation_target VALUES(0,'x')`;
    await command(c,setup);
    const write=mode==='update'?`UPDATE dbo.truncation_target SET s=${value}`:mode==='multirow'?`INSERT dbo.truncation_target VALUES(1,'y'),(2,${value})`:`INSERT dbo.truncation_target VALUES(1,${value})`;
    const sql=mode==='caught'?`BEGIN TRY ${write}; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n,ERROR_STATE() AS s,ERROR_SEVERITY() AS severity,ERROR_MESSAGE() AS message; END CATCH; SELECT id,CONVERT(VARBINARY(MAX),s) AS raw FROM dbo.truncation_target ORDER BY id`:`${write}; SELECT @@ERROR AS e,@@ROWCOUNT AS rc; SELECT id,CONVERT(VARBINARY(MAX),s) AS raw FROM dbo.truncation_target ORDER BY id`;
    cases.push({declaration,mode,setup,sql,result:canonical(await capture(c,sql))});
   }
  }
  for(const sql of [
   "BEGIN TRAN; INSERT dbo.truncation_target VALUES(1,REPLICATE('a',130)); SELECT @@TRANCOUNT AS tc,XACT_STATE() AS xs; ROLLBACK; SELECT COUNT(*) AS n FROM dbo.truncation_target",
   "SET XACT_ABORT ON; BEGIN TRY BEGIN TRAN; INSERT dbo.truncation_target VALUES(1,REPLICATE('a',130)); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n,@@TRANCOUNT AS tc,XACT_STATE() AS xs; IF @@TRANCOUNT>0 ROLLBACK; END CATCH; SET XACT_ABORT OFF",
  ]) {cases.push({setup:'',sql,result:canonical(await capture(c,sql))});}
  const setup="DROP TABLE IF EXISTS dbo.xact_target; CREATE TABLE dbo.xact_target(id INT PRIMARY KEY,s VARCHAR(1)); INSERT dbo.xact_target VALUES(0,'x')";
  const followupSql="SELECT @@TRANCOUNT AS tc,XACT_STATE() AS xs; SELECT id,s FROM dbo.xact_target ORDER BY id";
  const state="SELECT ERROR_NUMBER() AS n,@@TRANCOUNT AS tc,XACT_STATE() AS xs";
  const probes=[];
  for(const setting of ['OFF','ON']) for(const operation of ['insert','update']) {
   const write=operation==='insert'?"INSERT dbo.xact_target VALUES(2,'long')":"UPDATE dbo.xact_target SET s='long' WHERE id=0";
   const begin=`SET XACT_ABORT ${setting}; BEGIN TRAN; INSERT dbo.xact_target VALUES(1,'a');`;
   for(const mode of ['uncaught','caught_rollback','caught_commit','caught_write','caught_leave']) {
    let sql;
    if(mode==='uncaught') sql=`${begin} ${write}; SELECT 42 AS continued,@@TRANCOUNT AS tc,XACT_STATE() AS xs; IF @@TRANCOUNT>0 ROLLBACK`;
    else {
     const action=mode==='caught_commit'?"BEGIN TRY COMMIT; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS commit_error,XACT_STATE() AS xs; ROLLBACK; END CATCH":mode==='caught_write'?"BEGIN TRY INSERT dbo.xact_target VALUES(3,'z'); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS write_error,XACT_STATE() AS xs; END CATCH; ROLLBACK":mode==='caught_rollback'?"ROLLBACK":"";
     sql=`${begin} BEGIN TRY ${write}; END TRY BEGIN CATCH ${state}; ${action} END CATCH; SELECT 42 AS continued,@@TRANCOUNT AS tc,XACT_STATE() AS xs`;
    }
    probes.push({id:`${setting}-${operation}-${mode}`,sql});
   }
  }
  for(const setting of ['OFF','ON']) for(const [label,statement] of [
   ['raiserror',"RAISERROR('application error',16,1)"],
   ['throw',"THROW 50001,'application error',1"],
   ['constraint',"INSERT dbo.xact_target VALUES(0,'y')"],
   ['arithmetic',"SELECT 1/0 AS n"],
  ]) probes.push({id:`${setting}-${label}`,sql:`SET XACT_ABORT ${setting}; BEGIN TRAN; INSERT dbo.xact_target VALUES(1,'a'); BEGIN TRY ${statement}; END TRY BEGIN CATCH ${state}; ROLLBACK; END CATCH; SELECT @@TRANCOUNT AS tc,XACT_STATE() AS xs`});
  for(const severity of [10,11,16]) probes.push({id:`ON-raiserror-${severity}-commit`,sql:`SET XACT_ABORT ON; BEGIN TRAN; INSERT dbo.xact_target VALUES(1,'a'); SELECT XACT_STATE() AS before_error; BEGIN TRY RAISERROR('application error',${severity},1); END TRY BEGIN CATCH ${state}; END CATCH; BEGIN TRY COMMIT; SELECT 1 AS committed; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS commit_error,XACT_STATE() AS xs; ROLLBACK; END CATCH`});
  for(const setting of ['OFF','ON']) for(const [label,body] of [
   ['uncaught-raiserror',"RAISERROR('application error',16,1); SELECT 42 AS continued,@@TRANCOUNT AS tc,XACT_STATE() AS xs"],
   ['uncaught-throw',"THROW 50001,'application error',1; SELECT 42 AS continued"],
   ['uncaught-condition',"IF 1/0=0 SELECT 42 AS continued"],
   ['caught-condition',`BEGIN TRY IF 1/0=0 SELECT 42 AS continued; END TRY BEGIN CATCH ${state}; ROLLBACK; END CATCH`],
  ]) probes.push({id:`${setting}-${label}`,sql:`SET XACT_ABORT ${setting}; BEGIN TRAN; INSERT dbo.xact_target VALUES(1,'a'); ${body}`});
  for(const probe of probes) {
   const isolated=await connect(config);
   try {
    await command(isolated,setup);
    const result=canonical(await capture(isolated,probe.sql));
    const followup={sql:followupSql,result:canonical(await capture(isolated,followupSql))};
    cases.push({...probe,isolated:true,setup,result,followup});
   } finally {await new Promise(resolve=>{isolated.once('end',resolve);isolated.close();});}
  }
  await writeFile(process.argv[2]??'tests/reference/storage-diagnostic.json',JSON.stringify({image:container.image,server,cases},null,2)+'\n');
  console.log('Captured',cases.length,'storage diagnostic cases');
 } finally {c.close();}
});
