import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile, mkdir, mkdtemp, writeFile, realpath, symlink, link, rm, truncate} from 'node:fs/promises'
import {join} from 'node:path'
import {fileURLToPath} from 'node:url'
import {spawnSync} from 'node:child_process'
import {EventEmitter} from 'node:events'
import {createHash} from 'node:crypto'
import {cases, rowsFor, jsonSize, CAPTURE_LIMIT, validate, validateRetained, compare, guardOutput, persistCapture, finalizeCapture, readCaptureFile, exchange, budget} from '../scripts/capture-bulk-character-utf8-capacity.mjs'
const fixture = new URL('../reference/bulk-character-utf8-capacity.json', import.meta.url)
const load = async () => JSON.parse(await readFile(fixture, 'utf8'))
const find = (run, name) => run.observations.find(o => o.case.name === name)
const scratch = async work => {
  const root = fileURLToPath(new URL('../.tmp/',import.meta.url))
  await mkdir(root,{recursive:true})
  const dir = await realpath(await mkdtemp(join(root,'msduck-utf8-')))
  try {await work(dir)} finally {await rm(dir,{recursive:true,force:true})}
}

test('capture reads reject oversized sparse files and symlinks before allocating their payload', async () => {
  await scratch(async dir => {
    const file = join(dir,'large.json'); await writeFile(file,''); await truncate(file,CAPTURE_LIMIT+1)
    await assert.rejects(readCaptureFile(file), /bounded/)
    await symlink(file,join(dir,'alias'))
    await assert.rejects(readCaptureFile(join(dir,'alias')))
    await assert.rejects(readCaptureFile(dir), /regular/)
    await writeFile(file,'small'); assert.equal((await readCaptureFile(file)).toString(),'small')
  })
})

test('uniform request and response corruption is rejected independently of decoded summaries', async () => {
  const retained = await load()
  for (const direction of ['in','out']) {
    const actual = structuredClone(retained)
    for (const run of actual.runs) {
      const packets = find(run, 'utf8-two-byte-exact-source64-to-utf8-varchar').execution.packets
      const packet = packets.find(p => p.direction === direction && (direction === 'in' || p.rawHex.startsWith('07')))
      const bytes = Buffer.from(packet.rawHex, 'hex'); bytes[bytes.length - 1] ^= 1; packet.rawHex = bytes.toString('hex')
    }
    actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
    assert.throws(() => validate(actual), /fixed/)
  }
})

test('duplicate capture identities, added version cells and unsupported packet status cannot disappear in projections', async () => {
  const retained = await load()
  const duplicate = structuredClone(retained)
  duplicate.runs = Array.from({length:4},()=>structuredClone(retained.runs[0]))
  duplicate.comparisons = duplicate.runs.slice(1).map(run=>compare(run,duplicate.runs[0]))
  assert.throws(()=>validate(duplicate),/independent database/)
  const extra = structuredClone(retained)
  for(const run of extra.runs) run.version.result.sets[0].rows[0].push('unapproved extra cell')
  extra.comparisons = extra.runs.slice(1).map(run=>compare(run,extra.runs[0]))
  assert.throws(()=>validate(extra),/row width/)
  const status = structuredClone(retained)
  for(const run of status.runs) {
    const p=find(run,'utf8-two-byte-exact-source64-to-utf8-varchar').execution.packets.find(p=>p.direction==='in')
    const bytes=Buffer.from(p.rawHex,'hex');bytes[1]|=0x80;p.rawHex=bytes.toString('hex')
  }
  status.comparisons=status.runs.slice(1).map(run=>compare(run,status.runs[0]))
  assert.throws(()=>validate(status),/packet status/)
})

test('mixed inbound SPIDs across packets or phases and nonzero outbound SPIDs are rejected', async () => {
  const retained=await load()
  for(const mode of ['one-inbound-packet','whole-phase','outbound']) {
    const actual=structuredClone(retained)
    for(const run of actual.runs) {
      const o=find(run,'utf8-supplementary-exact-sourcemax-to-unicode-nvarchar')
      const packets=mode==='outbound' ? o.execution.packets.filter(p=>p.direction==='out').slice(0,1) : o.readback.packets.filter(p=>p.direction==='in').slice(0,mode==='whole-phase'?undefined:1)
      for(const p of packets) {const bytes=Buffer.from(p.rawHex,'hex');bytes.writeUInt16BE(0xffff,4);p.rawHex=bytes.toString('hex')}
    }
    actual.comparisons=actual.runs.slice(1).map(run=>compare(run,actual.runs[0]))
    assert.throws(()=>validate(actual),/SPID/)
  }
})

test('uniform request-type corruption cannot hide behind unchanged payload signatures', async () => {
  const retained=await load()
  for(const phase of ['metadata','setup','readback','cleanup','execution']) {
    const actual=structuredClone(retained)
    for(const run of actual.runs)for(const p of find(run,'utf8-supplementary-exact-sourcemax-to-unicode-nvarchar')[phase].packets.filter(p=>p.direction==='out'&&p.rawHex.startsWith('01'))) {
      const bytes=Buffer.from(p.rawHex,'hex');bytes[0]=6;p.rawHex=bytes.toString('hex')
    }
    actual.comparisons=actual.runs.slice(1).map(run=>compare(run,actual.runs[0]))
    assert.throws(()=>validate(actual),/message direction\/type/)
  }
})

// Four bounded persistence/comparison cases can be CPU-heavy on slow workers.
// This test deadline does not alter the collector's exchange/request limits.
test('asynchronous trace guards close the connection and preserve bounded failed evidence through the awaited exchange', {timeout:60000}, async () => {
  for(const mode of ['in','out','split-header','oversized-with-prefix']) {
    const direction=mode==='out'?'out':'in'
    const incoming=new EventEmitter(),outgoing=new EventEmitter()
    let closed=0
    const connection={messageIo:{outgoingMessageStream:outgoing,socket:incoming},close(){closed++}}
    const prefix=Buffer.from('040100090039010000','hex')
    const invalid=mode==='split-header'?Buffer.from('0401000000390100','hex')
      :mode==='oversized-with-prefix'?Buffer.concat([Buffer.from('04010009','hex'),Buffer.alloc(2*1024*1024+1,0x61)])
      :direction==='in'?Buffer.alloc(2*1024*1024+1,0x61):Buffer.from('0701000a00000100ff','hex')
    const record=await exchange(connection,()=>{
      queueMicrotask(()=>{
        incoming.emit('data',prefix)
        if(mode==='split-header'||mode==='oversized-with-prefix') {incoming.emit('data',invalid.subarray(0,4));incoming.emit('data',invalid.subarray(4))}
        else (direction==='in'?incoming:outgoing).emit('data',invalid)
      })
      return new Promise(()=>{})
    },budget())
    assert.equal(closed,1)
    assert.equal(incoming.listenerCount('data'),0)
    assert.equal(outgoing.listenerCount('data'),0)
    assert.deepEqual(record.packets,[{direction:'in',rawHex:prefix.toString('hex')}])
    assert.equal(record.result.traceFailure.direction,direction)
    assert.equal(record.result.traceFailure.observedBytes,invalid.length)
    assert.equal(record.result.traceFailure.sha256,createHash('sha256').update(invalid).digest('hex'))
    assert.equal(record.result.traceFailure.inputPrefixHex,invalid.subarray(0,256).toString('hex'))
    assert.ok(record.result.transportError)
    await scratch(async dir=>{
      const output=join(dir,'trace-failed.json')
      await assert.rejects(persistCapture({format:1,containers:[],runs:[],trace:record},output))
      const saved=JSON.parse(await readFile(output,'utf8'))
      assert.deepEqual(saved.trace,record)
      assert.equal(JSON.parse(await readFile(output+'.comparison.json','utf8')).retained,true)
    })
  }
})

test('aggregate payload exhaustion still returns already retained packets and reserved failure evidence', {timeout:10000}, async () => {
  const incoming=new EventEmitter(),outgoing=new EventEmitter()
  let closed=0
  const connection={messageIo:{outgoingMessageStream:outgoing,socket:incoming},close(){closed++}}
  const packet=Buffer.from('040100090039010000','hex')
  const limits=budget(packet.length*2+64)
  const record=await exchange(connection,()=>{
    queueMicrotask(()=>{incoming.emit('data',packet);incoming.emit('data',packet)})
    return new Promise(()=>{})
  },limits)
  assert.equal(closed,1)
  assert.equal(incoming.listenerCount('data'),0)
  assert.deepEqual(record.packets,[{direction:'in',rawHex:packet.toString('hex')}])
  assert.ok(record.result.captureBudgetFailure.message.includes('aggregate'))
  assert.ok(record.result.traceFailure.message.includes('aggregate'))
  assert.equal(record.result.resultOmitted,true)
  assert.equal(record.result.traceFailure.sha256,createHash('sha256').update(packet).digest('hex'))
  await scratch(async dir=>{
    const output=join(dir,'aggregate-failed.json'),partial={format:1,containers:[],runs:[],trace:record}
    await assert.rejects(persistCapture(partial,output))
    assert.deepEqual(JSON.parse(await readFile(output,'utf8')),partial)
    assert.deepEqual(JSON.parse(await readFile(output+'.comparison.json','utf8')).differences,compare(partial,await load()))
  })
})

test('JSON preflight bounds exact UTF8 escaped size and rejects excessive or cyclic captures before serialization', () => {
  for (const value of [null,true,false,42,-0,'a\n\u0000é🦆\ud800',[null,'x'],{x:[1,2],s:'"\\'}]) assert.equal(jsonSize(value), Buffer.byteLength(JSON.stringify(value)))
  assert.equal(jsonSize(''), 2)
  assert.throws(() => jsonSize('é', 3), /bounded/)
  const cyclic = {}; cyclic.self = cyclic
  assert.throws(() => jsonSize(cyclic), /acyclic/)
  assert.throws(() => jsonSize({missing:undefined}), /canonical/)
  assert.throws(() => jsonSize(Array(4)), /canonical/)
  assert.throws(() => jsonSize(new Date(0)), /plain/)
  const overridden=[];overridden.toJSON=()=> 'unbounded override'
  assert.throws(() => jsonSize(overridden), /plain/)
  let deep = null; for (let i = 0; i < 66; i++) deep = [deep]
  assert.throws(() => jsonSize(deep), /bounded/)
  assert.equal(Math.max(...cases.map(c => rowsFor(c).length)), 2)
  assert.throws(() => validateRetained(Buffer.alloc(0)), /fixed/)
  assert.throws(() => validateRetained({length:CAPTURE_LIMIT+1}), /bounded/)
})

test('existing files, hardlinks, symlink directories and regular-file ancestors are rejected before any Docker work', async () => {
  await scratch(async dir => {
    const original = join(dir, 'original'); await writeFile(original, 'retained')
    await link(original, join(dir,'hardlink'))
    await symlink(dir, join(dir,'alias'))
    for (const output of [original,join(dir,'hardlink'),join(dir,'alias','new.json'),join(original,'child.json')]) {
      await assert.rejects(guardOutput(output))
      const child = spawnSync(process.execPath, ['scripts/capture-bulk-character-utf8-capacity.mjs', output], {cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:5000})
      assert.notEqual(child.status, 0)
      assert.ok(!child.stderr.includes('spawn docker'))
    }
    assert.equal(await readFile(original,'utf8'), 'retained')
  })
})

test('invalid captures and complete comparison sidecars are preserved before semantic validation', async () => {
  await scratch(async dir => {
    const actual = await load(); for (const run of actual.runs) find(run,'utf8-two-byte-exact-source64-to-utf8-varchar').execution.result.rowCount = 99
    actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
    const output = join(dir,'failed.json')
    await assert.rejects(persistCapture(actual,output), /fixed/)
    const saved = JSON.parse(await readFile(output,'utf8')), comparison = JSON.parse(await readFile(output+'.comparison.json','utf8'))
    assert.equal(find(saved.runs[0],'utf8-two-byte-exact-source64-to-utf8-varchar').execution.result.rowCount,99)
    assert.equal(comparison.retained,true)
    assert.ok(comparison.differences.some(d => d.path.includes('/execution/result/rowCount')))
    assert.equal(jsonSize(saved),Buffer.byteLength(JSON.stringify(saved)))
  })
})

test('derived comparison overflow preserves all bounded raw runs and an explicit failed sidecar', async () => {
  await scratch(async dir => {
    for (const [count,size] of [[4,47*1024*1024/4],[2,0]]) {
    const raw = {format:1, runs:Array.from({length:count}, (_, i) => ({data:String(i).repeat(size)}))}
    if (!size) {
      const available = CAPTURE_LIMIT-jsonSize(raw)
      raw.runs[0].data='0'.repeat(Math.floor(available/2))
      raw.runs[1].data='1'.repeat(Math.ceil(available/2))
      assert.equal(jsonSize(raw),CAPTURE_LIMIT)
    } else assert.ok(jsonSize(raw) < CAPTURE_LIMIT)
    const output = join(dir,`comparison-overflow-${count}.json`)
    await assert.rejects(finalizeCapture(raw,output), /bounded/)
    const savedBytes = await readCaptureFile(output)
    if (!size) assert.equal(savedBytes.length,CAPTURE_LIMIT)
    const saved = JSON.parse(savedBytes.toString())
    assert.deepEqual(saved,raw)
    assert.equal(saved.comparisons,undefined)
    const sidecar = JSON.parse(await readFile(output+'.comparison.json','utf8'))
    assert.equal(sidecar.retained,true)
    assert.equal(sidecar.differencesOmitted,true)
    assert.equal(sidecar.differences,undefined)
    assert.ok(sidecar.failure.message.includes('bounded'))
    assert.ok(sidecar.captureFailure.message.includes('bounded'))
    assert.ok(jsonSize(sidecar) < 4096)
    }
  })
})

test('an aborted reference startup retains the partial capture and every comparison difference', async () => {
  await scratch(async dir => {
    const output=join(dir,'aborted.json')
    const child=spawnSync(process.execPath,['scripts/capture-bulk-character-utf8-capacity.mjs',output],{cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:10000})
    assert.notEqual(child.status,0)
    const partial=JSON.parse(await readFile(output,'utf8')),sidecar=JSON.parse(await readFile(output+'.comparison.json','utf8'))
    assert.deepEqual(partial.runs,[])
    assert.ok(partial.failure.message.includes('docker'))
    assert.equal(sidecar.retained,true)
    assert.deepEqual(sidecar.differences,compare(partial,await load()))
    assert.ok(sidecar.differences.some(d=>d.path==='/runs/0'))
  })
})




test('small-message packet repartition fails framing pins despite identical payload and recomputed comparisons',async()=>{
  const actual=await load()
  for(const run of actual.runs){
    const record=find(run,'utf8-two-byte-exact-source64-to-utf8-varchar').readback
    const index=record.packets.findLastIndex(p=>p.direction==='in'),p=record.packets[index]
    const bytes=Buffer.from(p.rawHex,'hex'),cut=8+Math.floor((bytes.length-8)/2)
    assert.ok(cut>8&&cut<bytes.length)
    const first=Buffer.from(bytes.subarray(0,cut)),second=Buffer.concat([bytes.subarray(0,8),bytes.subarray(cut)])
    first[1]=0;first.writeUInt16BE(first.length,2)
    second[6]=(first[6]+1)&255;second.writeUInt16BE(second.length,2)
    record.packets.splice(index,1,{...p,rawHex:first.toString('hex')},{...p,rawHex:second.toString('hex')})
    assert.equal(first.subarray(8).toString('hex')+second.subarray(8).toString('hex'),bytes.subarray(8).toString('hex'))
  }
  actual.comparisons=actual.runs.slice(1).map(run=>compare(run,actual.runs[0]))
  assert.throws(()=>validate(actual),/fixed complete packet framing/)
})



// Independent expected rows originate in the four identical SQL observations,
// including client display versus SQL units; never inferred from a JS decoder.

const observed = [["utf8-empty-source64-to-utf8-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-empty-source64-to-utf8-char",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"  ",{"kind":"binary","value":"2020"},"  ",{"kind":"binary","value":"20002000"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-empty-source64-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-empty-source64-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"  ",{"kind":"binary","value":"20002000"},"  ",{"kind":"binary","value":"20002000"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-empty-sourcemax-to-utf8-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-empty-sourcemax-to-utf8-char",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"  ",{"kind":"binary","value":"2020"},"  ",{"kind":"binary","value":"20002000"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-empty-sourcemax-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-empty-sourcemax-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"  ",{"kind":"binary","value":"20002000"},"  ",{"kind":"binary","value":"20002000"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-ascii-exact-source64-to-utf8-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"AA",{"kind":"binary","value":"4141"},"AA",{"kind":"binary","value":"41004100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-ascii-exact-source64-to-utf8-char",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"AA",{"kind":"binary","value":"4141"},"AA",{"kind":"binary","value":"41004100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-ascii-exact-source64-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"AA",{"kind":"binary","value":"41004100"},"AA",{"kind":"binary","value":"41004100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-ascii-exact-source64-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"AA",{"kind":"binary","value":"41004100"},"AA",{"kind":"binary","value":"41004100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-ascii-exact-sourcemax-to-utf8-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"AA",{"kind":"binary","value":"4141"},"AA",{"kind":"binary","value":"41004100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-ascii-exact-sourcemax-to-utf8-char",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"AA",{"kind":"binary","value":"4141"},"AA",{"kind":"binary","value":"41004100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-ascii-exact-sourcemax-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"AA",{"kind":"binary","value":"41004100"},"AA",{"kind":"binary","value":"41004100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-ascii-exact-sourcemax-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"AA",{"kind":"binary","value":"41004100"},"AA",{"kind":"binary","value":"41004100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-ascii-over-source64-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-ascii-over-source64-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-ascii-over-source64-to-unicode-nvarchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-ascii-over-source64-to-unicode-nchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-ascii-over-sourcemax-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-ascii-over-sourcemax-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-ascii-over-sourcemax-to-unicode-nvarchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-ascii-over-sourcemax-to-unicode-nchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-two-byte-exact-source64-to-utf8-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"c2a9"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-exact-source64-to-utf8-char",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"c2a9"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-exact-source64-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a900"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-exact-source64-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a900"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-exact-sourcemax-to-utf8-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"c2a9"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-exact-sourcemax-to-utf8-char",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"c2a9"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-exact-sourcemax-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a900"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-exact-sourcemax-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a900"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-over-source64-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: ''."}}],["utf8-two-byte-over-source64-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: ' '."}}],["utf8-two-byte-over-source64-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a900"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-over-source64-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a900"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-over-sourcemax-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: ''."}}],["utf8-two-byte-over-sourcemax-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: ' '."}}],["utf8-two-byte-over-sourcemax-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a900"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-over-sourcemax-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a900"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-source64-to-utf8-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"𐀀",{"kind":"binary","value":"f0908080"},"𐀀",{"kind":"binary","value":"00d800dc"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-source64-to-utf8-char",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"𐀀",{"kind":"binary","value":"f0908080"},"𐀀",{"kind":"binary","value":"00d800dc"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-source64-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"𐀀",{"kind":"binary","value":"00d800dc"},"𐀀",{"kind":"binary","value":"00d800dc"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-source64-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"𐀀",{"kind":"binary","value":"00d800dc"},"𐀀",{"kind":"binary","value":"00d800dc"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-sourcemax-to-utf8-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"𐀀",{"kind":"binary","value":"f0908080"},"𐀀",{"kind":"binary","value":"00d800dc"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-sourcemax-to-utf8-char",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"𐀀",{"kind":"binary","value":"f0908080"},"𐀀",{"kind":"binary","value":"00d800dc"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-sourcemax-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"𐀀",{"kind":"binary","value":"00d800dc"},"𐀀",{"kind":"binary","value":"00d800dc"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-sourcemax-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"𐀀",{"kind":"binary","value":"00d800dc"},"𐀀",{"kind":"binary","value":"00d800dc"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-over-source64-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: ''."}}],["utf8-supplementary-over-source64-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: ' '."}}],["utf8-supplementary-over-source64-to-unicode-nvarchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: '\ud800'."}}],["utf8-supplementary-over-source64-to-unicode-nchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: '\ud800'."}}],["utf8-supplementary-over-sourcemax-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: ''."}}],["utf8-supplementary-over-sourcemax-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: ' '."}}],["utf8-supplementary-over-sourcemax-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-over-sourcemax-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2," ",{"kind":"binary","value":"2000"}," ",{"kind":"binary","value":"2000"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-source64-to-utf8-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-source64-to-utf8-char",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-source64-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-source64-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-sourcemax-to-utf8-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-sourcemax-to-utf8-char",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-sourcemax-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-sourcemax-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-nul-over-source64-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nul-over-source64-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nul-over-source64-to-unicode-nvarchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nul-over-source64-to-unicode-nchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nul-over-sourcemax-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nul-over-sourcemax-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nul-over-sourcemax-to-unicode-nvarchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nul-over-sourcemax-to-unicode-nchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nbsp-over-source64-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nbsp-over-source64-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nbsp-over-source64-to-unicode-nvarchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nbsp-over-source64-to-unicode-nchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nbsp-over-sourcemax-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nbsp-over-sourcemax-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nbsp-over-sourcemax-to-unicode-nvarchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-nbsp-over-sourcemax-to-unicode-nchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'A'."}}],["utf8-incomplete-source64-to-utf8-varchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-source64-to-utf8-char",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-source64-to-unicode-nvarchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-source64-to-unicode-nchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-sourcemax-to-utf8-varchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-sourcemax-to-utf8-char",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-sourcemax-to-unicode-nvarchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-sourcemax-to-unicode-nchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-continuation-source64-to-utf8-varchar",{"rows":[[[9833,0,0,0]],[]],"errors":[[9833,2,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":9833,"code":"EREQUEST","message":"Invalid data for UTF8-encoded characters"}}],["utf8-continuation-source64-to-utf8-char",{"rows":[[[9833,0,0,0]],[]],"errors":[[9833,2,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":9833,"code":"EREQUEST","message":"Invalid data for UTF8-encoded characters"}}],["utf8-continuation-source64-to-unicode-nvarchar",{"rows":[[[9833,0,0,0]],[]],"errors":[[9833,2,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":9833,"code":"EREQUEST","message":"Invalid data for UTF8-encoded characters"}}],["utf8-continuation-source64-to-unicode-nchar",{"rows":[[[9833,0,0,0]],[]],"errors":[[9833,2,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":9833,"code":"EREQUEST","message":"Invalid data for UTF8-encoded characters"}}],["utf8-continuation-sourcemax-to-utf8-varchar",{"rows":[[[9833,0,0,0]],[]],"errors":[[9833,2,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":9833,"code":"EREQUEST","message":"Invalid data for UTF8-encoded characters"}}],["utf8-continuation-sourcemax-to-utf8-char",{"rows":[[[9833,0,0,0]],[]],"errors":[[9833,2,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":9833,"code":"EREQUEST","message":"Invalid data for UTF8-encoded characters"}}],["utf8-continuation-sourcemax-to-unicode-nvarchar",{"rows":[[[9833,0,0,0]],[]],"errors":[[9833,2,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":9833,"code":"EREQUEST","message":"Invalid data for UTF8-encoded characters"}}],["utf8-continuation-sourcemax-to-unicode-nchar",{"rows":[[[9833,0,0,0]],[]],"errors":[[9833,2,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":9833,"code":"EREQUEST","message":"Invalid data for UTF8-encoded characters"}}],["utf8-invalid-range-source64-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: ''."}}],["utf8-invalid-range-source64-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: '  '."}}],["utf8-invalid-range-source64-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"�B",{"kind":"binary","value":"fdff4200"},"�B",{"kind":"binary","value":"fdff4200"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-invalid-range-source64-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"�B",{"kind":"binary","value":"fdff4200"},"�B",{"kind":"binary","value":"fdff4200"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-invalid-range-sourcemax-to-utf8-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: ''."}}],["utf8-invalid-range-sourcemax-to-utf8-char",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: '  '."}}],["utf8-invalid-range-sourcemax-to-unicode-nvarchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"�B",{"kind":"binary","value":"fdff4200"},"�B",{"kind":"binary","value":"fdff4200"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-invalid-range-sourcemax-to-unicode-nchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"�B",{"kind":"binary","value":"fdff4200"},"�B",{"kind":"binary","value":"fdff4200"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-incomplete-suffix-source64-to-utf8-varchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-suffix-source64-to-utf8-char",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-suffix-source64-to-unicode-nvarchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-suffix-source64-to-unicode-nchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-suffix-sourcemax-to-utf8-varchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-suffix-sourcemax-to-utf8-char",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-suffix-sourcemax-to-unicode-nvarchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-incomplete-suffix-sourcemax-to-unicode-nchar",{"rows":[[[7339,0,0,0]],[]],"errors":[[7339,1,16]],"info":[],"rowCount":0,"error":{"number":7339,"code":"EREQUEST","message":"OLE DB provider 'STREAM' for linked server '(null)' returned invalid data for column '[!BulkInsert].value'."}}],["utf8-ascii-over-source64-to-cp1251-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-ascii-over-source64-to-cp1252-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-ascii-over-sourcemax-to-cp1251-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-ascii-over-sourcemax-to-cp1252-varchar",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"error":{"number":2628,"code":"EREQUEST","message":"String or binary data would be truncated in table 'msduck_audit_76c4d9000b994dad97e4158ffd4411c3.dbo.utf8_capacity_probe', column 'value'. Truncated value: 'AA'."}}],["utf8-two-byte-exact-source64-to-cp1251-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a9"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-exact-source64-to-cp1252-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a9"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-exact-sourcemax-to-cp1251-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a9"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-two-byte-exact-sourcemax-to-cp1252-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a9"},"©",{"kind":"binary","value":"a900"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-source64-to-cp1251-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"??",{"kind":"binary","value":"3f3f"},"??",{"kind":"binary","value":"3f003f00"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-source64-to-cp1252-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"??",{"kind":"binary","value":"3f3f"},"??",{"kind":"binary","value":"3f003f00"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-sourcemax-to-cp1251-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"??",{"kind":"binary","value":"3f3f"},"??",{"kind":"binary","value":"3f003f00"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-supplementary-exact-sourcemax-to-cp1252-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"??",{"kind":"binary","value":"3f3f"},"??",{"kind":"binary","value":"3f003f00"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-source64-to-cp1251-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-source64-to-cp1252-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-sourcemax-to-cp1251-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}],["utf8-space-over-sourcemax-to-cp1252-varchar",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"error":null}]]

test('all128 original isolated outcomes preserve native bytes, units, diagnostics and original counters in allfour runs',async()=>{
  const f=validateRetained(await readFile(fixture))
  assert.equal(cases.length,128);assert.equal(new Set(cases.map(c=>c.name)).size,128)
  assert.equal(cases.filter(c=>c.sourceWidth===64).length,64);assert.equal(cases.filter(c=>c.sourceWidth==='max').length,64)
  assert.equal(cases.filter(c=>c.target==='cp1251'||c.target==='cp1252').length,16)
  for(const run of f.runs)for(const [i,o]of run.observations.entries()) {
    assert.deepEqual(o.case,cases[i]);assert.deepEqual(o.input,rowsFor(cases[i]));assert.equal(o.input.length,2)
    const [name,x]=observed[i];assert.equal(o.case.name,name)
    assert.deepEqual(o.readback.result.sets.map(s=>s.rows),x.rows,name)
    assert.deepEqual(o.execution.result.errors.map(e=>[e.number,e.state,e.class]),x.errors,name)
    assert.deepEqual(o.execution.result.info.map(e=>[e.number,e.state,e.class]),x.info,name)
    assert.equal(o.execution.result.rowCount,x.rowCount,name)
    // Callback messages retain ephemeral server/database names and are already
    // pinned against each run's actual identities by validate().
    assert.equal(o.execution.result.error?.number??null,x.error?.number??null,name)
    assert.equal(o.readback.session,'original');assert.equal(o.recoveryReadback,undefined)
  }
  const replay=spawnSync(process.execPath,['scripts/capture-bulk-character-utf8-capacity.mjs','--replay-fixture'],{cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:15000})
  assert.equal(replay.status,0,replay.stderr)
})

test('uniform semantic, source, capacity, unit and provenance corruption cannot invent capacity results',async()=>{
  const retained=await load()
  const changes=[o=>{o.case.targetWidth++},o=>{o.case.sourceWidth='max'},o=>{o.input[1].valueHex='414141'},o=>{o.execution.result.rowCount=999},o=>{o.readback.result.sets[1].rows[1][2].value='41'}]
  for(const change of changes){const f=structuredClone(retained);for(const run of f.runs)change(find(run,'utf8-two-byte-exact-source64-to-utf8-varchar'));f.comparisons=f.runs.slice(1).map(r=>compare(r,f.runs[0]));assert.throws(()=>validate(f),/fixed/)}
  for(const mode of ['source','helper','omission']){const f=structuredClone(retained);if(mode==='source')f.provenance.sourceSha='0'.repeat(64);else if(mode==='helper')f.provenance.helperSha='0'.repeat(64);else for(const r of f.runs)r.observations.pop();f.comparisons=f.runs.slice(1).map(r=>compare(r,f.runs[0]));assert.throws(()=>validate(f))}
})

test('captured padding and conversion-aware boundaries remain distinct from unchanged native input',async()=>{
 const f=await load()
 const expected=[["empty","utf8-char","2020"],["empty","unicode-nchar","20002000"],["space-over","utf8-varchar","41"],["two-byte-over","unicode-nvarchar","a900"],["invalid-range","unicode-nvarchar","fdff4200"]]
 for(const run of f.runs)for(const width of [64,'max']){
  for(const [category,target,hex]of expected){const o=find(run,`utf8-${category}-source${width}-to-${target}`);assert.equal(o.readback.result.sets[1].rows[1][2].value,hex);assert.equal(o.execution.result.errors.length,0)}
  for(const category of ['nul-over','nbsp-over','two-byte-over','invalid-range']){const o=find(run,`utf8-${category}-source${width}-to-utf8-varchar`);assert.deepEqual(o.execution.result.errors.map(e=>[e.number,e.state,e.class]),[[2628,1,16]]);assert.deepEqual(o.readback.result.sets[1].rows,[])}
 }
})
