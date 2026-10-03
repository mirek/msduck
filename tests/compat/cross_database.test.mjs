// Three-part names that reach another database (issue #871;
// docs/databases.md), through tedious. The statements and the expected
// rows, descriptors and errors come from reference/cross-database.json,
// captured from SQL Server by scripts/capture-cross-database.mjs. Known
// differences are asserted as they are, not normalized away.
import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'
import { Connection, TYPES } from 'tedious'
import { capture } from '../../scripts/lib/compatibility.mjs'
import { query, start } from '../support/client.mjs'

const run = JSON.parse(readFileSync(new URL('../../reference/cross-database.json', import.meta.url))).runs[0]
const reference = new Map(run.map(observation => [observation.name, observation.result]))
const sql = name => run.find(observation => observation.name === name).sql
const setup = run.filter(observation => /^setup \d+$/.test(observation.name)).map(observation => observation.sql)
const singleSetup = run.filter(observation => /^single setup \d+$/.test(observation.name)).map(observation => observation.sql)
// Observed after the setup, in order, from the session in master.
const observations = run.filter(observation => !/^(server version|setup \d+|single setup \d+|single user .*)$/.test(observation.name))
  .map(observation => [observation.name, observation.sql])

// What a client sees of a batch: descriptors, rows and errors.
const brief = result => ({
  columns: result.sets.map(set => set.columns.map(c => [c.name, c.type, c.length, c.precision, c.scale, c.flags & 1])),
  rows: result.sets.map(set => set.rows),
  errors: result.errors.map(e => [e.number, e.state, e.class, e.message]),
})
const same = (name, actual) => assert.equal(JSON.stringify(brief(actual)), JSON.stringify(brief(reference.get(name))), name)

// msduck's raw differences from the reference, by observation.
const known = {
  // Expression metadata outside cross-database binding: msduck reports the
  // concatenation of a NOT NULL column as nullable and the product's
  // precision as 18, as it does in the current database.
  expressions: actual => {
    const [columns] = brief(actual).columns
    assert.deepEqual(columns[0], ['bang', 'NVarChar', 102, null, null, 1])
    assert.deepEqual(columns[3], ['twice', 'DecimalN', 17, 18, 2, 1])
    assert.deepEqual(brief(actual).rows, brief(reference.get('expressions')).rows)
  },
  // SCOPE_IDENTITY() is decimal(38, 0) on the wire, as in the current database.
  insert: actual => {
    assert.equal(actual.sets[0].columns[0].type, 'DecimalN')
    assert.deepEqual(brief(actual).rows, brief(reference.get('insert')).rows)
  },
  // DuckDB writes one attached database per transaction.
  'two databases in one transaction': actual => assert.deepEqual(brief(actual).errors, [[40515, 1, 16,
    "unsupported cross-database transaction: database 'xdb_foo' cannot be modified in a transaction that has already modified database 'master'; a transaction may write only one database"]]),
  // The error number matches; the message is DuckDB's, as in the current database.
  'unknown object': actual => {
    assert.equal(actual.errors.length, 1)
    assert.deepEqual([actual.errors[0].number, actual.errors[0].state, actual.errors[0].class], [208, 1, 16])
    assert.match(actual.errors[0].message, /Table with name missing does not exist/)
  },
  // DDL in another database is refused.
  'create table in another database': actual => assert.deepEqual(brief(actual).errors, [[40515, 1, 16,
    'unsupported reference to xdb_foo.dbo.made in another database; USE xdb_foo first']]),
}

async function another(t, first) {
  const connection = new Connection(first.config)
  connection.on('error', () => {})
  t.after(() => connection.close())
  await new Promise((resolve, reject) => connection.connect(error => error ? reject(error) : resolve()))
  return connection
}

test('three-part names read and write another database as SQL Server does', { timeout: 180000 }, async t => {
  // BACKUP and RESTORE can exceed the default request timeout on a busy host.
  const c = await start(t, { options: { requestTimeout: 60000 } })
  const directory = mkdtempSync(join(tmpdir(), 'msduck-cross-database-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  // The backup device lives in a scratch directory here.
  const local = sql => sql.replaceAll('/var/opt/mssql/data/xdb_foo.bak', join(directory, 'xdb_foo.bak'))
  for (const statement of setup) assert.deepEqual((await capture(c, statement)).errors, [], statement)
  const checked = []
  for (const [name, statement] of observations) {
    const actual = await capture(c, local(statement))
    if (known[name]) known[name](actual)
    else same(name, actual)
    checked.push(name)
  }
  assert.equal(checked.length, 27)

  // SINGLE_USER admits only the session that set it.
  for (const statement of singleSetup) assert.deepEqual((await capture(c, statement)).errors, [], statement)
  const other = await another(t, c)
  same('single user other session', await capture(other, sql('single user other session')))
  same('single user owner session', await capture(c, sql('single user owner session')))
})

test('prepared statements and RPC parameters reach another database', { timeout: 60000 }, async t => {
  const c = await start(t)
  for (const statement of setup) assert.deepEqual((await capture(c, statement)).errors, [], statement)
  const read = await query(c, 'SELECT i.name, LEN(i.name) AS length, l.label FROM xdb_foo.dbo.items i JOIN dbo.loc l ON l.id = i.id WHERE i.id = @id', [['id', TYPES.Int, 1]])
  assert.deepEqual(read.rows, [['hello', 5, 'one']])
  await query(c, 'INSERT xdb_foo.dbo.items (name, v) VALUES (@name, @v)', [['name', TYPES.NVarChar, 'rpc'], ['v', TYPES.VarChar, 'r']])
  assert.deepEqual((await query(c, 'SELECT name, v FROM xdb_foo..items WHERE id = 2')).rows, [['rpc', 'r']])
  assert.deepEqual((await query(c, 'SELECT DB_NAME()')).rows, [['master']])
})
