// Exact stored Unicode integer conversions from the pinned SQL Server image.
import assert from 'node:assert/strict'
import { writeFile } from 'node:fs/promises'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, command } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const samples = [
  ['NULL','CAST(NULL AS NVARCHAR(MAX))'], ['empty',"N''"], ['spaces',"N'   '"],
  ['positive',"N'+007'"], ['negative',"N'-7'"], ['padded',"N'  7  '"],
  ['tab',"NCHAR(9)+N'7'+NCHAR(9)"], ['newline',"NCHAR(10)+N'7'+NCHAR(13)"],
  ['nbsp',"NCHAR(160)+N'7'"], ['sign only',"N'+'"], ['decimal',"N'1.9'"],
  ['exponent',"N'1e2'"], ['nul',"N'7'+NCHAR(0)"], ['unicode digits',"N'１２'"],
  ['high unit',"LEFT(N'🦆',1)"], ['low unit',"RIGHT(N'🦆',1)"],
  ['tiny max',"N'255'"], ['tiny overflow',"N'256'"], ['small max',"N'32767'"],
  ['small overflow',"N'32768'"], ['int max',"N'2147483647'"], ['int overflow',"N'2147483648'"],
  ['int min',"N'-2147483648'"], ['int underflow',"N'-2147483649'"],
  ['big max',"N'9223372036854775807'"], ['big overflow',"N'9223372036854775808'"],
  ['big min',"N'-9223372036854775808'"], ['big underflow',"N'-9223372036854775809'"],
  ['minus only',"N'-'"], ['sign space',"N'+ 7'"],
  ['4000 units',"REPLICATE(CAST(N'0' AS NVARCHAR(MAX)),3999)+N'7'"],
  ['4001 units',"REPLICATE(CAST(N'0' AS NVARCHAR(MAX)),4000)+N'7'"],
  ['long invalid',"REPLICATE(CAST(N'x' AS NVARCHAR(MAX)),300)"],
  ['long spaces',"REPLICATE(CAST(N' ' AS NVARCHAR(MAX)),5000)+N'7'"],
  ['leading zeroes',"REPLICATE(CAST(N'0' AS NVARCHAR(MAX)),5000)+N'7'"],
]
await withReferenceContainer(async (config, container) => {
  const c = await connect(config)
  const record = async sql => ({sql,result:canonical(await capture(c,sql))})
  try {
    const results=[]
    for (const [sample,expression] of samples) {
      const setup=`DROP TABLE IF EXISTS dbo.unicode_integer_source; CREATE TABLE dbo.unicode_integer_source(s NVARCHAR(MAX)); INSERT INTO dbo.unicode_integer_source VALUES(${expression})`
      await command(c,setup)
      const source=await record('SELECT s,CONVERT(VARBINARY(MAX),s) AS raw FROM dbo.unicode_integer_source')
      assert.equal(source.result.errors.length,0)
      const [value,raw]=source.result.sets[0].rows[0]
      assert.deepEqual(raw,value===null ? null : {kind:'binary',value:Buffer.from(value,'utf16le').toString('hex')})
      const operations=[]
      for (const target of ['TINYINT','SMALLINT','INT','BIGINT']) for (const mode of ['CAST','TRY_CAST']) {
        operations.push({target,mode,...await record(`SELECT ${mode}(s AS ${target}) AS value FROM dbo.unicode_integer_source`)})
      }
      results.push({sample,expression,setup,source,operations})
    }
    await writeFile(process.argv[2] ?? 'reference/unicode-integer.json',JSON.stringify({image:container.image,results},null,2)+'\n')
    console.log(`Captured ${results.length*8} integer conversions`)
  } finally { c.close() }
})
