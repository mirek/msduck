// First-party evidence for docs/gaps-unicode-predicates.md: comparisons,
// LIKE, ordering, concatenation and character conversions over NVARCHAR and
// NCHAR columns. Each case runs its statements in order in a fresh database
// with the server's default collation, SQL_Latin1_General_CP1_CI_AS, which
// msduck reports and follows (case-insensitive, accent-sensitive, trailing
// spaces ignored by comparisons). Rows, diagnostics and completions are kept
// raw except the generated database name.
//
// node scripts/capture-gaps-unicode-predicates.mjs [output-directory]
// MSSQL_REFERENCE_IMAGE selects the image (default: the pinned reference).
import {execFile} from 'node:child_process'
import {promisify} from 'node:util'
import {mkdir, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference, assertSameCapture, connect, command} from './lib/reference.mjs'
import {TYPES, Request} from 'tedious'
import {capture, canonical} from './lib/compatibility.mjs'

// Values chosen around the edges of comparison: trailing spaces,
// case, NUL below the pad space, U+0100 (whose little-endian bytes sort
// first), a surrogate pair against U+E000, an isolated surrogate, empty,
// NULL, LIKE metacharacters and a value longer than 4000 characters.
const table = `CREATE TABLE t (id int NOT NULL PRIMARY KEY, n nvarchar(20) NULL, c nchar(6) NULL, v varchar(20) NULL, m nvarchar(max) NULL);
INSERT t VALUES
  (1, N'x', N'x', 'x', N'x'),
  (2, N'x  ', N'x', 'x  ', N'x   '),
  (3, N'X', N'X', 'X', N'X'),
  (4, N'a', N'a', 'a', N'a'),
  (5, N'a' + NCHAR(0), N'a' + NCHAR(0), NULL, N'a' + NCHAR(0)),
  (6, NCHAR(256), NCHAR(256), NULL, NCHAR(256)),
  (7, N'\u{1F986}', N'\u{1F986}', NULL, N'\u{1F986}'),
  (8, NCHAR(57344), NCHAR(57344), NULL, NCHAR(57344)),
  (9, CAST(0x3DD8 AS nvarchar(1)), CAST(0x3DD8 AS nvarchar(1)), NULL, CAST(0x3DD8 AS nvarchar(1))),
  (10, N'', N'', '', N''),
  (11, NULL, NULL, NULL, NULL),
  (12, N'ab%_[c]', N'ab%_[', 'ab%_[c]', N'ab%_[c]'),
  (13, N'b', N'b', 'b', REPLICATE(CAST(N'b' AS nvarchar(max)), 5000))`

const ids = where => `SELECT id FROM t WHERE ${where} ORDER BY id`

const text = `CREATE TABLE s (id int NOT NULL PRIMARY KEY, n nvarchar(20) NULL, c nchar(4) NULL, v varchar(10) NULL, m nvarchar(max) NULL);
INSERT s VALUES (1, N'ab', N'ab', 'xy', N'mm'), (2, NULL, NULL, NULL, NULL), (3, N'\u{1F986}é', N'z', 'q', N'x')`

export const cases = [
  ['equality', [
    table,
    ids("n = N'x'"),
    ids("n = 'x'"),
    ids("n = N'x   '"),
    ids("N'x' = n"),
    ids("n <> N'x'"),
    ids('n = v'),
    ids('n = m'),
    ids('n = c'),
    ids("c = N'x'"),
    ids("c = 'a'"),
    ids("m = N'x'"),
    ids("m = REPLICATE(CAST(N'b' AS nvarchar(max)), 5000)"),
    ids("n = N'\u{1F986}'"),
    ids('n = CAST(0x3DD8 AS nvarchar(1))'),
    ids("n = N''"),
    ids("n = N'a' + NCHAR(0)"),
    ids('n = NULL'),
    ids('n IS NULL'),
    ids('n IS NOT NULL AND v IS NULL'),
  ]],
  ['ordering-comparisons', [
    table,
    ids("n < N'a'"),
    ids("n > N'a'"),
    ids("n <= N'a'"),
    ids("n >= NCHAR(256)"),
    ids("n > N'\u{1F986}'"),
    ids("n < NCHAR(57344)"),
    ids("n BETWEEN N'a' AND N'x'"),
    ids("n NOT BETWEEN N'a' AND N'x'"),
    ids("n BETWEEN 'X' AND 'b'"),
    ids("c > N'a'"),
    ids("m >= N'b'"),
    ids('n < v'),
  ]],
  ['membership', [
    table,
    ids("n IN (N'a', 'X', N'\u{1F986}')"),
    ids("n NOT IN (N'a', N'x')"),
    ids("n IN (N'x   ', NULL)"),
    ids("n NOT IN (N'x', NULL)"),
    ids('n IN (SELECT v FROM t WHERE v IS NOT NULL)'),
    ids('n NOT IN (SELECT v FROM t WHERE v IS NOT NULL)'),
    ids('n NOT IN (SELECT v FROM t)'),
    ids('n IN (SELECT n FROM t WHERE id = 7)'),
    ids('v IN (SELECT n FROM t WHERE id < 5)'),
    ids("c IN (N'x', N'a')"),
  ]],
  ['order-by', [
    table,
    'SELECT id FROM t ORDER BY n, id',
    'SELECT id FROM t ORDER BY n DESC, id',
    'SELECT id FROM t ORDER BY c, id',
    'SELECT id FROM t ORDER BY m DESC, id',
    'SELECT id, n AS name FROM t WHERE id < 5 ORDER BY name, id',
    'SELECT id, n FROM t WHERE id IN (1, 2, 4, 6) ORDER BY 2 DESC, 1',
    'SELECT TOP 3 id FROM t ORDER BY n DESC, id',
    'SELECT id FROM t ORDER BY n, id OFFSET 2 ROWS FETCH NEXT 3 ROWS ONLY',
    'SELECT x.id FROM (SELECT TOP 4 id, n FROM t ORDER BY n DESC, id) x ORDER BY x.id',
  ]],
  ['like', [
    table,
    ids("n LIKE N'x%'"),
    ids("n LIKE 'x'"),
    ids("n LIKE N'x  '"),
    ids("n NOT LIKE N'%x%'"),
    ids("c LIKE N'x'"),
    ids("c LIKE N'x%'"),
    ids("n LIKE N'[a-x]'"),
    ids("n LIKE N'[^a-x]%'"),
    ids("n LIKE N'_'"),
    ids("n LIKE N'__'"),
    ids("n LIKE N'ab!%!_![c]' ESCAPE '!'"),
    ids("n LIKE N'%[%]%'"),
    ids("n LIKE N'%[[]c]'"),
    ids("m LIKE N'b%'"),
    ids("m LIKE N'%' + NCHAR(0)"),
    ids('v LIKE n'),
    ids("n LIKE N'x' ESCAPE 'ab'"),
  ]],
  ['variables-and-parameters', [
    table,
    "DECLARE @p nvarchar(20) = N'x '; SELECT id FROM t WHERE n = @p ORDER BY id",
    "DECLARE @v varchar(20) = 'a'; SELECT id FROM t WHERE n >= @v AND n < N'b' ORDER BY id",
    "DECLARE @l nvarchar(20) = N'x%'; SELECT id FROM t WHERE n LIKE @l ORDER BY id",
    {sql: 'SELECT id FROM t WHERE n = @p ORDER BY id', parameters: [['p', 'NVarChar', 'X ', {length: 20}]]},
    {sql: 'SELECT id FROM t WHERE n IN (@a, @b) ORDER BY id', parameters: [['a', 'NVarChar', '\u{1F986}', {length: 10}], ['b', 'VarChar', 'a', {length: 10}]]},
    {sql: 'SELECT id FROM t WHERE n LIKE @p ORDER BY id', parameters: [['p', 'NVarChar', '\ud83d%', {length: 10}]]},
    {sql: 'SELECT id FROM t WHERE n > @p ORDER BY id', parameters: [['p', 'NVarChar', '\u{1F986}', {length: 10}]]},
    "IF EXISTS (SELECT 1 FROM t WHERE n = N'x ') SELECT 1 ELSE SELECT 0",
    "EXEC sp_executesql N'SELECT id FROM t WHERE n <= @p AND n LIKE @l ORDER BY id', N'@p nvarchar(20), @l varchar(10)', @p = N'x ', @l = 'x%'",
    "DECLARE @n nvarchar(20); SELECT @n = n FROM t WHERE id = 7; SELECT id FROM t WHERE n = @n ORDER BY id",
  ]],
  ['case-and-joins', [
    table,
    "SELECT id, CASE n WHEN N'x' THEN 1 WHEN N'\u{1F986}' THEN 2 ELSE 0 END FROM t ORDER BY id",
    "SELECT id, CASE WHEN n > N'a' THEN 1 ELSE 0 END, IIF(n = 'X', 1, 0) FROM t ORDER BY id",
    'SELECT a.id, b.id FROM t a JOIN t b ON a.n = b.n AND a.id < b.id ORDER BY 1, 2',
    'SELECT a.id, b.id FROM t a JOIN t b ON a.n = b.v AND a.id <> b.id ORDER BY 1, 2',
    'SELECT a.id, b.id FROM t a LEFT JOIN t b ON a.n = b.m AND b.id > a.id WHERE a.id < 5 ORDER BY 1, 2',
    'SELECT a.id FROM t a WHERE EXISTS (SELECT 1 FROM t b WHERE b.n = a.n AND b.id <> a.id) ORDER BY a.id',
    "SELECT COUNT(*) FROM t WHERE n = N'x' OR c = N'a'",
  ]],
  ['dml-predicates', [
    table,
    "UPDATE t SET v = 'u' WHERE n = N'x'",
    "SELECT id FROM t WHERE v = 'u' ORDER BY id",
    "DELETE FROM t WHERE n LIKE N'a%' OR m = NCHAR(256)",
    'SELECT id FROM t ORDER BY id',
    "UPDATE d SET v = s.v FROM t AS d JOIN (VALUES (N'X ', 'm'), (N'\u{1F986}', 'd')) AS s(n, v) ON d.n = s.n",
    "SELECT id, v FROM t WHERE v IN ('m', 'd') ORDER BY id",
    "MERGE t AS d USING (VALUES (N'x  ', 'g'), (N'\u{1F986}', 'h'), (N'new', 'i')) AS s(n, v) ON d.n = s.n WHEN MATCHED AND d.n LIKE N'x%' THEN UPDATE SET v = s.v WHEN NOT MATCHED THEN INSERT (id, n, v) VALUES (20, s.n, s.v);",
    "SELECT id, v FROM t WHERE v IN ('g', 'h', 'i') ORDER BY id",
    "UPDATE t SET n = n + N'!' WHERE c = N'x'",
    "SELECT id, n FROM t WHERE n LIKE N'%!' ORDER BY id",
  ]],
  ['concatenation-and-conversion', [
    text,
    `SELECT CONVERT(nvarchar(200), JSON_VALUE(N'{"value":"abc"}', N'$.value')) AS a, CAST(JSON_VALUE(N'{"value":"abc"}', N'$.value') AS nvarchar(2)) AS b`,
    'SELECT id, n + N\'!\' AS a, c + N\'|\' AS b, v + n AS c, N\'x\' + n AS d, n + c + v AS e, n + n AS f FROM s ORDER BY id',
    'SELECT id, CONVERT(nvarchar(5), n) AS a, CAST(n AS nvarchar(2)) AS b, CAST(n AS nvarchar(max)) AS c, CAST(c AS nvarchar(10)) AS d, CAST(n AS nchar(3)) AS e, TRY_CAST(n AS nvarchar(3)) AS f, CAST(m AS nvarchar(1)) AS g FROM s ORDER BY id',
    'SELECT id, CAST(n AS varchar(10)) AS a, CONVERT(varchar(3), c) AS b FROM s ORDER BY id',
    "SELECT id, CONCAT(n, N'-', c) AS a, LEN(n + N'  ') AS b, LEFT(n, 1) + N'!' AS c, UPPER(n) + N'!' AS d, CAST(n + N'!' AS nvarchar(3)) AS e FROM s ORDER BY id",
    "DECLARE @x nvarchar(20); SELECT @x = n FROM s WHERE id = 3; SELECT @x + N'!' AS a, LEN(@x) AS b, DATALENGTH(@x) AS c",
    "DECLARE @y nvarchar(20); SET @y = (SELECT n FROM s WHERE id = 1); SELECT @y + N'?' AS a",
    "SELECT id, ISNULL(n, N'-') AS a, COALESCE(n, v) AS b, COALESCE(n, N'none') AS c, IIF(id = 1, n, N'no') AS d, CASE WHEN id = 3 THEN n ELSE N'other' END AS e, NULLIF(n, N'ab') AS f FROM s ORDER BY id",
    "SELECT n FROM s WHERE id = 1 UNION ALL SELECT N'lit' UNION ALL SELECT v FROM s WHERE id = 1",
    "SELECT id, REPLACE(n, N'a', N'A') AS a FROM s ORDER BY id",
    "SELECT SUBSTRING(n, 2, 1) AS a, REVERSE(n) AS b, STRING_AGG(n, N',') AS c FROM s WHERE id = 1 GROUP BY n",
  ]],
]

/// Run one step: a SQL batch, or `{sql, parameters}` as an RPC request
/// (sp_executesql) with `[name, tedious type name, value, options]` parameters.
export function run(connection, step) {
  if (typeof step === 'string') return capture(connection, step)
  return new Promise(resolve => {
    const result = {sets: [], done: [], errors: [], info: [], returnStatus: null}
    const onError = error => result.errors.push({number: error.number, state: error.state, class: error.class, lineNumber: error.lineNumber, message: error.message})
    connection.on('errorMessage', onError)
    const request = new Request(step.sql, (error, rowCount) => {
      connection.off('errorMessage', onError)
      result.rowCount = rowCount
      if (error && !result.errors.length) result.errors.push({message: error.message, number: error.number ?? null})
      resolve(result)
    })
    request.on('columnMetadata', metadata => result.sets.push({columns: metadata.map(c => ({name: c.colName, type: c.type.name, length: c.dataLength ?? null})), rows: []}))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => c.value)))
    for (const [name, type, value, options] of step.parameters) request.addParameter(name, TYPES[type], value, options)
    connection.execSql(request)
  })
}

// Importing this module (the tedious tests do) only reads the cases.
if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) await captureReference()

async function captureReference() {
  const owner = `gaps-unicode-predicates-v1:${process.pid}`
  const exec = promisify(execFile)
  async function labeledDocker(args, env) {
    const actual = args[0] === 'run' ? ['run', '--label', `msduck.task=${owner}`, ...args.slice(1)] : args
    return (await exec('docker', actual, {env: {...process.env, ...env}, maxBuffer: 4 * 1024 * 1024})).stdout.trim()
  }
  const redact = value => JSON.parse(JSON.stringify(value).replace(/msduck_audit_[0-9a-f]{32}/g, 'msduck_audit_<database>'))
  // The server's default collation, as every msduck database reports.
  const defaults = {connect, command}

  const output = resolve(process.argv[2] ?? 'artifacts/compatibility/gaps-unicode-predicates-reference')
  await mkdir(output, {recursive: true})
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      const results = []
      for (const [id, statements] of cases) {
        results.push(await isolatedReference(config, async connection => {
          const steps = []
          for (const sql of statements) {
            const tokens = []
            const debugToken = connection.debug.token.bind(connection.debug)
            connection.debug.token = token => { if (token.name.startsWith('DONE')) tokens.push(JSON.parse(JSON.stringify(token))); debugToken(token) }
            const result = canonical(await run(connection, sql))
            connection.debug.token = debugToken
            steps.push(redact({sql, result, completion: tokens}))
          }
          return {id, steps}
        }, defaults))
      }
      runs.push(results)
    }
    await writeFile(resolve(output, 'runs.json'), JSON.stringify(runs, null, 2) + '\n')
    assertSameCapture(runs[0], runs[1], 'Fresh captures differ')
    const version = await isolatedReference(config, async connection => canonical(await capture(connection, 'SELECT @@VERSION AS version')).sets[0].rows[0][0])
    const actual = {image: container.image, version, collation: 'SQL_Latin1_General_CP1_CI_AS', identicalFreshCaptures: 2, cases: runs[0]}
    await writeFile(resolve(output, 'gaps-unicode-predicates.json'), JSON.stringify(actual) + '\n')
    console.log(`Captured ${cases.length} cases twice identically in ${output}`)
  }, {docker: labeledDocker})
}
