// Focused catalog rows; full descriptor and completion parity is still tracked
// by the retained SQL Server capture and is not claimed by these assertions.
import assert from 'node:assert/strict'
import {readFileSync} from 'node:fs'
import {test} from 'node:test'
import {start,query} from '../support/client.mjs'

const reference=JSON.parse(readFileSync(new URL('../../reference/gaps-catalog.json',import.meta.url),'utf8'))
const defaults=reference.runs[0].find(record=>record.name==='defaults')

test('PRIMARY KEY and UNIQUE identities match controlled catalog object rows',async t=>{
 const connection=await start(t)
 await query(connection,'CREATE TABLE dbo.catalog_parent(a INT NOT NULL,b INT NOT NULL,label NVARCHAR(40),CONSTRAINT PK_catalog_parent PRIMARY KEY(a,b),CONSTRAINT UQ_catalog_parent_label UNIQUE(label)); CREATE TABLE dbo.catalog_child(a INT NOT NULL,b INT NOT NULL,CONSTRAINT PK_catalog_child PRIMARY KEY(a,b))')
 const sql="SELECT name,RTRIM(type) AS type,type_desc,OBJECT_NAME(parent_object_id) AS parent FROM sys.objects WHERE parent_object_id IN(OBJECT_ID('dbo.catalog_parent'),OBJECT_ID('dbo.catalog_child')) AND RTRIM(type) IN('PK','UQ') ORDER BY name"
 const expected=reference.runs[0].find(r=>r.name==='constraint objects').result.sets[0].rows.filter(row=>['PK','UQ'].includes(row[1]))
 assert.deepEqual((await query(connection,sql)).rows,expected)
 const idSql="SELECT OBJECT_ID('dbo.PK_catalog_child','PK') AS id"
 const original=(await query(connection,idSql)).rows[0][0]
 assert.equal(typeof original,'number')
 await query(connection,'BEGIN TRANSACTION; DROP TABLE dbo.catalog_child; CREATE TABLE dbo.catalog_child(a INT,b INT,CONSTRAINT PK_catalog_child PRIMARY KEY(a,b))')
 assert.notEqual((await query(connection,idSql)).rows[0][0],original)
 await query(connection,'ROLLBACK')
 assert.deepEqual((await query(connection,idSql)).rows,[[original]])
})

test('DEFAULT expression families retain SQL Server catalog text',async t=>{
 const connection=await start(t)
 const records=reference.definitionProfile.runs[0]
 await query(connection,records.find(r=>r.name==='setup default definitions').sql)
 for(const name of ['default definitions','default object definitions']){
  const record=records.find(r=>r.name===name)
  assert.deepEqual((await query(connection,record.sql)).rows,record.result.sets[0].rows,name)
 }
})

test('named DEFAULT catalog rows match the controlled SQL Server fixture',async t=>{
 const connection=await start(t)
 await query(connection,'CREATE TABLE dbo.catalog_child(a INT,b INT,state BIT CONSTRAINT DF_catalog_child_state DEFAULT(1))')
 const result=await query(connection,defaults.sql)
 assert.deepEqual(result.rows,defaults.result.sets[0].rows)
 const definition=await query(connection,"SELECT OBJECT_DEFINITION(OBJECT_ID('dbo.DF_catalog_child_state')) AS definition")
 assert.deepEqual(definition.rows,[[defaults.result.sets[0].rows[0][3]]])
 await query(connection,'DROP TABLE dbo.catalog_child')
 assert.deepEqual((await query(connection,defaults.sql)).rows,[])
 assert.deepEqual((await query(connection,"SELECT OBJECT_DEFINITION(OBJECT_ID('dbo.DF_catalog_child_state')) AS definition")).rows,[[null]])
})

test('rolled-back DEFAULT definitions leave no catalog objects',async t=>{
 const connection=await start(t)
 await query(connection,'BEGIN TRANSACTION; CREATE TABLE dbo.catalog_rollback(id INT CONSTRAINT DF_catalog_rollback DEFAULT(1)); ROLLBACK')
 assert.deepEqual((await query(connection,"SELECT name FROM sys.default_constraints WHERE name='DF_catalog_rollback'")).rows,[])
 assert.deepEqual((await query(connection,"SELECT OBJECT_ID('dbo.DF_catalog_rollback') AS id")).rows,[[null]])
})

test('computed catalog rows match the controlled SQL Server fixture',async t=>{
 const connection=await start(t)
 await query(connection,'CREATE TABLE dbo.catalog_child(a INT NOT NULL,b INT NOT NULL,label VARCHAR(40),doubled AS a*2 PERSISTED)')
 const capture=reference.runs[0].find(r=>r.name==='computed')
 assert.deepEqual((await query(connection,capture.sql)).rows,capture.result.sets[0].rows)
 await query(connection,'BEGIN TRANSACTION; ALTER TABLE dbo.catalog_child DROP COLUMN doubled')
 assert.deepEqual((await query(connection,capture.sql)).rows,[])
 await query(connection,'ROLLBACK')
 assert.deepEqual((await query(connection,capture.sql)).rows,capture.result.sets[0].rows)
 await query(connection,'DROP TABLE dbo.catalog_child')
 assert.deepEqual((await query(connection,capture.sql)).rows,[])
})
