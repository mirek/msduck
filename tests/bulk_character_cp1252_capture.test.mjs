import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile, mkdir, mkdtemp, writeFile, realpath, symlink, link, rm, truncate} from 'node:fs/promises'
import {join} from 'node:path'
import {fileURLToPath} from 'node:url'
import {spawnSync} from 'node:child_process'
import {EventEmitter} from 'node:events'
import {createHash} from 'node:crypto'
import {cases, rowsFor, jsonSize, CAPTURE_LIMIT, validate, validateRetained, compare, guardOutput, persistCapture, finalizeCapture, readCaptureFile, exchange, budget} from '../scripts/capture-bulk-character-cp1252.mjs'
const fixture = new URL('../reference/bulk-character-cp1252.json', import.meta.url)
const load = async () => JSON.parse(await readFile(fixture, 'utf8'))
const find = (run, name) => run.observations.find(o => o.case.name === name)
const scratch = async work => {
  const root = fileURLToPath(new URL('../.tmp/',import.meta.url))
  await mkdir(root,{recursive:true})
  const dir = await realpath(await mkdtemp(join(root,'msduck-cp1252-')))
  try {await work(dir)} finally {await rm(dir,{recursive:true,force:true})}
}

test('uniform MAX packet repartition cannot hide behind identical payloads and recomputed comparisons', async () => {
  for (const name of ['cp1252-fragmented-to-unicode','cp1252-fragmented-to-utf8']) for (const phase of ['execution','readback']) {
    const actual = await load()
    for (const run of actual.runs) {
      const record = find(run,name)[phase]
      const packets = record.packets.filter(p => phase==='readback' ? p.direction==='in' : p.direction==='out' && p.rawHex.startsWith('07'))
      const originalPayload = packets.map(p=>p.rawHex.slice(16)).join('')
      const first = Buffer.from(packets[1].rawHex,'hex'), second = Buffer.from(packets[2].rawHex,'hex')
      const shorter = first.subarray(0,first.length-1)
      const longer = Buffer.concat([second.subarray(0,8),first.subarray(first.length-1),second.subarray(8)])
      shorter.writeUInt16BE(shorter.length,2); longer.writeUInt16BE(longer.length,2)
      packets[1].rawHex=shorter.toString('hex'); packets[2].rawHex=longer.toString('hex')
      assert.equal(packets.map(p=>p.rawHex.slice(16)).join(''),originalPayload)
    }
    actual.comparisons=actual.runs.slice(1).map(run=>compare(run,actual.runs[0]))
    assert.throws(()=>validate(actual),/fixed complete packet framing/)
  }
})

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
      const packets = find(run, 'cp1252-every-byte-to-cp1252').execution.packets
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
    const p=find(run,'cp1252-every-byte-to-cp1252').execution.packets.find(p=>p.direction==='in')
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
      const o=find(run,'cp1252-fragmented-to-unicode')
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
    for(const run of actual.runs)for(const p of find(run,'cp1252-fragmented-to-unicode')[phase].packets.filter(p=>p.direction==='out'&&p.rawHex.startsWith('01'))) {
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
  assert.equal(Math.max(...cases.map(c => rowsFor(c).length)), 258)
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
      const child = spawnSync(process.execPath, ['scripts/capture-bulk-character-cp1252.mjs', output], {cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:5000})
      assert.notEqual(child.status, 0)
      assert.ok(!child.stderr.includes('spawn docker'))
    }
    assert.equal(await readFile(original,'utf8'), 'retained')
  })
})

test('invalid captures and complete comparison sidecars are preserved before semantic validation', async () => {
  await scratch(async dir => {
    const actual = await load(); for (const run of actual.runs) find(run,'cp1252-every-byte-to-cp1252').execution.result.rowCount = 99
    actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
    const output = join(dir,'failed.json')
    await assert.rejects(persistCapture(actual,output), /fixed/)
    const saved = JSON.parse(await readFile(output,'utf8')), comparison = JSON.parse(await readFile(output+'.comparison.json','utf8'))
    assert.equal(find(saved.runs[0],'cp1252-every-byte-to-cp1252').execution.result.rowCount,99)
    assert.equal(comparison.retained,true)
    assert.ok(comparison.differences.some(d => d.path.includes('/execution/result/rowCount')))
    assert.equal(jsonSize(saved),Buffer.byteLength(JSON.stringify(saved)))
  })
})

test('derived comparison overflow preserves all bounded raw runs and an explicit failed sidecar', async () => {
  await scratch(async dir => {
    for (const [count,size] of [[4,11*1024*1024],[2,0]]) {
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
    const child=spawnSync(process.execPath,['scripts/capture-bulk-character-cp1252.mjs',output],{cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:10000})
    assert.notEqual(child.status,0)
    const partial=JSON.parse(await readFile(output,'utf8')),sidecar=JSON.parse(await readFile(output+'.comparison.json','utf8'))
    assert.deepEqual(partial.runs,[])
    assert.ok(partial.failure.message.includes('docker'))
    assert.equal(sidecar.retained,true)
    assert.deepEqual(sidecar.differences,compare(partial,await load()))
    assert.ok(sidecar.differences.some(d=>d.path==='/runs/0'))
  })
})


test('all four runs retain every CP1252 byte, SQL Unicode units and NULL/empty across all target domains', async () => {
  const value=validateRetained(await readFile(fixture))
  assert.equal(cases.length,12)
  assert.equal(cases.reduce((n,c)=>n+rowsFor(c).length,0),1068)
  for(const run of value.runs) {
    const original=find(run,'cp1252-every-byte-to-cp1252').readback.result.sets[1].rows
    assert.equal(original.length,258)
    assert.deepEqual(original.slice(0,2),[[1,null,null,null,null],[2,'',{kind:'binary',value:''},'',{kind:'binary',value:''}]])
    for(const target of ['cp1252','cp1251','utf8','unicode']) {
      const o=find(run,`cp1252-every-byte-to-${target}`),rows=o.readback.result.sets[1].rows
      assert.equal(rows.length,258);assert.equal(o.execution.result.error,null)
      assert.deepEqual(o.readback.result.sets[0].rows,[[0,258,0,0]])
      assert.equal(rows[0][2],null);assert.equal(rows[1][2].value,'')
      for(let b=0;b<256;b++) {
        const row=rows[b+2],hex=b.toString(16).padStart(2,'0')
        assert.equal(o.input[b+2].valueHex,hex)
        assert.equal(row[0],b+3)
        if(target==='cp1252')assert.equal(row[2].value,hex)
        if(target==='utf8'||target==='unicode')assert.equal(row[4].value,original[b+2][4].value)
        if(target==='unicode')assert.equal(row[2].value,row[4].value)
      }
    }
    for(const o of run.observations){assert.deepEqual(o.execution.result.errors,[]);assert.equal(o.execution.result.error,null);assert.deepEqual(o.readback.result.sets[0].rows,[[0,o.input.length,0,0]])}
  }
  const replay=spawnSync(process.execPath,['scripts/capture-bulk-character-cp1252.mjs','--replay-fixture'],{cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:15000})
  assert.equal(replay.status,0,replay.stderr)
})

test('undefined-byte display differences and mixed/fragmented native projections remain exact', async () => {
  const value=await load()
  for(const run of value.runs) {
    for(const b of [0x81,0x8d,0x8f,0x90,0x9d]) {
      const hex=b.toString(16),row=find(run,'cp1252-every-byte-to-cp1252').readback.result.sets[1].rows[b+2]
      assert.equal(row[1],'�');assert.equal(row[2].value,hex);assert.equal(row[4].value,hex+'00');assert.equal(row[3].charCodeAt(0),b)
      const cp1=find(run,'cp1252-every-byte-to-cp1251').readback.result.sets[1].rows[b+2]
      assert.equal(cp1[2].value,'3f');assert.equal(cp1[4].value,'3f00')
      assert.equal(find(run,'cp1252-every-byte-to-utf8').readback.result.sets[1].rows[b+2][2].value,'c2'+hex)
    }
    for(const target of ['cp1252','cp1251','utf8','unicode']) {
      const rows=find(run,`cp1252-every-byte-to-${target}`).readback.result.sets[1].rows
      const native=rows.slice(2).map(r=>r[2].value).join(''),units=rows.slice(2).map(r=>r[4].value).join('')
      const mixed=find(run,`cp1252-mixed-to-${target}`)
      assert.equal(mixed.readback.result.sets[1].rows.at(-1)[2].value,native)
      assert.equal(mixed.readback.result.sets[1].rows.at(-1)[4].value,units)
      const large=find(run,`cp1252-fragmented-to-${target}`),last=large.readback.result.sets[1].rows.at(-1)
      const undefinedNative=[0x81,0x8d,0x90,0x8f,0x9d].map(b=>rows[b+2][2].value).join('')
      const asciiA=rows[0x41+2][2].value,asciiB=rows[0x42+2][2].value
      assert.equal(last[2].value,asciiA.repeat(401)+native.repeat(33)+asciiB.repeat(402)+undefinedNative)
      assert.ok(last[2].value.length/2>8000)
      assert.ok(large.execution.packets.filter(p=>p.direction==='out'&&p.rawHex.startsWith('07')).length>2)
      if(target==='cp1252')assert.equal(last[2].value,large.input.at(-1).valueHex)
    }
  }
})

test('uniform table/undefined-byte/descriptor/provenance corruption and omitted controls fail fixed pins', async () => {
  const retained=await load()
  for(const [name,mutate] of [
    ['cp1252-every-byte-to-cp1252',o=>{o.readback.result.sets[1].rows[0x81+2][4].value='fdff'}],
    ['cp1252-every-byte-to-cp1251',o=>{o.readback.result.sets[1].rows[0x81+2][2].value='81'}],
    ['cp1252-every-byte-to-utf8',o=>{o.readback.result.sets[1].rows[0x8d+2][2].value='efbfbd'}],
    ['cp1252-every-byte-to-unicode',o=>{o.metadata.result.sets[0].columns[0].flags^=1}],
    ['cp1252-mixed-to-cp1252',o=>{o.readback.result.sets[0].rows[0][1]=1}]
  ]) {
    const actual=structuredClone(retained);for(const run of actual.runs)mutate(find(run,name))
    actual.comparisons=actual.runs.slice(1).map(run=>compare(run,actual.runs[0]))
    assert.throws(()=>validate(actual),/fixed/)
  }
  const omitted=structuredClone(retained);for(const run of omitted.runs)run.observations.splice(1,1)
  omitted.comparisons=omitted.runs.slice(1).map(run=>compare(run,omitted.runs[0]))
  assert.throws(()=>validate(omitted))
  for(const field of ['helperSha','sourceSha','nodeVersion']) {
    const actual=structuredClone(retained);actual.provenance[field]='0'.repeat(64)
    assert.throws(()=>validate(actual),/fixed/)
  }
  const altered=structuredClone(retained);altered.provenance.helpers['capture-bulk-character-capacity.mjs']='0'.repeat(64)
  assert.throws(()=>validate(altered),/fixed/)
})
