#!/usr/bin/env node
// Owner-controlled assignment evidence; captured SQL/diagnostics are data.
import {mkdir} from 'node:fs/promises'
import {dirname,resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {canonical} from './lib/compatibility.mjs'
import {connect,assertSameCapture,refuseExistingFixture,writeNewFixture} from './lib/reference.mjs'
import {referenceImage,withReferenceContainer} from './lib/reference-container.mjs'
import {captureBatch} from './capture-order-token.mjs'

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
 ['transaction prior write',`INSERT dbo.guid_assignments VALUES(20,'${guid}')`],
 ['transaction malformed insert',"INSERT dbo.guid_assignments VALUES(21,'bad')"],
 ['transaction state','SELECT @@TRANCOUNT AS transaction_count,XACT_STATE() AS transaction_state'],
 ['transaction commit','COMMIT'],
 ['transaction readback','SELECT id,g FROM dbo.guid_assignments ORDER BY id'],
 ['alter setup',`CREATE TABLE dbo.guid_alter(id INT PRIMARY KEY,g VARCHAR(100)); INSERT dbo.guid_alter VALUES(1,'{${guid}}EXTRA'),(2,NULL)`],
 ['valid alter','ALTER TABLE dbo.guid_alter ALTER COLUMN g UNIQUEIDENTIFIER NULL'],
 ['alter readback','SELECT id,g FROM dbo.guid_alter ORDER BY id'],
 ['invalid alter setup',`CREATE TABLE dbo.guid_alter_bad(id INT PRIMARY KEY,g VARCHAR(100)); INSERT dbo.guid_alter_bad VALUES(1,'${guid}'),(2,'bad')`],
 ['invalid alter','ALTER TABLE dbo.guid_alter_bad ALTER COLUMN g UNIQUEIDENTIFIER NULL'],
 ['invalid alter readback','SELECT id,g FROM dbo.guid_alter_bad ORDER BY id'],
]
const failures=new Set(['malformed insert','multirow malformed insert','multirow malformed update','transaction malformed insert','invalid alter'])
async function run(){
 return withReferenceContainer(async(config,metadata)=>{
  assertSameCapture(metadata.image,referenceImage,'pinned image')
  const connection=await connect(config)
  try{
   const records=[]
   for(const [name,sql] of plan){
    const result=canonical(await captureBatch(connection,sql))
    if(Boolean(result.errors.length)!==failures.has(name))throw Error(name+': unexpected outcome '+JSON.stringify(result.errors))
    records.push({name,sql,result})
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
 console.log(JSON.stringify({output,records:runs.map(run=>run.length)}))
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await main(process.argv.slice(2))
