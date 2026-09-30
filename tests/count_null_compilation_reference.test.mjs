import assert from 'node:assert/strict'
import {test} from 'node:test'
import {queries,retained,validate} from '../scripts/capture-count-null-compilation.mjs'

const operator=name=>name.includes('big')?'count_big':name==='count nested aggregate NULL'?'sum':'count'
const nullErrors=new Set(['count NULL','count big NULL','count parenthesized NULL','count distinct NULL','count big distinct NULL','count NULL no source','count NULL empty','count NULL window','count big NULL frame','count unary NULL','count nested aggregate NULL','sum nested count NULL','count NULL prior result','count NULL prior write','count NULL unreachable','count NULL after RETURN','count NULL same scope catch','count NULL update aggregate'])

test('COUNT NULL capture preserves complete raw requests and independent runs',async()=>{
 const {runs}=await retained()
 assert.equal(queries.length,35);assert.equal(runs[0].length,72)
 for(const run of runs)for(const record of run.slice(2)){
  if(nullErrors.has(record.name)){
   assert.deepEqual(record.result.errors.map(e=>[e.number,e.state,e.class,e.message]),[[8117,1,16,`Operand data type NULL is invalid for ${operator(record.name)} operator.`]],record.name)
   assert.deepEqual(record.result.sets,[],record.name)
  }
  const precedence={'count all NULL CASE':8133,'count scalar subquery NULL':130,'count NULL missing table':208,'count NULL missing predicate':207}
  if(precedence[record.name])assert.deepEqual(record.result.errors.map(e=>e.number),[precedence[record.name]],record.name)
 }
})

test('compilation rejects prior results writes and unreachable NULL calls',async()=>{
 const {runs}=await retained()
 for(const run of runs)for(const record of run.slice(2)){
  assert.deepEqual(record.followup.result.sets.map(set=>set.rows),[[[record.name==='typed NULL prior write'?5:4,3]],[[0]],[[42]]],record.name+' '+record.mode)
  if(['count NULL prior result','count NULL prior write','count NULL unreachable','count NULL after RETURN','count NULL same scope catch'].includes(record.name)){
   assert.deepEqual(record.result.sets,[],record.name)
   assert.deepEqual(record.result.errors.map(e=>e.number),[8117],record.name)
  }
  if(record.name==='count NULL dynamic catch'){
   assert.deepEqual(record.result.errors,[])
   assert.deepEqual(record.result.sets.map(set=>set.rows),[[[8117,1,16,'Operand data type NULL is invalid for count operator.']]])
  }
 }
})

test('typed NULL declarations remain valid and preparation retains its error sequence',async()=>{
 const {runs}=await retained()
 for(const run of runs)for(const record of run.slice(2)){
  if(['count typed NULL','count converted NULL','count character NULL','count NULL plus one','typed NULL prior write'].includes(record.name)){
   assert.deepEqual(record.result.errors,[]);assert.deepEqual(record.result.sets.map(set=>set.rows),[[[0]]])
   assert.equal(record.result.sets[0].columns[0].length,4)
  }
  if(record.name==='count big typed NULL'){
   assert.deepEqual(record.result.errors,[]);assert.deepEqual(record.result.sets[0].rows,[['0']]);assert.equal(record.result.sets[0].columns[0].length,8)
  }
  if(record.name.startsWith('count declared ')){
   assert.deepEqual(record.result.errors,[]);assert.deepEqual(record.result.sets[0].rows,[[0,'0']])
   assert.deepEqual(record.result.sets[0].columns.map(c=>c.length),[4,8])
  }
  if(['prepare count NULL','prepare count big NULL'].includes(record.name)){
   assert.deepEqual(record.result.errors.map(e=>[e.number,e.state,e.class]),[[8117,1,16],[8180,1,16]])
   assert.deepEqual(record.result.sets.map(set=>set.rows),[[[null]]])
  }
  if(record.name==='prepare declared NULL'){
   assert.deepEqual(record.result.errors,[]);assert.deepEqual(record.result.sets.map(set=>set.rows),[[],[[0,'0']],[[4,'4']]])
   assert(record.result.sets.every(set=>JSON.stringify(set.columns)===JSON.stringify(record.result.sets[0].columns)))
  }
 }
})

test('capture validation rejects removed phases and corrupted token data',async()=>{
 const {runs}=await retained()
 assert.throws(()=>validate(runs[0].slice(0,-1)))
 const missing=structuredClone(runs[0]);delete missing[2].followup
 assert.throws(()=>validate(missing),/missing COUNT NULL reset/)
 const phase=structuredClone(runs[0]);phase[2].followup.result.doneTokens.pop()
 assert.throws(()=>validate(phase))
 const damaged=structuredClone(runs[0]);damaged[2].result.doneTokens[0].status^=2
 assert.throws(()=>validate(damaged))
 const reset=structuredClone(runs[0]);reset[2].reset.sql='SELECT 1'
 assert.throws(()=>validate(reset))
})
