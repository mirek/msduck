import assert from 'node:assert/strict'
import { test } from 'node:test'
import { canonical, complete, differences } from '../scripts/lib/compatibility.mjs'
import { referenceConfig } from '../scripts/lib/reference.mjs'

test('comparison retains metadata, row order, missing fields and escaped paths', () => {
  const left = { columns:[{type:'VarChar',length:1}], rows:[[1],[2]], 'a/b~c':null }
  const right = { columns:[{type:'Char',length:2}], rows:[[2],[1]] }
  assert.deepEqual(differences(left,right).map(d => d.path), ['/columns/0/type','/columns/0/length','/rows/0/0','/rows/1/0','/a~1b~0c'])
  assert.deepEqual(differences(left,right).at(-1), {path:'/a~1b~0c',local:null,reference:{kind:'missing'}})
  assert.deepEqual(differences(canonical(left),canonical(left)), [])
  assert.notEqual(differences([],{}).length, 0)
})

test('capture serialization keeps binary, date and bigint values explicit', () => {
  assert.deepEqual(canonical([Buffer.from([0,255]),new Date('2020-01-01T00:00:00Z'),9223372036854775807n,undefined]), [
    {kind:'binary',value:'00ff'}, {kind:'date',value:'2020-01-01T00:00:00.000Z'}, {kind:'bigint',value:'9223372036854775807'}, {kind:'missing'}
  ])
})

test('capture preserves differences smaller than one millisecond', () => {
  const local = new Date('0001-01-01T00:00:00Z')
  const reference = new Date(local)
  local.nanosecondsDelta = 0.0000001
  reference.nanosecondsDelta = 0.0000002
  assert.deepEqual(differences(canonical(local), canonical(reference)), [
    { path: '/nanosecondsDelta', local: 0.0000001, reference: 0.0000002 }
  ])
})

test('incomplete setup or reuse cannot become a compatibility match', () => {
  const good = {execution:{errors:[{number:245}]},reuse:{errors:[]}}
  assert.equal(complete(good), true)
  for (const bad of [{skipped:'setup failed'}, {...good,transportError:'closed'}, {...good,reuse:{errors:[{number:1}]}}, {...good,cleanup:{errors:[{number:1}]}}]) assert.equal(complete(bad), false)
})

test('reference configuration requires credentials and defaults to verified TLS', () => {
  assert.throws(() => referenceConfig({}), /MSSQL_REFERENCE_HOST is required/)
  const env = {MSSQL_REFERENCE_HOST:'localhost',MSSQL_REFERENCE_USER:'tester',MSSQL_REFERENCE_PASSWORD:'test-only'}
  const config = referenceConfig(env)
  assert.equal(config.options.encrypt, true)
  assert.equal(config.options.trustServerCertificate, false)
  assert.equal(config.options.database, 'master')
  for (const port of ['0','65536','1.5','wrong']) assert.throws(() => referenceConfig({...env,MSSQL_REFERENCE_PORT:port}), /PORT/)
  assert.throws(() => referenceConfig({...env,MSSQL_REFERENCE_ENCRYPT:'yes'}), /true or false/)
})

test('reference isolation drops only its successfully created database after success or failure', async () => {
  const { EventEmitter } = await import('node:events')
  const { isolatedReference } = await import('../scripts/lib/reference.mjs')
  for (const failure of [null,'create','work']) {
    const sql = [], connections = []
    const operations = {
      async connect(config) {
        const connection = new EventEmitter()
        connection.config = config
        connection.closed = false
        connection.close = () => { connection.closed = true; connection.emit('end') }
        connections.push(connection)
        return connection
      },
      async command(_connection, text) {
        sql.push(text)
        if (failure === 'create' && text.startsWith('CREATE')) throw new Error('creation failed')
      }
    }
    const work = async connection => {
      assert.match(connection.config.options.database, /^msduck_audit_[0-9a-f]{32}$/)
      if (failure === 'work') throw new Error('work failed')
      return 'captured'
    }
    const run = isolatedReference({options:{database:'master'}},work,operations)
    if (failure) await assert.rejects(run, /failed/)
    else assert.equal(await run, 'captured')
    assert.match(sql[0], /^CREATE DATABASE \[msduck_audit_[0-9a-f]{32}\]$/)
    if (failure === 'create') assert.equal(sql.length, 1)
    else assert.deepEqual(sql, [sql[0],sql[0].replace('CREATE','DROP')])
    assert.equal(connections.every(c => c.closed), true)
  }
})

test('reference container owns lifecycle and keeps credentials out of Docker arguments', async () => {
  const {withReferenceContainer}=await import('../scripts/lib/reference-container.mjs')
  const calls=[];let attempts=0;let closed=0;let password
  const result=await withReferenceContainer(async (config,meta)=>{
    assert.equal(config.server,'localhost')
    assert.equal(config.options.port,15433)
    assert.equal(config.authentication.options.password,password)
    assert.match(meta.name,/^msduck-reference-/)
    return 42
  },{
    docker:async (args,env)=>{calls.push(args);if(args[0]==='run'){password=env.MSSQL_SA_PASSWORD;assert.equal(args.includes(password),false);return 'container-id'}if(args[0]==='port')return '127.0.0.1:15433';if(args[0]==='inspect')return 'true';return ''},
    connect:async()=>{if(attempts++===0)throw new Error('not ready');return {close(){closed++}}},delay:async()=>{}
  })
  assert.equal(result,42)
  assert.equal(attempts,2)
  assert.equal(closed,1)
  assert.deepEqual(calls.at(-1),['rm','--force',calls[0][4]])
})

test('reference container cleans up startup and workload failures', async () => {
  const {withReferenceContainer}=await import('../scripts/lib/reference-container.mjs')
  for(const failure of ['run','port','readiness','work']){
    const calls=[]
    await assert.rejects(withReferenceContainer(async()=>{throw new Error('work failed')},{
      docker:async args=>{calls.push(args);if(args[0]===failure)throw new Error(`${failure} failed`);if(args[0]==='port')return '127.0.0.1:15433';if(args[0]==='inspect')return failure==='readiness'?'false':'true';return ''},
      connect:async()=>({close(){}}),delay:async()=>{}
    }))
    assert.deepEqual(calls.at(-1),['rm','--force',calls[0][4]])
  }
})

test('reference container labels its owner and tolerates auto-removal races', async () => {
  const {withReferenceContainer}=await import('../scripts/lib/reference-container.mjs')
  for (const [remaining, outcome] of [[0,'ok'],[3,'ok'],[99,'fail'],[-1,'fail']]) {
    const calls=[];let listed=0;let slept=0
    const run=withReferenceContainer(async()=>7,{
      delay:async()=>{slept++},connect:async()=>({close(){}}),
      docker:async args=>{
        calls.push(args)
        if(args[0]==='port')return '127.0.0.1:15433'
        if(args[0]==='inspect')return 'true'
        if(args[0]==='rm')throw new Error('removal already in progress')
        if(args[0]==='ps'){if(remaining<0)throw new Error('daemon unavailable');return listed++<remaining?'abc123':''}
        return ''
      }
    })
    if(outcome==='ok')assert.equal(await run,7)
    else await assert.rejects(run,/Could not remove owned reference container msduck-reference-/)
    const name=calls[0][4]
    assert.deepEqual(calls[0].slice(5,9),['--label','msduck.reference=1','--label',`msduck.owner=${process.cwd()}:${process.pid}`])
    const polls=calls.filter(c=>c[0]==='ps')
    assert(polls.every(c=>c.join(' ')===`ps --all --quiet --filter name=^/${name}$`))
    assert.equal(polls.length, remaining<0?1:Math.min(remaining+1,30))
    assert(slept<=30)
  }
})

test('reference container cleans up after invalid bindings timeout and cancellation', async () => {
  const {withReferenceContainer}=await import('../scripts/lib/reference-container.mjs')
  for(const failure of ['binding','timeout','abort']) {
    const controller=new AbortController()
    const calls=[];let clock=0
    await assert.rejects(withReferenceContainer(async()=>assert.fail('work must not run'),{
      signal:controller.signal,timeout:1,now:()=>clock++,delay:async()=>{},
      docker:async args=>{calls.push(args);if(args[0]==='run'&&failure==='abort')controller.abort();if(args[0]==='port')return failure==='binding'?'0.0.0.0:15433':'127.0.0.1:15433';if(args[0]==='inspect')return 'true';return ''},
      connect:async()=>{throw new Error('not ready')}
    }))
    assert.deepEqual(calls.at(-1),['rm','--force',calls[0][4]])
  }
})
