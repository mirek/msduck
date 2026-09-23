// SQL Server ground truth for the six currently recognized non-SC collations.
import assert from 'node:assert/strict'
import {writeFile} from 'node:fs/promises'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {connect,command} from './lib/reference.mjs'
import {canonical} from './lib/compatibility.mjs'
const collations = ['SQL_Latin1_General_CP1_CI_AS','Latin1_General_100_CI_AS','Latin1_General_100_CS_AS','Latin1_General_100_CI_AI','Latin1_General_100_CS_AI','Latin1_General_100_BIN2']
const probes = [
  ['ASCII', "N'AbZz'"],
  ['case edge characters', "N'İΣẞßǅKıI'"],
  ['Greek context', "N'ΟΣ ΣΟΣ Σ'"],
  ['supplementary letters', "N'𐐀𐐨'"],
  ['isolated high surrogate', "LEFT(N'🦆',1)"],
  ['isolated low surrogate', "RIGHT(N'🦆',1)"],
  ['surrogate pair and ASCII', "N'A🦆Z'"],
  ['empty', "N''"],
  ['NULL', 'CAST(NULL AS NVARCHAR(4))'],
]
const inputs = {ASCII:'AbZz','case edge characters':'İΣẞßǅKıI','Greek context':'ΟΣ ΣΟΣ Σ','supplementary letters':'𐐀𐐨','isolated high surrogate':'\ud83e','isolated low surrogate':'\udd86','surrogate pair and ASCII':'A🦆Z',empty:'',NULL:null}
await withReferenceContainer(async(config,container)=>{
 const c=await connect(config)
 try {
  const version=await command(c,'SELECT @@VERSION AS version')
  const results=[]
  for(const collation of collations){
   // Fixed allowlisted names, never user-supplied SQL fragments.
   const prefix=`WITH units AS (SELECT value AS unit,NCHAR(value) COLLATE ${collation} AS ch FROM GENERATE_SERIES(0,65535)), mapped AS (SELECT unit,UNICODE(ch) AS original,UNICODE(LOWER(ch)) AS lower_unit,UNICODE(UPPER(ch)) AS upper_unit,DATALENGTH(LOWER(ch)) AS lower_bytes,DATALENGTH(UPPER(ch)) AS upper_bytes FROM units) `
   const statisticsQuery=prefix+'SELECT COUNT(*) AS total,COUNT(original) AS original_nonnull,COUNT(lower_unit) AS lower_nonnull,COUNT(upper_unit) AS upper_nonnull,MIN(lower_bytes) AS lower_min_bytes,MAX(lower_bytes) AS lower_max_bytes,MIN(upper_bytes) AS upper_min_bytes,MAX(upper_bytes) AS upper_max_bytes,SUM(CASE WHEN original=unit THEN 1 ELSE 0 END) AS original_identity FROM mapped'
   const statistics=canonical(await command(c,statisticsQuery))
   assert.deepEqual(statistics.sets[0].rows,[[65536,65536,65536,65536,2,2,2,2,65536]],collation)
   const mappingQuery=prefix+'SELECT unit,lower_unit,upper_unit FROM mapped WHERE lower_unit<>unit OR upper_unit<>unit OR lower_unit IS NULL OR upper_unit IS NULL ORDER BY unit'
   const mapping=canonical(await command(c,mappingQuery))
   const rows=mapping.sets[0].rows
   let previous=-1
   for(const [unit,lower,upper] of rows){
    assert.ok(Number.isInteger(unit)&&unit>previous&&unit<=65535)
    for(const n of [lower,upper])assert.ok(Number.isInteger(n)&&n>=0&&n<=65535)
    assert.ok(lower!==unit||upper!==unit)
    previous=unit
   }
   const strings=[]
   for(const [name,expression] of probes){
    const query=`SELECT LOWER((${expression}) COLLATE ${collation}) AS lower_text,UPPER((${expression}) COLLATE ${collation}) AS upper_text`
    strings.push({name,query,reference:canonical(await command(c,query))})
   }
   const map = new Map(rows.map(([unit,lower,upper])=>[unit,[lower,upper]]))
   for(const probe of strings){
    const input=inputs[probe.name]
    for(let direction=0;direction<2;direction++){
     const mapped=input===null?null:Array.from({length:input.length},(_,i)=>String.fromCharCode(map.get(input.charCodeAt(i))?.[direction]??input.charCodeAt(i))).join('')
     assert.equal(mapped,probe.reference.sets[0].rows[0][direction],`${collation}: ${probe.name}`)
    }
   }
   results.push({collation,statisticsQuery,statistics,mappingQuery,mapping,strings})
   console.log(JSON.stringify({collation,changedUnits:rows.length,strings:strings.map(x=>({name:x.name,rows:x.reference.sets[0].rows}))}))
  }
  await writeFile(process.argv[2]??'reference/unicode-case.json',JSON.stringify({image:container.image,version,encoding:'UTF-16 code units; unlisted BMP units map to themselves; these collations are non-SC',results},null,2)+'\n')
 }finally{c.close()}
})
