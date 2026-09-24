import assert from 'node:assert/strict'
import { test } from 'node:test'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { Connection } from 'tedious'
import { start, query } from './support/client.mjs'

test('column collations survive physical lowering and failed DDL remains atomic', async t => {
  const c = await start(t)
  await query(c, 'CREATE TABLE dbo.collation_probe(a NVARCHAR(8) COLLATE Latin1_General_100_BIN2,b VARCHAR(9) COLLATE SQL_Latin1_General_CP1_CI_AS)')
  await query(c, 'ALTER TABLE dbo.collation_probe ADD c NCHAR(4) COLLATE Latin1_General_100_BIN2')
  assert.deepEqual((await query(c, "SELECT name,max_length,collation_name FROM sys.columns WHERE object_id=OBJECT_ID(N'dbo.collation_probe') ORDER BY column_id")).rows, [
    ['a',16,'Latin1_General_100_BIN2'],['b',9,'SQL_Latin1_General_CP1_CI_AS'],['c',8,'Latin1_General_100_BIN2'],
  ])
  await query(c, "INSERT INTO dbo.collation_probe(a) VALUES(N'ÿ'),(N'Ā')")
  assert.deepEqual((await query(c,'SELECT MIN(a),MAX(a) FROM dbo.collation_probe')).rows,[['ÿ','Ā']])
  await assert.rejects(query(c, 'ALTER TABLE dbo.collation_probe ADD bad NVARCHAR(8) COLLATE unsupported_collation'), /unsupported column collation/)
  await assert.rejects(query(c, 'CREATE TABLE dbo.collation_bad(a INT COLLATE Latin1_General_100_BIN2)'), /character column/)
  assert.deepEqual((await query(c, "SELECT COUNT(*) FROM sys.columns WHERE object_id=OBJECT_ID(N'dbo.collation_probe'); SELECT OBJECT_ID(N'dbo.collation_bad')")).rows, [[3],[null]])
})

test('explicit column collation persists across server restart', async t => {
  const directory = await mkdtemp(join(tmpdir(),'msduck-column-collation-'))
  t.after(() => rm(directory,{recursive:true,force:true}))
  async function run(sql) {
    const server = spawn('target/debug/msduck',['--listen','127.0.0.1:0','--database',join(directory,'test.duckdb')],{stdio:['ignore','ignore','pipe']})
    const exited = once(server,'exit')
    let c
    try {
      const port = await new Promise((resolve,reject) => {
        let logs=''
        const timer=setTimeout(()=>reject(Error(`startup: ${logs}`)),20000)
        server.once('error',e=>{clearTimeout(timer);reject(e)})
        server.once('exit',code=>{clearTimeout(timer);reject(Error(`server exit ${code}: ${logs}`))})
        server.stderr.on('data',data=>{
          logs+=data.toString()
          const match=logs.match(/listening on 127\.0\.0\.1:(\d+)/)
          if(match){clearTimeout(timer);resolve(Number(match[1]))}
        })
      })
      c=new Connection({server:'127.0.0.1',authentication:{type:'default',options:{userName:'sa',password:'development'}},options:{port,encrypt:false,database:'master',requestTimeout:5000}})
      c.on('error',()=>{})
      await new Promise((resolve,reject)=>c.connect(e=>e?reject(e):resolve()))
      return await query(c,sql)
    } finally {
      c?.close()
      server.kill()
      await exited
    }
  }
  await run("CREATE TABLE dbo.persisted(s NVARCHAR(8) COLLATE Latin1_General_100_BIN2); INSERT INTO dbo.persisted VALUES(N'ÿ'),(N'Ā')")
  assert.deepEqual((await run("SELECT collation_name FROM sys.columns WHERE object_id=OBJECT_ID(N'dbo.persisted'); SELECT MIN(s),MAX(s) FROM dbo.persisted")).rows,[['Latin1_General_100_BIN2'],['ÿ','Ā']])
})
