import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {setTimeout as delay} from 'node:timers/promises'
import {Request, TYPES} from 'tedious'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {connect, command, isolatedReference} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

// Observe only post-login plaintext TDS. No authentication traffic or credentials.
function tracePackets(connection) {
  const packets = [], buffers = {in: Buffer.alloc(0), out: Buffer.alloc(0)}
  let total = 0
  const observe = direction => chunk => {
    total += chunk.length
    assert.ok(total <= 4 * 1024 * 1024, 'bounded Attention capture')
    buffers[direction] = Buffer.concat([buffers[direction], chunk])
    while (buffers[direction].length >= 8) {
      const length = buffers[direction].readUInt16BE(2)
      assert.ok(length >= 8, 'valid packet length')
      if (buffers[direction].length < length) break
      packets.push({direction, hex: buffers[direction].subarray(0, length).toString('hex')})
      buffers[direction] = buffers[direction].subarray(length)
    }
  }
  const incoming = connection.messageIo.securePair?.cleartext ?? connection.messageIo.socket
  const outgoing = connection.messageIo.outgoingMessageStream
  const onIn = observe('in'), onOut = observe('out')
  incoming.on('data', onIn); outgoing.on('data', onOut)
  return () => {
    incoming.off('data', onIn); outgoing.off('data', onOut)
    assert.equal(buffers.in.length + buffers.out.length, 0, 'no partial captured packets')
    return packets
  }
}

// Header SPIDs and packet splitting can vary; keep full raw packets separately.
// Compare exact reassembled response payloads and EOM boundaries, never token sorting.
function responseMessages(packets) {
  const messages = []
  let parts = []
  for (const packet of packets.filter(p => p.direction === 'in')) {
    const b = Buffer.from(packet.hex, 'hex')
    assert.equal(b[0], 4)
    parts.push(b.subarray(8))
    if (b[1] & 1) { messages.push(Buffer.concat(parts).toString('hex')); parts = [] }
  }
  assert.equal(parts.length, 0)
  return messages
}

function decodeResponses(messages, transactionDescriptor) {
  return messages.map(hex => {
    const b = Buffer.from(hex, 'hex'), tokens = []
    for (let at=0; at<b.length;) {
      const token = b[at]
      if ([0xfd,0xfe,0xff].includes(token)) {
        assert.ok(at+13<=b.length)
        tokens.push({token,status:b.readUInt16LE(at+1),command:b.readUInt16LE(at+3),count:b.readBigUInt64LE(at+5).toString()})
        at+=13
      } else {
        assert.equal(token,0xe3,'unexpected token retained in raw capture; extend decoder explicitly')
        const length=b.readUInt16LE(at+1)
        const body=b.subarray(at+3,at+3+length)
        assert.equal(body.length,length)
        assert.equal(body[0],10,'rollback ENVCHANGE')
        assert.equal(body[1],0,'empty new descriptor')
        assert.equal(body[2],8,'eight-byte old descriptor')
        assert.equal(body.length,11)
        assert.equal(body.subarray(3).toString('hex'),transactionDescriptor,'rollback must identify this active transaction')
        tokens.push({token,type:10,newDescriptor:'',oldDescriptor:'active transaction descriptor (verified)'})
        at+=3+length
      }
    }
    return tokens
  })
}

export const cases = []
for (const mode of ['batch', 'rpc', 'prepared'])
  for (const transaction of [false, true])
    for (const xactAbort of [false, true])
      for (const tryCatch of [false, true])
        cases.push({mode, transaction, xactAbort, tryCatch})

async function oneCase(config, entry) {
  return isolatedReference(config, async connection => {
    const observer = await connect(config)
    try {
      const setup = await command(connection, `CREATE TABLE dbo.cancel_probe(n INT); SET XACT_ABORT ${entry.xactAbort ? 'ON' : 'OFF'}; SET DATEFIRST 2; ${entry.transaction ? 'BEGIN TRANSACTION;' : ''}`)
      const spid = (await command(connection, 'SELECT @@SPID AS session_id')).sets[0].rows[0][0]
      assert.ok(Number.isInteger(spid))
      const body = `INSERT dbo.cancel_probe VALUES(1); IF @hold=1 WAITFOR DELAY '00:00:45'; INSERT dbo.cancel_probe VALUES(2); SELECT 42 AS completed`
      const sql = entry.tryCatch ? `BEGIN TRY ${body} END TRY BEGIN CATCH INSERT dbo.cancel_probe VALUES(3); SELECT ERROR_NUMBER() AS caught END CATCH` : body
      let done
      const request = new Request(entry.mode === 'batch' ? sql.replaceAll('@hold', '1') : sql, (error, rowCount) => done?.({error: error ? {code: error.code ?? null, number: error.number ?? null, message: error.message} : null, rowCount: rowCount ?? null}))
      if (entry.mode !== 'batch') request.addParameter('hold', TYPES.Int, 1)
      if (entry.mode === 'prepared') {
        await new Promise((resolve, reject) => {
          request.once('prepared', resolve)
          request.once('error', reject)
          done = result => result.error && reject(new Error(result.error.message))
          connection.prepare(request)
        })
      }
      const events = []
      const onInfo = value => events.push({kind:'info', number:value.number, message:value.message})
      const onError = value => events.push({kind:'error', number:value.number, state:value.state, class:value.class, message:value.message})
      connection.on('infoMessage', onInfo); connection.on('errorMessage', onError)
      request.on('row', row => events.push({kind:'row', values:row.map(c=>c.value)}))
      for (const kind of ['done','doneInProc','doneProc']) request.on(kind, (count, more, status) => events.push({kind, count:count??null, more, status:status??null}))
      const transactionDescriptor = connection.currentTransactionDescriptor().toString('hex')
      const stopTrace = tracePackets(connection)
      let finished = false, packets, cancellation, active
      const completion = new Promise(resolve => {done = result => {finished = true; resolve(result)}})
      try {
        if (entry.mode === 'batch') connection.execSqlBatch(request)
        else if (entry.mode === 'rpc') connection.execSql(request)
        else connection.execute(request, {hold:1})
        const deadline = Date.now() + 15000
        while (true) {
          assert.equal(finished, false, 'request must still be running before cancellation')
          const observation = await command(observer, `SELECT command, wait_type FROM sys.dm_exec_requests WHERE session_id=${spid}`)
          active = observation.sets[0].rows.find(row => row[1] === 'WAITFOR')
          if (active) break
          assert.ok(Date.now() < deadline, 'observer did not establish active WAITFOR')
          await delay(10)
        }
        assert.equal(finished, false)
        assert.equal(connection.cancel(), true, 'client accepted cancellation')
        cancellation = await completion
      } finally {
        packets = stopTrace()
        connection.off('infoMessage', onInfo); connection.off('errorMessage', onError)
      }
      assert.equal(cancellation.error?.code, 'ECANCEL')
      assert.ok(packets.some(p => p.direction==='out' && Buffer.from(p.hex,'hex')[0]===6), 'Attention sent')
      const cancellationEvents = [...events]
      const reuse = await command(connection, 'SELECT @@TRANCOUNT AS transaction_count, XACT_STATE() AS transaction_state, @@DATEFIRST AS first_day; SELECT n FROM dbo.cancel_probe ORDER BY n; SELECT 1 AS reusable')
      await command(connection, 'IF @@TRANCOUNT>0 ROLLBACK TRANSACTION')
      let preparedReuse = null, serverHandleReuse = null
      if (entry.mode === 'prepared') {
        request.error = undefined
        preparedReuse = await new Promise(resolve => {done = resolve; connection.execute(request, {hold:0})})
        // A cancelled tedious Request remains cancelled. Probe the same server
        // handle with a fresh RPC Request instead of resetting driver internals.
        const call = (procedure, values) => new Promise(resolve => {
          const next = new Request(procedure, (error,rowCount) => resolve({error:error?{code:error.code??null,number:error.number??null,message:error.message}:null,rowCount:rowCount??null}))
          for(const [name,value] of Object.entries(values)) next.addParameter(name,TYPES.Int,value)
          connection.callProcedure(next)
        })
        serverHandleReuse = await call('sp_execute', {handle:request.handle, hold:0})
        assert.equal(serverHandleReuse.error, null)
        const unprepare = await call('sp_unprepare', {handle:request.handle})
        assert.equal(unprepare.error, null)
      }
      return canonical({entry, setup, active, cancellation, events:cancellationEvents, reuse, preparedReuse, serverHandleReuse, responses:decodeResponses(responseMessages(packets),transactionDescriptor), rawResponses:responseMessages(packets), transactionDescriptor, packets})
    } finally {observer.close()}
  })
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const output = resolve(process.argv[2] ?? 'artifacts/compatibility/attention-reference')
  await mkdir(output, {recursive:true})
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let repeat=0; repeat<2; repeat++) {
      const results = []
      for (const entry of cases) {
        results.push(await oneCase(config, entry))
        await writeFile(resolve(output, `run-${repeat}.json`), JSON.stringify(results,null,2)+'\n')
        console.log(`Captured ${repeat+1}: ${JSON.stringify(entry)}`)
      }
      runs.push(results)
    }
    const observations = runs.map(run => run.map(({packets, rawResponses, transactionDescriptor, ...result}) => result))
    await writeFile(resolve(output, 'runs.json'), JSON.stringify(runs,null,2)+'\n')
    assert.deepEqual(observations[0], observations[1], 'Repeated observations differ; raw traces retained')
    const actual = {image:container.image, identicalFreshCaptures:2, transport:'TLS', results:observations[0], rawCaptures:runs.map(run=>run.map(({entry,packets,rawResponses,transactionDescriptor})=>({entry,packets,responses:rawResponses,transactionDescriptor})))}
    await writeFile(resolve(output, 'attention.json'), JSON.stringify(actual,null,2)+'\n')
    let fixture
    try {fixture = JSON.parse(await readFile(new URL('../reference/attention.json', import.meta.url),'utf8'))}
    catch(error) {if(error.code!=='ENOENT') throw error}
    if(fixture) {
      assert.equal(actual.image,fixture.image)
      assert.equal(actual.transport,fixture.transport)
      assert.deepEqual(actual.results, fixture.results, 'Retained Attention observations differ')
    }
    console.log(`Captured ${actual.results.length} cancellation scenarios twice${fixture?' and matched fixture':''}`)
  })
}
