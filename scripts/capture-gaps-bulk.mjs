// First-party bulk-load evidence for docs/gaps-bulk.md. Each case runs its
// steps in order in a fresh database. A step is a SQL batch, or a bulk load
// through tedious (`newBulkLoad`/`execBulkLoad`, the path mssql's
// `request.bulk()` uses), whose INSERT BULK statement and BulkLoadBCP
// message tedious generates. Rows, diagnostics and DONE tokens are kept raw
// except the generated database name (<database>) and the value of 2628's
// "Truncated value", which SQL Server fills from uninitialized memory on
// this path (<value>).
//
// node scripts/capture-gaps-bulk.mjs [output-directory]
// MSSQL_REFERENCE_IMAGE selects the image (default: the pinned reference).
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { TYPES } from 'tedious'
import { capture, canonical } from './lib/compatibility.mjs'

// tedious sends a datetimeoffset with the client's local offset; fix it so
// captures and replays agree on every host.
process.env.TZ = 'UTC'

const generated = (count, row) => ({ count, row })
const date = iso => ({ date: iso })
const hex = text => ({ hex: text })
const max = 'max'

const clientTypes = [
  ['id', 'Int', { nullable: false }],
  ['big', 'BigInt'], ['small', 'SmallInt'], ['tiny', 'TinyInt'], ['flag', 'Bit'],
  ['amount', 'Decimal', { precision: 18, scale: 4 }], ['price', 'Money'],
  ['ratio', 'Float'], ['single', 'Real'],
  ['code', 'Char', { length: 4 }], ['name', 'VarChar', { length: 20 }],
  ['uname', 'NVarChar', { length: 20 }], ['ucode', 'NChar', { length: 3 }],
  ['born', 'Date'], ['stamp', 'DateTime'], ['at2', 'DateTime2', { scale: 7 }],
  ['small_at', 'SmallDateTime'], ['moment', 'DateTimeOffset', { scale: 7 }],
  ['clock', 'Time', { scale: 7 }], ['guid', 'UniqueIdentifier'],
  ['bin', 'VarBinary', { length: 10 }], ['fixed', 'Binary', { length: 4 }],
  ['notes', 'NVarChar', { length: max }], ['ansi', 'VarChar', { length: max }],
  ['blob', 'VarBinary', { length: max }],
]

export const cases = [
  ['client-types', [
    'CREATE TABLE items (id int NOT NULL, big bigint NULL, small smallint NULL, tiny tinyint NULL, flag bit NULL, amount decimal(18,4) NULL, price money NULL, ratio float NULL, single real NULL, code char(4) NULL, name varchar(20) NULL, uname nvarchar(20) NULL, ucode nchar(3) NULL, born date NULL, stamp datetime NULL, at2 datetime2(7) NULL, small_at smalldatetime NULL, moment datetimeoffset(7) NULL, clock time(7) NULL, guid uniqueidentifier NULL, bin varbinary(10) NULL, fixed binary(4) NULL, notes nvarchar(max) NULL, ansi varchar(max) NULL, blob varbinary(max) NULL)',
    { bulk: 'items', columns: clientTypes, rows: [
      [1, '9007199254740991', -32768, 255, true, '12345678901234.5678', 123456.7891, 1.5, 0.25, 'ab', 'plain', 'žluťoučký 🦆', 'Ωx', date('2024-02-29T00:00:00.000Z'), date('2024-01-02T03:04:05.123Z'), date('2024-01-02T03:04:05.123Z'), date('2024-01-02T03:04:00.000Z'), date('2024-01-02T03:04:05.123Z'), date('1970-01-01T13:14:15.678Z'), '6F9619FF-8B86-D011-B42D-00C04FC964FF', hex('0102ff'), hex('cafebabe'), 'ň'.repeat(5000), 'a'.repeat(9000), hex('ab'.repeat(9000))],
      [2, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null],
      [3, '-9223372036854775808', 0, 0, false, '-0.0001', -12.5, -1e300, -3.5, '', '', '', '', date('0001-01-01T00:00:00.000Z'), date('1753-01-01T00:00:00.000Z'), date('2099-12-31T23:59:59.999Z'), date('1900-01-01T00:00:00.000Z'), date('2024-06-30T22:00:00.000Z'), date('1970-01-01T00:00:00.000Z'), '00000000-0000-0000-0000-000000000000', hex(''), hex('00000000'), '', '', hex('')],
    ] },
    'SELECT @@ROWCOUNT AS loaded',
    'SELECT id, big, small, tiny, flag, amount, price, ratio, single, code, name, uname, ucode, born, stamp, at2, small_at, moment, clock, guid, bin, fixed, len(notes) AS notes, len(ansi) AS ansi, datalength(blob) AS blob FROM items ORDER BY id',
  ]],
  ['mssql-table', [
    "if object_id('[dbo].[people]') is null create table [dbo].[people] ([id] int not null, [name] nvarchar (50) null, [score] decimal (10, 2) null, [joined] datetime null, [active] bit null, [photo] varbinary (max) null)",
    { bulk: '[dbo].[people]', columns: [['id', 'Int', { nullable: false }], ['name', 'NVarChar', { length: 50, nullable: true }], ['score', 'Decimal', { precision: 10, scale: 2, nullable: true }], ['joined', 'DateTime', { nullable: true }], ['active', 'Bit', { nullable: true }], ['photo', 'VarBinary', { length: max, nullable: true }]], rows: [
      [1, 'Ada', 99.5, date('2020-05-06T07:08:09.000Z'), true, hex('89504e47')],
      [2, 'Žofie', null, null, false, null],
      [3, null, -1.25, date('1999-12-31T23:59:59.997Z'), null, hex('')],
    ] },
    'SELECT * FROM [dbo].[people] ORDER BY id',
    "if object_id('tempdb..[#staging]') is null create table [#staging] ([k] int not null, [v] nvarchar (20) null)",
    { bulk: '[#staging]', columns: [['k', 'Int', { nullable: false }], ['v', 'NVarChar', { length: 20, nullable: true }]], rows: [[1, 'one'], [2, null]] },
    'SELECT k, v FROM #staging ORDER BY k',
  ]],
  ['column-metadata', [
    'CREATE TABLE n (a int NULL, b int NOT NULL, s varchar(5) NULL, m nvarchar(max) NULL)',
    // The NULLABLE flag must equal the column's nullability (4816).
    { bulk: 'n', columns: [['b', 'Int', { nullable: true }]], rows: [[1]] },
    { bulk: 'n', columns: [['a', 'Int', { nullable: false }], ['b', 'Int', { nullable: false }]], rows: [[1, 1]] },
    { bulk: 'n', columns: [['b', 'Int', { nullable: false }], ['s', 'VarChar', { length: 5, nullable: false }]], rows: [[2, 'x']] },
    // A MAX column needs a MAX wire type (4816).
    { bulk: 'n', columns: [['b', 'Int', { nullable: false }], ['m', 'NVarChar', { length: 10 }]], rows: [[3, 'x']] },
    { bulk: 'n', columns: [['b', 'Int', { nullable: false }], ['m', 'NVarChar', { length: max }]], rows: [[4, 'max']] },
    // The wire type must be the declared type (4816); another declared type
    // converts like INSERT.
    { bulk: 'n', insertSql: 'insert bulk n ([b] int, [s] varchar(3))', columns: [['b', 'Int', { nullable: false }], ['s', 'Int', { nullable: true }]], rows: [[5, 12345]] },
    { bulk: 'n', columns: [['b', 'BigInt', { nullable: false }], ['s', 'Int', { nullable: true }]], rows: [[6, 12]] },
    // Different column counts in the statement and the metadata (4804).
    { bulk: 'n', insertSql: 'insert bulk n ([b] int)', columns: [['b', 'Int', { nullable: false }], ['a', 'Int']], rows: [[7, 7]] },
    { bulk: 'n', insertSql: 'insert bulk n ([b] int, [a] int)', columns: [['b', 'Int', { nullable: false }]], rows: [[8]] },
    // Columns bind by position in the statement, not by metadata name.
    { bulk: 'n', insertSql: 'insert bulk n ([a] int, [b] int)', columns: [['x', 'Int'], ['y', 'Int', { nullable: false }]], rows: [[9, 10]] },
    'SELECT a, b, s, m FROM n ORDER BY b',
    // tedious sends a binary(n) value shorter than n with length n, so the
    // row does not match its metadata.
    'CREATE TABLE fixed (b binary(4) NULL)',
    { bulk: 'fixed', columns: [['b', 'Binary', { length: 4 }]], rows: [[hex('cafe')]] },
    { bulk: 'fixed', columns: [['b', 'Binary', { length: 4 }]], rows: [[hex('cafebabe')]] },
    'SELECT b FROM fixed',
  ]],
  ['statement-errors', [
    'CREATE TABLE t (id int NOT NULL, v AS id * 2)',
    { bulk: 'nosuch', columns: [['id', 'Int', { nullable: false }]], rows: [[1]] },
    { bulk: 't', columns: [['nosuch', 'Int']], rows: [[1]] },
    { bulk: 't', columns: [['id', 'Int', { nullable: false }], ['ID', 'Int', { nullable: false }]], rows: [[1, 1]] },
    { bulk: 't', columns: [['v', 'Int']], rows: [[1]] },
    'insert bulk t ([id] notatype)',
    'insert bulk t ([id] int) WITH (KEEP_IDENTITY)',
    'insert bulk t ([id] int) WITH (FOO)',
    'insert bulk t',
    'SELECT 1; insert bulk t ([id] int)',
    'insert bulk t ([id] int); SELECT 1',
    // Another request where the bulk data should follow (4022).
    'insert bulk t ([id] int)',
    'SELECT 1 AS skipped',
    'SELECT 2 AS next',
    { bulk: 't', columns: [['id', 'Int', { nullable: false }]], options: {}, optionsSql: ' with (keep_nulls, tablock, ROWS_PER_BATCH = 10, KILOBYTES_PER_BATCH = 5, ORDER (id ASC), CHECK_CONSTRAINTS, FIRE_TRIGGERS)', rows: [[1]] },
    'SELECT id, v FROM t',
  ]],
  ['zero-rows', [
    'CREATE TABLE t (id int NULL)',
    // tedious sends only DONE for zero rows (4804).
    { bulk: 't', columns: [['id', 'Int']], rows: [] },
    // COLMETADATA, no rows, DONE.
    { bulk: 't', columns: [['id', 'Int']], rows: [], metadataOnly: true },
    'SELECT count(*) FROM t',
  ]],
  ['identity', [
    'CREATE TABLE items (id int IDENTITY(10,5) NOT NULL CONSTRAINT pk_items PRIMARY KEY, v varchar(10) NULL)',
    { bulk: 'items', columns: [['v', 'VarChar', { length: 10 }]], rows: [['a'], ['b']] },
    "SELECT IDENT_CURRENT('items') AS current_identity, @@IDENTITY AS last_identity",
    // Listing the identity column keeps its values (clients' KEEP_IDENTITY).
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['v', 'VarChar', { length: 10 }]], rows: [[100, 'c'], [200, 'd']] },
    "SELECT IDENT_CURRENT('items') AS current_identity",
    { bulk: 'items', columns: [['v', 'VarChar', { length: 10 }]], rows: [['e']] },
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['v', 'VarChar', { length: 10 }]], rows: [[100, 'dup']] },
    'SELECT id, v FROM items ORDER BY id',
    "SELECT IDENT_CURRENT('items') AS current_identity",
  ]],
  ['keep-nulls', [
    "CREATE TABLE items (id int NOT NULL, v varchar(10) NULL CONSTRAINT df_v DEFAULT 'dflt', n int NULL CONSTRAINT df_n DEFAULT 42, w int NULL)",
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['v', 'VarChar', { length: 10 }], ['n', 'Int'], ['w', 'Int']], rows: [[1, null, null, null], [2, 'x', 5, 6], [3, null, 7, null]] },
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['v', 'VarChar', { length: 10 }], ['n', 'Int'], ['w', 'Int']], rows: [[4, null, null, null]], options: { keepNulls: true } },
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }]], rows: [[5]] },
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }]], rows: [[6]], options: { keepNulls: true } },
    'SELECT id, v, n, w FROM items ORDER BY id',
  ]],
  ['check-constraints', [
    'CREATE TABLE items (id int NOT NULL, v int NULL CONSTRAINT ck_items_v CHECK (v > 0))',
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['v', 'Int']], rows: [[1, -1], [2, 5]] },
    'SELECT name, is_not_trusted, is_disabled FROM sys.check_constraints',
    'DELETE items WHERE v < 0',
    'ALTER TABLE items WITH CHECK CHECK CONSTRAINT ALL',
    'SELECT name, is_not_trusted, is_disabled FROM sys.check_constraints',
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['v', 'Int']], rows: [[3, 7], [4, -4]], options: { checkConstraints: true } },
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['v', 'Int']], rows: [[5, 7]], options: { checkConstraints: true } },
    'SELECT name, is_not_trusted, is_disabled FROM sys.check_constraints',
    'SELECT id, v FROM items ORDER BY id',
    'CREATE TABLE parent (id int NOT NULL CONSTRAINT pk_parent PRIMARY KEY)',
    'INSERT parent VALUES (1)',
    'CREATE TABLE child (id int NOT NULL, p int NULL CONSTRAINT fk_child_parent REFERENCES parent(id))',
    { bulk: 'child', columns: [['id', 'Int', { nullable: false }], ['p', 'Int']], rows: [[1, 1], [2, 99]] },
    'SELECT name, is_not_trusted, is_disabled FROM sys.foreign_keys',
    'DELETE child WHERE p = 99',
    'ALTER TABLE child WITH CHECK CHECK CONSTRAINT ALL',
    { bulk: 'child', columns: [['id', 'Int', { nullable: false }], ['p', 'Int']], rows: [[3, 1], [4, 98]], options: { checkConstraints: true } },
    'SELECT name, is_not_trusted, is_disabled FROM sys.foreign_keys',
    'SELECT id, p FROM child ORDER BY id',
  ]],
  ['triggers', [
    'CREATE TABLE items (id int NOT NULL)',
    'CREATE TABLE audit (n int NULL, note varchar(20) NULL)',
    "CREATE TRIGGER tr_items ON items AFTER INSERT AS INSERT audit SELECT count(*), 'fired' FROM inserted",
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }]], rows: [[1], [2]] },
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }]], rows: [[3], [4], [5]], options: { fireTriggers: true } },
    'SELECT n, note FROM audit',
    'SELECT count(*) FROM items',
  ]],
  ['statement-failures', [
    'CREATE TABLE items (id int NOT NULL CONSTRAINT pk_items PRIMARY KEY, s varchar(5) NULL, d datetime NULL, name varchar(10) NOT NULL CONSTRAINT df_items_name DEFAULT \'x\')',
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }]], rows: [[1], [2], [1]] },
    'SELECT @@ERROR AS error, @@ROWCOUNT AS loaded',
    'SELECT count(*) FROM items',
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['s', 'VarChar', { length: 30 }]], rows: [[1, 'abc'], [2, 'abcdefgh']] },
    { bulk: 'items', columns: [['id', 'VarChar', { length: 10, nullable: false }]], rows: [['zzz']] },
    'SELECT @@ERROR AS error',
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['d', 'VarChar', { length: 10 }]], rows: [[3, 'zzz']] },
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['name', 'VarChar', { length: 10, nullable: false }]], rows: [[4, null]], options: { keepNulls: true } },
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['name', 'VarChar', { length: 10, nullable: false }]], rows: [[5, null]] },
    'CREATE TABLE req (a int NULL, b int NOT NULL)',
    { bulk: 'req', columns: [['a', 'Int']], rows: [[1]] },
    'SELECT id, s, d, name FROM items ORDER BY id',
  ]],
  ['transactions', [
    'CREATE TABLE items (id int NOT NULL CONSTRAINT pk_items PRIMARY KEY)',
    'BEGIN TRAN',
    'INSERT items VALUES (50)',
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }]], rows: [[1], [1]] },
    'SELECT @@TRANCOUNT AS trancount, XACT_STATE() AS state',
    'SELECT count(*) FROM items',
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }]], rows: [[2], [3]] },
    'SELECT count(*) FROM items',
    'ROLLBACK',
    'SELECT count(*) FROM items',
    'SET XACT_ABORT ON',
    'BEGIN TRAN',
    'INSERT items VALUES (60)',
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }]], rows: [[9], [9]] },
    'SELECT @@TRANCOUNT AS trancount, XACT_STATE() AS state',
    'SELECT count(*) FROM items',
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }]], rows: [[10], [10]] },
    'SELECT count(*) FROM items',
    'SET XACT_ABORT OFF',
  ]],
  ['nocount', [
    'CREATE TABLE items (id int NULL)',
    'SET NOCOUNT ON',
    { bulk: 'items', columns: [['id', 'Int']], rows: [[1], [2]] },
    'SELECT @@ROWCOUNT AS loaded',
    'SET NOCOUNT OFF',
    { bulk: 'items', columns: [['id', 'Int']], rows: [[3]] },
    'SELECT @@ROWCOUNT AS loaded',
    'SELECT count(*) FROM items',
  ]],
  ['names', [
    'CREATE SCHEMA s2',
    'CREATE TABLE s2.items (x int NULL)',
    { bulk: '[s2].[items]', columns: [['x', 'Int']], rows: [[1]] },
    { bulk: '{db}.s2.items', columns: [['x', 'Int']], rows: [[2]] },
    { bulk: 'S2 . ITEMS', columns: [['X', 'Int']], rows: [[3]] },
    'SELECT x FROM s2.items ORDER BY x',
  ]],
  ['large', [
    'CREATE TABLE items (id int NOT NULL CONSTRAINT pk_items PRIMARY KEY, name nvarchar(40) NULL, n int NULL CONSTRAINT df_items_n DEFAULT 7)',
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['name', 'NVarChar', { length: 40 }], ['n', 'Int']], rows: generated(2500, i => [i, i % 5 ? `name ${i} ž` : null, i % 3 ? i : null]) },
    'SELECT count(*), sum(CAST(id AS bigint)), sum(CAST(n AS bigint)), count(name), min(name), max(name) FROM items',
    'SELECT count(*) FROM items WHERE n = 7',
    { bulk: 'items', columns: [['id', 'Int', { nullable: false }], ['name', 'NVarChar', { length: 40 }], ['n', 'Int']], rows: generated(1500, i => [i === 1499 ? 0 : 10000 + i, null, i]) },
    'SELECT count(*) FROM items',
  ]],
]

/** The rows a bulk step sends. */
export function stepRows(step) {
  const rows = Array.isArray(step.rows) ? step.rows : Array.from({ length: step.rows.count }, (_, i) => step.rows.row(i))
  return rows.map(row => row.map(value => {
    if (value && typeof value === 'object' && 'date' in value) return new Date(value.date)
    if (value && typeof value === 'object' && 'hex' in value) return Buffer.from(value.hex, 'hex')
    return value
  }))
}

/** A JSON description of a step, as the reference records it. */
export function describe(step) {
  if (typeof step === 'string') return step
  return { ...step, rows: Array.isArray(step.rows) ? `${step.rows.length} rows` : `${step.rows.count} generated rows` }
}

// The DONE tokens of the current request, recorded through tedious's debug
// hook (as scripts/capture-gaps-keys.mjs does).
function recordDone(connection) {
  const tokens = []
  const original = connection.debug.token.bind(connection.debug)
  connection.debug.token = token => {
    if (token.name.startsWith('DONE')) tokens.push({ name: token.name, more: token.more, sqlError: token.sqlError, attention: token.attention, serverError: token.serverError, rowCount: token.rowCount ?? null, curCmd: token.curCmd })
    original(token)
  }
  return () => { connection.debug.token = original; return tokens }
}

/** Run one bulk step through tedious and return its diagnostics and DONE tokens. */
export function bulkLoad(connection, step, database) {
  return new Promise(resolvePromise => {
    const result = { errors: [], info: [] }
    const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, message: e.message })
    const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, message: e.message })
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    const stop = recordDone(connection)
    const table = step.bulk.replaceAll('{db}', database)
    const load = connection.newBulkLoad(table, step.options ?? {}, (error, rowCount) => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.rowCount = rowCount ?? null
      result.error = error ? (error.code ?? 'error') : null
      result.completion = stop()
      resolvePromise(result)
    })
    for (const [name, type, options = {}] of step.columns) {
      const column = { nullable: true, ...options }
      if (column.length === max) column.length = Infinity
      load.addColumn(name, TYPES[type], column)
    }
    if (step.optionsSql !== undefined) load.getOptionsSql = () => step.optionsSql
    if (step.insertSql !== undefined) load.getBulkInsertSql = () => step.insertSql
    if (step.metadataOnly) {
      const transform = load.rowToPacketTransform
      transform._flush = function (callback) {
        this.push(load.getColMetaData())
        this.push(load.createDoneToken())
        process.nextTick(callback)
      }
    }
    connection.execBulkLoad(load, stepRows(step))
  })
}

/** Run one step: diagnostics, rows and completion. */
export async function runStep(connection, step, database) {
  if (typeof step !== 'string') return canonical(await bulkLoad(connection, step, database))
  const stop = recordDone(connection)
  const result = canonical(await capture(connection, step))
  result.completion = stop()
  return result
}

export function redact(value, database) {
  return JSON.parse(JSON.stringify(value)
    .replaceAll(database, '<database>')
    .replace(/(Truncated value: ')(?:[^'\\]|\\.)*('\.)/g, '$1<value>$2'))
}

// Importing this module (tests/compat/bulk.test.mjs does) only reads the cases.
if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) await captureReference()

async function captureReference() {
  const assert = (await import('node:assert/strict')).default
  const { withReferenceContainer } = await import('./lib/reference-container.mjs')
  const { isolatedReference, assertSameCapture } = await import('./lib/reference.mjs')
  const owner = `gaps-bulk-v1:${process.pid}`
  const exec = promisify(execFile)
  async function labeledDocker(args, env) {
    const actual = args[0] === 'run' ? ['run', '--label', `msduck.task=${owner}`, ...args.slice(1)] : args
    return (await exec('docker', actual, { env: { ...process.env, ...env }, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()
  }
  const output = resolve(process.argv[2] ?? 'artifacts/compatibility/gaps-bulk-reference')
  await mkdir(output, { recursive: true })
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      const results = []
      for (const [id, steps] of cases.filter(([id]) => !process.env.CASES || process.env.CASES.split(',').includes(id))) {
        results.push(await isolatedReference(config, async connection => {
          const database = (await capture(connection, 'SELECT DB_NAME()')).sets[0].rows[0][0]
          const recorded = []
          for (const step of steps) recorded.push(redact({ step: describe(step), result: await runStep(connection, step, database) }, database))
          return { id, steps: recorded }
        }))
      }
      runs.push(results)
    }
    await writeFile(resolve(output, 'runs.json'), JSON.stringify(runs, null, 2) + '\n')
    assertSameCapture(runs[0], runs[1], 'Fresh bulk captures differ')
    const version = await isolatedReference(config, async connection => canonical(await capture(connection, 'SELECT @@VERSION AS version')).sets[0].rows[0][0])
    const actual = { image: container.image, version, identicalFreshCaptures: 2, cases: runs[0] }
    await writeFile(resolve(output, 'gaps-bulk.json'), JSON.stringify(actual) + '\n')
    if (!process.env.CASES) assert.ok(actual.cases.length === cases.length)
    console.log(`Captured ${cases.length} bulk cases twice identically in ${output}`)
  }, { docker: labeledDocker })
}
