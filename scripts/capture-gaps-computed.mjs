// First-party evidence for docs/gaps-computed.md: computed columns over
// Unicode and JSON expressions, and column DEFAULTs that read session state.
// Each case runs its statements in order in a fresh database, through a
// connection that reports a fixed workstation and application name. Rows,
// diagnostics and completions are kept raw except the generated database
// name and the login name, which differ between runs and hosts.
//
// node scripts/capture-gaps-computed.mjs [output-directory]
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

/** LOGIN7 names the capture and the client tests send. */
export const client = {workstationId: 'computed-host', appName: 'computed-app'}

const columns = "SELECT c.name, c.is_computed, c.is_nullable, TYPE_NAME(c.user_type_id) AS type, c.max_length, COLUMNPROPERTY(c.object_id, c.name, 'IsComputed') AS property FROM sys.columns c WHERE c.object_id = OBJECT_ID('items') ORDER BY c.column_id"

export const cases = [
  ['json-persisted', [
    "CREATE TABLE items (id int NOT NULL, body nvarchar(max) NOT NULL, value AS (CONVERT(nvarchar(200), JSON_VALUE(body, N'$.value'))) PERSISTED)",
    `INSERT items (id, body) VALUES (1, N'{"value":"abc"}'), (2, N'{"value":"ü\u{1F986}"}'), (3, N'{"other":1}'), (4, N'{"value":12.5}')`,
    'SELECT id, value FROM items ORDER BY id',
    columns,
    'CREATE INDEX ix_items_value ON items(value)',
    "SELECT id FROM items WHERE value = N'abc'",
    `UPDATE items SET body = N'{"value":"xyz"}' WHERE id = 1`,
    'SELECT id, value FROM items WHERE id = 1',
    "INSERT items (id, body) VALUES (5, N'not json')",
    'SELECT count(*) FROM items',
    "INSERT items (id, body, value) VALUES (6, N'{}', N'x')",
    "UPDATE items SET value = N'x'",
  ]],
  ['unicode-virtual', [
    'CREATE TABLE items (id int NOT NULL, first nvarchar(50) NULL, last nvarchar(50) NULL, body nvarchar(max) NULL, code nchar(4) NULL, full_name AS (first + N\' \' + last), upper_first AS UPPER(first), body_length AS LEN(body), id_text AS CONVERT(nvarchar(10), id), initials AS LEFT(first, 1) + LEFT(last, 1), json_name AS JSON_VALUE(body, N\'$.name\'), code_trim AS RTRIM(code))',
    `INSERT items (id, first, last, body, code) VALUES (1, N'Ann', N'Lee', N'{"name":"ž\u{1F986}"}', N'ab'), (2, N'böb', NULL, NULL, NULL)`,
    'SELECT id, full_name, upper_first, body_length, id_text, initials, json_name, code_trim FROM items ORDER BY id',
    columns,
    'CREATE INDEX ix_items_upper ON items(upper_first)',
    "SELECT id FROM items WHERE upper_first = N'ANN'",
    'CREATE INDEX ix_items_json ON items(json_name)',
  ]],
  ['unicode-persisted-max', [
    'CREATE TABLE items (id int NOT NULL, body nvarchar(max) NULL, copy AS (body + N\'!\') PERSISTED, head AS CONVERT(nvarchar(20), LEFT(body, 3)) PERSISTED)',
    "INSERT items (id, body) VALUES (1, N'hello'), (2, NULL)",
    'SELECT id, copy, head FROM items ORDER BY id',
    columns,
    'CREATE INDEX ix_items_copy ON items(copy)',
    'CREATE INDEX ix_items_head ON items(head)',
    "SELECT id FROM items WHERE head = N'hel'",
  ]],
  ['carrier-types', [
    "CREATE TABLE items (id int NOT NULL, stamp datetime2(3) NULL, amount money NULL, bin varbinary(8) NULL, code nchar(4) NULL, day AS CONVERT(date, stamp), label AS CONVERT(nvarchar(30), amount), later AS DATEADD(day, 1, stamp), doubled AS amount * 2, padded AS code + N'|', blen AS DATALENGTH(bin))",
    "INSERT items (id, stamp, amount, bin, code) VALUES (1, '2024-01-02 03:04:05.678', 12.5, 0x0A0B, N'ab'), (2, NULL, NULL, NULL, NULL)",
    'SELECT id, day, label, later, doubled, padded, blen FROM items ORDER BY id',
    columns,
  ]],
  ['unicode-errors', [
    "CREATE TABLE items (a nvarchar(10) NULL, b AS UPPER(a), c AS b + N'x')",
    'CREATE TABLE items (a nvarchar(10) NULL, b AS UPPER(missing))',
    'CREATE TABLE items (a nvarchar(10) NULL, b AS a + CONVERT(nvarchar(36), NEWID()) PERSISTED)',
    "CREATE TABLE items (id int NOT NULL, body nvarchar(max) NULL, value AS JSON_VALUE(body, N'$.v') PERSISTED)",
    "INSERT items (id, body) VALUES (1, N'{\"v\":\"x\"}')",
    'INSERT items (id, body, value) VALUES (2, NULL, NULL)',
    "INSERT items VALUES (3, N'{}')",
    'SELECT id, value FROM items ORDER BY id',
  ]],
  ['session-defaults', [
    "CREATE TABLE items (id int NOT NULL, value nvarchar(100) NULL DEFAULT (CONVERT(nvarchar(100), SESSION_CONTEXT(N'foo'))), n int NULL DEFAULT (CONVERT(int, SESSION_CONTEXT(N'n'))), who nvarchar(128) NULL DEFAULT (SUSER_SNAME()), host nvarchar(128) NULL DEFAULT (HOST_NAME()), app nvarchar(128) NULL DEFAULT (APP_NAME()), short_app nvarchar(5) NULL DEFAULT (CONVERT(nvarchar(5), APP_NAME())))",
    'INSERT items (id) VALUES (1)',
    "EXEC sp_set_session_context N'foo', N'bar'",
    "EXEC sp_set_session_context N'n', 42",
    'INSERT items (id) VALUES (2)',
    "EXEC sp_set_session_context N'foo', N'ü\u{1F986}'",
    "INSERT items (id, value) VALUES (3, N'explicit')",
    'INSERT items (id) SELECT 4',
    'INSERT items (id, value) VALUES (5, DEFAULT)',
    'SELECT id, value, n, who, host, app, short_app FROM items ORDER BY id',
    'SELECT SUSER_SNAME() AS login, HOST_NAME() AS host, APP_NAME() AS app',
    "SELECT name, is_nullable, TYPE_NAME(user_type_id) AS type, max_length FROM sys.columns WHERE object_id = OBJECT_ID('items') ORDER BY column_id",
  ]],
  ['session-default-conversions', [
    "CREATE TABLE items (id int NOT NULL, label varchar(20) NULL DEFAULT (CONVERT(varchar(20), SESSION_CONTEXT(N'k'))), big bigint NULL DEFAULT (CAST(SESSION_CONTEXT(N'k') AS bigint)), flag bit NULL DEFAULT (CONVERT(bit, SESSION_CONTEXT(N'b'))), login_upper nvarchar(128) NULL DEFAULT (UPPER(SYSTEM_USER)), original nvarchar(128) NULL DEFAULT (ORIGINAL_LOGIN()))",
    "EXEC sp_set_session_context N'k', 7",
    "EXEC sp_set_session_context N'b', 1",
    'INSERT items (id) VALUES (1)',
    "EXEC sp_set_session_context N'k', N'12'",
    'INSERT items (id) VALUES (2)',
    "EXEC sp_set_session_context N'k', N'abc'",
    'INSERT items (id) VALUES (3)',
    'SELECT id, label, big, flag, login_upper, original FROM items ORDER BY id',
  ]],
  ['session-default-variant', [
    "CREATE TABLE items (id int NOT NULL, value nvarchar(100) NULL DEFAULT (SESSION_CONTEXT(N'foo')))",
    "EXEC sp_set_session_context N'foo', N'bar'",
    'INSERT items (id) VALUES (1)',
    'SELECT id, value FROM items',
    "CREATE TABLE variants (id int NOT NULL, v sql_variant NULL DEFAULT (SESSION_CONTEXT(N'foo')))",
    'INSERT variants (id) VALUES (1)',
    "SELECT id, CONVERT(nvarchar(20), v), SQL_VARIANT_PROPERTY(v, 'BaseType') FROM variants",
  ]],
  ['session-default-values', [
    "CREATE TABLE logs (id int IDENTITY(1, 1) NOT NULL, who nvarchar(128) NULL DEFAULT (SUSER_SNAME()), tenant nvarchar(50) NULL DEFAULT (CONVERT(nvarchar(50), SESSION_CONTEXT(N'Tenant'))), note varchar(10) NULL)",
    'INSERT logs DEFAULT VALUES',
    "EXEC sp_set_session_context @key = N'tenant', @value = N'acme', @read_only = 0",
    "INSERT logs (note) VALUES ('a'), ('b')",
    "INSERT logs (note) SELECT 'c' UNION ALL SELECT 'd'",
    'SELECT id, who, tenant, note FROM logs ORDER BY id',
  ]],
  ['session-default-alter', [
    'CREATE TABLE items (id int NOT NULL)',
    'INSERT items VALUES (1)',
    "EXEC sp_set_session_context N'foo', N'bar'",
    "ALTER TABLE items ADD value nvarchar(100) NULL DEFAULT (CONVERT(nvarchar(100), SESSION_CONTEXT(N'foo'))) WITH VALUES",
    "ALTER TABLE items ADD host nvarchar(128) NULL DEFAULT (HOST_NAME())",
    'INSERT items (id) VALUES (2)',
    'SELECT id, value, host FROM items ORDER BY id',
  ]],
  ['session-default-case', [
    "CREATE TABLE items (id int NOT NULL, value nvarchar(100) NULL DEFAULT CASE WHEN SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'BaseType') = N'nvarchar' THEN CONVERT(nvarchar(100), SESSION_CONTEXT(N'foo')) ELSE NULL END)",
    'INSERT items (id) VALUES (1)',
    "EXEC sp_set_session_context N'foo', N'bar'",
    'INSERT items (id) VALUES (2)',
    'INSERT items (id) SELECT 3 UNION ALL SELECT 4',
    "EXEC sp_set_session_context N'foo', 42",
    'INSERT items (id) VALUES (5)',
    "EXEC sp_set_session_context N'foo', NULL",
    'INSERT items (id) VALUES (6)',
    "EXEC sp_set_session_context N'foo', N'baz'",
    'MERGE items AS t USING (SELECT 7 AS id) AS s ON t.id = s.id WHEN NOT MATCHED THEN INSERT (id) VALUES (s.id);',
    'INSERT items (id, value) VALUES (8, DEFAULT)',
    'SELECT id, value FROM items ORDER BY id',
  ]],
  ['session-default-properties', [
    "CREATE TABLE props (id int NOT NULL, base nvarchar(128) NULL DEFAULT (CONVERT(nvarchar(128), SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType'))), max_length int NULL DEFAULT (CONVERT(int, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'MaxLength'))), prec int NULL DEFAULT (CONVERT(int, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'Precision'))), scale int NULL DEFAULT (CONVERT(int, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'Scale'))), total int NULL DEFAULT (CONVERT(int, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'TotalBytes'))), coll nvarchar(128) NULL DEFAULT (CONVERT(nvarchar(128), SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'Collation'))), is_int bit NULL DEFAULT (CASE WHEN SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') = 'int' THEN 1 ELSE 0 END))",
    'INSERT props (id) VALUES (0)',
    "EXEC sp_set_session_context N'k', N'bar'",
    'INSERT props (id) VALUES (1)',
    "DECLARE @v nvarchar(50) = N'wide'; EXEC sp_set_session_context N'k', @v",
    'INSERT props (id) VALUES (2)',
    "EXEC sp_set_session_context N'k', 7",
    'INSERT props (id) VALUES (3)',
    "DECLARE @v bit = 1; EXEC sp_set_session_context N'k', @v",
    'INSERT props (id) VALUES (4)',
    "DECLARE @v tinyint = 200; EXEC sp_set_session_context N'k', @v",
    'INSERT props (id) VALUES (5)',
    "DECLARE @v smallint = -3; EXEC sp_set_session_context N'k', @v",
    'INSERT props (id) VALUES (6)',
    "DECLARE @v bigint = 5000000000; EXEC sp_set_session_context N'k', @v",
    'INSERT props (id) VALUES (7)',
    'SELECT id, base, max_length, prec, scale, total, coll, is_int FROM props ORDER BY id',
  ]],
  ['session-context-properties-select', [
    "SELECT SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') AS base, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'MaxLength') AS max_length",
    "EXEC sp_set_session_context N'k', N'bar'",
    "SELECT SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') AS base, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'MaxLength') AS max_length, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'Precision') AS prec, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'Scale') AS scale",
    "SELECT CASE WHEN SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') = N'nvarchar' THEN CONVERT(nvarchar(100), SESSION_CONTEXT(N'k')) ELSE NULL END AS value",
    "SELECT CONVERT(nvarchar(128), SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType')) AS base, CONVERT(int, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'MaxLength')) AS max_length",
    "EXEC sp_set_session_context N'k', 42",
    "SELECT SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') AS base, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'MaxLength') AS max_length, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'Precision') AS prec, SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'Scale') AS scale",
    "SELECT CASE WHEN SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType') = N'nvarchar' THEN CONVERT(nvarchar(100), SESSION_CONTEXT(N'k')) ELSE NULL END AS value",
  ]],
  ['session-default-positions', [
    "CREATE TABLE a (v nvarchar(10) NULL DEFAULT (ISNULL(SESSION_CONTEXT(N'foo'), N'x')))",
    "CREATE TABLE b (v nvarchar(10) NULL DEFAULT (COALESCE(SESSION_CONTEXT(N'foo'), N'x')))",
    "CREATE TABLE c (v nvarchar(10) NULL DEFAULT (CASE WHEN 1 = 1 THEN SESSION_CONTEXT(N'foo') END))",
    "CREATE TABLE d (v nvarchar(128) NULL DEFAULT (SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'BaseType')))",
    "CREATE TABLE e (id int IDENTITY(1, 1) NOT NULL, v nvarchar(10) NULL DEFAULT (ISNULL(CONVERT(nvarchar(10), SESSION_CONTEXT(N'foo')), N'none')), w nvarchar(10) NULL DEFAULT (COALESCE(CONVERT(nvarchar(10), SESSION_CONTEXT(N'foo')), N'none')), present int NULL DEFAULT (CASE WHEN SESSION_CONTEXT(N'foo') IS NULL THEN 0 ELSE 1 END), iif_value nvarchar(10) NULL DEFAULT (IIF(SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'foo'), 'BaseType') = N'int', N'number', N'other')))",
    'INSERT e DEFAULT VALUES',
    "EXEC sp_set_session_context N'foo', N'bar'",
    'INSERT e DEFAULT VALUES',
    "EXEC sp_set_session_context N'foo', 5",
    'INSERT e DEFAULT VALUES',
    'SELECT id, v, w, present, iif_value FROM e ORDER BY id',
  ]],
  ['session-variant-writes', [
    "EXEC sp_set_session_context N'k', N'bar'",
    'CREATE TABLE t (v nvarchar(128) NULL, n int NULL)',
    "INSERT t (v) VALUES (SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType'))",
    "INSERT t (n) VALUES (SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'MaxLength'))",
    "INSERT t (v) SELECT SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType')",
    "INSERT t (v) VALUES (N'x'); UPDATE t SET v = SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType')",
    "DECLARE @v nvarchar(128); SET @v = SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType'); SELECT @v AS v",
    'SELECT v, n FROM t',
    "CREATE TABLE a (v int NULL DEFAULT (ISNULL(CAST(NULL AS int), SESSION_CONTEXT(N'k'))))",
    "CREATE TABLE b (v nvarchar(10) NULL DEFAULT (ISNULL(N'x', SQL_VARIANT_PROPERTY(SESSION_CONTEXT(N'k'), 'BaseType'))))",
    "CREATE TABLE c (v nvarchar(10) NULL DEFAULT (ISNULL(CONVERT(nvarchar(10), SESSION_CONTEXT(N'k')), SESSION_CONTEXT(N'k'))))",
    "CREATE TABLE d (v int NULL DEFAULT (NULLIF(1, SESSION_CONTEXT(N'k'))))",
  ]],
]

// Importing this module (tests/compat/computed.test.mjs does) only reads the cases.
if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) await captureReference()

async function captureReference() {
  const owner = `gaps-computed-v1:${process.pid}`
  const exec = promisify(execFile)
  async function labeledDocker(args, env) {
    const actual = args[0] === 'run' ? ['run', '--label', `msduck.task=${owner}`, ...args.slice(1)] : args
    return (await exec('docker', actual, {env: {...process.env, ...env}, maxBuffer: 4 * 1024 * 1024})).stdout.trim()
  }
  const redact = value => JSON.parse(JSON.stringify(value).replace(/msduck_audit_[0-9a-f]{32}/g, 'msduck_audit_<database>'))

  const output = resolve(process.argv[2] ?? 'artifacts/compatibility/gaps-computed-reference')
  await mkdir(output, {recursive: true})
  await withReferenceContainer(async (config, container) => {
    const named = {...config, options: {...config.options, ...client}}
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      const results = []
      for (const [id, statements] of cases.filter(([id]) => !process.env.CASES || process.env.CASES.split(',').includes(id))) {
        results.push(await isolatedReference(named, async connection => {
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
    assertSameCapture(runs[0], runs[1], 'Fresh computed captures differ')
    const version = await isolatedReference(config, async connection => canonical(await capture(connection, 'SELECT @@VERSION AS version')).sets[0].rows[0][0])
    const actual = {image: container.image, version, identicalFreshCaptures: 2, client, cases: runs[0]}
    await writeFile(resolve(output, 'gaps-computed.json'), JSON.stringify(actual) + '\n')
    if (!process.env.CASES) assert.ok(actual.cases.length === cases.length)
    console.log(`Captured ${cases.length} computed cases twice identically in ${output}`)
  }, {docker: labeledDocker})
}
