// First-party rowversion and identity evidence for docs/gaps-rowversion_identity.md.
// Each case runs its statements in order, each as its own batch, in a fresh
// database; rows, column metadata, diagnostics and completions are kept raw
// except the generated database name, which differs between runs.
//
// node scripts/capture-gaps-rowversion_identity.mjs [output-directory]
// MSSQL_REFERENCE_IMAGE selects the image (default: the pinned reference).
// CASES=a,b limits the run to the named cases (the fixture is then not
// complete; use it only for exploration).
import assert from 'node:assert/strict'
import {execFile} from 'node:child_process'
import {promisify} from 'node:util'
import {mkdir, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference, assertSameCapture} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

const variant = expr => `SQL_VARIANT_PROPERTY(${expr},'BaseType') AS base, SQL_VARIANT_PROPERTY(${expr},'Precision') AS prec, SQL_VARIANT_PROPERTY(${expr},'Scale') AS scale`
const columns = table => `SELECT c.name, t.name AS type_name, c.system_type_id, c.max_length, c.precision, c.scale, c.is_nullable, c.is_identity FROM sys.columns c JOIN sys.types t ON t.user_type_id=c.user_type_id WHERE c.object_id=OBJECT_ID('${table}') ORDER BY c.column_id`

export const cases = [
  ['rowversion-basic', [
    'SELECT @@DBTS AS dbts',
    'CREATE TABLE items(id int, value int, rv rowversion NOT NULL)',
    'SELECT @@DBTS AS dbts',
    'INSERT items(id,value) VALUES(1,10)',
    'SELECT @@DBTS AS dbts',
    'SELECT id, value, rv, CASE WHEN rv=@@DBTS THEN 1 ELSE 0 END AS is_last FROM items',
    'INSERT items(id,value) VALUES(2,20),(3,30)',
    'SELECT id, value, rv FROM items ORDER BY id',
    'UPDATE items SET value=11 WHERE id=1',
    'SELECT id, value, rv FROM items ORDER BY id',
    'UPDATE items SET value=value WHERE id=2',
    'SELECT id, value, rv FROM items ORDER BY id',
    'UPDATE items SET value=0 WHERE id=99',
    'SELECT @@DBTS AS dbts',
    'DELETE items WHERE id=3',
    'SELECT @@DBTS AS dbts',
    columns('items'),
    'SELECT rv FROM items WHERE 1=0',
  ]],
  ['rowversion-explicit', [
    'CREATE TABLE items(id int, value int, rv rowversion NOT NULL)',
    'INSERT items(id,value,rv) VALUES(1,10,0x01)',
    'INSERT items(id,value,rv) VALUES(2,20,NULL)',
    'INSERT items(id,value,rv) VALUES(3,30,DEFAULT)',
    'INSERT items VALUES(4,40,DEFAULT)',
    'INSERT items VALUES(5,50,NULL)',
    'INSERT items VALUES(6,60,0x01)',
    'INSERT items VALUES(7,70)',
    'INSERT items(id,value,rv) SELECT 8,80,0x01',
    'UPDATE items SET rv=0x01',
    'UPDATE items SET rv=DEFAULT',
    'SELECT id, value, rv FROM items ORDER BY id',
    'INSERT items DEFAULT VALUES',
    'SELECT id, value, rv FROM items ORDER BY id',
  ]],
  ['timestamp-type', [
    'CREATE TABLE items(id int, value int, rv timestamp NOT NULL)',
    'INSERT items(id,value) VALUES(1,10)',
    'SELECT id, value, rv FROM items',
    columns('items'),
    'CREATE TABLE stamps(id int, timestamp)',
    'INSERT stamps(id) VALUES(1)',
    'SELECT id, timestamp FROM stamps',
    columns('stamps'),
    'CREATE TABLE nullable(id int, rv rowversion NULL)',
    'INSERT nullable(id) VALUES(1)',
    'SELECT id, rv FROM nullable',
    columns('nullable'),
    'CREATE TABLE two(id int, a rowversion, b timestamp)',
    'ALTER TABLE items ADD rv2 rowversion',
    'CREATE TABLE unspecified(id int, rv rowversion)',
    columns('unspecified'),
    'CREATE TABLE defaulted(id int, rv rowversion DEFAULT 0x01)',
    "SELECT name FROM sys.types WHERE system_type_id=189",
  ]],
  ['rowversion-dml-shapes', [
    'CREATE TABLE items(id int, value int, rv rowversion)',
    'INSERT items(id,value) VALUES(1,10),(2,20)',
    'INSERT items(id,value,rv) SELECT 9,90,NULL',
    'UPDATE i SET value=11 FROM items i WHERE i.id=1',
    'SELECT id, value, rv FROM items ORDER BY id',
    'BEGIN TRAN; UPDATE items SET value=12 WHERE id=1; ROLLBACK',
    'UPDATE items SET value=13 WHERE id=2',
    'SELECT id, value, rv FROM items ORDER BY id',
    'SELECT @@DBTS AS dbts',
    'DECLARE @before binary(8) = (SELECT rv FROM items WHERE id=1); UPDATE items SET value=14 WHERE id=1 AND rv=@before; SELECT @@ROWCOUNT AS updated',
    'DECLARE @stale binary(8) = 0x00000000000007D1; UPDATE items SET value=15 WHERE id=1 AND rv=@stale; SELECT @@ROWCOUNT AS updated',
  ]],
  ['rowversion-database-counter', [
    'CREATE TABLE a(id int, rv rowversion)',
    'CREATE TABLE b(id int, rv rowversion)',
    'INSERT a(id) VALUES(1)',
    'INSERT b(id) VALUES(1)',
    'INSERT a(id) VALUES(2)',
    'SELECT \'a\' AS t, id, rv FROM a UNION ALL SELECT \'b\', id, rv FROM b ORDER BY rv',
    'BEGIN TRAN; INSERT a(id) VALUES(3); ROLLBACK',
    'SELECT @@DBTS AS dbts',
    'INSERT a(id) VALUES(4)',
    'SELECT id, rv FROM a ORDER BY id',
    'SELECT CAST(rv AS bigint) AS n, CONVERT(varchar(20), rv, 1) AS text FROM a ORDER BY id',
    'SELECT MIN_ACTIVE_ROWVERSION() AS m, @@DBTS AS d',
    'ALTER TABLE b ADD v int NULL',
    'UPDATE b SET v=1',
    'SELECT id, v, rv FROM b',
    'CREATE TABLE c(id int)',
    'INSERT c VALUES(1)',
    'ALTER TABLE c ADD rv rowversion',
    'SELECT id, rv FROM c',
    'SELECT @@DBTS AS dbts',
  ]],
  ['decimal-identity', [
    'CREATE TABLE items(id decimal(15,0) IDENTITY(1,1) NOT NULL, value int)',
    'INSERT items(value) VALUES(10)',
    'INSERT items(value) VALUES(20),(30)',
    'SELECT id, value FROM items ORDER BY id',
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i, IDENT_CURRENT(\'items\') AS c',
    `SELECT ${variant('SCOPE_IDENTITY()')}`,
    `SELECT ${variant('@@IDENTITY')}`,
    `SELECT ${variant("IDENT_CURRENT('items')")}`,
    'CREATE TABLE n(id numeric(10,0) IDENTITY(100,5), value int)',
    'INSERT n(value) VALUES(1),(2)',
    'SELECT id, value FROM n ORDER BY id',
    columns('n'),
    "SELECT name, seed_value, increment_value, last_value FROM sys.identity_columns WHERE object_id=OBJECT_ID('n')",
    `SELECT ${variant('seed_value')} FROM sys.identity_columns WHERE object_id=OBJECT_ID('n')`,
    "SELECT IDENT_SEED('n') AS seed, IDENT_INCR('n') AS incr, IDENT_CURRENT('n') AS cur",
    'CREATE TABLE bad(id decimal(5,2) IDENTITY, value int)',
    'CREATE TABLE bad2(id float IDENTITY, value int)',
    'CREATE TABLE big(id decimal(38,0) IDENTITY(1000000000000000000000,1), value int)',
    'INSERT big(value) VALUES(1)',
    'SELECT id FROM big',
    'CREATE TABLE nul(id decimal(10,0) IDENTITY NULL, value int)',
    'CREATE TABLE def(id decimal(10,0) IDENTITY DEFAULT 1, value int)',
    'CREATE TABLE small(id decimal(2,0) IDENTITY(98,1), value int)',
    'INSERT small(value) VALUES(1),(2)',
    'INSERT small(value) VALUES(3)',
    'SELECT id, value FROM small ORDER BY id',
  ]],
  ['small-identity', [
    'CREATE TABLE t(id tinyint IDENTITY(250,2), value int)',
    'INSERT t(value) VALUES(1),(2),(3)',
    'INSERT t(value) VALUES(4)',
    'SELECT id, value FROM t ORDER BY id',
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i',
    'CREATE TABLE s(id smallint IDENTITY(-5,-3), value int)',
    'INSERT s(value) VALUES(1),(2)',
    'SELECT id, value FROM s ORDER BY id',
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i, IDENT_CURRENT(\'s\') AS c',
    `SELECT ${variant('SCOPE_IDENTITY()')}`,
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i',
  ]],
  ['scope-identity', [
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i',
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i WHERE 1=0',
    'CREATE TABLE items(id int IDENTITY(10,10), value int)',
    'CREATE TABLE plain(value int)',
    'INSERT items(value) VALUES(1)',
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i',
    'INSERT items(value) VALUES(2); SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i',
    'INSERT plain VALUES(1); SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i',
    'INSERT items(value) VALUES(3); INSERT plain VALUES(2); SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i',
    "INSERT items(value) VALUES(4); INSERT items(value) SELECT value FROM plain WHERE 1=0; SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i",
    'BEGIN TRAN; INSERT items(value) VALUES(6); ROLLBACK; SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i, IDENT_CURRENT(\'items\') AS c',
    'DECLARE @id int; INSERT items(value) VALUES(7); SET @id=SCOPE_IDENTITY(); SELECT @id AS id',
    'DECLARE @id numeric(38,0)=@@IDENTITY; SELECT @id AS id',
    'INSERT items(value) VALUES(8); UPDATE items SET value=9 WHERE value=8; SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i',
    'INSERT items(value) VALUES(10); DELETE plain; SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i',
    'SELECT id, value FROM items ORDER BY id',
    "SELECT IDENT_CURRENT('items') AS c, IDENT_CURRENT('plain') AS p, IDENT_CURRENT('missing') AS m",
    "INSERT items(value) OUTPUT inserted.id VALUES(11); SELECT SCOPE_IDENTITY() AS s",
    'SELECT scope_identity() AS lower_case',
    'INSERT items(value) VALUES(12); INSERT plain SELECT 1 WHERE 1=0; SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i',
    // A conversion failure after allocation (SQL Server consumes the value).
    "INSERT items(value) VALUES(5); INSERT items(value) VALUES('x'); SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i",
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i',
  ]],
  ['identity-insert', [
    'CREATE TABLE items(id int IDENTITY(1,1), value int)',
    'INSERT items(value) VALUES(1)',
    'SET IDENTITY_INSERT items ON',
    'INSERT items(id,value) VALUES(10,2)',
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i, IDENT_CURRENT(\'items\') AS c',
    'INSERT items(id,value) VALUES(5,3)',
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i, IDENT_CURRENT(\'items\') AS c',
    'INSERT items VALUES(20,4)',
    'INSERT items(value) VALUES(5)',
    'INSERT items DEFAULT VALUES',
    'INSERT items(id,value) VALUES(NULL,6)',
    'INSERT items(id,value) VALUES(DEFAULT,6)',
    'INSERT items(id,value) VALUES(10,7),(30,8),(25,9)',
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i, IDENT_CURRENT(\'items\') AS c',
    'INSERT items(id,value) SELECT 40,10',
    'SELECT SCOPE_IDENTITY() AS s, @@IDENTITY AS i, IDENT_CURRENT(\'items\') AS c',
    'SET IDENTITY_INSERT items OFF',
    'INSERT items(value) VALUES(11)',
    'INSERT items(id,value) VALUES(100,12)',
    'SELECT id, value FROM items ORDER BY id, value',
    'SET IDENTITY_INSERT items OFF',
  ]],
  ['identity-insert-rules', [
    'CREATE TABLE a(id int IDENTITY, value int)',
    'CREATE TABLE b(id bigint IDENTITY(100,-1), value int)',
    'CREATE TABLE plain(value int)',
    'SET IDENTITY_INSERT a ON',
    'SET IDENTITY_INSERT a ON',
    'SET IDENTITY_INSERT b ON',
    'SET IDENTITY_INSERT plain ON',
    'SET IDENTITY_INSERT missing ON',
    'SET IDENTITY_INSERT dbo.missing ON',
    'SET IDENTITY_INSERT b OFF',
    'SET IDENTITY_INSERT [dbo].[a] OFF',
    'SET IDENTITY_INSERT dbo.b ON',
    'INSERT b(id,value) VALUES(50,1)',
    'INSERT b(id,value) VALUES(200,2)',
    'SET IDENTITY_INSERT b OFF',
    'INSERT b(value) VALUES(3)',
    'SELECT id, value FROM b ORDER BY id',
    "SELECT IDENT_CURRENT('b') AS c",
    'INSERT a(id,value) VALUES(1,1)',
    'SET IDENTITY_INSERT plain OFF',
    'SET IDENTITY_INSERT b ON; INSERT b(id,value) VALUES(7,7); SET IDENTITY_INSERT b OFF; SELECT SCOPE_IDENTITY() AS s',
    'SET IDENTITY_INSERT b ON',
    "SELECT name, last_value FROM sys.identity_columns WHERE object_id=OBJECT_ID('b')",
  ]],
  ['identity-insert-decimal', [
    'CREATE TABLE items(id decimal(10,0) IDENTITY(1,1) NOT NULL, value int)',
    'INSERT items(value) VALUES(1)',
    'SET IDENTITY_INSERT items ON',
    'INSERT items(id,value) VALUES(50,2)',
    'SET IDENTITY_INSERT items OFF',
    'INSERT items(value) VALUES(3)',
    'SELECT id, value FROM items ORDER BY id',
    'SELECT SCOPE_IDENTITY() AS s',
    `SELECT ${variant('SCOPE_IDENTITY()')}`,
  ]],
]

// Importing this module only reads the cases.
if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) await captureReference()

async function captureReference() {
  const owner = `gaps-rowversion-identity-v1:${process.pid}`
  const exec = promisify(execFile)
  async function labeledDocker(args, env) {
    const actual = args[0] === 'run' ? ['run', '--label', `msduck.task=${owner}`, ...args.slice(1)] : args
    return (await exec('docker', actual, {env: {...process.env, ...env}, maxBuffer: 4 * 1024 * 1024})).stdout.trim()
  }
  const redact = value => JSON.parse(JSON.stringify(value).replace(/msduck_audit_[0-9a-f]{32}/g, 'msduck_audit_<database>'))

  const output = resolve(process.argv[2] ?? 'artifacts/compatibility/gaps-rowversion_identity-reference')
  await mkdir(output, {recursive: true})
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      const results = []
      for (const [id, statements] of cases.filter(([id]) => !process.env.CASES || process.env.CASES.split(',').includes(id))) {
        results.push(await isolatedReference(config, async connection => {
          const steps = []
          for (const sql of statements) {
            const tokens = []
            const debugToken = connection.debug.token.bind(connection.debug)
            connection.debug.token = token => { if (token.name.startsWith('DONE')) tokens.push(JSON.parse(JSON.stringify(token))); debugToken(token) }
            const result = canonical(await capture(connection, sql))
            connection.debug.token = debugToken
            steps.push(redact({sql, result, completion: tokens}))
          }
          return {id, steps}
        }))
      }
      runs.push(results)
    }
    await writeFile(resolve(output, 'runs.json'), JSON.stringify(runs, null, 2) + '\n')
    assertSameCapture(runs[0], runs[1], 'Fresh rowversion/identity captures differ')
    const version = await isolatedReference(config, async connection => canonical(await capture(connection, 'SELECT @@VERSION AS version')).sets[0].rows[0][0])
    const actual = {image: container.image, version, identicalFreshCaptures: 2, cases: runs[0]}
    await writeFile(resolve(output, 'gaps-rowversion_identity.json'), JSON.stringify(actual) + '\n')
    if (!process.env.CASES) assert.ok(actual.cases.length === cases.length)
    console.log(`Captured ${actual.cases.length} rowversion/identity cases twice identically in ${output}`)
  }, {docker: labeledDocker})
}
