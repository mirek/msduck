// Focused catalog rows; full descriptor and completion parity is still tracked
// by the retained SQL Server capture and is not claimed by these assertions.
import assert from 'node:assert/strict'
import {readFileSync} from 'node:fs'
import {test} from 'node:test'
import {start,query} from '../support/client.mjs'

const reference=JSON.parse(readFileSync(new URL('../../reference/gaps-catalog.json',import.meta.url),'utf8'))
const defaults=reference.runs[0].find(record=>record.name==='defaults')

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
