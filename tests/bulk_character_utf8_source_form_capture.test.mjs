import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile, mkdir, mkdtemp, writeFile, realpath, symlink, link, rm, truncate} from 'node:fs/promises'
import {join} from 'node:path'
import {fileURLToPath} from 'node:url'
import {spawnSync} from 'node:child_process'
import {EventEmitter} from 'node:events'
import {createHash} from 'node:crypto'
import {cases, rowsFor, jsonSize, CAPTURE_LIMIT, validate, validateRetained, compare, guardOutput, persistCapture, finalizeCapture, readCaptureFile, exchange, budget} from '../scripts/capture-bulk-character-utf8-source-form.mjs'
const fixture = new URL('../reference/bulk-character-utf8-source-form.json', import.meta.url)
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
      const packets = find(run, 'utf8-valid-c2a9-source64-to-utf8').execution.packets
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
    const p=find(run,'utf8-valid-c2a9-source64-to-utf8').execution.packets.find(p=>p.direction==='in')
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
      const o=find(run,'utf8-valid-f48fbfbf-sourcemax-to-unicode')
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
    for(const run of actual.runs)for(const p of find(run,'utf8-valid-f48fbfbf-sourcemax-to-unicode')[phase].packets.filter(p=>p.direction==='out'&&p.rawHex.startsWith('01'))) {
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
      const child = spawnSync(process.execPath, ['scripts/capture-bulk-character-utf8-source-form.mjs', output], {cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:5000})
      assert.notEqual(child.status, 0)
      assert.ok(!child.stderr.includes('spawn docker'))
    }
    assert.equal(await readFile(original,'utf8'), 'retained')
  })
})

test('invalid captures and complete comparison sidecars are preserved before semantic validation', async () => {
  await scratch(async dir => {
    const actual = await load(); for (const run of actual.runs) find(run,'utf8-valid-c2a9-source64-to-utf8').execution.result.rowCount = 99
    actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
    const output = join(dir,'failed.json')
    await assert.rejects(persistCapture(actual,output), /fixed/)
    const saved = JSON.parse(await readFile(output,'utf8')), comparison = JSON.parse(await readFile(output+'.comparison.json','utf8'))
    assert.equal(find(saved.runs[0],'utf8-valid-c2a9-source64-to-utf8').execution.result.rowCount,99)
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
    const child=spawnSync(process.execPath,['scripts/capture-bulk-character-utf8-source-form.mjs',output],{cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:10000})
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
    const record=find(run,'utf8-valid-c2a9-source64-to-utf8').readback
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
const observed = [["utf8-valid-00-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-00-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-00-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"\u0000",{"kind":"binary","value":"00"},"\u0000",{"kind":"binary","value":"0000"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-00-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"\u0000",{"kind":"binary","value":"0000"},"\u0000",{"kind":"binary","value":"0000"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-7f-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-7f-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-7f-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"",{"kind":"binary","value":"7f"},"",{"kind":"binary","value":"7f00"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-7f-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"",{"kind":"binary","value":"7f00"},"",{"kind":"binary","value":"7f00"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-c280-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-c280-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-c280-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"",{"kind":"binary","value":"c280"},"",{"kind":"binary","value":"8000"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-c280-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"",{"kind":"binary","value":"8000"},"",{"kind":"binary","value":"8000"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-c2a9-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-c2a9-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-c2a9-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"c2a9"},"©",{"kind":"binary","value":"a900"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-c2a9-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"©",{"kind":"binary","value":"a900"},"©",{"kind":"binary","value":"a900"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-dfbf-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-dfbf-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-dfbf-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"߿",{"kind":"binary","value":"dfbf"},"߿",{"kind":"binary","value":"ff07"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-dfbf-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"߿",{"kind":"binary","value":"ff07"},"߿",{"kind":"binary","value":"ff07"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-e0a080-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-e0a080-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-e0a080-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"ࠀ",{"kind":"binary","value":"e0a080"},"ࠀ",{"kind":"binary","value":"0008"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-e0a080-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"ࠀ",{"kind":"binary","value":"0008"},"ࠀ",{"kind":"binary","value":"0008"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-ed9fbf-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-ed9fbf-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-ed9fbf-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"퟿",{"kind":"binary","value":"ed9fbf"},"퟿",{"kind":"binary","value":"ffd7"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-ed9fbf-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"퟿",{"kind":"binary","value":"ffd7"},"퟿",{"kind":"binary","value":"ffd7"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-ee8080-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-ee8080-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-ee8080-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"",{"kind":"binary","value":"ee8080"},"",{"kind":"binary","value":"00e0"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-ee8080-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"",{"kind":"binary","value":"00e0"},"",{"kind":"binary","value":"00e0"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-efbfbf-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-efbfbf-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-efbfbf-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"￿",{"kind":"binary","value":"efbfbf"},"￿",{"kind":"binary","value":"ffff"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-efbfbf-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"￿",{"kind":"binary","value":"ffff"},"￿",{"kind":"binary","value":"ffff"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-f0908080-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-f0908080-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-f0908080-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"𐀀",{"kind":"binary","value":"f0908080"},"𐀀",{"kind":"binary","value":"00d800dc"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-f0908080-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"𐀀",{"kind":"binary","value":"00d800dc"},"𐀀",{"kind":"binary","value":"00d800dc"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-f48fbfbf-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-f48fbfbf-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-f48fbfbf-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"􏿿",{"kind":"binary","value":"f48fbfbf"},"􏿿",{"kind":"binary","value":"ffdbffdf"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-f48fbfbf-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"􏿿",{"kind":"binary","value":"ffdbffdf"},"􏿿",{"kind":"binary","value":"ffdbffdf"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-c2bf-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-c2bf-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-valid-c2bf-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"¿",{"kind":"binary","value":"c2bf"},"¿",{"kind":"binary","value":"bf00"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-valid-c2bf-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"¿",{"kind":"binary","value":"bf00"},"¿",{"kind":"binary","value":"bf00"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-truncated-c2-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-c2-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-c2-sourcemax-to-utf8",{"rows":[],"errors":[[4896,7,17]],"info":[],"counters":[[4896,0,0,0]]}],["utf8-truncated-c2-sourcemax-to-unicode",{"rows":[],"errors":[[7339,1,16]],"info":[],"counters":[[7339,0,0,0]]}],["utf8-truncated-df-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-df-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-df-sourcemax-to-utf8",{"rows":[],"errors":[[4896,7,17]],"info":[],"counters":[[4896,0,0,0]]}],["utf8-truncated-df-sourcemax-to-unicode",{"rows":[],"errors":[[7339,1,16]],"info":[],"counters":[[7339,0,0,0]]}],["utf8-truncated-e0a0-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-e0a0-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-e0a0-sourcemax-to-utf8",{"rows":[],"errors":[[4896,7,17]],"info":[],"counters":[[4896,0,0,0]]}],["utf8-truncated-e0a0-sourcemax-to-unicode",{"rows":[],"errors":[[7339,1,16]],"info":[],"counters":[[7339,0,0,0]]}],["utf8-truncated-ed9f-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-ed9f-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-ed9f-sourcemax-to-utf8",{"rows":[],"errors":[[4896,7,17]],"info":[],"counters":[[4896,0,0,0]]}],["utf8-truncated-ed9f-sourcemax-to-unicode",{"rows":[],"errors":[[7339,1,16]],"info":[],"counters":[[7339,0,0,0]]}],["utf8-truncated-f09080-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-f09080-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-f09080-sourcemax-to-utf8",{"rows":[],"errors":[[4896,7,17]],"info":[],"counters":[[4896,0,0,0]]}],["utf8-truncated-f09080-sourcemax-to-unicode",{"rows":[],"errors":[[7339,1,16]],"info":[],"counters":[[7339,0,0,0]]}],["utf8-truncated-f48fbf-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-f48fbf-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-truncated-f48fbf-sourcemax-to-utf8",{"rows":[],"errors":[[4896,7,17]],"info":[],"counters":[[4896,0,0,0]]}],["utf8-truncated-f48fbf-sourcemax-to-unicode",{"rows":[],"errors":[[7339,1,16]],"info":[],"counters":[[0,0,0,0]]}],["utf8-malformed-c228-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-c228-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-c228-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"�(",{"kind":"binary","value":"c228"},"�(",{"kind":"binary","value":"fdff2800"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-malformed-c228-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"�(",{"kind":"binary","value":"fdff2800"},"�(",{"kind":"binary","value":"fdff2800"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-malformed-e08080-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-e08080-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-e08080-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"���",{"kind":"binary","value":"e08080"},"��",{"kind":"binary","value":"fdfffdff"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-malformed-e08080-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"��",{"kind":"binary","value":"fdfffdff"},"��",{"kind":"binary","value":"fdfffdff"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-malformed-eda080-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-eda080-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-eda080-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"���",{"kind":"binary","value":"eda080"},"��",{"kind":"binary","value":"fdfffdff"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-malformed-eda080-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"��",{"kind":"binary","value":"fdfffdff"},"��",{"kind":"binary","value":"fdfffdff"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-malformed-f0808080-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-f0808080-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-f0808080-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"����",{"kind":"binary","value":"f0808080"},"���",{"kind":"binary","value":"fdfffdfffdff"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-malformed-f0808080-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"���",{"kind":"binary","value":"fdfffdfffdff"},"���",{"kind":"binary","value":"fdfffdfffdff"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-malformed-f4908080-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-f4908080-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-f4908080-sourcemax-to-utf8",{"rows":[[1,null,null,null,null],[2,"����",{"kind":"binary","value":"f4908080"},"���",{"kind":"binary","value":"fdfffdfffdff"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-malformed-f4908080-sourcemax-to-unicode",{"rows":[[1,null,null,null,null],[2,"���",{"kind":"binary","value":"fdfffdfffdff"},"���",{"kind":"binary","value":"fdfffdfffdff"}]],"errors":[],"info":[],"counters":[[0,2,0,0]]}],["utf8-malformed-f5808080-source64-to-utf8",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-f5808080-source64-to-unicode",{"rows":[],"errors":[[4816,2,16]],"info":[],"counters":[[4816,0,0,0]]}],["utf8-malformed-f5808080-sourcemax-to-utf8",{"rows":[],"errors":[[9833,2,16]],"info":[[3621,0,0]],"counters":[[9833,0,0,0]]}],["utf8-malformed-f5808080-sourcemax-to-unicode",{"rows":[],"errors":[[9833,2,16]],"info":[[3621,0,0]],"counters":[[9833,0,0,0]]}]]

test('all24 vectors preserve both source forms and all96 complete isolated native/unit/display/error outcomes',async()=>{
  const f=validateRetained(await readFile(fixture))
  const hexes=['00','7f','c280','c2a9','dfbf','e0a080','ed9fbf','ee8080','efbfbf','f0908080','f48fbfbf','c2bf','c2','df','e0a0','ed9f','f09080','f48fbf','c228','e08080','eda080','f0808080','f4908080','f5808080']
  const names=hexes.flatMap((hex,i)=>[64,'max'].flatMap(width=>['utf8','unicode'].map(target=>`utf8-${i<12?'valid':i<18?'truncated':'malformed'}-${hex}-source${width}-to-${target}`)))
  assert.equal(cases.length,96);assert.equal(cases.reduce((n,c)=>n+rowsFor(c).length,0),192)
  assert.deepEqual(cases.map(c=>c.name),names);assert.deepEqual(observed.map(([name])=>name),names)
  for(const run of f.runs)for(const [i,o]of run.observations.entries()){
    const expected=observed[i][1],width=i%4<2?64:'max'
    assert.equal(o.case.sourceWidth,width);assert.equal(o.case.wireWidth,width);assert.equal(o.case.targetWidth,'max')
    assert.deepEqual(o.input,[{id:1,valueHex:null},{id:2,valueHex:hexes[Math.floor(i/4)]}])
    assert.deepEqual(o.readback.result.sets[1].rows,expected.rows,o.case.name)
    assert.deepEqual(o.execution.result.errors.map(e=>[e.number,e.state,e.class]),expected.errors,o.case.name)
    assert.deepEqual(o.execution.result.info.map(e=>[e.number,e.state,e.class]),expected.info,o.case.name)
    assert.deepEqual(o.readback.result.sets[0].rows,expected.counters,o.case.name)
    assert.equal(o.readback.session,'original');assert.equal(o.recoveryReadback,undefined)
    assert.equal(o.execution.result.rowCount,expected.errors.length?0:2)
    if(expected.errors.length){assert.equal(o.execution.result.error.number,expected.errors[0][0]);assert.deepEqual(expected.rows,[])}
    else{assert.equal(o.execution.result.error,null);assert.deepEqual(expected.rows[0],[1,null,null,null,null])}
  }
  const replay=spawnSync(process.execPath,['scripts/capture-bulk-character-utf8-source-form.mjs','--replay-fixture'],{cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:15000})
  assert.equal(replay.status,0,replay.stderr)
})

test('bounded admission failures and the exact MAX counter difference versus913 remain explicit',async()=>{
  const f=await load()
  const ancestorBytes=await readCaptureFile(new URL('../reference/bulk-character-utf8-boundary.json',import.meta.url))
  assert.equal(createHash('sha256').update(ancestorBytes).digest('hex'),'fa1e3ae36794cfc4b197d2ff122c5ab5effb99f3b449d0ad2eafea354c18be6a')
  const ancestor=JSON.parse(ancestorBytes)
  for(const run of f.runs)for(const o of run.observations){
    if(o.case.sourceWidth===64){
      assert.deepEqual(o.execution.result.errors.map(e=>[e.number,e.state,e.class]),[[4816,2,16]])
      assert.equal(o.execution.result.errors[0].message,'Invalid column type from bcp client for colid 2.')
      assert.deepEqual(o.readback.result.sets[0].rows,[[4816,0,0,0]])
      assert.deepEqual(o.readback.result.sets[1].rows,[])
    }else for(const previous of ancestor.runs){
      const old=find(previous,o.case.name.replace('-sourcemax',''))
      if(o.case.name==='utf8-truncated-f48fbf-sourcemax-to-unicode'){
        assert.deepEqual(o.readback.result.sets[0].rows,[[0,0,0,0]])
        assert.deepEqual(old.readback.result.sets[0].rows,[[7339,0,0,0]])
        assert.deepEqual(o.readback.result.sets[1].rows,old.readback.result.sets[1].rows)
      }else assert.deepEqual(o.readback.result.sets.map(s=>s.rows),old.readback.result.sets.map(s=>s.rows))
      assert.deepEqual(o.execution.result.errors.map(e=>[e.number,e.state,e.class,e.message]),old.execution.result.errors.map(e=>[e.number,e.state,e.class,e.message]))
      assert.deepEqual(o.execution.result.info.map(e=>[e.number,e.state,e.class,e.message]),old.execution.result.info.map(e=>[e.number,e.state,e.class,e.message]))
      assert.equal(o.execution.result.rowCount,old.execution.result.rowCount)
    }
  }
})

test('uniform source-form/type/byte/unit/error/provenance corruption and omitted probes fail fixed pins',async()=>{
  const retained=await load()
  const changes=[
    ['utf8-valid-00-source64-to-utf8',o=>{o.execution.result.errors[0].number=7339}],
    ['utf8-valid-c2a9-source64-to-unicode',o=>{o.case.sourceWidth='max'}],
    ['utf8-malformed-e08080-sourcemax-to-utf8',o=>{o.readback.result.sets[1].rows[1][4].value='fdfffdfffdff'}],
    ['utf8-malformed-eda080-sourcemax-to-utf8',o=>{o.readback.result.sets[1].rows[1][2].value='efbfbdefbfbdefbfbd'}],
    ['utf8-truncated-c2-sourcemax-to-unicode',o=>{o.execution.result.errors[0].number=4896}],
    ['utf8-valid-c2a9-source64-to-unicode',o=>{o.metadata.result.sets[0].columns[0].length=65535}]
  ]
  for(const [name,mutate]of changes){const f=structuredClone(retained);for(const run of f.runs)mutate(find(run,name));f.comparisons=f.runs.slice(1).map(r=>compare(r,f.runs[0]));assert.throws(()=>validate(f),/fixed/)}
  const omitted=structuredClone(retained);for(const run of omitted.runs)run.observations.pop();omitted.comparisons=omitted.runs.slice(1).map(r=>compare(r,omitted.runs[0]));assert.throws(()=>validate(omitted))
  for(const field of ['sourceSha','helperSha','nodeVersion']){const f=structuredClone(retained);f.provenance[field]='0'.repeat(64);assert.throws(()=>validate(f),/fixed/)}
  const f=structuredClone(retained);f.provenance.helpers['capture-bulk-character-utf8-boundary.mjs']='0'.repeat(64);assert.throws(()=>validate(f),/fixed/)
})
