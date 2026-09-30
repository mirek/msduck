import assert from 'node:assert/strict'
import {test} from 'node:test'
import {retained,queries,validate} from '../scripts/capture-count-ranking-declarations.mjs'

test('COUNT/ranking reference retains complete matching fresh requests',async()=>{
 const {runs}=await retained()
 assert.equal(queries.length,30);assert.equal(runs[0].length,62)
 for(const run of runs)for(const record of run.slice(2)){
  assert(record.result.sets.every(set=>set.rows.every(row=>row.length===set.columns.length)))
  const errors={'count NULL':8117,'count missing':207,'count missing table':208,'row number missing':207,'row number no order':4112,'row number args':4114}[record.name]
  assert.deepEqual(record.result.errors.map(error=>error.number),errors===undefined?[]:[errors],record.name)
 }
})
test('fixed INT/BIGINT widths survive empty/derived/prepared phases',async()=>{
 const {runs}=await retained()
 for(const run of runs)for(const record of run.slice(2)){
  for(const set of record.result.sets)for(const column of set.columns){
   if(['c','d'].includes(column.name)&&!record.name.includes('division'))assert.equal(column.length,column.name==='d'||record.name.includes('big')?8:4,record.name)
   if(column.name==='r')assert.equal(column.length,8,record.name)
  }
  if(record.name==='prepared count')assert.deepEqual(record.result.sets.map(set=>set.rows),[[],[[4,'4']],[[0,'0']],[[3,'3']]])
  if(record.name==='prepared row number')assert.deepEqual(record.result.sets.map(set=>set.rows.length),[0,4,0,3])
 }
})
test('declaration capture rejects missing records and altered raw wire',async()=>{
 const {runs}=await retained();assert.throws(()=>validate(runs[0].slice(0,-1)))
 const damaged=structuredClone(runs[0]);const record=damaged.find(record=>record.result.events.some(event=>event.kind==='ORDER'))
 record.result.events.find(event=>event.kind==='ORDER').hex='a9010001'
 assert.throws(()=>validate(damaged),/odd ORDER payload/)
})
