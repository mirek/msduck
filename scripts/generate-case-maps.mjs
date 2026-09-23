// Input: reference/unicode-case.json from owner-authored commit 3dfbe04.
// Node strings preserve isolated UTF-16 units, unlike UTF-8-only JSON readers.
import assert from 'node:assert/strict'
import {createHash} from 'node:crypto'
import {readFile,writeFile} from 'node:fs/promises'
const [input,mode] = process.argv.slice(2)
assert.ok(input && (!mode || mode==='--check'),'Usage: node scripts/generate-case-maps.mjs CAPTURE.json [--check]')
const raw = await readFile(input)
const fixture = JSON.parse(raw)
const names = ['SQL_Latin1_General_CP1_CI_AS','Latin1_General_100_CI_AS','Latin1_General_100_CS_AS','Latin1_General_100_CI_AI','Latin1_General_100_CS_AI','Latin1_General_100_BIN2']
const inputs = {ASCII:'AbZz','case edge characters':'İΣẞßǅKıI','Greek context':'ΟΣ ΣΟΣ Σ','supplementary letters':'𐐀𐐨','isolated high surrogate':'\ud83e','isolated low surrogate':'\udd86','surrogate pair and ASCII':'A🦆Z',empty:'',NULL:null}
assert.deepEqual(fixture.results.map(r=>r.collation),names)
const maps = []
for (const result of fixture.results) {
  assert.deepEqual(result.statistics.errors,[])
  assert.deepEqual(result.statistics.sets[0].rows,[[65536,65536,65536,65536,2,2,2,2,65536]])
  assert.deepEqual(result.mapping.errors,[])
  assert.equal(result.mapping.sets.length,1)
  const rows=result.mapping.sets[0].rows
  assert.equal(rows.length,maps.length===0?1326:1764)
  let previous=-1
  for(const row of rows) {
    assert.equal(row.length,3)
    assert.ok(row.every(n=>Number.isInteger(n)&&n>=0&&n<=65535))
    assert.ok(row[0]>previous && (row[0]!==row[1] || row[0]!==row[2]))
    previous=row[0]
  }
  const map=new Map(rows.map(r=>[r[0],r.slice(1)]))
  assert.deepEqual(result.strings.map(p=>p.name),Object.keys(inputs))
  for(const probe of result.strings) {
    assert.deepEqual(probe.reference.errors,[])
    const source=inputs[probe.name]
    const expected=[0,1].map(direction=>source===null?null:Array.from({length:source.length},(_,i)=>String.fromCharCode(map.get(source.charCodeAt(i))?.[direction]??source.charCodeAt(i))).join(''))
    assert.deepEqual(probe.reference.sets[0].rows,[expected])
  }
  for(let unit=0xd800;unit<=0xdfff;unit++) assert.ok(!map.has(unit))
  if(maps.length>1) assert.deepEqual(rows,maps[1])
  maps.push(rows)
}
const table=(name,rows)=>`const ${name}: &[[u16; 3]] = &[\n${rows.map(row=>'    ['+row.map(n=>'0x'+n.toString(16).padStart(4,'0')).join(', ')+'],').join('\n')}\n];\n`
const path=new URL('../crates/msduck-core/src/case_mapping.rs',import.meta.url)
const original=await readFile(path,'utf8')
const begin='// BEGIN GENERATED CASE MAPS\n',end='// END GENERATED CASE MAPS'
assert.equal(original.split(begin).length,2);assert.equal(original.split(end).length,2)
const generated=`// Generated from SQL Server 2025 CU7; do not edit these delta tables.\n// Capture SHA-256: ${createHash('sha256').update(raw).digest('hex')}\n${table('SQL_LATIN1',maps[0])}\n${table('LATIN1_100',maps[1])}`
const updated=original.slice(0,original.indexOf(begin)+begin.length)+generated+original.slice(original.indexOf(end))
if(mode==='--check') assert.equal(original,updated,'Generated casing tables differ')
else await writeFile(path,updated)
console.log(JSON.stringify({collations:maps.length,probes:maps.length*Object.keys(inputs).length,deltaRows:maps[0].length+maps[1].length,checked:mode==='--check'}))
