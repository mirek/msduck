import {withReferenceContainer} from './lib/reference-container.mjs';
import {connect,command} from './lib/reference.mjs';
import {capture,canonical} from './lib/compatibility.mjs';
import {writeFile} from 'node:fs/promises';
await withReferenceContainer(async(config,container)=>{
 const c=await connect(config);const cases=[];
 try {
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
  await writeFile(process.argv[2]??'tests/reference/storage-diagnostic.json',JSON.stringify({image:container.image,cases},null,2)+'\n');
  console.log('Captured',cases.length,'storage diagnostic cases');
 } finally {c.close();}
});
