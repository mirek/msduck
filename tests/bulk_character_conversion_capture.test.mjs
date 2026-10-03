import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile, mkdtemp, writeFile, realpath, symlink, link, rm, truncate} from 'node:fs/promises'
import {join} from 'node:path'
import {tmpdir} from 'node:os'
import {spawnSync} from 'node:child_process'
import {cases, rowsFor, jsonSize, CAPTURE_LIMIT, validate, validateRetained, compare, guardOutput, persistCapture, readCaptureFile} from '../scripts/capture-bulk-character-conversion.mjs'
const fixture = new URL('../reference/bulk-character-conversion.json', import.meta.url)
const load = async () => JSON.parse(await readFile(fixture, 'utf8'))
const find = (run, name) => run.observations.find(o => o.case.name === name)
const scratch = async work => {const dir = await realpath(await mkdtemp(join(tmpdir(), 'msduck-conversion-'))); try {await work(dir)} finally {await rm(dir, {recursive: true, force: true})}}

test('four captures preserve all CP1251 native bytes and distinguish undefined-byte SQL units from client display', async () => {
  const value = validateRetained(await readFile(fixture))
  assert.equal(cases.length, 69)
  for (const run of value.runs) {
    const same = find(run, 'cp1251-every-byte-cp1251').readback.result.sets[1].rows
    const unicode = find(run, 'cp1251-every-byte-unicode').readback.result.sets[1].rows
    assert.equal(same.length, 258)
    assert.deepEqual(same.slice(0, 2), [[1,null,null,null,null],[2,'',{kind:'binary',value:''},'',{kind:'binary',value:''}]])
    for (let byte = 0; byte < 256; byte++) {
      const hex = byte.toString(16).padStart(2, '0')
      assert.equal(same[byte + 2][2].value, hex)
      assert.equal(same[byte + 2][4].value, unicode[byte + 2][2].value)
      assert.equal(unicode[byte + 2][2].value, unicode[byte + 2][4].value)
    }
    assert.deepEqual(same[0x98 + 2], [155,'�',{kind:'binary',value:'98'},'\u0098',{kind:'binary',value:'9800'}])
    assert.equal(same[0xff + 2][4].value, '4f04')
    assert.equal(same[2][4].value, '0000')
  }
  const replay = spawnSync(process.execPath, ['scripts/capture-bulk-character-conversion.mjs', '--replay-fixture'], {cwd: new URL('../', import.meta.url), encoding: 'utf8', env: {...process.env, PATH: ''}, timeout: 10000})
  assert.equal(replay.status, 0, replay.stderr)
})

test('measured malformed UTF8 outcomes retain native bytes, replacement units, complete failures and atomicity', async () => {
  const value = await load()
  for (const run of value.runs) {
    for (const hex of ['80','bf','c0af','c1bf','e228a1','f5808080','e282','f09f92']) for (const target of ['utf8','cp1252','unicode']) {
      const o = find(run, `utf8-${hex}-${target}`)
      const number = ['e282','f09f92'].includes(hex) ? 7339 : 9833
      assert.equal(o.execution.result.errors[0].number, number)
      assert.equal(o.execution.result.errors[0].state, number === 9833 ? 2 : 1)
      assert.equal(o.execution.result.errors[0].class, 16)
      assert.equal(o.execution.result.error.number, number)
      assert.deepEqual(o.execution.result.info.map(e => e.number), number === 9833 ? [3621] : [])
      assert.deepEqual(o.readback.result.sets[0].rows, [[number,0,0,0]])
      assert.deepEqual(o.readback.result.sets[1].rows, [])
    }
    for (const [hex, units, publicCount] of [['e080af',2,3],['eda080',2,3],['f08080af',3,4],['f4908080',3,4]]) {
      const o = find(run, `utf8-${hex}-utf8`), row = o.readback.result.sets[1].rows[3]
      assert.equal(o.execution.result.error, null)
      assert.equal(row[1], '�'.repeat(publicCount))
      assert.equal(row[2].value, hex)
      assert.equal(row[3], '�'.repeat(units))
      assert.equal(row[4].value, 'fdff'.repeat(units))
    }
  }
})

test('source declaration, target capacity and fixed padding remain measured contracts', async () => {
  const value = await load()
  for (const run of value.runs) {
    assert.equal(find(run, 'declare-cp1251-wire-cp1252').readback.result.sets[1].rows[3][3], 'Привет')
    assert.equal(find(run, 'declare-cp1252-wire-cp1251').readback.result.sets[1].rows[3][3], 'Ïðèâåò')
    assert.equal(find(run, 'declare-utf8-wire-cp1252').readback.result.sets[1].rows[3][2].value, 'c328')
    assert.equal(find(run, 'omitted-wire-collation').execution.result.errors[0].number, 4002)
    assert.equal(find(run, 'omitted-wire-collation').execution.result.errors[0].state, 2)
    assert.equal(find(run, 'zero-wire-collation').execution.result.error, null)
    assert.equal(find(run, 'cp1251-to-utf8-copyright-width1').readback.result.sets[1].rows[3][2].value, '')
    assert.equal(find(run, 'cp1251-to-utf8-copyright-width2').readback.result.sets[1].rows[3][2].value, 'c2a9')
    for (const name of ['utf8-accent-width1','utf8-omega-to-cp1252-width1','utf8-emoji-to-cp1252-width1','utf8-emoji-to-nchar-width1']) {
      const o = find(run, name)
      assert.equal(o.execution.result.errors[0].number, 2628)
      assert.ok(o.execution.result.errors[0].message.includes(run.version.result.sets[0].rows[0][1] + '.dbo.conversion_probe'))
      assert.deepEqual(o.readback.result.sets[1].rows, [])
    }
    assert.ok(find(run, 'utf8-emoji-to-nchar-width1').execution.result.error.message.includes('\ud83e'))
    assert.equal(find(run, 'utf8-char-short-source').execution.result.errors[0].number, 4815)
    assert.equal(find(run, 'utf8-char-short-source').execution.result.errors[0].class, 17)
    assert.equal(find(run, 'cp1252-char-padding').readback.result.sets[1].rows[3][2].value, 'e9202020')
    assert.equal(find(run, 'utf8-char-padding').readback.result.sets[1].rows[3][2].value, 'c3a92020')
  }
})

test('uniform semantic corruption fails independent pins even with recomputed complete difference summaries', async () => {
  const retained = await load()
  for (const [name, mutate] of [
    ['cp1251-every-byte-unicode', o => {o.readback.result.sets[1].rows[154][4].value = 'fdff'}],
    ['utf8-e080af-utf8', o => {o.readback.result.sets[1].rows[3][2].value = 'efbfbdefbfbd'}],
    ['utf8-e282-unicode', o => {o.execution.result.errors[0].number = 9833}],
    ['utf8-omega-to-cp1252-width1', o => {o.execution.result.errors[0].message = o.execution.result.errors[0].message.replace("'O'", "'?'" )}],
    ['zero-wire-collation', o => {o.execution.result.rowCount = 4}],
    ['cp1252-char-padding', o => {o.metadata.result.sets[0].columns[0].flags ^= 1}],
    ['zero-wire-collation', o => {o.readback.session = 'replacement'}],
    ['zero-wire-collation', o => {o.setup.sql = 'SELECT 1'}]
  ]) {
    const actual = structuredClone(retained)
    for (const run of actual.runs) mutate(find(run, name))
    actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
    assert.throws(() => validate(actual), /fixed/)
  }
})

test('MAX request and response packets split inside UTF8 characters without changing native values', async () => {
  for (const run of (await load()).runs) {
    const o = find(run, 'utf8-max-fragmented')
    const outgoing = o.execution.packets.filter(p => p.direction === 'out' && p.rawHex.startsWith('07'))
    assert.ok(outgoing.length > 2)
    assert.equal(outgoing[1].rawHex.slice(-2), 'f0')
    assert.equal(outgoing[2].rawHex.slice(16,22), '9fa686')
    const incoming = o.readback.packets.filter(p => p.direction === 'in')
    assert.equal(incoming[1].rawHex.slice(-2), 'f0')
    assert.equal(incoming[2].rawHex.slice(16,30), 'f40100009fa686')
    assert.equal(o.readback.result.sets[1].rows[3][2].value, rowsFor(o.case)[3].valueHex)
    const converted = find(run, 'cp1251-to-utf8-max-fragmented')
    assert.equal(converted.readback.result.sets[1].rows[3][2].value, '61'.repeat(401) + Buffer.from('Привет'.repeat(3000)).toString('hex'))
    const convertedPackets = converted.readback.packets.filter(p => p.direction === 'in')
    assert.equal(convertedPackets[1].rawHex.slice(-2),'d0')
    assert.equal(convertedPackets[2].rawHex.slice(16,26),'f4010000b5')
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
      const packets = find(run, 'cp1251-every-byte-cp1251').execution.packets
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
    const p=find(run,'zero-wire-collation').execution.packets.find(p=>p.direction==='in')
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
      const o=find(run,'utf8-max-fragmented')
      const packets=mode==='outbound' ? o.execution.packets.filter(p=>p.direction==='out').slice(0,1) : o.readback.packets.filter(p=>p.direction==='in').slice(0,mode==='whole-phase'?undefined:1)
      for(const p of packets) {const bytes=Buffer.from(p.rawHex,'hex');bytes.writeUInt16BE(0xffff,4);p.rawHex=bytes.toString('hex')}
    }
    actual.comparisons=actual.runs.slice(1).map(run=>compare(run,actual.runs[0]))
    assert.throws(()=>validate(actual),/SPID/)
  }
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
      const child = spawnSync(process.execPath, ['scripts/capture-bulk-character-conversion.mjs', output], {cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:5000})
      assert.notEqual(child.status, 0)
      assert.ok(!child.stderr.includes('spawn docker'))
    }
    assert.equal(await readFile(original,'utf8'), 'retained')
  })
})

test('invalid captures and complete comparison sidecars are preserved before semantic validation', async () => {
  await scratch(async dir => {
    const actual = await load(); for (const run of actual.runs) find(run,'zero-wire-collation').execution.result.rowCount = 4
    actual.comparisons = actual.runs.slice(1).map(run => compare(run, actual.runs[0]))
    const output = join(dir,'failed.json')
    await assert.rejects(persistCapture(actual,output), /fixed/)
    const saved = JSON.parse(await readFile(output,'utf8')), comparison = JSON.parse(await readFile(output+'.comparison.json','utf8'))
    assert.equal(find(saved.runs[0],'zero-wire-collation').execution.result.rowCount,4)
    assert.equal(comparison.retained,true)
    assert.ok(comparison.differences.some(d => d.path.includes('/execution/result/rowCount')))
    assert.equal(jsonSize(saved),Buffer.byteLength(JSON.stringify(saved)))
  })
})

test('an aborted reference startup retains the partial capture and every comparison difference', async () => {
  await scratch(async dir => {
    const output=join(dir,'aborted.json')
    const child=spawnSync(process.execPath,['scripts/capture-bulk-character-conversion.mjs',output],{cwd:new URL('../',import.meta.url),encoding:'utf8',env:{...process.env,PATH:''},timeout:10000})
    assert.notEqual(child.status,0)
    const partial=JSON.parse(await readFile(output,'utf8')),sidecar=JSON.parse(await readFile(output+'.comparison.json','utf8'))
    assert.deepEqual(partial.runs,[])
    assert.ok(partial.failure.message.includes('docker'))
    assert.equal(sidecar.retained,true)
    assert.deepEqual(sidecar.differences,compare(partial,await load()))
    assert.ok(sidecar.differences.some(d=>d.path==='/runs/0'))
  })
})
