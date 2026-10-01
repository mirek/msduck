#!/usr/bin/env node
// Owner-controlled SQL Server reference. Captured SQL and diagnostics are data.
import {mkdir} from 'node:fs/promises'
import {dirname,resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {canonical} from './lib/compatibility.mjs'
import {connect,assertSameCapture,refuseExistingFixture,writeNewFixture} from './lib/reference.mjs'
import {referenceImage,withReferenceContainer} from './lib/reference-container.mjs'
import {captureBatch} from './capture-order-token.mjs'

export const catalogViews=['foreign_keys','foreign_key_columns','key_constraints','default_constraints','check_constraints','computed_columns','triggers','sql_modules','procedures','database_files']
export const catalogSetup=`CREATE TABLE dbo.catalog_parent(a INT NOT NULL,b INT NOT NULL,label NVARCHAR(40),CONSTRAINT PK_catalog_parent PRIMARY KEY(a,b),CONSTRAINT UQ_catalog_parent_label UNIQUE(label));
CREATE TABLE dbo.catalog_child(a INT NOT NULL,b INT NOT NULL,state BIT CONSTRAINT DF_catalog_child_state DEFAULT(1),doubled AS(a*2) PERSISTED,CONSTRAINT PK_catalog_child PRIMARY KEY(a,b),CONSTRAINT FK_catalog_child_parent FOREIGN KEY(a,b) REFERENCES dbo.catalog_parent(a,b) ON DELETE CASCADE ON UPDATE CASCADE,CONSTRAINT CK_catalog_child_a CHECK(a>0)); CREATE INDEX IX_catalog_parent_label ON dbo.catalog_parent(label);`
export const catalogQueries=[
 ...catalogViews.map(view=>['empty schema '+view,`SELECT TOP(0) * FROM sys.${view}`]),
 ['constraint objects',"SELECT name,RTRIM(type) AS type,type_desc,OBJECT_NAME(parent_object_id) AS parent FROM sys.objects WHERE parent_object_id IN(OBJECT_ID('dbo.catalog_parent'),OBJECT_ID('dbo.catalog_child')) ORDER BY name"],
 ['key relationships',"SELECT name,RTRIM(type) AS type,OBJECT_NAME(parent_object_id) AS parent,unique_index_id,is_system_named FROM sys.key_constraints WHERE parent_object_id IN(OBJECT_ID('dbo.catalog_parent'),OBJECT_ID('dbo.catalog_child')) ORDER BY name"],
 ['foreign relationships',"SELECT name,OBJECT_NAME(parent_object_id) AS child,OBJECT_NAME(referenced_object_id) AS parent,key_index_id,is_disabled,is_not_for_replication,is_not_trusted,delete_referential_action,delete_referential_action_desc,update_referential_action,update_referential_action_desc,is_system_named FROM sys.foreign_keys WHERE name='FK_catalog_child_parent'"],
 ['foreign columns',"SELECT OBJECT_NAME(constraint_object_id) AS name,constraint_column_id,OBJECT_NAME(parent_object_id) AS child,COL_NAME(parent_object_id,parent_column_id) AS child_column,OBJECT_NAME(referenced_object_id) AS parent,COL_NAME(referenced_object_id,referenced_column_id) AS parent_column FROM sys.foreign_key_columns WHERE constraint_object_id=OBJECT_ID('dbo.FK_catalog_child_parent') ORDER BY constraint_column_id"],
 ['defaults',"SELECT name,OBJECT_NAME(parent_object_id) AS parent,parent_column_id,definition,is_system_named FROM sys.default_constraints WHERE name='DF_catalog_child_state'"],
 ['checks',"SELECT name,OBJECT_NAME(parent_object_id) AS parent,parent_column_id,definition,is_disabled,is_not_for_replication,is_not_trusted,uses_database_collation,is_system_named FROM sys.check_constraints WHERE name='CK_catalog_child_a'"],
 ['computed',"SELECT name,column_id,system_type_id,user_type_id,max_length,precision,scale,is_nullable,definition,uses_database_collation,is_persisted FROM sys.computed_columns WHERE object_id=OBJECT_ID('dbo.catalog_child')"],
 ['view definition',"SELECT OBJECT_DEFINITION(OBJECT_ID('dbo.catalog_view')) AS definition"],
 ['procedure definition',"SELECT definition,uses_ansi_nulls,uses_quoted_identifier,is_schema_bound,uses_database_collation,is_recompiled,null_on_null_input,execute_as_principal_id,uses_native_compilation,is_inlineable,inline_type FROM sys.sql_modules WHERE object_id=OBJECT_ID('dbo.catalog_proc')"],
 ['procedures',"SELECT name,RTRIM(type) AS type,type_desc,is_auto_executed,is_execution_replicated,is_repl_serializable,skips_repl_constraints FROM sys.procedures WHERE name='catalog_proc'"],
 ['triggers',"SELECT name,parent_class,parent_class_desc,OBJECT_NAME(parent_id) AS parent,RTRIM(type) AS type,type_desc,is_ms_shipped,is_disabled,is_not_for_replication,is_instead_of_trigger FROM sys.triggers WHERE name='catalog_trigger'"],
 ['pkeys named',"EXEC sys.sp_pkeys @table_name=N'catalog_parent',@table_owner=N'dbo'"],
 ['pkeys positional',"EXEC sys.sp_pkeys N'catalog_parent',N'dbo'"],
 ['pkeys missing',"EXEC sys.sp_pkeys N'absent',N'dbo'"],
 ['fkeys named',"EXEC sys.sp_fkeys @pktable_name=N'catalog_parent',@pktable_owner=N'dbo',@fktable_name=N'catalog_child',@fktable_owner=N'dbo'"],
 ['fkeys positional',"EXEC sys.sp_fkeys N'catalog_parent',N'dbo',NULL,N'catalog_child',N'dbo'"],
 ['database files',"SELECT file_id,type,type_desc,data_space_id,name,state,state_desc,max_size,growth,is_percent_growth FROM sys.database_files ORDER BY file_id"],
 ['pkeys missing parameter','EXEC sys.sp_pkeys'],
 ['fkeys missing parameters','EXEC sys.sp_fkeys'],
 ['rename missing object',"EXEC sys.sp_rename N'dbo.absent',N'renamed',N'OBJECT'"],
 ['rename invalid object type',"EXEC sys.sp_rename N'dbo.catalog_parent',N'renamed',N'INVALID'"],
 ['rename index',"EXEC sys.sp_rename N'dbo.catalog_parent.IX_catalog_parent_label',N'IX_catalog_parent_renamed',N'INDEX'"],
 ['renamed index',"SELECT name,index_id,is_unique,is_primary_key,is_unique_constraint FROM sys.indexes WHERE object_id=OBJECT_ID('dbo.catalog_parent') ORDER BY index_id"],
 ['rename table',"EXEC sys.sp_rename N'dbo.catalog_child',N'catalog_renamed'"],
 ['rename column',"EXEC sys.sp_rename N'dbo.catalog_renamed.a',N'new_a',N'COLUMN'"],
 ['rename constraint',"EXEC sys.sp_rename N'dbo.PK_catalog_child',N'PK_catalog_renamed',N'OBJECT'"],
 ['rename module',"EXEC sys.sp_rename N'dbo.catalog_proc',N'catalog_renamed_proc',N'OBJECT'"],
 ['retained renamed module',"SELECT OBJECT_DEFINITION(OBJECT_ID('dbo.catalog_renamed_proc')) AS definition"],
 ['renamed relations',"SELECT fk.name,OBJECT_NAME(fk.parent_object_id) AS child,COL_NAME(c.parent_object_id,c.parent_column_id) AS child_column FROM sys.foreign_keys fk JOIN sys.foreign_key_columns c ON c.constraint_object_id=fk.object_id WHERE fk.name='FK_catalog_child_parent' ORDER BY c.constraint_column_id"],
]

export async function observeCatalog(connection){
 const records=[]
 const record=async(name,sql)=>{
  const result=canonical(await captureBatch(connection,sql))
  records.push({name,sql,result})
  return result
 }
 await record('version',"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version")
 for(const [name,sql] of [
  ['setup tables',catalogSetup],
  ['setup view','CREATE VIEW dbo.catalog_view AS SELECT a,b,label FROM dbo.catalog_parent'],
  ['setup procedure','CREATE PROCEDURE dbo.catalog_proc AS SELECT a,b FROM dbo.catalog_parent'],
  ['setup trigger','CREATE TRIGGER dbo.catalog_trigger ON dbo.catalog_child AFTER INSERT AS BEGIN SET NOCOUNT ON; END'],
 ]){
  const result=await record(name,sql)
  if(result.errors.length)throw Error(name+': '+JSON.stringify(result.errors).slice(0,800))
 }
 for(const [name,sql] of catalogQueries)await record(name,sql)
 return records
}

async function referenceRun(){
 return withReferenceContainer(async(config,metadata)=>{
  assertSameCapture(metadata.image,referenceImage,'pinned reference image')
  const admin=await connect(config)
  try {
   const result=await captureBatch(admin,'CREATE DATABASE msduck_catalog_reference')
   if(result.errors.length)throw Error('reference database creation failed')
  }finally{admin.close()}
  const connection=await connect({...config,options:{...config.options,database:'msduck_catalog_reference'}})
  try{return await observeCatalog(connection)}finally{connection.close()}
 },{image:referenceImage})
}

async function main(args){
 if(args.length>1)throw Error('usage: capture-gaps-catalog.mjs [new-output.json]')
 const output=resolve(args[0]??'reference/gaps-catalog.json')
 await refuseExistingFixture(output)
 const runs=[await referenceRun(),await referenceRun()]
 // Clock/ID/file values are retained in both raw runs. Only the complete empty
 // view responses are asserted equal here; no dynamic differences are erased.
 for(const view of catalogViews){
  const name='empty schema '+view
  const results=runs.map(run=>run.find(record=>record.name===name)?.result)
  if(results.some(result=>!result||result.errors.length||result.sets.length!==1||result.sets[0].rows.length))throw Error('missing schema capture: '+view)
  assertSameCapture(results[0],results[1],name)
 }
 await mkdir(dirname(output),{recursive:true})
 await writeNewFixture(output,{image:referenceImage,contract:'Complete empty schemas agree; other responses retained raw in both runs without normalization.',runs})
 console.log(JSON.stringify({output,records:runs.map(run=>run.length),schemaControls:catalogViews.length}))
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await main(process.argv.slice(2))
