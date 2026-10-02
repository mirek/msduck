#!/usr/bin/env node
// Owner-controlled assignment evidence; captured SQL/diagnostics are data.
import {mkdir} from 'node:fs/promises'
import {dirname,resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {TYPES} from 'tedious'
import {canonical} from './lib/compatibility.mjs'
import {connect,assertSameCapture,refuseExistingFixture,writeNewFixture} from './lib/reference.mjs'
import {referenceImage,withReferenceContainer} from './lib/reference-container.mjs'
import {captureBatch,captureRpc} from './capture-order-token.mjs'

const guid='00112233-4455-6677-8899-aabbccddeeff'
export const plan=[
 ['version',"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version"],
 ['setup','CREATE TABLE dbo.guid_assignments(id INT PRIMARY KEY,g UNIQUEIDENTIFIER)'],
 ['canonical varchar',`INSERT dbo.guid_assignments VALUES(1,'${guid}')`],
 ['braced nvarchar',`INSERT dbo.guid_assignments VALUES(2,N'{${guid}}')`],
 ['suffix varchar',`INSERT dbo.guid_assignments VALUES(3,'${guid}EXTRA')`],
 ['braced suffix nvarchar',`INSERT dbo.guid_assignments VALUES(4,N'{${guid}}EXTRA')`],
 ['typed null','INSERT dbo.guid_assignments VALUES(5,CAST(NULL AS NVARCHAR(100)))'],
 ['typed guid',`INSERT dbo.guid_assignments VALUES(6,CAST('${guid}' AS UNIQUEIDENTIFIER))`],
 ['malformed insert',"INSERT dbo.guid_assignments VALUES(10,N'not-a-guid')"],
 ['multirow malformed insert',`INSERT dbo.guid_assignments VALUES(11,'${guid}'),(12,'bad')`],
 ['insert readback','SELECT id,g FROM dbo.guid_assignments ORDER BY id'],
 ['valid update',`UPDATE dbo.guid_assignments SET g=N'{${guid}}EXTRA' WHERE id=5`],
 ['multirow malformed update',`UPDATE dbo.guid_assignments SET g=CASE WHEN id=1 THEN '${guid}' ELSE 'bad' END WHERE id IN(1,2)`],
 ['update readback','SELECT id,g FROM dbo.guid_assignments ORDER BY id'],
 ['transaction begin','BEGIN TRANSACTION'],
 ['transaction state after begin','SELECT @@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state,SESSIONPROPERTY(\'ANSI_WARNINGS\') AS ansi_warnings,@@OPTIONS&16384 AS xact_abort'],
 ['transaction prior write',`INSERT dbo.guid_assignments VALUES(20,'${guid}')`],
 ['transaction state after prior write','SELECT @@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state'],
 ['transaction malformed insert',"INSERT dbo.guid_assignments VALUES(21,'bad')"],
 ['transaction state','SELECT @@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state'],
 ['transaction commit','COMMIT'],
 ['transaction readback','SELECT id,g FROM dbo.guid_assignments ORDER BY id'],
 ['caught conversion transaction',`BEGIN TRANSACTION; INSERT dbo.guid_assignments VALUES(30,'${guid}'); BEGIN TRY INSERT dbo.guid_assignments VALUES(31,'bad'); END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS error_number,ERROR_STATE() AS error_state,ERROR_MESSAGE() AS error_message,@@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state; END CATCH; IF @@TRANCOUNT>0 COMMIT; SELECT id,g FROM dbo.guid_assignments WHERE id IN(30,31) ORDER BY id`],
 ['caught conversion readback','SELECT @@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state; SELECT id,g FROM dbo.guid_assignments WHERE id IN(30,31) ORDER BY id'],
 ['parameter varchar','INSERT dbo.guid_assignments VALUES(40,@g)',{type:'VarChar',value:guid+'EXTRA',length:100}],
 ['parameter nvarchar','INSERT dbo.guid_assignments VALUES(41,@g)',{type:'NVarChar',value:'{'+guid+'}EXTRA',length:100}],
 ['parameter guid','INSERT dbo.guid_assignments VALUES(42,@g)',{type:'UniqueIdentifier',value:guid}],
 ['parameter null','INSERT dbo.guid_assignments VALUES(43,@g)',{type:'NVarChar',value:null,length:100}],
 ['parameter malformed','INSERT dbo.guid_assignments VALUES(44,@g)',{type:'NVarChar',value:'bad',length:100}],
 ['parameter readback','SELECT id,g FROM dbo.guid_assignments WHERE id>=40 ORDER BY id'],
 ['alter setup',`CREATE TABLE dbo.guid_alter(id INT PRIMARY KEY,g VARCHAR(100)); INSERT dbo.guid_alter VALUES(1,'{${guid}}EXTRA'),(2,NULL)`],
 ['valid alter','ALTER TABLE dbo.guid_alter ALTER COLUMN g UNIQUEIDENTIFIER NULL'],
 ['alter readback','SELECT id,g FROM dbo.guid_alter ORDER BY id'],
 ['invalid alter setup',`CREATE TABLE dbo.guid_alter_bad(id INT PRIMARY KEY,g VARCHAR(100)); INSERT dbo.guid_alter_bad VALUES(1,'${guid}'),(2,'bad')`],
 ['invalid alter','ALTER TABLE dbo.guid_alter_bad ALTER COLUMN g UNIQUEIDENTIFIER NULL'],
 ['invalid alter readback','SELECT id,g FROM dbo.guid_alter_bad ORDER BY id'],
]
const failures=new Set(['malformed insert','multirow malformed insert','multirow malformed update','transaction malformed insert','transaction commit','caught conversion transaction','parameter malformed','invalid alter'])
async function parameterCapture(connection,sql,parameter){
 // Supply typed parameters to the existing full token observer's Request.
 // The temporary hook belongs to this single sequential connection only.
 const execSql=connection.execSql
 connection.execSql=function(request){
  request.addParameter('g',TYPES[parameter.type],parameter.value,parameter.length?{length:parameter.length}:{})
  return execSql.call(this,request)
 }
 try{return await captureRpc(connection,sql)}finally{connection.execSql=execSql}
}
async function run(){
 return withReferenceContainer(async(config,metadata)=>{
  assertSameCapture(metadata.image,referenceImage,'pinned image')
  const connection=await connect(config)
  try{
   const records=[]
   for(const [name,sql,parameter] of plan){
    const result=canonical(await (parameter?parameterCapture(connection,sql,parameter):captureBatch(connection,sql)))
    records.push({name,sql,...(parameter?{parameter}:{}),result})
   }
   return records
  }finally{connection.close()}
 },{image:referenceImage})
}
async function main(args){
 if(args.length!==1)throw Error('usage: capture-guid-assignment.mjs new-output.json')
 const output=resolve(args[0]);await refuseExistingFixture(output)
 const runs=[await run(),await run()]
 assertSameCapture(runs[0],runs[1],'complete GUID assignment observations')
 await mkdir(dirname(output),{recursive:true})
 await writeNewFixture(output,{image:referenceImage,contract:'Two fresh complete GUID assignment runs agree without normalization.',runs})
 const unexpected=runs[0].filter(record=>Boolean(record.result.errors.length)!==failures.has(record.name))
 // Preserve every observation before rejecting a mistaken success/error control.
 if(unexpected.length)throw Error('Retained unexpected outcomes: '+JSON.stringify(unexpected.map(({name,result})=>({name,errors:result.errors}))))
 console.log(JSON.stringify({output,records:runs.map(run=>run.length)}))
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await main(process.argv.slice(2))
