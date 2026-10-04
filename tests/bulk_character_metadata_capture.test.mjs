import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile, mkdir, mkdtemp, writeFile, realpath, symlink, link, rm, truncate} from 'node:fs/promises'
import {join} from 'node:path'
import {fileURLToPath} from 'node:url'
import {spawnSync} from 'node:child_process'
import {EventEmitter} from 'node:events'
import {createHash} from 'node:crypto'
import {cases, rowsFor, jsonSize, CAPTURE_LIMIT, validate, validateRetained, compare, guardOutput, persistCapture, finalizeCapture, readCaptureFile, exchange, budget} from '../scripts/capture-bulk-character-metadata.mjs'
const fixture = new URL('../reference/bulk-character-metadata.json', import.meta.url)
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
      const packets = find(run, 'cp1252-varchar8-wirevarchar8-target8-baseline').execution.packets
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
    const p=find(run,'cp1252-varchar8-wirevarchar8-target8-baseline').execution.packets.find(p=>p.direction==='in')
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
      const o=find(run,'cp1252-varcharmax-wirevarcharmax-targetmax-baseline')
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
    for(const run of actual.runs)for(const p of find(run,'cp1252-varcharmax-wirevarcharmax-targetmax-baseline')[phase].packets.filter(p=>p.direction==='out'&&p.rawHex.startsWith('01'))) {
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
      const child = spawnSync(process.execPath, ['scripts/capture-bulk-character-metadata.mjs', output], {cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:5000})
      assert.notEqual(child.status, 0)
      assert.ok(!child.stderr.includes('spawn docker'))
    }
    assert.equal(await readFile(original,'utf8'), 'retained')
  })
})

test('invalid captures and complete comparison sidecars are preserved before semantic validation', async () => {
  await scratch(async dir => {
    const actual = await load(); for (const run of actual.runs) find(run,'cp1252-varchar8-wirevarchar8-target8-baseline').execution.result.rowCount = 99
    actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
    const output = join(dir,'failed.json')
    await assert.rejects(persistCapture(actual,output), /fixed/)
    const saved = JSON.parse(await readFile(output,'utf8')), comparison = JSON.parse(await readFile(output+'.comparison.json','utf8'))
    assert.equal(find(saved.runs[0],'cp1252-varchar8-wirevarchar8-target8-baseline').execution.result.rowCount,99)
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
    const child=spawnSync(process.execPath,['scripts/capture-bulk-character-metadata.mjs',output],{cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:10000})
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
    const record=find(run,'cp1252-varchar8-wirevarchar8-target8-baseline').readback
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


const observed = [["cp1252-varchar1-wirevarchar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar1-wirevarchar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar1-wirevarchar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varchar1-wirevarchar8-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar1-wirevarchar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar1-wirevarchar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varchar1-wirevarcharmax-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar1-wirevarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar1-wirevarcharmax-targetmax-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar8-wirevarchar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar8-wirevarchar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar8-wirevarchar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varchar8-wirevarchar8-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar8-wirevarchar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar8-wirevarchar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varchar8-wirevarcharmax-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar8-wirevarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar8-wirevarcharmax-targetmax-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varcharmax-wirevarchar1-target1-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varcharmax-wirevarchar1-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varcharmax-wirevarchar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varcharmax-wirevarchar8-target1-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varcharmax-wirevarchar8-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varcharmax-wirevarchar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varcharmax-wirevarcharmax-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varcharmax-wirevarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varcharmax-wirevarcharmax-targetmax-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-char1-wirechar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-char1-wirechar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-char1-wirechar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-char1-wirechar8-target1-baseline",{"rows":[[[4815,0,0,0]],[]],"errors":[[4815,1,17]],"info":[],"rowCount":0,"callback":4815}],["cp1252-char1-wirechar8-target8-baseline",{"rows":[[[4815,0,0,0]],[]],"errors":[[4815,1,17]],"info":[],"rowCount":0,"callback":4815}],["cp1252-char1-wirechar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-char8-wirechar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-char8-wirechar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A       ",{"kind":"binary","value":"4120202020202020"},"A       ",{"kind":"binary","value":"41002000200020002000200020002000"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-char8-wirechar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-char8-wirechar8-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-char8-wirechar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A       ",{"kind":"binary","value":"4120202020202020"},"A       ",{"kind":"binary","value":"41002000200020002000200020002000"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-char8-wirechar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varchar8-wirechar8-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,1,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-char8-wirevarchar8-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,1,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varchar1-wirevarchar8-target8-null-only",{"rows":[[[0,1,0,0]],[[1,null,null,null,null]]],"errors":[],"info":[],"rowCount":1,"callback":null}],["cp1252-varchar1-wirevarchar8-target8-empty",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varcharmax-wirevarchar8-target8-null-only",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varcharmax-wirevarchar8-target8-empty",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-varchar8-wirevarcharmax-target8-null-only",{"rows":[[[0,1,0,0]],[[1,null,null,null,null]]],"errors":[],"info":[],"rowCount":1,"callback":null}],["cp1252-varchar8-wirevarcharmax-target8-empty",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-varchar1-wirevarchar8-target8-declared-over",{"rows":[[[4815,0,0,0]],[]],"errors":[[4815,1,17]],"info":[],"rowCount":0,"callback":4815}],["cp1252-varchar8-wirevarchar8-target1-target-over",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"callback":2628}],["cp1251-varchar1-wirevarchar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar1-wirevarchar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar1-wirevarchar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varchar1-wirevarchar8-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar1-wirevarchar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar1-wirevarchar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varchar1-wirevarcharmax-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar1-wirevarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar1-wirevarcharmax-targetmax-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar8-wirevarchar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar8-wirevarchar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar8-wirevarchar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varchar8-wirevarchar8-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar8-wirevarchar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar8-wirevarchar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varchar8-wirevarcharmax-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar8-wirevarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar8-wirevarcharmax-targetmax-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varcharmax-wirevarchar1-target1-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varcharmax-wirevarchar1-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varcharmax-wirevarchar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varcharmax-wirevarchar8-target1-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varcharmax-wirevarchar8-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varcharmax-wirevarchar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varcharmax-wirevarcharmax-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varcharmax-wirevarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varcharmax-wirevarcharmax-targetmax-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-char1-wirechar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-char1-wirechar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-char1-wirechar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-char1-wirechar8-target1-baseline",{"rows":[[[4815,0,0,0]],[]],"errors":[[4815,1,17]],"info":[],"rowCount":0,"callback":4815}],["cp1251-char1-wirechar8-target8-baseline",{"rows":[[[4815,0,0,0]],[]],"errors":[[4815,1,17]],"info":[],"rowCount":0,"callback":4815}],["cp1251-char1-wirechar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-char8-wirechar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-char8-wirechar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A       ",{"kind":"binary","value":"4120202020202020"},"A       ",{"kind":"binary","value":"41002000200020002000200020002000"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-char8-wirechar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-char8-wirechar8-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-char8-wirechar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A       ",{"kind":"binary","value":"4120202020202020"},"A       ",{"kind":"binary","value":"41002000200020002000200020002000"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-char8-wirechar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varchar8-wirechar8-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,1,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-char8-wirevarchar8-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,1,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varchar1-wirevarchar8-target8-null-only",{"rows":[[[0,1,0,0]],[[1,null,null,null,null]]],"errors":[],"info":[],"rowCount":1,"callback":null}],["cp1251-varchar1-wirevarchar8-target8-empty",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varcharmax-wirevarchar8-target8-null-only",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varcharmax-wirevarchar8-target8-empty",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1251-varchar8-wirevarcharmax-target8-null-only",{"rows":[[[0,1,0,0]],[[1,null,null,null,null]]],"errors":[],"info":[],"rowCount":1,"callback":null}],["cp1251-varchar8-wirevarcharmax-target8-empty",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1251-varchar1-wirevarchar8-target8-declared-over",{"rows":[[[4815,0,0,0]],[]],"errors":[[4815,1,17]],"info":[],"rowCount":0,"callback":4815}],["cp1251-varchar8-wirevarchar8-target1-target-over",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"callback":2628}],["utf8-varchar1-wirevarchar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar1-wirevarchar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar1-wirevarchar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varchar1-wirevarchar8-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar1-wirevarchar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar1-wirevarchar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varchar1-wirevarcharmax-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar1-wirevarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar1-wirevarcharmax-targetmax-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar8-wirevarchar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar8-wirevarchar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar8-wirevarchar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varchar8-wirevarchar8-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar8-wirevarchar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar8-wirevarchar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varchar8-wirevarcharmax-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar8-wirevarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar8-wirevarcharmax-targetmax-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varcharmax-wirevarchar1-target1-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varcharmax-wirevarchar1-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varcharmax-wirevarchar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varcharmax-wirevarchar8-target1-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varcharmax-wirevarchar8-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varcharmax-wirevarchar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varcharmax-wirevarcharmax-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varcharmax-wirevarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varcharmax-wirevarcharmax-targetmax-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-char1-wirechar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-char1-wirechar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-char1-wirechar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-char1-wirechar8-target1-baseline",{"rows":[[[4815,0,0,0]],[]],"errors":[[4815,1,17]],"info":[],"rowCount":0,"callback":4815}],["utf8-char1-wirechar8-target8-baseline",{"rows":[[[4815,0,0,0]],[]],"errors":[[4815,1,17]],"info":[],"rowCount":0,"callback":4815}],["utf8-char1-wirechar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-char8-wirechar1-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-char8-wirechar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A       ",{"kind":"binary","value":"4120202020202020"},"A       ",{"kind":"binary","value":"41002000200020002000200020002000"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-char8-wirechar1-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-char8-wirechar8-target1-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"41"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-char8-wirechar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A       ",{"kind":"binary","value":"4120202020202020"},"A       ",{"kind":"binary","value":"41002000200020002000200020002000"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-char8-wirechar8-targetmax-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varchar8-wirechar8-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,1,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-char8-wirevarchar8-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,1,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varchar1-wirevarchar8-target8-null-only",{"rows":[[[0,1,0,0]],[[1,null,null,null,null]]],"errors":[],"info":[],"rowCount":1,"callback":null}],["utf8-varchar1-wirevarchar8-target8-empty",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varcharmax-wirevarchar8-target8-null-only",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varcharmax-wirevarchar8-target8-empty",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["utf8-varchar8-wirevarcharmax-target8-null-only",{"rows":[[[0,1,0,0]],[[1,null,null,null,null]]],"errors":[],"info":[],"rowCount":1,"callback":null}],["utf8-varchar8-wirevarcharmax-target8-empty",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"",{"kind":"binary","value":""},"",{"kind":"binary","value":""}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["utf8-varchar1-wirevarchar8-target8-declared-over",{"rows":[[[4815,0,0,0]],[]],"errors":[[4815,1,17]],"info":[],"rowCount":0,"callback":4815}],["utf8-varchar8-wirevarchar8-target1-target-over",{"rows":[[[2628,0,0,0]],[]],"errors":[[2628,1,16]],"info":[[3621,0,0]],"rowCount":0,"callback":2628}],["cp1252-nvarchar1-wirenvarchar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-nvarchar1-wirenvarchar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-nvarchar1-wirenvarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-nvarchar8-wirenvarchar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-nvarchar8-wirenvarchar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-nvarchar8-wirenvarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-nvarcharmax-wirenvarchar1-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-nvarcharmax-wirenvarchar8-target8-baseline",{"rows":[[[4816,0,0,0]],[]],"errors":[[4816,2,16]],"info":[],"rowCount":0,"callback":4816}],["cp1252-nvarcharmax-wirenvarcharmax-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-nchar1-wirenchar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A",{"kind":"binary","value":"4100"},"A",{"kind":"binary","value":"4100"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-nchar1-wirenchar8-target8-baseline",{"rows":[[[4815,0,0,0]],[]],"errors":[[4815,1,17]],"info":[],"rowCount":0,"callback":4815}],["cp1252-nchar8-wirenchar1-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A       ",{"kind":"binary","value":"41002000200020002000200020002000"},"A       ",{"kind":"binary","value":"41002000200020002000200020002000"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}],["cp1252-nchar8-wirenchar8-target8-baseline",{"rows":[[[0,2,0,0]],[[1,null,null,null,null],[2,"A       ",{"kind":"binary","value":"41002000200020002000200020002000"},"A       ",{"kind":"binary","value":"41002000200020002000200020002000"}]]],"errors":[],"info":[],"rowCount":2,"callback":null}]]

test('all160 declared/wire/target profiles retain every original row, counter and diagnostic in allfour runs',async()=>{
 const f=validateRetained(await readFile(fixture));assert.equal(cases.length,160);assert.equal(new Set(cases.map(c=>c.name)).size,160)
 assert.equal(cases.reduce((n,c)=>n+rowsFor(c).length,0),311)
 for(const run of f.runs)for(const [i,o]of run.observations.entries()){
  const [name,x]=observed[i];assert.equal(o.case.name,name);assert.deepEqual(o.case,cases[i]);assert.deepEqual(o.input,rowsFor(cases[i]));
  assert.deepEqual(o.readback.result.sets.map(s=>s.rows),x.rows,name);assert.deepEqual(o.execution.result.errors.map(e=>[e.number,e.state,e.class]),x.errors,name);assert.deepEqual(o.execution.result.info.map(e=>[e.number,e.state,e.class]),x.info,name);assert.equal(o.execution.result.rowCount,x.rowCount,name);assert.equal(o.execution.result.error?.number??null,x.callback,name);assert.equal(o.readback.session,'original');assert.equal(o.recoveryReadback,undefined)
 }
 const replay=spawnSync(process.execPath,['scripts/capture-bulk-character-metadata.mjs','--replay-fixture'],{cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:15000});assert.equal(replay.status,0,replay.stderr)
})

test('uniform width/family/native/counter corruption and omitted profiles cannot disappear behind recomputed comparisons',async()=>{
 const retained=await load();const name='cp1252-varchar8-wirevarchar8-target8-baseline';
 const changes=[o=>{o.case.wireWidth=1},o=>{o.case.sourceWidth=1},o=>{o.case.wireFamily='char'},o=>{o.readback.result.sets[0].rows[0][0]=0x1234},o=>{o.readback.result.sets[1].rows[1][2].value='4141'},o=>{o.metadata.result.sets[0].columns[0].length=65535}];
 for(const change of changes){const a=structuredClone(retained);for(const r of a.runs)change(find(r,name));a.comparisons=a.runs.slice(1).map(r=>compare(r,a.runs[0]));assert.throws(()=>validate(a),/fixed/)}
 for(const mode of ['source','helper','omission']){const a=structuredClone(retained);if(mode==='source')a.provenance.sourceSha='0'.repeat(64);else if(mode==='helper')a.provenance.helperSha='0'.repeat(64);else for(const r of a.runs)r.observations.pop();a.comparisons=a.runs.slice(1).map(r=>compare(r,a.runs[0]));assert.throws(()=>validate(a))}
})

test('original malformed TYPE_INFO width and ROW length corruption is rejected before inferred summaries',async()=>{
 const retained=await load()
 for(const mode of ['width','row-length']){const a=structuredClone(retained);for(const r of a.runs){const o=find(r,'cp1252-varchar8-wirevarchar8-target8-baseline');const p=o.execution.packets.find(p=>p.direction==='out'&&p.rawHex.startsWith('07'));const b=Buffer.from(p.rawHex,'hex');const col=b.indexOf(Buffer.from('a708000904d00034','hex'));assert.ok(col>=0);if(mode==='width')b.writeUInt16LE(0xffff,col+1);else{const row=b.indexOf(Buffer.from('d10402000000010041','hex'),col);assert.ok(row>=0);b.writeUInt16LE(0x7fff,row+6)}p.rawHex=b.toString('hex')};a.comparisons=a.runs.slice(1).map(r=>compare(r,a.runs[0]));assert.throws(()=>validate(a),/fixed/)}
 assert.throws(()=>rowsFor({...cases[0],values:[null,'41'.repeat(17)]}));assert.throws(()=>rowsFor({...cases[0],values:[null,'4']}))
})

test('NULL admission, row declaration limits and target capacity remain distinct measured boundaries',async()=>{
 const f=await load()
 for(const run of f.runs)for(const profile of ['cp1252','cp1251','utf8']){
  const error=(name,expected)=>assert.deepEqual(find(run,name).execution.result.errors.map(e=>[e.number,e.state,e.class]),expected)
  error(`${profile}-varchar1-wirevarchar8-target8-baseline`,[])
  error(`${profile}-varchar1-wirevarcharmax-target8-baseline`,[])
  error(`${profile}-varchar1-wirevarchar8-target8-null-only`,[])
  error(`${profile}-varchar8-wirevarcharmax-target8-null-only`,[])
  error(`${profile}-varcharmax-wirevarchar8-target8-null-only`,[[4816,2,16]])
  error(`${profile}-varchar1-wirevarchar8-target8-declared-over`,[[4815,1,17]])
  error(`${profile}-varchar8-wirevarchar8-target1-target-over`,[[2628,1,16]])
  error(`${profile}-varchar8-wirechar8-target8-baseline`,[[4816,1,16]])
  error(`${profile}-char8-wirevarchar8-target8-baseline`,[[4816,1,16]])
  const padded=find(run,`${profile}-char8-wirechar1-target8-baseline`)
  assert.equal(padded.input[1].valueHex,'41')
  assert.equal(padded.readback.result.sets[1].rows[1][2].value,'41'+'20'.repeat(7))
 }
 // Declaration units remain distinct from actual Unicode TYPE_INFO byte width.
 for(const run of f.runs)for(const family of ['nvarchar','nchar']){
  const o=find(run,`cp1252-${family}8-wire${family}8-target8-baseline`)
  assert.equal(o.metadata.result.sets[0].columns[0].length,16)
  assert.equal(o.case.wireWidth,8)
  assert.equal(o.readback.result.sets[1].rows[1][4].value,family==='nchar'?'4100'+'2000'.repeat(7):'4100')
 }
})
