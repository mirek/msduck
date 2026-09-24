// SQL Server ground truth for JSON consumers of stored UTF-16 text.
// Node JSON serialization retains isolated surrogate units; binary columns
// independently verify the code units returned by the TDS driver.
import assert from 'node:assert/strict'
import { writeFile } from 'node:fs/promises'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, command } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const output = process.argv[2] ?? 'reference/unicode-json-storage.json'
const samples = [
  ['null', 'CAST(NULL AS NVARCHAR(8))'],
  ['empty', "N''"],
  ['ASCII', "N'abc'"],
  ['BMP', "N'£雪'"],
  ['pair', "N'🦆'"],
  ['high unit', "LEFT(N'🦆',1)"],
  ['low unit', "RIGHT(N'🦆',1)"],
  ['high then ASCII', "LEFT(N'🦆',1)+N'x'"],
  ['low then high', "RIGHT(N'🦆',1)+LEFT(N'🦆',1)"],
  ['escapes', "N'\"/\\'+NCHAR(9)+NCHAR(10)+NCHAR(0)"],
]
const types = ['NVARCHAR(24)', 'NCHAR(24)', 'NVARCHAR(MAX)']
const table = 'dbo.unicode_json_source'
const scalar = expr => `SELECT ${expr} AS value,DATALENGTH(${expr}) AS bytes,CONVERT(VARBINARY(MAX),${expr}) AS raw FROM ${table}`
const operations = [
  ['source', scalar('s')],
  ['document', scalar('j')],
  ['ISJSON', `SELECT ISJSON(j),ISJSON(j,VALUE),ISJSON(j,OBJECT),ISJSON(j,SCALAR) FROM ${table}`],
  ['JSON_VALUE', scalar("JSON_VALUE(j,N'$.s')")],
  ['JSON_QUERY', scalar("JSON_QUERY(j,N'$.a')")],
  ['JSON_PATH_EXISTS', `SELECT JSON_PATH_EXISTS(j,p),JSON_PATH_EXISTS(j,N'$.missing') FROM ${table}`],
  ['STRING_ESCAPE', scalar('STRING_ESCAPE(s,f)')],
  ['OPENJSON', `SELECT o.[key],o.value,o.type,DATALENGTH(o.value) AS bytes,CONVERT(VARBINARY(MAX),o.value) AS raw FROM ${table} CROSS APPLY OPENJSON(j) o ORDER BY o.[key]`],
  ['OPENJSON WITH', `SELECT o.s,DATALENGTH(o.s) AS bytes,CONVERT(VARBINARY(MAX),o.s) AS raw,o.a FROM ${table} CROSS APPLY OPENJSON(j) WITH(s NVARCHAR(MAX) '$.s',a NVARCHAR(MAX) '$.a' AS JSON) o`],
  ['FOR JSON', scalar(`(SELECT s AS value FROM ${table} FOR JSON PATH, INCLUDE_NULL_VALUES)`)],
  ['FOR JSON fragment', scalar(`(SELECT JSON_QUERY(j,N'$.a') AS a FROM ${table} FOR JSON PATH, INCLUDE_NULL_VALUES)`)],
]

await withReferenceContainer(async (config, container) => {
  let connection = await connect(config)
  const record = async sql => ({sql, result: canonical(await capture(connection, sql))})
  try {
    const version = await record('SELECT @@VERSION AS version')
    const results = []
    for (const type of types) for (const [sample, expression] of samples) {
      const setup = `DROP TABLE IF EXISTS ${table}; CREATE TABLE ${table}(s ${type},j NVARCHAR(MAX),p NVARCHAR(100),f NVARCHAR(10)); INSERT INTO ${table}(s,p,f) VALUES(${expression},N'$.s',N'json'); UPDATE ${table} SET j=CASE WHEN s IS NULL THEN NULL ELSE N'{"s":"'+STRING_ESCAPE(s,'json')+N'","a":["'+STRING_ESCAPE(s,'json')+N'"]}' END`
      await command(connection, setup)
      const captured = []
      for (const [operation, sql] of operations) {
        const result = await record(sql)
        if (['source','document','JSON_VALUE','JSON_QUERY','STRING_ESCAPE','FOR JSON','FOR JSON fragment'].includes(operation)) {
          for (const set of result.result.sets) for (const [value, bytes, raw] of set.rows) {
            if (value === null) {
              assert.equal(bytes, null)
              assert.equal(raw, null)
            } else {
              const encoded = Buffer.from(value, 'utf16le')
              assert.equal(String(bytes), String(encoded.length))
              assert.deepEqual(raw, {kind:'binary',value:encoded.toString('hex')})
            }
          }
        }
        captured.push({operation,...result})
      }
      connection.close()
      connection = await connect(config)
      assert.deepEqual(await record(operations[1][1]), captured[1] && {sql:captured[1].sql,result:captured[1].result}, 'stored JSON must retain exact units after reconnect')
      results.push({type,sample,expression,setup,operations:captured})
    }
    const documents = [
      ['escaped high', '{"s":"\\ud800","a":["\\ud800"]}'],
      ['escaped low', '{"s":"\\udc00","a":["\\udc00"]}'],
      ['escaped pair', '{"s":"\\ud83e\\udd86","a":["\\ud83e\\udd86"]}'],
      ['escaped low high', '{"s":"\\udc00\\ud800","a":["\\udc00\\ud800"]}'],
      ['invalid trailing', '{"s":"ok","a":[1],"bad":invalid}'],
    ]
    const escaped = []
    for (const [sample, document] of documents) {
      const setup = `UPDATE ${table} SET j=N'${document.replaceAll("'", "''")}'`
      await command(connection, setup)
      const captured = []
      for (const [operation, sql] of operations.filter(([name]) => ['ISJSON','JSON_VALUE','JSON_QUERY','JSON_PATH_EXISTS','OPENJSON','OPENJSON WITH','FOR JSON fragment'].includes(name))) {
        const result = await record(sql)
        if (['JSON_VALUE','JSON_QUERY','FOR JSON fragment'].includes(operation)) {
          for (const set of result.result.sets) for (const [value,bytes,raw] of set.rows) {
            if (value === null) { assert.equal(bytes,null); assert.equal(raw,null) }
            else {
              const encoded = Buffer.from(value,'utf16le')
              assert.equal(String(bytes),String(encoded.length))
              assert.deepEqual(raw,{kind:'binary',value:encoded.toString('hex')})
            }
          }
        }
        captured.push({operation,...result})
      }
      escaped.push({sample,document,setup,operations:captured})
    }
    const keys = []
    for (const [sample, expression] of samples) {
      const setup = `UPDATE ${table} SET s=${expression}; UPDATE ${table} SET j=CASE WHEN s IS NULL THEN NULL ELSE N'{"'+STRING_ESCAPE(s,'json')+N'":7}' END,p=CASE WHEN s IS NULL THEN NULL ELSE N'$."'+STRING_ESCAPE(s,'json')+N'"' END`
      await command(connection,setup)
      const queries = [
        ['key source',scalar('s')],
        ['stored path',scalar('p')],
        ['key lookup',`SELECT JSON_VALUE(j,p),JSON_PATH_EXISTS(j,p) FROM ${table}`],
        ['OPENJSON key',`SELECT o.[key],DATALENGTH(o.[key]) AS bytes,CONVERT(VARBINARY(MAX),o.[key]) AS raw,o.value FROM ${table} CROSS APPLY OPENJSON(j) o`],
      ]
      const captured = []
      for (const [operation,sql] of queries) captured.push({operation,...await record(sql)})
      keys.push({sample,expression,setup,operations:captured})
    }
    const nullPaths = []
    for (const source of ["N'{}'",'NULL']) for (const path of ['NULL','CAST(NULL AS NVARCHAR(100))','p']) {
      const setup = `UPDATE ${table} SET j=${source},p=NULL`
      await command(connection,setup)
      for (const operation of ['JSON_VALUE','JSON_QUERY','JSON_PATH_EXISTS']) {
        nullPaths.push({source,path,operation,setup,...await record(`SELECT ${operation}(j,${path}) AS value FROM ${table}`)})
      }
    }
    await writeFile(output, JSON.stringify({image:container.image,version,types,samples,results,escaped,keys,nullPaths},null,2)+'\n')
    console.log(JSON.stringify({output,storedCases:results.length,escapedCases:escaped.length,keyCases:keys.length,nullPathCases:nullPaths.length,operations:results.reduce((n,r)=>n+r.operations.length,0)+escaped.reduce((n,r)=>n+r.operations.length,0)+keys.reduce((n,r)=>n+r.operations.length,0)+nullPaths.length}))
  } finally { connection.close() }
})
