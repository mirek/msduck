import { writeFile } from 'node:fs/promises'
import { TYPES } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

// SQL Server 2025 ground truth for STRING_ESCAPE's second argument. Every
// capture includes raw descriptors, errors, and DONE-family events.
const output = process.argv[2] ?? 'reference/string-escape-format.json'

const expressions = [
  ['json', "'json'"],
  ['json trailing space', "'json '"],
  ['json two spaces', "'json  '"],
  ['JSON mixed case spaces', "N'JsOn  '"],
  ['json char8', "CAST('json' AS CHAR(8))"],
  ['json nchar8', "CAST(N'json' AS NCHAR(8))"],
  ['json varchar8', "CAST('json' AS VARCHAR(8))"],
  ['json nvarchar8', "CAST(N'json' AS NVARCHAR(8))"],
  ['json leading space', "' json'"],
  ['json trailing tab', "'json'+CHAR(9)"],
  ['json trailing newline', "'json'+CHAR(10)"],
  ['json trailing NBSP', "N'json'+NCHAR(160)"],
  ['json suffix x', "'jsonx'"],
  ['literal NULL', 'NULL'],
  ['typed varchar NULL', 'CAST(NULL AS VARCHAR(8))'],
  ['typed nvarchar NULL', 'CAST(NULL AS NVARCHAR(8))'],
  ['typed char NULL', 'CAST(NULL AS CHAR(8))'],
  ['typed nchar NULL', 'CAST(NULL AS NCHAR(8))'],
]

function rpc(connection, sql, type, value) {
  return capture({
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch(request) {
      request.addParameter('f', type, value)
      connection.execSql(request)
    },
  }, sql)
}

async function observe(connection) {
  const results=[]
  for (const [name, format] of expressions) {
    const sql=`SELECT STRING_ESCAPE(N'x',${format}) AS escaped`
    results.push({name,sql,result:canonical(await capture(connection,sql))})
  }
  for (const [name,source,format] of [
    ['NULL source invalid', 'CAST(NULL AS NVARCHAR(8))', "'xml'"],
    ['NULL source padded', 'CAST(NULL AS NVARCHAR(8))', "'json '"],
    ['NULL source typed NULL', 'CAST(NULL AS NVARCHAR(8))', 'CAST(NULL AS NVARCHAR(8))'],
    ['empty source invalid', "N''", "'xml'"],
  ]) {
    const sql=`SELECT STRING_ESCAPE(${source},${format}) AS escaped`
    results.push({name,sql,result:canonical(await capture(connection,sql))})
  }
  for(const [name,type,value] of [
    ['RPC nvarchar NULL',TYPES.NVarChar,null],
    ['RPC varchar NULL',TYPES.VarChar,null],
    ['RPC nvarchar padded',TYPES.NVarChar,'json '],
    ['RPC varchar padded',TYPES.VarChar,'json '],
    ['RPC nvarchar leading',TYPES.NVarChar,' json'],
  ]) {
    const sql='SELECT STRING_ESCAPE(N\'x\',@f) AS escaped'
    results.push({name,sql,parameter:{type:type.name,value},result:canonical(await rpc(connection,sql,type,value))})
  }
  return results
}

await withReferenceContainer(async (config,container)=>{
  const runs=[]
  for(let i=0;i<2;i++)runs.push(await isolatedReference(config,observe))
  assertSameCapture(runs[0],runs[1],'STRING_ESCAPE format matrix differs across fresh databases')
  await writeFile(output,JSON.stringify({image:container.image,runs},null,2)+'\n')
  console.log(`Captured ${runs[0].length} STRING_ESCAPE format cases twice`)
})
