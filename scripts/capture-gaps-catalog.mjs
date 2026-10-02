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
CREATE TABLE dbo.catalog_child(a INT NOT NULL,b INT NOT NULL,state BIT CONSTRAINT DF_catalog_child_state DEFAULT(1),doubled AS(a*2) PERSISTED,CONSTRAINT PK_catalog_child PRIMARY KEY(a,b),CONSTRAINT FK_catalog_child_parent FOREIGN KEY(a,b) REFERENCES dbo.catalog_parent(a,b) ON DELETE CASCADE ON UPDATE CASCADE,CONSTRAINT CK_catalog_child_a CHECK(a>0)); CREATE INDEX IX_catalog_parent_label ON dbo.catalog_parent(label); CREATE TABLE dbo.catalog_rename_target(id INT,label NVARCHAR(20));`
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
 ['procedures',"SELECT name,RTRIM(type) AS type,type_desc,is_auto_executed,is_execution_replicated,is_repl_serializable_only,skips_repl_constraints FROM sys.procedures WHERE name='catalog_proc'"],
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
 ['rename plain column',"EXEC sys.sp_rename N'dbo.catalog_rename_target.label',N'renamed_label',N'COLUMN'"],
 ['renamed column',"SELECT name,column_id,system_type_id,max_length FROM sys.columns WHERE object_id=OBJECT_ID('dbo.catalog_rename_target') ORDER BY column_id"],
 ['rename column with enforced dependencies',"EXEC sys.sp_rename N'dbo.catalog_renamed.a',N'new_a',N'COLUMN'"],
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

// Exercise declaration text independently of stored values: clocks and NEWID
// are never evaluated by these metadata queries.
export const definitionDefaults=[
 ['positive','INT','1'],['negative','INT','-1'],['zeros','INT','0001'],
 ['decimal','DECIMAL(10,2)','1.25'],['exponent','FLOAT','1e2'],
 ['string','VARCHAR(30)',"'a''b'"],['unicode','NVARCHAR(30)',"N'a''b'"],
 ['null','INT','NULL'],['clock','DATETIME','GETDATE()'],
 ['cast','BIGINT','CAST(1 AS BIGINT)'],['convert','INT',"CONVERT(INT,'2')"],
 ['uuid','UNIQUEIDENTIFIER','NEWID()'],['binary','VARBINARY(4)','0x01'],
 ['sum','INT','1+2'],['nested','INT','(((1)))'],
]
export const definitionComputed=[
 ['multiply','a*2'],['add','a+2'],['nested','a*(b+1)'],['divide','a/2'],
 ['coalesce','COALESCE(b,0)'],['cast','CAST(a AS BIGINT)'],['isnull','ISNULL(b,0)'],
 ['case','CASE WHEN a>0 THEN a ELSE 0 END'],['lower','LOWER(label)'],
]
export const definitionSetup=[
 ['setup default definitions',`CREATE TABLE dbo.definition_defaults(${definitionDefaults.map(([name,type,expr])=>`d_${name} ${type} CONSTRAINT DF_definition_${name} DEFAULT(${expr})`).join(',')})`],
 ['setup computed definitions',`CREATE TABLE dbo.definition_computed(a INT NOT NULL,b INT NULL,label VARCHAR(20),${definitionComputed.map(([name,expr])=>`c_${name} AS(${expr})`).join(',')})`],
]
export const definitionQueries=[
 ['empty schema default_constraints','SELECT TOP(0) * FROM sys.default_constraints'],
 ['empty schema computed_columns','SELECT TOP(0) * FROM sys.computed_columns'],
 ['default definitions',"SELECT name,parent_column_id,definition,is_system_named FROM sys.default_constraints WHERE parent_object_id=OBJECT_ID('dbo.definition_defaults') ORDER BY parent_column_id"],
 ['computed definitions',"SELECT name,column_id,system_type_id,user_type_id,max_length,precision,scale,is_nullable,definition,uses_database_collation,is_persisted FROM sys.computed_columns WHERE object_id=OBJECT_ID('dbo.definition_computed') ORDER BY column_id"],
 ['default object definitions',"SELECT name,OBJECT_DEFINITION(object_id) AS definition FROM sys.default_constraints WHERE parent_object_id=OBJECT_ID('dbo.definition_defaults') ORDER BY parent_column_id"],
]

export async function observeDefinitions(connection){
 const records=[]
 for(const [name,sql] of [
  ['version',"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version"],
  ...definitionSetup,...definitionQueries,
 ]){
  const result=canonical(await captureBatch(connection,sql))
  if(result.errors.length)throw Error(name+': '+JSON.stringify(result.errors).slice(0,800))
  records.push({name,sql,result})
 }
 return records
}

export const namespaceQueries=[
 ['version',"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version"],
 ['namespace table setup','CREATE TABLE dbo.namespace_defaults(id INT CONSTRAINT DF_namespace DEFAULT(1))'],
 ['existing table default precedence','CREATE TABLE dbo.namespace_defaults(id INT CONSTRAINT DF_namespace DEFAULT(1))'],
 ['namespace view setup','CREATE VIEW dbo.namespace_view AS SELECT 1 AS id'],
 ['existing view default precedence','CREATE TABLE dbo.namespace_view(id INT CONSTRAINT DF_namespace DEFAULT(1))'],
 ['existing default namespace collision','CREATE TABLE dbo.namespace_failed(id INT CONSTRAINT DF_namespace DEFAULT(1))'],
 ['namespace key setup','CREATE TABLE dbo.namespace_key(id INT CONSTRAINT PK_namespace PRIMARY KEY)'],
 ['existing key default collision','CREATE TABLE dbo.namespace_failed_key(id INT CONSTRAINT PK_namespace DEFAULT(1))'],
 ['namespace check setup','CREATE TABLE dbo.namespace_check(id INT CONSTRAINT CK_namespace CHECK(id>0))'],
 ['existing check default collision','CREATE TABLE dbo.namespace_failed_check(id INT CONSTRAINT CK_namespace DEFAULT(1))'],
]
export async function observeNamespace(connection){
 const records=[]
 for(const [name,sql] of namespaceQueries){records.push({name,sql,result:canonical(await captureBatch(connection,sql))})}
 return records
}

// The gaps-catalog-v2 profile: CHECK/DEFAULT/computed definition text,
// sp_rename, sp_pkeys and sp_fkeys contracts, module catalogs,
// INFORMATION_SCHEMA and key layout. Queries avoid IDs and clocks, so both
// fresh runs must agree completely. Statements expected to fail are marked.
const checkList=(table,checks)=>`CREATE TABLE dbo.${table}(a INT,b INT,c INT,s VARCHAR(20),n NVARCHAR(20),d DATETIME,m DECIMAL(10,2),${checks.map(([name,expr])=>`CONSTRAINT ${name} CHECK (${expr})`).join(',')})`
export const v2Checks=[
 ['k01','a IN (1,2,3)'],['k02',"s IN ('x','y')"],['k03',"s LIKE 'a%'"],['k04','a BETWEEN 1 AND 10'],['k05','a > 0 AND b < 5'],
 ['k06','(a > 0 OR b > 0) AND a <> 3'],['k07','NOT a = 1'],['k08','a IS NOT NULL'],['k09','b IS NULL OR b >= a'],['k10','len(s) > 2'],
 ['k11','upper(s) = s'],['k12','a % 2 = 0'],['k13','a != 4'],['k14','d < dateadd(day, 1, getdate())'],['k15','-a < 0'],
 ['k16',"s + 'x' <> 'yx'"],['k17','a NOT IN (7,8)'],['k18','a NOT BETWEEN 100 AND 200'],['k19',"s NOT LIKE 'z%'"],['k20','m > 1.5 AND m < -2.25e1'],
 ['k21','CASE a WHEN 1 THEN 1 ELSE 0 END = 1'],['k22','CAST(s AS INT) > 0'],['k23',"CONVERT(VARCHAR(10), d, 120) > '2000'"],['k24',"n = N'abc'"],['k25','a & 1 = 0'],
 ['k26',"CAST(a AS DECIMAL(5,2)) > 0 AND CAST(s AS NVARCHAR(MAX)) <> '' AND CAST(d AS DATETIME2(3)) > '2000-01-01'"],['k27','datediff(day, d, getdate()) < 5 AND year(d) > 2000'],
 ['k29','coalesce(a, b, 0) >= 0 AND isnull(b, 0) >= 0 AND nullif(a, 0) > 0'],['k30',"s LIKE '%[0-9]%' ESCAPE '\\'"],['k31','[a] > (0)'],['k32','a = 1 OR a = 2 OR a = 3'],
 ['k34','a > 0 AND (b > 0 OR s IS NULL) AND (b < 10 OR a < 5)'],['k35',"lower(s) = 'x' COLLATE Latin1_General_BIN"],['k36','a = +1'],['k37','~a = 0'],
 ['k38','a * (b + 1) - 2 / (a - 1) > 0'],['k40',"s = 'it''s'"],['k42','a = 0x10'],['k43','TRY_CAST(s AS INT) IS NOT NULL'],['k44','iif(a > 0, 1, 0) = 1'],
 ['k45',"left(s, 2) = 'ab' AND substring(s, 1, 2) = 'ab' AND charindex('a', s) > 0 AND ltrim(rtrim(s)) <> '' AND replace(s, 'a', 'b') <> ''"],
 ['k46','abs(a) < 10 AND round(m, 1) > 0 AND power(a, 2) < 100'],['k47','a IN (1)'],
 ['k48',"CAST(a AS BIT) = 1 AND CAST(a AS FLOAT) > 0 AND CAST(a AS VARCHAR) <> '' AND CAST(d AS DATE) > '2000' AND CAST(a AS NUMERIC) > 0"],
 ['k49','a = 1.0 AND m = 0.50 AND m = .5 AND m = 5. AND a = 1e0 AND a = 2147483648 AND a = -0'],
 ['k50','a - (b - c) > 0'],['k51','(a - b) - c > 0'],['k52','a + b + c > 0'],['k53','a * b * c > 0'],['k54','a / (b * c) > 0'],['k55','(a + b) * c > 0'],
 ['k56','a = b + c'],['k57','a + b = c'],['k58','a = b'],['k59','a = (b)'],['k60','((a > 0))'],['k61','a > 0 OR b > 0 OR c > 0'],['k62','a > 0 AND b > 0 OR c > 0'],
 ['k63','a > 0 AND (b > 0 OR c > 0)'],['k64','NOT (a > 0 AND b > 0)'],['k65','a IN (1, b, c + 1)'],['k66','-(a + b) < 0'],['k67','a - -1 > 0'],['k68',"s + 'x' + s <> 'y'"],
 ['k69','a | b ^ c > 0'],['k70','a NOT IN (1) AND NOT (a IN (2,3))'],
 ['q01','-(a*b) < 0'],['q03','a*-b < 0'],['q04','~(a+b) = 0'],['q05','abs(a+b) < 10 AND isnull(a+1,0) > 0 AND abs(a*b) < 5'],['q06','CAST(a+1 AS bigint) > 0 AND CAST(a*2 AS bigint) > 0'],
 ['q07','CASE WHEN a+1>0 THEN a+1 ELSE a*2 END > 0'],['q08','(a > 0 AND b > 0) AND c > 0'],['q09','a > 0 OR (b > 0 OR c > 0)'],['q10','a BETWEEN 1 AND 2 AND b > 0'],
 ['q11','a IN (1,2) AND b > 0'],['q12','b > 0 OR a IN (1,2)'],['q13','NOT (a IS NULL)'],['q14','NOT NOT a > 0'],['q15','a BETWEEN b+1 AND c*2'],
 ['q16',"CONVERT(NVARCHAR(20), a) <> '' AND CONVERT(VARBINARY(MAX), s) <> 0x AND CONVERT(CHAR(1), s) <> '' AND CONVERT(TIME(7), d) > '00:00' AND CONVERT(FLOAT(53), a) > 0 AND CONVERT(REAL, a) > 0 AND CONVERT(SMALLINT,a) > 0 AND CONVERT(TINYINT,a) >= 0 AND CONVERT(DATETIMEOFFSET(2), d) > '2000' AND CONVERT(UNIQUEIDENTIFIER, s) IS NULL AND CONVERT(MONEY, a) > 0 AND CONVERT(SMALLDATETIME, d) > '2000' AND CONVERT(VARCHAR(MAX), a) <> '' AND CONVERT(DECIMAL, a) > 0 AND CONVERT(NCHAR(2), s) <> '' AND CONVERT(BINARY(4), a) <> 0x00 AND CONVERT(DECIMAL(5), a) > 0"],
 ['q17',"s COLLATE Latin1_General_CS_AS = 'x'"],['q18',"a > 0 AND s LIKE 'x%' AND b IS NULL"],['q19','CASE WHEN a > 0 AND b > 0 THEN 1 WHEN a IS NULL THEN 2 END = 1'],
 ['q20',"dateadd(month, -1, d) < d AND datepart(dw, d) > 0 AND datename(month, d) <> '' AND eomonth(d) > d"],['q21','a = 1 AND b = 2 OR a = 3 AND b = 4'],
 ['q22',"s = '' AND s <> N'' AND s IS NOT NULL"],['q23','a > b - 1'],['q24','a - 1 > b'],['q25','a + b * c > 0'],['q26','a * b + c > 0'],['q27','a % b * c > 0'],
 ['q28',"len(s + 'x') > 0"],['q29','a > 0 AND NOT b > 0'],['q30','try_convert(int, s) > 0'],['q31',"concat(s, 'x', a) <> '' AND iif(a > 0, 'y', 'n') = 'y'"],
 ['q32','cast(cast(a AS bigint) AS int) > 0'],
]
export const v2Defaults=[
 ['a1','DATETIME','(getutcdate())'],['a2','NVARCHAR(50)','suser_sname()'],['a3','VARCHAR(5)',"''"],['a4','NVARCHAR(5)',"N''"],['a5','DECIMAL(5,1)','0.0'],
 ['a6','DECIMAL(5,1)','-1.5'],['a7','INT','(0)'],['a9','DATETIME','CURRENT_TIMESTAMP'],['a10','VARCHAR(10)',"'abc' + 'd'"],
 ['a11','INT','1 * 2'],['a12','INT','-(1)'],['a13','INT','1 + 2 * 3'],['a14','BIT','0'],['a15','FLOAT','1.5e0'],['a16','DATETIME2','sysutcdatetime()'],['a17','INT','NULL'],
 ['a18','VARCHAR(10)',"(('x'))"],['a19','INT','((1)+(2))'],['a20','BIGINT','9223372036854775807'],['a21','INT','+5'],['a23','DATE',"'2020-01-01'"],
 ['a24','INT','1 - 2 - 3'],['a25','INT','2 * (3 + 4)'],
]
export const v2Statements=[
 // Definition text.
 ['setup checks one',checkList('v2_checks_a',v2Checks.slice(0,40))],
 ['setup checks two',checkList('v2_checks_b',v2Checks.slice(40,80))],
 ['setup checks three',checkList('v2_checks_c',v2Checks.slice(80))],
 ['check definitions',"SELECT name,definition,uses_database_collation FROM sys.check_constraints WHERE parent_object_id IN (OBJECT_ID('dbo.v2_checks_a'),OBJECT_ID('dbo.v2_checks_b'),OBJECT_ID('dbo.v2_checks_c')) ORDER BY name"],
 ['check object definitions',"SELECT name,OBJECT_DEFINITION(object_id) AS definition FROM sys.check_constraints WHERE parent_object_id=OBJECT_ID('dbo.v2_checks_a') ORDER BY name"],
 ['setup defaults',`CREATE TABLE dbo.v2_defaults(${v2Defaults.map(([name,type,expr])=>`${name} ${type} DEFAULT ${expr}`).join(',')},named INT CONSTRAINT DF_v2_named DEFAULT 7)`],
 ['default definitions',"SELECT c.name AS column_name,LEFT(d.name,4) AS prefix,d.definition,d.is_system_named,COLUMNPROPERTY(d.parent_object_id,c.name,'ColumnId') AS column_id FROM sys.default_constraints d JOIN sys.columns c ON c.object_id=d.parent_object_id AND c.column_id=d.parent_column_id WHERE d.parent_object_id=OBJECT_ID('dbo.v2_defaults') ORDER BY d.parent_column_id"],
 ['setup default naming','CREATE TABLE dbo.abcdefghijklmnopqrstuvwxyz(colabcdefghij INT DEFAULT 1, c INT DEFAULT 2, ccccc INT DEFAULT 3, cccccc INT DEFAULT 4); CREATE TABLE dbo.t(colabcdefghij INT DEFAULT 1, c INT DEFAULT 2); CREATE TABLE dbo.tabletwelve(colabcdefghij INT DEFAULT 1, ab INT DEFAULT 2)'],
 // Generated names end with the object ID in hexadecimal (8 digits).
 ['default naming',"SELECT OBJECT_NAME(parent_object_id) AS table_name,COL_NAME(parent_object_id,parent_column_id) AS column_name,LEFT(name,LEN(name)-8) AS prefix,LEN(name) AS length FROM sys.default_constraints WHERE parent_object_id IN (OBJECT_ID('dbo.abcdefghijklmnopqrstuvwxyz'),OBJECT_ID('dbo.t'),OBJECT_ID('dbo.tabletwelve'),OBJECT_ID('dbo.v2_defaults')) ORDER BY table_name,parent_column_id"],
 ['default column links',"SELECT COUNT(*) AS linked FROM sys.columns c JOIN sys.default_constraints d ON d.object_id=c.default_object_id WHERE c.object_id=OBJECT_ID('dbo.v2_defaults')"],
 ['setup constraint naming','CREATE TABLE dbo.ck_parent(id INT PRIMARY KEY); CREATE TABLE dbo.abcdefghijklmnopqrstuvwxyz_ck(colabcdefghij INT CHECK (colabcdefghij > 0), y INT CHECK (y > 0), z INT, refabcdefghij INT REFERENCES dbo.ck_parent(id), r INT REFERENCES dbo.ck_parent(id), CHECK (z > 0 AND y > 0))'],
 ['constraint naming',"SELECT RTRIM(type) AS type,LEFT(name,LEN(name)-8) AS prefix,LEN(name) AS length FROM sys.objects WHERE parent_object_id=OBJECT_ID('dbo.abcdefghijklmnopqrstuvwxyz_ck') AND RTRIM(type) IN ('C','F') ORDER BY prefix"],
 ['setup alter defaults',"CREATE TABLE dbo.v2_alter(id INT, v INT); ALTER TABLE dbo.v2_alter ADD CONSTRAINT DF_v2_alter_v DEFAULT (5) FOR v; ALTER TABLE dbo.v2_alter ADD w INT DEFAULT -2; ALTER TABLE dbo.v2_alter ADD DEFAULT 9 FOR id"],
 ['alter defaults',"SELECT COL_NAME(parent_object_id,parent_column_id) AS column_name,CASE WHEN is_system_named=1 THEN LEFT(name,13) ELSE name END AS name,definition,is_system_named FROM sys.default_constraints WHERE parent_object_id=OBJECT_ID('dbo.v2_alter') ORDER BY parent_column_id"],
 ['setup computed','CREATE TABLE dbo.v2_computed(a INT NOT NULL,b INT,s VARCHAR(10),d DATETIME,x1 AS a + b,x2 AS (a * b) PERSISTED,x4 AS CAST(a AS VARCHAR(10)),x5 AS -a,x6 AS a,x7 AS 1,x8 AS year(d),x9 AS ISNULL(b, a) PERSISTED NOT NULL,x10 AS CASE WHEN a IN (1,2) THEN 1 END)'],
 ['computed definitions',"SELECT name,column_id,system_type_id,max_length,precision,scale,is_nullable,definition,is_persisted,uses_database_collation FROM sys.computed_columns WHERE object_id=OBJECT_ID('dbo.v2_computed') ORDER BY column_id"],
 ['setup computed text',"CREATE TABLE dbo.v2_computed_text(s VARCHAR(10),x3 AS s + 'x')"],
 ['computed text definitions',"SELECT name,column_id,system_type_id,max_length,is_nullable,definition FROM sys.computed_columns WHERE object_id=OBJECT_ID('dbo.v2_computed_text') ORDER BY column_id"],
 // Key layout.
 ['setup layout','CREATE TABLE dbo.v2_l1(id INT NOT NULL CONSTRAINT pk_v2_l1 PRIMARY KEY NONCLUSTERED, code INT NOT NULL CONSTRAINT uq_v2_l1 UNIQUE CLUSTERED); CREATE TABLE dbo.v2_l2(a INT NOT NULL, b INT NOT NULL, CONSTRAINT pk_v2_l2 PRIMARY KEY (a DESC, b), CONSTRAINT uq_v2_l2 UNIQUE (b DESC)); CREATE TABLE dbo.v2_l3(a INT NOT NULL, b INT NOT NULL); ALTER TABLE dbo.v2_l3 ADD CONSTRAINT pk_v2_l3 PRIMARY KEY NONCLUSTERED (a), CONSTRAINT uq_v2_l3 UNIQUE CLUSTERED (b DESC)'],
 ['layout indexes',"SELECT OBJECT_NAME(object_id) AS table_name,name,index_id,type,type_desc,is_unique,is_primary_key,is_unique_constraint FROM sys.indexes WHERE object_id IN (OBJECT_ID('dbo.v2_l1'),OBJECT_ID('dbo.v2_l2'),OBJECT_ID('dbo.v2_l3')) ORDER BY table_name,index_id"],
 ['layout index columns',"SELECT OBJECT_NAME(object_id) AS table_name,index_id,index_column_id,COL_NAME(object_id,column_id) AS column_name,key_ordinal,is_descending_key,is_included_column FROM sys.index_columns WHERE object_id IN (OBJECT_ID('dbo.v2_l1'),OBJECT_ID('dbo.v2_l2'),OBJECT_ID('dbo.v2_l3')) ORDER BY table_name,index_id,index_column_id"],
 ['layout key constraints',"SELECT name,OBJECT_NAME(parent_object_id) AS table_name,unique_index_id,is_system_named,is_enforced FROM sys.key_constraints WHERE parent_object_id IN (OBJECT_ID('dbo.v2_l1'),OBJECT_ID('dbo.v2_l2'),OBJECT_ID('dbo.v2_l3')) ORDER BY name"],
 // Modules.
 ['setup procedure',"CREATE PROCEDURE dbo.v2_proc @a INT = 5, @b NVARCHAR(20) OUTPUT, @c DECIMAL(10,2), @d VARCHAR(MAX) = NULL, @e DATETIME2(3) = '2000-01-01' AS SELECT @a"],
 ['setup scalar function',"CREATE FUNCTION dbo.v2_scalar(@x INT, @y VARCHAR(10) = 'q') RETURNS BIGINT AS BEGIN RETURN @x END"],
 ['setup inline function','CREATE FUNCTION dbo.v2_inline(@x INT) RETURNS TABLE AS RETURN SELECT @x AS v'],
 ['setup table function','CREATE FUNCTION dbo.v2_table(@x INT) RETURNS @t TABLE(v INT) AS BEGIN INSERT @t VALUES(@x); RETURN END'],
 ['setup trigger table','CREATE TABLE dbo.v2_tt(id INT)'],
 ['setup trigger','CREATE TRIGGER dbo.v2_trg ON dbo.v2_tt INSTEAD OF INSERT AS SET NOCOUNT ON'],
 ['setup view','CREATE VIEW dbo.v2_view AS SELECT id FROM dbo.v2_tt'],
 ['parameters',"SELECT OBJECT_NAME(object_id) AS module,name,parameter_id,system_type_id,user_type_id,max_length,precision,scale,is_output,is_cursor_ref,has_default_value,is_xml_document,default_value,xml_collection_id,is_readonly,is_nullable FROM sys.parameters WHERE object_id IN (OBJECT_ID('dbo.v2_proc'),OBJECT_ID('dbo.v2_scalar'),OBJECT_ID('dbo.v2_inline'),OBJECT_ID('dbo.v2_table')) ORDER BY module,parameter_id"],
 ['sql modules',"SELECT OBJECT_NAME(object_id) AS module,definition,uses_ansi_nulls,uses_quoted_identifier,is_schema_bound,uses_database_collation,is_recompiled,null_on_null_input,execute_as_principal_id,uses_native_compilation,inline_type,is_inlineable FROM sys.sql_modules WHERE object_id IN (OBJECT_ID('dbo.v2_proc'),OBJECT_ID('dbo.v2_scalar'),OBJECT_ID('dbo.v2_inline'),OBJECT_ID('dbo.v2_table'),OBJECT_ID('dbo.v2_trg'),OBJECT_ID('dbo.v2_view')) ORDER BY module"],
 ['triggers',"SELECT name,parent_class,parent_class_desc,OBJECT_NAME(parent_id) AS parent,type,type_desc,is_ms_shipped,is_disabled,is_not_for_replication,is_instead_of_trigger FROM sys.triggers WHERE name='v2_trg'"],
 ['object definitions',"SELECT OBJECT_DEFINITION(OBJECT_ID('dbo.v2_tt')) AS table_definition,OBJECT_DEFINITION(OBJECT_ID('dbo.v2_trg')) AS trigger_definition,OBJECT_DEFINITION(-1) AS missing,OBJECT_DEFINITION(NULL) AS null_id,OBJECT_DEFINITION(OBJECT_ID('dbo.v2_view')) AS view_definition,OBJECT_DEFINITION(OBJECT_ID('dbo.v2_scalar')) AS function_definition"],
 ['default object definition',"SELECT OBJECT_DEFINITION(OBJECT_ID('dbo.DF_v2_named')) AS definition"],
 ['empty schema parameters','SELECT TOP(0) * FROM sys.parameters'],
 // sp_pkeys and sp_fkeys.
 ['setup keys','CREATE TABLE dbo.v2_p(a INT NOT NULL, b INT NOT NULL, u INT NOT NULL CONSTRAINT UQ_v2_p_u UNIQUE, CONSTRAINT PK_v2_p PRIMARY KEY (b, a)); CREATE TABLE dbo.v2_f(x INT, y INT, z INT CONSTRAINT FK_v2_f_u REFERENCES dbo.v2_p(u) ON DELETE SET NULL, CONSTRAINT FK_v2_f_p FOREIGN KEY (y, x) REFERENCES dbo.v2_p(b, a) ON UPDATE SET DEFAULT)'],
 ['pkeys default owner',"EXEC sp_pkeys 'v2_p'"],
 ['pkeys explicit nulls',"EXEC sp_pkeys @table_name = 'v2_p', @table_owner = NULL, @table_qualifier = NULL"],
 ['pkeys case',"EXEC sp_pkeys 'V2_P'"],
 ['pkeys wildcard',"EXEC sp_pkeys 'v2_%'"],
 ['pkeys other owner',"EXEC sp_pkeys 'v2_p', 'nobody'"],
 ['pkeys without key',"EXEC sp_pkeys 'v2_f'"],
 ['pkeys other database',"DECLARE @r INT; EXEC @r = sp_pkeys 'v2_p', 'dbo', 'master'; SELECT @r AS status"],
 ['pkeys bogus parameter',"EXEC sp_pkeys @table_name = 'v2_p', @bogus = 1"],
 ['pkeys too many',"EXEC sp_pkeys 'v2_p', 'dbo', 'msduck_catalog_reference', 'extra'"],
 ['fkeys primary',"EXEC sp_fkeys 'v2_p'"],
 ['fkeys foreign',"EXEC sp_fkeys @fktable_name = 'v2_f'"],
 ['fkeys both',"EXEC sp_fkeys @pktable_name = 'v2_p', @fktable_name = 'v2_f'"],
 ['fkeys missing',"EXEC sp_fkeys @pktable_name = 'nope'"],
 ['fkeys nulls',"DECLARE @r INT; EXEC @r = sp_fkeys NULL, NULL, NULL, NULL, NULL, NULL; SELECT @r AS status"],
 ['fkeys other database',"EXEC sp_fkeys @fktable_name = 'v2_f', @fktable_qualifier = 'master'"],
 ['keys nocount',"SET NOCOUNT ON; EXEC sp_pkeys 'v2_p'; EXEC sp_fkeys 'v2_p'; SET NOCOUNT OFF"],
 ['keys in try',"BEGIN TRY EXEC sp_pkeys 'v2_p', 'dbo', 'master' END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number END CATCH"],
 // sp_rename.
 ['setup rename',"CREATE TABLE dbo.v2_rp(id INT CONSTRAINT PK_v2_rp PRIMARY KEY, code INT CONSTRAINT UQ_v2_rp UNIQUE, v INT CONSTRAINT CK_v2_rp_v CHECK (v > 0), w INT CONSTRAINT DF_v2_rp_w DEFAULT 1, z INT, y INT, cz AS z * 2); CREATE TABLE dbo.v2_rc(id INT, rp_id INT CONSTRAINT FK_v2_rc_rp REFERENCES dbo.v2_rp(id)); CREATE INDEX IX_v2_rp_y ON dbo.v2_rp(y)"],
 ['rename checked column',"EXEC sp_rename 'dbo.v2_rp.v', 'v2', 'COLUMN'"],
 ['rename key column',"EXEC sp_rename 'dbo.v2_rp.id', 'id2', 'COLUMN'"],
 ['rename foreign column',"EXEC sp_rename 'dbo.v2_rc.rp_id', 'rp_id2', 'COLUMN'"],
 ['rename default column',"EXEC sp_rename 'dbo.v2_rp.w', 'w2', 'COLUMN'"],
 ['rename indexed column',"EXEC sp_rename 'dbo.v2_rp.y', 'y2', 'COLUMN'"],
 ['rename computed source column',"EXEC sp_rename 'dbo.v2_rp.z', 'z2', 'COLUMN'"],
 ['rename computed column',"DECLARE @r INT; EXEC @r = sp_rename 'dbo.v2_rp.cz', 'cz2', 'COLUMN'; SELECT @r AS status"],
 ['rename duplicate column',"EXEC sp_rename 'dbo.v2_rp.code', 'w2', 'COLUMN'"],
 ['rename missing column',"EXEC sp_rename 'dbo.v2_rp.nope', 'w3', 'COLUMN'"],
 ['rename column without type',"EXEC sp_rename 'v2_rp.code', 'code2'"],
 ['rename duplicate table',"EXEC sp_rename 'dbo.v2_rp', 'v2_rc'"],
 ['rename table',"EXEC sp_rename 'dbo.v2_rp', 'v2_rp2'"],
 ['rename table as object',"EXEC sp_rename 'v2_rp2', 'v2_rp3', 'OBJECT'"],
 ['rename check',"EXEC sp_rename 'dbo.CK_v2_rp_v', 'CK_v2_new'"],
 ['rename default',"EXEC sp_rename 'DF_v2_rp_w', 'DF_v2_new', 'OBJECT'"],
 ['rename unique',"EXEC sp_rename 'UQ_v2_rp', 'UQ_v2_new'"],
 ['rename unique as index',"EXEC sp_rename 'dbo.v2_rp3.UQ_v2_new', 'UQ_v2_new2', 'INDEX'"],
 ['rename index without type',"EXEC sp_rename 'dbo.v2_rp3.IX_v2_rp_y', 'IX_v2_new'"],
 ['rename index',"EXEC sp_rename N'dbo.v2_rp3.IX_v2_new', N'IX_v2_new2', N'INDEX'"],
 ['rename index without table',"EXEC sp_rename 'IX_v2_new2', 'IX_v2_new3', 'INDEX'"],
 ['renamed objects',"SELECT name,type FROM sys.objects WHERE parent_object_id=OBJECT_ID('dbo.v2_rp3') OR object_id=OBJECT_ID('dbo.v2_rp3') ORDER BY name"],
 ['renamed indexes',"SELECT name,index_id,type_desc FROM sys.indexes WHERE object_id=OBJECT_ID('dbo.v2_rp3') ORDER BY index_id"],
 ['renamed columns',"SELECT name,column_id FROM sys.columns WHERE object_id=OBJECT_ID('dbo.v2_rp3') ORDER BY column_id"],
 ['renamed keys',"EXEC sp_pkeys 'v2_rp3'; EXEC sp_fkeys 'v2_rp3'"],
 ['renamed rows',"INSERT dbo.v2_rp3(id2,code2,v,y2,z) VALUES(1,2,3,4,5); INSERT dbo.v2_rc(id,rp_id2) VALUES(1,1); SELECT id2,code2,v,w2,y2,z,cz FROM dbo.v2_rp3"],
 ['rename null objname',"EXEC sp_rename NULL, 'x'"],
 ['rename null newname',"EXEC sp_rename 'dbo.v2_rp3', NULL"],
 ['rename empty newname',"DECLARE @r INT; EXEC @r = sp_rename 'dbo.v2_rp3', ''; SELECT @r AS status"],
 ['rename dotted newname',"EXEC sp_rename 'dbo.v2_rc', 'dbo.v2_rc4'"],
 ['dotted table',"SELECT name FROM sys.tables WHERE name LIKE '%v2_rc4%'"],
 ['rename missing',"DECLARE @r INT; EXEC @r = sp_rename 'dbo.v2_missing', 'v2_x'; SELECT @r AS status"],
 ['rename lower objtype',"EXEC sp_rename 'dbo.v2_missing', 'v2_x', 'object'"],
 ['rename bogus parameter',"EXEC sp_rename @objname='dbo.v2_rp3', @newname='v2_x', @bogus=1"],
 ['rename missing newname',"EXEC sp_rename 'dbo.v2_rp3'"],
 ['rename other database',"EXEC sp_rename 'other.dbo.v2_rp3', 'v2_x'"],
 ['rename in try',"BEGIN TRY EXEC sp_rename 'dbo.v2_missing', 'v2_x' END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number END CATCH"],
 ['rename error count',"EXEC sp_rename 'dbo.v2_missing', 'v2_x'; SELECT @@ERROR AS error"],
 ['setup filtered index','CREATE TABLE dbo.v2_fi(id INT, v INT, code INT); CREATE INDEX ix_v2_fi ON dbo.v2_fi(code) WHERE v > 0'],
 ['rename filtered column',"DECLARE @r INT; EXEC @r = sp_rename 'dbo.v2_fi.v', 'v2', 'COLUMN'; SELECT @r AS status"],
 ['filtered index after rename',"SELECT name,filter_definition FROM sys.indexes WHERE name='ix_v2_fi'"],
 ['rename view',"EXEC sp_rename 'dbo.v2_view', 'v2_view2'"],
 ['renamed view definition',"SELECT OBJECT_DEFINITION(OBJECT_ID('dbo.v2_view2')) AS definition"],
 ['rename function',"EXEC sp_rename 'dbo.v2_scalar', 'v2_scalar2'"],
 ['renamed function call','SELECT dbo.v2_scalar2(4, DEFAULT) AS value'],
 // INFORMATION_SCHEMA.
 ['setup information schema',"CREATE TABLE dbo.v2_ist(a INT NOT NULL CONSTRAINT PK_v2_ist PRIMARY KEY, b NVARCHAR(20) CONSTRAINT DF_v2_ist_b DEFAULT N'x', c VARCHAR(MAX), d DECIMAL(10,2), e FLOAT, f REAL, g DATETIME2(3), h DATETIME, i BIT, j MONEY, k VARBINARY(10), m CHAR(3), o TIME(4), p UNIQUEIDENTIFIER, q TINYINT, r DATE, s NUMERIC(5), t AS a*2, CONSTRAINT UQ_v2_ist UNIQUE (d DESC, e), CONSTRAINT CK_v2_ist CHECK (a > 0)); CREATE TABLE dbo.v2_isf(x INT, y INT CONSTRAINT FK_v2_isf REFERENCES dbo.v2_ist(a) ON DELETE CASCADE)"],
 ...['TABLES','COLUMNS','VIEWS','ROUTINES','TABLE_CONSTRAINTS','KEY_COLUMN_USAGE','REFERENTIAL_CONSTRAINTS','CHECK_CONSTRAINTS','SCHEMATA','PARAMETERS','CONSTRAINT_COLUMN_USAGE'].map(view=>['empty schema information_schema '+view.toLowerCase(),`SELECT TOP(0) * FROM INFORMATION_SCHEMA.${view}`]),
 ['information schema tables',"SELECT * FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_NAME IN ('v2_ist','v2_isf','v2_view2') ORDER BY TABLE_NAME"],
 ['information schema columns',"SELECT * FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_NAME = 'v2_ist' ORDER BY ORDINAL_POSITION"],
 ['information schema view columns',"SELECT * FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_NAME = 'v2_view2'"],
 ['information schema table constraints',"SELECT * FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS WHERE TABLE_NAME IN ('v2_ist','v2_isf') ORDER BY CONSTRAINT_NAME"],
 ['information schema key column usage',"SELECT * FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE WHERE TABLE_NAME IN ('v2_ist','v2_isf') ORDER BY CONSTRAINT_NAME,ORDINAL_POSITION"],
 ['information schema referential constraints',"SELECT * FROM INFORMATION_SCHEMA.REFERENTIAL_CONSTRAINTS WHERE CONSTRAINT_NAME = 'FK_v2_isf'"],
 ['information schema check constraints',"SELECT * FROM INFORMATION_SCHEMA.CHECK_CONSTRAINTS WHERE CONSTRAINT_NAME = 'CK_v2_ist'"],
 ['information schema constraint column usage',"SELECT * FROM INFORMATION_SCHEMA.CONSTRAINT_COLUMN_USAGE WHERE TABLE_NAME IN ('v2_ist','v2_isf') ORDER BY CONSTRAINT_NAME,COLUMN_NAME"],
 ['information schema views',"SELECT * FROM INFORMATION_SCHEMA.VIEWS WHERE TABLE_NAME = 'v2_view2'"],
 ['information schema schemata',"SELECT * FROM INFORMATION_SCHEMA.SCHEMATA WHERE SCHEMA_NAME IN ('dbo','guest','INFORMATION_SCHEMA','sys') ORDER BY LOWER(SCHEMA_NAME)"],
 ['information schema routines',"SELECT SPECIFIC_SCHEMA,SPECIFIC_NAME,ROUTINE_SCHEMA,ROUTINE_NAME,ROUTINE_TYPE,DATA_TYPE,CHARACTER_MAXIMUM_LENGTH,NUMERIC_PRECISION,NUMERIC_PRECISION_RADIX,NUMERIC_SCALE,DATETIME_PRECISION,ROUTINE_BODY,ROUTINE_DEFINITION,IS_DETERMINISTIC,SQL_DATA_ACCESS,IS_NULL_CALL,SCHEMA_LEVEL_ROUTINE,MAX_DYNAMIC_RESULT_SETS,IS_USER_DEFINED_CAST,IS_IMPLICITLY_INVOCABLE FROM INFORMATION_SCHEMA.ROUTINES WHERE ROUTINE_NAME IN ('v2_proc','v2_scalar2','v2_inline','v2_table') ORDER BY ROUTINE_NAME"],
 ['information schema parameters',"SELECT * FROM INFORMATION_SCHEMA.PARAMETERS WHERE SPECIFIC_NAME IN ('v2_proc','v2_scalar2','v2_inline') ORDER BY SPECIFIC_NAME,ORDINAL_POSITION"],
 // Altered modules keep CREATE in their text.
 ['alter view','ALTER VIEW dbo.v2_view2 AS SELECT id FROM dbo.v2_tt WHERE id > 0'],
 ['altered view definition',"SELECT OBJECT_DEFINITION(OBJECT_ID('dbo.v2_view2')) AS definition"],
 ['create or alter procedure','CREATE OR ALTER PROCEDURE dbo.v2_proc AS SELECT 1 AS one'],
 ['altered procedure definition',"SELECT definition FROM sys.sql_modules WHERE object_id=OBJECT_ID('dbo.v2_proc')"],
 // Temporary objects stay in tempdb.
 ['temporary objects',"CREATE TABLE #v2_temp(a INT CONSTRAINT DF_v2_temp DEFAULT 1, b INT CHECK (b > 0)); CREATE TABLE ##v2_global(a INT); DECLARE @v TABLE(a INT PRIMARY KEY); SELECT (SELECT COUNT(*) FROM sys.tables WHERE name LIKE '%v2_temp%' OR name LIKE '%v2_global%' OR name LIKE '%msduck%') AS tables,(SELECT COUNT(*) FROM sys.objects WHERE name LIKE '%v2_temp%' OR name LIKE '%v2_global%' OR name LIKE '%msduck%') AS objects,(SELECT COUNT(*) FROM sys.all_objects WHERE name LIKE '%v2_temp%' OR name LIKE '%v2_global%' OR name LIKE '%msduck%') AS all_objects,(SELECT COUNT(*) FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_NAME LIKE '%v2_temp%' OR TABLE_NAME LIKE '%v2_global%' OR TABLE_NAME LIKE '%msduck%') AS information_schema,(SELECT COUNT(*) FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_NAME LIKE '%msduck%') AS information_schema_columns,(SELECT COUNT(*) FROM sys.default_constraints WHERE name = 'DF_v2_temp') AS defaults,(SELECT COUNT(*) FROM sys.check_constraints WHERE OBJECT_NAME(parent_object_id) LIKE '%v2_temp%') AS checks; DROP TABLE #v2_temp; DROP TABLE ##v2_global"],
]
export const v2Failures=new Set(['pkeys other database','pkeys bogus parameter','pkeys too many','fkeys nulls','fkeys other database',
 'rename checked column','rename computed source column','rename computed column','rename duplicate column','rename missing column','rename duplicate table',
 'rename index without table','rename null objname','rename null newname','rename empty newname','rename missing','rename lower objtype','rename bogus parameter',
 'rename missing newname','rename other database','rename error count','rename filtered column'])

export async function observeV2(connection){
 const records=[]
 for(const [name,sql] of [['version',"SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS version"],...v2Statements]){
  const result=canonical(await captureBatch(connection,sql))
  if(Boolean(result.errors.length)!==v2Failures.has(name))throw Error('unexpected error outcome: '+name+' '+JSON.stringify(result.errors).slice(0,800))
  records.push({name,sql,result})
 }
 return records
}

async function referenceRun(observe=observeCatalog){
 return withReferenceContainer(async(config,metadata)=>{
  assertSameCapture(metadata.image,referenceImage,'pinned reference image')
  const admin=await connect(config)
  try {
   const result=await captureBatch(admin,'CREATE DATABASE msduck_catalog_reference')
   if(result.errors.length)throw Error('reference database creation failed')
  }finally{admin.close()}
  const connection=await connect({...config,options:{...config.options,database:'msduck_catalog_reference'}})
  try{return await observe(connection)}finally{connection.close()}
 },{image:referenceImage})
}

async function main(args){
 const definitions=args[0]==='--definitions'
 const namespace=args[0]==='--namespace'
 const v2=args[0]==='--v2'
 if(definitions||namespace||v2)args=args.slice(1)
 if(args.length>1)throw Error('usage: capture-gaps-catalog.mjs [--definitions|--namespace|--v2] [new-output.json]')
 if((definitions||namespace||v2)&&!args[0])throw Error('profile requires a new output path')
 const output=resolve(args[0]??'reference/gaps-catalog.json')
 await refuseExistingFixture(output)
 if(v2){
  const runs=[await referenceRun(observeV2),await referenceRun(observeV2)]
  assertSameCapture(runs[0],runs[1],'complete v2 profile')
  await mkdir(dirname(output),{recursive:true})
  await writeNewFixture(output,{image:referenceImage,contract:'Both fresh runs agree completely; queries select no IDs or clocks.',runs})
  console.log(JSON.stringify({output,records:runs.map(run=>run.length)}))
  return
 }
 const observe=namespace?observeNamespace:definitions?observeDefinitions:observeCatalog
 const runs=[await referenceRun(observe),await referenceRun(observe)]
 const failures=new Set(namespace?['existing table default precedence','existing view default precedence','existing default namespace collision','existing key default collision','existing check default collision']:['pkeys missing parameter','fkeys missing parameters','rename missing object','rename invalid object type','rename column with enforced dependencies'])
 for(const run of runs)for(const record of run){
  if(Boolean(record.result.errors.length)!==failures.has(record.name))throw Error('unexpected error outcome: '+record.name+' '+JSON.stringify(record.result.errors).slice(0,800))
 }
 // Clock/ID/file values are retained in both raw runs. Only the complete empty
 // view responses are asserted equal here; no dynamic differences are erased.
 const views=namespace?[]:definitions?['default_constraints','computed_columns']:catalogViews
 for(const view of views){
  const name='empty schema '+view
  const results=runs.map(run=>run.find(record=>record.name===name)?.result)
  if(results.some(result=>!result||result.errors.length||result.sets.length!==1||result.sets[0].rows.length))throw Error('missing schema capture: '+view)
  assertSameCapture(results[0],results[1],name)
 }
 if(definitions||namespace)assertSameCapture(runs[0],runs[1],namespace?'complete namespace profile':'complete definition profile')
 await mkdir(dirname(output),{recursive:true})
 await writeNewFixture(output,{image:referenceImage,contract:'Complete empty schemas agree; other responses retained raw in both runs without normalization.',runs})
 console.log(JSON.stringify({output,records:runs.map(run=>run.length),schemaControls:views.length,definitions}))
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))await main(process.argv.slice(2))
