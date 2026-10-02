import test from 'node:test'
import assert from 'node:assert/strict'
import {readFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {start} from './support/client.mjs'
import {observe,verifyObservations} from '../scripts/capture-guid-assignment.mjs'

export function registerGuidAssignment(register) {
register('GUID assignment rows, diagnostics, completion fields and transaction effects match pinned captures',{timeout:60000},async t=>{
 const reference=JSON.parse(await readFile(new URL('../reference/guid-assignment.json',import.meta.url)))
 assert.deepEqual(reference.runs[0],reference.runs[1],'fresh reference agreement')
 const connection=await start(t)
 const records=await observe(connection)
 verifyObservations(records)
 assert.deepEqual(records.map(record=>record.name),reference.runs[0].map(record=>record.name))
 for(const [index,record] of records.entries()){
  // ProductVersion reports msduck's emulated version, separately from GUID
  // assignment behavior. Full descriptors, events and completion differences
  // remain preserved by capture-guid-assignment --replay; this test covers the
  // rows and diagnostics stated in its name, not complete wire compatibility.
  if(record.name==='version')continue
  const expected=reference.runs[0][index].result
  assert.deepEqual(record.result.sets.map(set=>set.rows),expected.sets.map(set=>set.rows),record.name+' rows')
  assert.deepEqual(record.result.errors,expected.errors,record.name+' errors')
  assert.deepEqual(record.result.info,expected.info,record.name+' information')
  assert.deepEqual(record.result.doneTokens,expected.doneTokens,record.name+' completion fields')
  if(expected.sets.length===0)assert.deepEqual(record.result.events,expected.events,record.name+' non-result event order')
 }
})
}

if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))registerGuidAssignment(test)
