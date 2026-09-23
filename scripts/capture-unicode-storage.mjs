// Preserve raw UTF-16 evidence, including isolated surrogates, in Node JSON.
import assert from 'node:assert/strict'
import { writeFile } from 'node:fs/promises'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, command } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const output = process.argv[2] ?? 'reference/unicode-storage.json'
const samples = [
  ['legacy upper', "UPPER(N'ƀ')"],
  ['modern upper', "UPPER(N'ƀ' COLLATE Latin1_General_100_CI_AS)"],
  ['lower ASCII', "LOWER(N'ABC')"],
  ['empty', "N''"],
  ['null', 'CAST(NULL AS NVARCHAR(8))'],
  ['supplementary exact fit', "N'🦆x'"],
  ['high surrogate', "LEFT(N'🦆',1)"],
  ['low surrogate', "RIGHT(N'🦆',1)"],
  ['upper high surrogate', "UPPER(LEFT(N'🦆',1)+N'a')"],
  ['overflow', "N'abcd'"],
  ['surrogate overflow', "N'🦆🦆'"],
  ['trailing spaces', "N'ab   '"],
]
const types = ['NVARCHAR(3)', 'NCHAR(3)', 'NVARCHAR(MAX)']
const operations = ['values', 'select', 'update', 'default', 'variable']
const table = 'dbo.unicode_storage_reference'
const read = `SELECT id,s,DATALENGTH(s) AS bytes,CONVERT(VARBINARY(MAX),s) AS raw FROM ${table} ORDER BY id`

await withReferenceContainer(async (config, container) => {
  let connection
  let tokens = []
  async function open() {
    connection = await connect(config)
    const debug = connection.debug.token.bind(connection.debug)
    connection.debug.token = token => {
      if (token.name.startsWith('DONE')) tokens.push({...token})
      debug(token)
    }
  }
  async function record(query) {
    tokens = []
    const result = canonical(await capture(connection, query))
    assert.ok(tokens.length > 0, query)
    return { query, result, tokens: canonical(tokens) }
  }
  await open()
  try {
    const version = await command(connection, 'SELECT @@VERSION AS version')
    const results = []
    for (const type of types) for (const [sample, expression] of samples) for (const operation of operations) {
      const setup = `DROP TABLE IF EXISTS ${table}; CREATE TABLE ${table}(id INT NOT NULL,s ${type}${operation === 'default' ? ` DEFAULT (${expression})` : ''});${operation === 'update' ? ` INSERT INTO ${table} VALUES(1,N'old'),(2,N'old');` : ''}`
      await command(connection, setup)
      const write = {
        values: `INSERT INTO ${table} VALUES(1,N'ok'),(2,${expression})`,
        select: `INSERT INTO ${table} SELECT 1,N'ok' UNION ALL SELECT 2,${expression}`,
        update: `UPDATE ${table} SET s=${expression}`,
        default: `INSERT INTO ${table}(id) VALUES(1),(2)`,
        variable: `DECLARE @v NVARCHAR(MAX)=${expression}; INSERT INTO ${table} VALUES(1,N'ok'),(2,@v)`,
      }[operation]
      const written = await record(write)
      const overflow = type !== 'NVARCHAR(MAX)' && ['overflow','surrogate overflow'].includes(sample)
      assert.deepEqual(written.result.errors.map(e => e.number), overflow ? [2628] : [], `${type}: ${sample}: ${operation}`)
      const beforeReconnect = await record(read)
      assert.equal(beforeReconnect.result.errors.length, 0)
      assert.equal(beforeReconnect.result.sets[0].rows.length, overflow && operation !== 'update' ? 0 : 2)
      for (const [,value,bytes,raw] of beforeReconnect.result.sets[0].rows) {
        if (value === null) {
          assert.equal(bytes, null)
          assert.equal(raw, null)
        } else {
          const encoded = Buffer.from(value, 'utf16le')
          assert.equal(String(bytes), String(encoded.length))
          assert.deepEqual(raw, {kind:'binary',value:encoded.toString('hex')}, 'driver string must preserve stored UTF-16 units')
        }
      }
      if (overflow && operation === 'update') {
        assert.deepEqual(beforeReconnect.result.sets[0].rows.map(r => r[1]), ['old','old'])
      }
      connection.close()
      await open()
      const afterReconnect = await record(read)
      assert.deepEqual(afterReconnect, beforeReconnect, 'reconnect must retain exact rows and descriptors')
      results.push({type,sample,expression,operation,setup,written,beforeReconnect,afterReconnect})
    }
    assert.equal(results.length, 180)
    await writeFile(output, JSON.stringify({image:container.image,version,types,samples,operations,results}, null, 2)+'\n')
    console.log(JSON.stringify({output,cases:results.length,errorCases:results.filter(r=>r.written.result.errors.length).length}))
  } finally { connection?.close() }
})
