// First-party key and index evidence for docs/gaps-keys.md. Each case runs its
// statements in order in a fresh database; rows, diagnostics and completions
// are kept raw except the random hexadecimal suffix of system-generated
// constraint names and the generated database name, which differ between
// runs and are replaced by <hash> and <database>.
//
// node scripts/capture-gaps-keys.mjs [output-directory]
// MSSQL_REFERENCE_IMAGE selects the image (default: the pinned reference).
import assert from 'node:assert/strict'
import {execFile} from 'node:child_process'
import {promisify} from 'node:util'
import {mkdir, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference, assertSameCapture} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

const indexes = "SELECT o.name AS table_name,i.name,i.index_id,i.type_desc,i.is_unique,i.is_primary_key,i.is_unique_constraint,i.has_filter,i.filter_definition,i.ignore_dup_key,i.fill_factor FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id WHERE o.type='U' ORDER BY o.name,i.index_id"
export const cases = [
  ['nvarchar-primary-key', [
    'CREATE TABLE items (id nvarchar(450) NOT NULL CONSTRAINT pk_items PRIMARY KEY NONCLUSTERED)',
    "INSERT items VALUES (N'abc')",
    "INSERT items VALUES (N'abc')",
    "INSERT items VALUES (N'ABC')",
    "INSERT items VALUES (N'abc  ')",
    "INSERT items VALUES (N'abd'), (N'abd')",
    "UPDATE items SET id = N'abc' WHERE id = N'ABC'",
    'SELECT id FROM items ORDER BY id',
    'INSERT items VALUES (NULL)',
    indexes,
  ]],
  ['nvarchar-composite-primary-key', [
    'CREATE TABLE items (tenant nvarchar(20) NOT NULL, code nvarchar(20) NOT NULL, n int, CONSTRAINT pk_items PRIMARY KEY CLUSTERED (tenant, code))',
    "INSERT items VALUES (N'a', N'x', 1), (N'a', N'y', 2), (N'b', N'x', 3)",
    "INSERT items VALUES (N'a', N'x', 4)",
    'SELECT tenant, code, n FROM items ORDER BY n',
  ]],
  ['nvarchar-unique-constraint', [
    'CREATE TABLE items (id int NOT NULL, name nvarchar(50) NULL CONSTRAINT uq_items_name UNIQUE)',
    "INSERT items VALUES (1, N'one'), (2, NULL)",
    "INSERT items VALUES (3, N'one')",
    'INSERT items VALUES (4, NULL)',
    'SELECT id, name FROM items ORDER BY id',
  ]],
  ['nvarchar-indexes', [
    'CREATE TABLE items (id int NOT NULL, name nvarchar(100) NULL, code nchar(4) NULL)',
    'CREATE NONCLUSTERED INDEX ix_items_name ON items(name)',
    'CREATE UNIQUE INDEX ux_items_code ON items(code)',
    "INSERT items VALUES (1, N'a', N'x'), (2, N'a', N'y')",
    "INSERT items VALUES (3, N'b', N'x')",
    "INSERT items VALUES (4, N'b', N'x   ')",
    'INSERT items VALUES (5, NULL, NULL)',
    'INSERT items VALUES (6, NULL, NULL)',
    'SELECT id, name, code FROM items ORDER BY id',
    'DROP INDEX ux_items_code ON items',
    'INSERT items VALUES (7, NULL, NULL)',
    'SELECT count(*) FROM items',
  ]],
  ['datetimeoffset-keys', [
    'CREATE TABLE items (id int NOT NULL, occurred datetimeoffset NOT NULL, CONSTRAINT pk_items PRIMARY KEY CLUSTERED(id, occurred))',
    "INSERT items VALUES (1, '2024-01-02 10:00:00 +01:00')",
    "INSERT items VALUES (1, '2024-01-02 10:00:00 +01:00')",
    "INSERT items VALUES (1, '2024-01-02 09:00:00 +00:00')",
    "INSERT items VALUES (1, '2024-01-02 10:00:00 +00:00')",
    'SELECT id, CONVERT(varchar(40), occurred, 127) FROM items ORDER BY occurred',
    'CREATE TABLE events (id int NOT NULL, at datetimeoffset(3) NULL)',
    'CREATE INDEX ix_events_at ON events(at)',
    'CREATE UNIQUE INDEX ux_events_at ON events(at)',
    "INSERT events VALUES (1, '2024-01-02 10:00:00.123 +02:00')",
    "INSERT events VALUES (2, '2024-01-02 08:00:00.123 +00:00')",
  ]],
  ['column-level-list', [
    'CREATE TABLE items (id int NOT NULL CONSTRAINT uq_items UNIQUE(id))',
    'INSERT items VALUES (1)',
    'INSERT items VALUES (1)',
    'CREATE TABLE things (id int NOT NULL CONSTRAINT pk_things PRIMARY KEY(id), v int)',
    'INSERT things VALUES (1, 1)',
    'INSERT things VALUES (1, 2)',
    'CREATE TABLE pairs (a int NOT NULL, b int NOT NULL CONSTRAINT pk_pairs PRIMARY KEY (a, b))',
    'INSERT pairs VALUES (1, 1), (1, 2)',
    'INSERT pairs VALUES (1, 1)',
    'CREATE TABLE plain (id int PRIMARY KEY (id))',
    'CREATE TABLE other (id int NOT NULL UNIQUE NONCLUSTERED (id))',
    indexes,
  ]],
  ['unique-single-null', [
    'CREATE TABLE items(value int UNIQUE)',
    'INSERT items VALUES(NULL)',
    'INSERT items VALUES(NULL)',
    'INSERT items VALUES(1), (2)',
    'INSERT items VALUES(2)',
    'UPDATE items SET value = NULL WHERE value = 1',
    'SELECT value FROM items ORDER BY value',
    'CREATE TABLE pairs(a int NULL, b int NULL, CONSTRAINT uq_pairs UNIQUE(a, b))',
    'INSERT pairs VALUES(1, NULL), (2, NULL), (NULL, NULL)',
    'INSERT pairs VALUES(1, NULL)',
    'INSERT pairs VALUES(NULL, NULL)',
    'CREATE TABLE named(value varchar(10) NULL CONSTRAINT uq_named UNIQUE)',
    'INSERT named VALUES(NULL)',
    'INSERT named VALUES(NULL)',
  ]],
  ['unique-index-null', [
    'CREATE TABLE items(id int, value varchar(20) NULL)',
    'CREATE UNIQUE INDEX ux_items_value ON items(value)',
    "INSERT items VALUES(1, NULL), (2, 'a')",
    'INSERT items VALUES(3, NULL)',
    "INSERT items VALUES(4, 'a')",
    "INSERT items VALUES(5, 'A')",
    'SELECT id, value FROM items ORDER BY id',
  ]],
  ['filtered-unique', [
    'CREATE TABLE items(id int NOT NULL, email nvarchar(200) NULL)',
    'CREATE UNIQUE INDEX ux_items_email ON items(email) WHERE email IS NOT NULL',
    'INSERT items VALUES(1, NULL), (2, NULL), (3, NULL)',
    "INSERT items VALUES(4, N'a@b.c')",
    "INSERT items VALUES(5, N'a@b.c')",
    "UPDATE items SET email = N'a@b.c' WHERE id = 1",
    'SELECT id, email FROM items ORDER BY id',
    'CREATE TABLE ranged(id int NOT NULL, n int NULL)',
    'CREATE UNIQUE INDEX ux_ranged ON ranged(n) WHERE n > 0',
    'INSERT ranged VALUES(1, 0), (2, 0), (3, -1), (4, -1), (5, 1)',
    'INSERT ranged VALUES(6, 1)',
    'UPDATE ranged SET n = 1 WHERE id = 1',
    'SELECT id, n FROM ranged ORDER BY id',
    indexes,
    'DROP INDEX ux_items_email ON items',
    "INSERT items VALUES(5, N'a@b.c')",
  ]],
  ['clustered-include-options', [
    'CREATE TABLE items(id int NOT NULL, value int NULL, label nvarchar(30) NULL)',
    'CREATE CLUSTERED INDEX ix_items ON items(id)',
    'CREATE INDEX ix_items_value ON items(id) INCLUDE(value, label)',
    'CREATE INDEX ix_items_positive ON items(id) WHERE id>0',
    'CREATE NONCLUSTERED INDEX ix_items_opts ON items(value DESC) INCLUDE(label) WHERE value IS NOT NULL WITH (FILLFACTOR = 80, PAD_INDEX = ON, SORT_IN_TEMPDB = ON, STATISTICS_NORECOMPUTE = OFF, DROP_EXISTING = OFF, ONLINE = OFF, ALLOW_ROW_LOCKS = ON, ALLOW_PAGE_LOCKS = ON, IGNORE_DUP_KEY = OFF, DATA_COMPRESSION = NONE)',
    'INSERT items VALUES(1, 1, N"a"), (1, 2, N"b")'.replaceAll('"', "'"),
    'SELECT id, value, label FROM items ORDER BY value',
    indexes,
    'CREATE CLUSTERED INDEX ix_items_second ON items(value)',
    'DROP INDEX ix_items ON items',
    'DROP INDEX ix_items_value ON items',
    'DROP INDEX ix_items_positive ON items, ix_items_opts ON items',
    indexes,
    'CREATE TABLE keyed(id int NOT NULL, code int NOT NULL)',
    'CREATE UNIQUE CLUSTERED INDEX ux_keyed ON keyed(id, code) WITH (FILLFACTOR = 90)',
    'INSERT keyed VALUES(1, 1), (1, 2)',
    'INSERT keyed VALUES(1, 1)',
    'DROP INDEX ux_keyed ON keyed',
    'INSERT keyed VALUES(1, 1)',
    'SELECT count(*) FROM keyed',
  ]],
  ['clustered-conflicts', [
    'CREATE TABLE items(id int NOT NULL PRIMARY KEY, value int)',
    'CREATE CLUSTERED INDEX ix_items_value ON items(value)',
    'CREATE TABLE nc(id int NOT NULL CONSTRAINT pk_nc PRIMARY KEY NONCLUSTERED, value int)',
    'CREATE CLUSTERED INDEX ix_nc_value ON nc(value)',
    'CREATE CLUSTERED INDEX ix_nc_again ON nc(id)',
    'CREATE TABLE two(a int NOT NULL CONSTRAINT pk_two PRIMARY KEY CLUSTERED, b int NOT NULL CONSTRAINT uq_two UNIQUE CLUSTERED)',
    'DROP INDEX pk_nc ON nc',
    'DROP INDEX ix_nc_value ON nc WITH (ONLINE = OFF)',
    indexes,
  ]],
  ['ignore-dup-key', [
    'CREATE TABLE items(id int NOT NULL, value int)',
    'CREATE UNIQUE INDEX ux_items ON items(id) WITH (IGNORE_DUP_KEY = ON)',
    'INSERT items VALUES(1, 1)',
    'INSERT items VALUES(1, 2), (2, 2)',
    'SELECT id, value FROM items ORDER BY id',
    'UPDATE items SET id = 1 WHERE id = 2',
  ]],
  ['create-unique-over-duplicates', [
    'CREATE TABLE items(id int NOT NULL, name nvarchar(20))',
    "INSERT items VALUES(1, N'x'), (2, N'x'), (3, NULL), (4, NULL)",
    'CREATE UNIQUE INDEX ux_items_name ON items(name)',
    'CREATE UNIQUE INDEX ux_items_name_filtered ON items(name) WHERE id > 1 AND id < 4',
    'CREATE UNIQUE INDEX ux_items_name_nn ON items(name) WHERE name IS NOT NULL',
    'CREATE UNIQUE INDEX ux_items_id ON items(id)',
    'CREATE INDEX ux_items_id ON items(name)',
  ]],
  ['message-values', [
    'CREATE TABLE items(a nvarchar(10) NOT NULL, b int NULL, c datetimeoffset(2) NULL, d varchar(5) NULL, e uniqueidentifier NULL, f bit NULL, g decimal(9,2) NULL, h datetime2(3) NULL, CONSTRAINT uq_items UNIQUE(a, b, c, d, e, f, g, h))',
    "INSERT items VALUES(N'it''s', NULL, '2024-05-06 07:08:09.12 -03:30', 'v', '6F9619FF-8B86-D011-B42D-00C04FC964FF', 1, 12.5, '2024-01-02 03:04:05.678')",
    "INSERT items VALUES(N'it''s', NULL, '2024-05-06 07:08:09.12 -03:30', 'v', '6F9619FF-8B86-D011-B42D-00C04FC964FF', 1, 12.5, '2024-01-02 03:04:05.678')",
    'CREATE TABLE seq(id int NOT NULL PRIMARY KEY)',
    'INSERT seq VALUES(1)',
    'BEGIN TRY INSERT seq VALUES(1) END TRY BEGIN CATCH SELECT ERROR_NUMBER(), ERROR_SEVERITY(), ERROR_STATE(), ERROR_MESSAGE() END CATCH',
    'CREATE UNIQUE INDEX ux_seq ON seq(id)',
    'BEGIN TRY INSERT seq VALUES(1) END TRY BEGIN CATCH SELECT ERROR_NUMBER(), ERROR_SEVERITY(), ERROR_STATE(), ERROR_MESSAGE() END CATCH',
    'CREATE TABLE more(m money NOT NULL, f float NOT NULL, t time(3) NOT NULL, dt datetime NOT NULL, sd smalldatetime NOT NULL, b varbinary(4) NOT NULL, c char(4) NOT NULL, d date NOT NULL, CONSTRAINT uq_more UNIQUE(m, f, t, dt, sd, b, c, d))',
    "INSERT more VALUES(12.5, 1.5, '01:02:03.5', '2024-01-02 03:04:05', '2024-01-02 03:04:00', 0x01AB, 'a', '2024-01-02')",
    "INSERT more VALUES(12.5, 1.5, '01:02:03.5', '2024-01-02 03:04:05', '2024-01-02 03:04:00', 0x01AB, 'a', '2024-01-02')",
  ]],
  ['diagnostics', [
    'CREATE TABLE t1(a int NULL PRIMARY KEY)',
    'CREATE TABLE t2(a int PRIMARY KEY, b int PRIMARY KEY)',
    'CREATE TABLE t3(a nvarchar(max) NOT NULL PRIMARY KEY)',
    'CREATE TABLE t4(a int, CONSTRAINT uq_t4 UNIQUE(a, a))',
    'CREATE TABLE t5(a int, CONSTRAINT uq_t5 UNIQUE(b))',
    'CREATE TABLE t6(a int CONSTRAINT k6 UNIQUE, b int CONSTRAINT k6 UNIQUE)',
    'CREATE TABLE t7(a int CONSTRAINT k7 PRIMARY KEY)',
    'CREATE TABLE t8(a int CONSTRAINT k7 UNIQUE)',
    'CREATE TABLE t9(a int, b nvarchar(max))',
    'CREATE INDEX ix_t9 ON t9(missing)',
    'CREATE INDEX ix_t9 ON t9(a) INCLUDE(missing)',
    'CREATE INDEX ix_t9 ON t9(b)',
    'CREATE INDEX ix_t9 ON t9(a, a)',
    'CREATE INDEX ix_t9 ON t9(a) WITH (NOT_AN_OPTION = ON)',
    'CREATE INDEX ix_t9 ON t9(a) WITH (IGNORE_DUP_KEY = ON)',
    'CREATE INDEX ix_t9 ON t9(a) WITH (FILLFACTOR = 0)',
    'CREATE INDEX ix_t9 ON t9(a) WITH (DROP_EXISTING = ON)',
    'CREATE INDEX ix_t9 ON t9(a) WHERE missing > 0',
    'CREATE INDEX ix_t9 ON t9(a)',
    'CREATE UNIQUE INDEX ix_t9 ON t9(a) WITH (DROP_EXISTING = ON)',
    'INSERT t9 VALUES(1, NULL), (1, NULL)',
    'CREATE INDEX ix_t9 ON t9(a) WITH (DROP_EXISTING = ON)',
    'INSERT t9 VALUES(1, NULL), (1, NULL)',
    'SELECT count(*) FROM t9',
    'CREATE INDEX k7 ON t7(a)',
  ]],
  ['alter-with-keys', [
    'CREATE TABLE items(id nvarchar(10) NOT NULL CONSTRAINT pk_items PRIMARY KEY, code int NULL CONSTRAINT uq_items_code UNIQUE, v int NULL, w int NULL, x int NULL)',
    'CREATE INDEX ix_items_v ON items(v) INCLUDE(w)',
    "INSERT items VALUES (N'a', NULL, 1, 1, 1)",
    'ALTER TABLE items ADD extra int NULL',
    'ALTER TABLE items ALTER COLUMN x bigint NULL',
    'ALTER TABLE items DROP COLUMN extra',
    'ALTER TABLE items DROP COLUMN code',
    'ALTER TABLE items DROP COLUMN v',
    'ALTER TABLE items DROP COLUMN w',
    'ALTER TABLE items ALTER COLUMN code bigint NULL',
    'ALTER TABLE items ALTER COLUMN id nvarchar(20) NOT NULL',
    "INSERT items VALUES (N'b', NULL, 2, 2, 2)",
    "INSERT items VALUES (N'a', 5, 2, 2, 2)",
    'DROP INDEX ix_items_v ON items',
    'ALTER TABLE items DROP COLUMN v, w',
    'SELECT id, code, x FROM items',
  ]],
]

// Importing this module (tests/compat/keys.test.mjs does) only reads the cases.
if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) await captureReference()

async function captureReference() {
  const owner = `gaps-keys-v1:${process.pid}`
  const exec = promisify(execFile)
  async function labeledDocker(args, env) {
    const actual = args[0] === 'run' ? ['run', '--label', `msduck.task=${owner}`, ...args.slice(1)] : args
    return (await exec('docker', actual, {env: {...process.env, ...env}, maxBuffer: 4 * 1024 * 1024})).stdout.trim()
  }
  const redact = value => JSON.parse(JSON.stringify(value).replace(/(__[0-9A-Za-z_]+?__)[0-9A-F]{16}/g, '$1<hash>').replace(/msduck_audit_[0-9a-f]{32}/g, 'msduck_audit_<database>'))

  const output = resolve(process.argv[2] ?? 'artifacts/compatibility/gaps-keys-reference')
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
    assertSameCapture(runs[0], runs[1], 'Fresh key captures differ')
    const version = await isolatedReference(config, async connection => canonical(await capture(connection, 'SELECT @@VERSION AS version')).sets[0].rows[0][0])
    const actual = {image: container.image, version, identicalFreshCaptures: 2, cases: runs[0]}
    await writeFile(resolve(output, 'gaps-keys.json'), JSON.stringify(actual) + '\n')
    if (!process.env.CASES) assert.ok(actual.cases.length === cases.length)
    console.log(`Captured ${cases.length} key cases twice identically in ${output}`)
  }, {docker: labeledDocker})
}
