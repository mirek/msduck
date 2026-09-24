// Character extrema: capture ordering, exact selected payloads and declarations.
import assert from 'node:assert/strict'
import { writeFile } from 'node:fs/promises'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, command } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const samples = [
  [1,1,'NULL'],[2,1,"N''"],[3,1,"N' '"],[4,1,"N'a'"],[5,1,"N'A'"],
  [6,1,"N'a '"],[7,1,"N'b'"],[8,1,"N'é'"],[9,1,"N'e'"],[10,1,"N'ß'"],
  [11,1,"N'Ā'"],[12,1,"N'ÿ'"],[13,1,"LEFT(N'🦆',1)"],
  [14,1,"RIGHT(N'🦆',1)"],[15,1,"N'🦆'"],[16,1,"NCHAR(0)"],
  [17,2,'NULL'],[18,2,'NULL'],
  [19,3,"N'a '"],[20,3,"N'a'"],[21,3,"N'A'"],
  [22,4,"N'ÿ'"],[23,4,"N'Ā'"],
  [24,5,"LEFT(N'🦆',1)"],[25,5,"RIGHT(N'🦆',1)"],[26,5,"N'🦆'"],
]
const declarations=['NVARCHAR(24)','NCHAR(24)','NVARCHAR(MAX)','VARCHAR(24)','CHAR(24)']
const collations=[null,'Latin1_General_100_BIN2','Latin1_General_100_CI_AS','Latin1_General_100_CI_AS_SC']
const table='dbo.character_extrema_source'
const shape='lo,hi,DATALENGTH(lo) AS lo_bytes,DATALENGTH(hi) AS hi_bytes,CONVERT(VARBINARY(MAX),lo) AS lo_raw,CONVERT(VARBINARY(MAX),hi) AS hi_raw'
await withReferenceContainer(async (config,container)=>{
  const c=await connect(config)
  const record=async sql=>({sql,result:canonical(await capture(c,sql))})
  try {
    const database=await record("SELECT CONVERT(NVARCHAR(128),DATABASEPROPERTYEX(DB_NAME(),'Collation')) AS collation")
    const results=[]
    for(const declaration of declarations) for(const collation of collations) {
      const setup=`DROP TABLE IF EXISTS ${table}; CREATE TABLE ${table}(id INT,g INT,s ${declaration}${collation ? ' COLLATE '+collation : ''}); INSERT INTO ${table} VALUES ${samples.map(([id,g,e])=>`(${id},${g},${e})`).join(',')}`
      await command(c,setup)
      const operations=[]
      for(const [operation,sql] of [
        ['source',`SELECT id,g,s,DATALENGTH(s) AS bytes,CONVERT(VARBINARY(MAX),s) AS raw FROM ${table} ORDER BY id`],
        ['scalar',`SELECT ${shape} FROM (SELECT MIN(s) AS lo,MAX(s) AS hi FROM ${table}) q`],
        ['grouped',`SELECT g,${shape} FROM (SELECT g,MIN(s) AS lo,MAX(s) AS hi FROM ${table} GROUP BY g) q ORDER BY g`],
        ['empty',`SELECT ${shape} FROM (SELECT MIN(s) AS lo,MAX(s) AS hi FROM ${table} WHERE 1=0) q`],
        ['window',`SELECT id,g,${shape} FROM (SELECT id,g,MIN(s) OVER(PARTITION BY g ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS lo,MAX(s) OVER(PARTITION BY g ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS hi FROM ${table}) q ORDER BY id`],
        ['distinct',`SELECT g,${shape} FROM (SELECT g,MIN(DISTINCT s) AS lo,MAX(DISTINCT s) AS hi FROM ${table} GROUP BY g) q ORDER BY g`],
        ['reverse ties',`SELECT ${shape} FROM (SELECT MIN(s) AS lo,MAX(s) AS hi FROM (SELECT TOP (100) s FROM ${table} WHERE g=3 ORDER BY id DESC) t) q`],
      ]) {
        const captured=await record(sql)
        assert.equal(captured.result.errors.length,0,`${declaration} ${collation} ${operation}`)
        for (const set of captured.result.sets) for (const row of set.rows) {
          const fields=operation==='source' ? [[row[2],row[3],row[4]]] : [
            [row.at(-6),row.at(-4),row.at(-2)],[row.at(-5),row.at(-3),row.at(-1)],
          ]
          for(const [value,bytes,raw] of fields) {
            if(value===null) {assert.equal(bytes,null);assert.equal(raw,null)}
            else {
              assert.equal(raw.kind,'binary')
              assert.equal(Number(bytes),raw.value.length/2)
              if(declaration.startsWith('N')) assert.equal(Buffer.from(value,'utf16le').toString('hex'),raw.value)
            }
          }
        }
        operations.push({operation,...captured})
      }
      results.push({declaration,collation,setup,operations})
    }
    await writeFile(process.argv[2]??'reference/character-extrema.json',JSON.stringify({image:container.image,database,samples,results},null,2)+'\n')
    console.log(`Captured ${results.length*7} extrema queries`)
  } finally {c.close()}
})
