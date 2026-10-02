#!/usr/bin/env node
// Owner-controlled assignment evidence; captured SQL/diagnostics are data.
import {mkdir,readFile} from 'node:fs/promises'
import {createHash} from 'node:crypto'
import {dirname,resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {TYPES} from 'tedious'
import {canonical,differences} from './lib/compatibility.mjs'
import {connect,assertSameCapture,refuseExistingFixture,writeNewFixture} from './lib/reference.mjs'
import {referenceImage,withReferenceContainer} from './lib/reference-container.mjs'
import {captureBatch,captureRpc} from './capture-order-token.mjs'
import {start} from '../tests/support/client.mjs'

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
export function verifyObservations(records){
 const result=name=>{
  const record=records.find(record=>record.name===name)
  if(!record)throw Error('Missing GUID assignment observation: '+name)
  return record.result
 }
 const rows=name=>result(name).sets.map(set=>set.rows)
 for(const record of records){
  if(Boolean(record.result.errors.length)!==failures.has(record.name))throw Error('Unexpected outcome: '+record.name)
  if(failures.has(record.name)&&!['transaction commit','caught conversion transaction'].includes(record.name)){
   assertSameCapture(record.result.errors,[{number:8169,state:2,class:16,lineNumber:1,message:'Conversion failed when converting from a character string to uniqueidentifier.'}],record.name+' diagnostics')
  }
 }
 assertSameCapture(rows('transaction state after begin'),[[[1,1,1,0]]],'transaction begin and XACT_ABORT OFF')
 assertSameCapture(rows('transaction state after prior write'),[[[1,1]]],'transaction prior write remains active')
 assertSameCapture(rows('transaction state'),[[[0,0]]],'conversion aborts explicit transaction')
 assertSameCapture(rows('transaction readback'),rows('update readback'),'aborted transaction preserves pre-transaction rows only')
 assertSameCapture(rows('caught conversion readback'),[[[0,0]],[]],'caught failed commit rolls back prior write')
 assertSameCapture(result('transaction commit').errors.map(error=>error.number),[3902],'COMMIT after rollback')
 assertSameCapture(result('caught conversion transaction').errors.map(error=>error.number),[3930,3998],'uncommittable transaction completion')
 assertSameCapture(rows('invalid alter readback'),[[[1,guid],[2,'bad']]],'failed ALTER preserves source storage')
 const upper='00112233-4455-6677-8899-AABBCCDDEEFF'
 assertSameCapture(rows('parameter readback'),[[[40,upper],[41,upper],[42,upper],[43,null]]],'typed parameter assignments')
}
export async function observe(connection){
 const records=[]
 for(const [name,sql,parameter] of plan){
  const result=canonical(await (parameter?parameterCapture(connection,sql,parameter):captureBatch(connection,sql)))
  records.push({name,sql,...(parameter?{parameter}:{}),result})
 }
 return records
}
async function replay(output){
 await refuseExistingFixture(output)
 const raw=await readFile(new URL('../reference/guid-assignment.json',import.meta.url))
 const reference=JSON.parse(raw)
 assertSameCapture(reference.runs[0],reference.runs[1],'retained reference runs')
 verifyObservations(reference.runs[0])
 const cleanup=[]
 try{
  // A new ephemeral listener/database belongs to this invocation. No shared
  // test port, supplementary setup or reference normalization is used.
  const connection=await start({after:callback=>cleanup.push(callback)})
  const records=await observe(connection)
  const delta=differences(records,reference.runs[0])
  await mkdir(dirname(output),{recursive:true})
  await writeNewFixture(output,{
   contract:'Complete local replay; raw differences are compatibility gaps, not a passing comparison.',
   referenceSha256:createHash('sha256').update(raw).digest('hex'),records,differences:delta,
  })
  console.log(JSON.stringify({output,records:records.length,differences:delta.length}))
 }finally{for(const callback of cleanup.reverse())await callback()}
}
async function run(){
 return withReferenceContainer(async(config,metadata)=>{
  assertSameCapture(metadata.image,referenceImage,'pinned image')
  const connection=await connect(config)
  try{return await observe(connection)}finally{connection.close()}
 },{image:referenceImage})
}
async function main(args){
 if(args.length===2&&args[0]==='--replay')return replay(resolve(args[1]))
 if(args.length!==1)throw Error('usage: capture-guid-assignment.mjs [--replay] new-output.json')
 const output=resolve(args[0]);await refuseExistingFixture(output)
 const runs=[await run(),await run()]
 assertSameCapture(runs[0],runs[1],'complete GUID assignment observations')
 await mkdir(dirname(output),{recursive:true})
 await writeNewFixture(output,{image:referenceImage,contract:'Two fresh complete GUID assignment runs agree without normalization.',runs})
 // Preserve every observation before rejecting a mistaken success/error control.
 verifyObservations(runs[0])
 console.log(JSON.stringify({output,records:runs.map(run=>run.length)}))
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await main(process.argv.slice(2))
