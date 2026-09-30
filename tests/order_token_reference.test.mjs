import assert from 'node:assert/strict'
import test from 'node:test'
import { decodeOrder, readRawOrder, retained } from '../scripts/capture-order-token.mjs'

test('ORDER reader checks lengths and retains exact ordinal bytes', () => {
  const bytes=Buffer.from('a9060003000100ffff','hex')
  assert.deepEqual(decodeOrder(bytes),{hex:bytes.toString('hex'),length:6,ordinals:[3,1,65535]})
  for(let end=0;end<bytes.length;end++) assert.throws(()=>decodeOrder(bytes.subarray(0,end)))
  assert.throws(()=>decodeOrder(Buffer.concat([bytes,Buffer.from([0])])) )
  assert.throws(()=>decodeOrder(Buffer.from('a9010001','hex')))
  assert.throws(()=>decodeOrder(Buffer.from('a8060003000100ffff','hex')))
  const maximum=Buffer.alloc(65537)
  maximum[0]=0xa9;maximum.writeUInt16LE(65534,1)
  assert.equal(decodeOrder(maximum).ordinals.length,32767)
})

test('ORDER capture retains a token fragmented at every payload boundary',async () => {
  const payload=Buffer.from('0600030001000200','hex')
  for(let split=0;split<payload.length;split++) {
    let waits=0
    const parser={buffer:payload.subarray(0,split),position:0,async waitForChunk(){waits++;this.buffer=payload}}
    assert.deepEqual((await readRawOrder(parser)).ordinals,[3,1,2])
    assert.equal(waits,1)
    assert.equal(parser.position,0,'capture must leave decoding to Tedious')
  }
})

test('retained ORDER reference contains complete identical fresh runs',async () => {
  const capture=await retained()
  const records=capture.runs[0]
  assert.equal(records.length,68)
  const ascending=records.find(r=>r.name==='ascending'&&r.mode==='batch')
  assert(ascending.result.events.some(e=>e.kind==='ORDER'))
  const phaseCount=records.reduce((n,r)=>n+(r.mode==='prepared'?2+r.executions.length:1),0)
  assert.equal(phaseCount,76)
})

test('captured ORDER ordinals preserve hidden keys, key order and empty results',async () => {
  const {runs}=await retained()
  for(const mode of ['batch','rpc']) {
    const orders=name=>runs[0].find(r=>r.name===name&&r.mode===mode).result.events.filter(e=>e.kind==='ORDER').map(e=>e.ordinals)
    assert.deepEqual(orders('keys'),[[2,1]])
    assert.deepEqual(orders('nonprojected key'),[[0]])
    assert.deepEqual(orders('mixed projected key'),[[0,1]])
    assert.deepEqual(orders('empty'),[[2,1]])
    assert.deepEqual(orders('TOP empty'),[[1]])
    assert.deepEqual(orders('ascending'),orders('descending'),'direction is absent from token')
    for(const name of ['unordered','all NULL','window only','derived internal order','clustered unordered','error before order'])assert.deepEqual(orders(name),[],name)
  }
  for(const record of runs[0].filter(r=>r.mode==='prepared')) {
    const expected=record.name==='prepared projected'?[2,1]:[0]
    assert.deepEqual(record.preparation.events.filter(e=>e.kind==='ORDER').map(e=>e.ordinals),[expected])
    assert.equal(record.preparation.sets[0].rows.length,0,'preparation must not execute')
    for(const execution of record.executions)assert.deepEqual(execution.result.events.filter(e=>e.kind==='ORDER').map(e=>e.ordinals),[expected])
    assert.equal(record.executions[1].result.sets[0].rows.length,0,'empty prepared result missing')
  }
})
