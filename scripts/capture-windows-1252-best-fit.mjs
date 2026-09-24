// Capture SQL conversion, not a Unicode normalization approximation or a wire codec.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { writeFile } from 'node:fs/promises'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, command } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const collations = ['SQL_Latin1_General_CP1_CI_AS','Latin1_General_100_CI_AS','Latin1_General_100_CS_AS','Latin1_General_100_CI_AI','Latin1_General_100_CS_AI','Latin1_General_100_BIN2']
const hex = units => {
  const bytes=Buffer.alloc(units.length*2)
  units.forEach((u,i)=>bytes.writeUInt16LE(u,i*2))
  return bytes.toString('hex')
}
await withReferenceContainer(async (config,container) => {
  const c=await connect(config)
  const record=async sql=>({sql,result:canonical(await capture(c,sql))})
  try {
    const digits=Array.from({length:16},(_,n)=>`(${n})`).join(',')
    await command(c,`CREATE TABLE #units(unit INT NOT NULL,s NVARCHAR(1)); WITH digits AS (SELECT n FROM (VALUES ${digits}) d(n)), units AS (SELECT a.n+16*b.n+256*c.n+4096*d.n AS n FROM digits a CROSS JOIN digits b CROSS JOIN digits c CROSS JOIN digits d) INSERT INTO #units SELECT n,CONVERT(NVARCHAR(1),CONVERT(BINARY(1),n%256)+CONVERT(BINARY(1),n/256)) FROM units`)
    const maps=[]
    for(const collation of collations) {
      const sql=`SELECT unit,CONVERT(VARBINARY(8),CONVERT(VARCHAR(8),s COLLATE ${collation})) AS raw FROM #units ORDER BY unit`
      const result=await command(c,sql)
      assert.equal(result.sets.length,1)
      assert.equal(result.sets[0].rows.length,65536)
      const map=Buffer.alloc(65536)
      result.sets[0].rows.forEach(([unit,bytes],i)=>{
        assert.equal(unit,i)
        assert.ok(Buffer.isBuffer(bytes))
        assert.equal(bytes.length,1,`${collation}: U+${unit.toString(16)}`)
        map[unit]=bytes[0]
      })
      maps.push({collation,sql,sha256:createHash('sha256').update(map).digest('hex'),hex:map.toString('hex')})
      console.log(`Captured ${collation}: ${maps.at(-1).sha256}`)
    }
    const first=Buffer.from(maps[0].hex,'hex')
    const spaces=Array.from(first.entries()).filter(([u,b])=>b===32 && u!==32).map(([u])=>u)
    const samples=[[],[0],[0x100],[0x61,0x100],[0xd83e],[0xdd86],[0xd83e,0xdd86],[0x61,0xd83e,0xdd86],[0x61,32,32],...spaces.map(u=>[0x61,u,u])]
    const probes=[]
    for(const units of samples) for(const declaration of ['VARCHAR(1)','VARCHAR(2)','VARCHAR(3)','CHAR(1)','CHAR(2)','CHAR(3)','VARCHAR(MAX)']) {
      const sourceHex=hex(units)
      const source=`CONVERT(NVARCHAR(MAX),0x${sourceHex})`
      const cast=await record(`SELECT CONVERT(VARBINARY(MAX),CAST(${source} AS ${declaration})) AS raw,CONVERT(VARBINARY(MAX),TRY_CAST(${source} AS ${declaration})) AS try_raw`)
      await command(c,`DROP TABLE IF EXISTS dbo.best_fit_target; CREATE TABLE dbo.best_fit_target(s ${declaration})`)
      const storage=await record(`INSERT INTO dbo.best_fit_target VALUES(${source}); SELECT CONVERT(VARBINARY(MAX),s) AS raw FROM dbo.best_fit_target`)
      probes.push({sourceHex,declaration,cast,storage})
    }
    await writeFile(process.argv[2]??'reference/windows-1252-best-fit.json',JSON.stringify({image:container.image,maps,spaceMappings:spaces,probes},null,2)+'\n')
    // Optional generated core lookup file. Refuse to collapse different
    // collation maps into one table if a future reference changes behavior.
    if(process.argv[3]) {
      for(const map of maps) assert.equal(map.hex,maps[0].hex)
      await writeFile(process.argv[3],first)
    }
    console.log(`Captured ${maps.length*65536} unit conversions and ${probes.length} cast/storage boundaries`)
  } finally {c.close()}
})
