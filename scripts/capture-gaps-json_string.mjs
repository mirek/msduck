#!/usr/bin/env node
// SQL Server evidence for FOR JSON AUTO, JSON_MODIFY editing details,
// ordered STRING_AGG, STRING_SPLIT and HASHBYTES (issue #724). Earlier
// first-party fixtures already cover JSON_MODIFY, STRING_AGG, STRING_SPLIT
// and HASHBYTES in depth (reference/json-constructors.json,
// string-agg.json, string-split.json, hashbytes-checksum.json); this
// capture adds FOR JSON AUTO nesting, the workload repros and the
// whitespace rules of JSON_MODIFY inserts, deletes and appends.
//
// Usage:
//   capture-gaps-json_string.mjs [output]          capture two fresh containers
//   capture-gaps-json_string.mjs --write-fixture   also write reference/gaps-json_string.json
//   capture-gaps-json_string.mjs --check           validate the retained fixture
//   capture-gaps-json_string.mjs --compare PORT    diff a local msduck server with the fixture
//
// The image defaults to the pinned reference image; MSSQL_REFERENCE_IMAGE
// selects another (the retained fixture records the image it used).
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { isDeepStrictEqual } from 'node:util'
import { Connection, Request } from 'tedious'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { assertSameCapture, describeFirstDifference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/gaps-json_string.json', import.meta.url)
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const compareAt = args.indexOf('--compare')
const comparePort = compareAt >= 0 ? Number(args[compareAt + 1]) : null
const paths = args.filter((arg, i) => !arg.startsWith('--') && !(compareAt >= 0 && i === compareAt + 1))
const output = resolve(paths[0] ?? 'artifacts/compatibility/gaps-json_string-reference/capture.json')
const image = process.env.MSSQL_REFERENCE_IMAGE ?? referenceImage

function connect(config) {
  return new Promise((resolve, reject) => {
    const connection = new Connection(config)
    connection.on('error', () => {})
    connection.connect(error => error ? reject(error) : resolve(connection))
  })
}
const close = connection => new Promise(resolve => {
  if (connection.closed) return resolve()
  connection.once('end', resolve)
  connection.close()
})

// One batch: descriptors, rows and diagnostics.
function run(connection, sql) {
  return new Promise(resolve => {
    const result = { sets: [], messages: [] }
    const onError = m => result.messages.push({ number: m.number, state: m.state, class: m.class, message: m.message })
    const request = new Request(sql, error => {
      connection.off('errorMessage', onError)
      if (error && !result.messages.length) result.transport = error.message
      resolve(result)
    })
    connection.on('errorMessage', onError)
    request.on('columnMetadata', metadata => result.sets.push({
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, nullable: (c.flags & 1) === 1 })), rows: [],
    }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => Buffer.isBuffer(c.value) ? `0x${c.value.toString('hex')}` : c.value)))
    connection.execSqlBatch(request)
  })
}

const setup = [
  `CREATE TABLE dbo.a(id int PRIMARY KEY, name varchar(10)); CREATE TABLE dbo.b(id int, a_id int, x varchar(10), n int); CREATE TABLE dbo.c(id int, b_id int, y int); CREATE TABLE dbo.d(k int, v int); CREATE TABLE dbo.e(s varchar(10), t int); CREATE TABLE dbo.docs(id int, doc nvarchar(200), csv nvarchar(20), tag varchar(20))`,
  `INSERT dbo.a VALUES (1,'one'),(2,'two'),(3,NULL); INSERT dbo.b VALUES (10,1,'p',NULL),(11,1,'q',5),(12,2,'r',6); INSERT dbo.c VALUES (100,10,7),(101,10,8),(102,12,9); INSERT dbo.d VALUES (1,1),(1,1),(1,2); INSERT dbo.e VALUES ('a',1),('A',2),('a ',3); INSERT dbo.docs VALUES (1,N'{"a":1}',N'x|y',N'é'),(2,N'{"a":2,"list":[1]}',N'z','b'),(3,NULL,NULL,NULL)`,
]

const batches = [
  // Workload repros (generic identifiers).
  ['repro for json auto no table', `SELECT 1 AS value FOR JSON AUTO`],
  ['repro json_modify', `SELECT JSON_MODIFY(N'{"a":1}', N'$.a', 2) AS value`],
  ['repro ordered string_agg', `SELECT STRING_AGG(CAST(id AS varchar(10)), ',') WITHIN GROUP (ORDER BY id) AS value FROM dbo.a`],
  ['repro ordered string_agg desc', `SELECT STRING_AGG(CAST(id AS varchar(10)), ',') WITHIN GROUP (ORDER BY id DESC) AS value FROM dbo.a`],
  ['repro hashbytes md5 nvarchar', `SELECT HASHBYTES('MD5', N'foo') AS value`],
  ['repro string_split', `SELECT value FROM STRING_SPLIT('a,b', ',')`],
  // FOR JSON AUTO.
  ['auto single table', `SELECT id, name FROM dbo.a ORDER BY id FOR JSON AUTO`],
  ['auto include null values', `SELECT id, name FROM dbo.a ORDER BY id FOR JSON AUTO, INCLUDE_NULL_VALUES`],
  ['auto root', `SELECT id, name FROM dbo.a ORDER BY id FOR JSON AUTO, ROOT('r')`],
  ['auto without array wrapper', `SELECT id, name FROM dbo.a WHERE id=1 FOR JSON AUTO, WITHOUT_ARRAY_WRAPPER`],
  ['auto without array wrapper rows', `SELECT a.id, b.x FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO, WITHOUT_ARRAY_WRAPPER`],
  ['auto join', `SELECT a.id, a.name, b.x, b.n FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY a.id, b.id FOR JSON AUTO`],
  ['auto left join', `SELECT a.id, a.name, b.x, b.n FROM dbo.a a LEFT JOIN dbo.b b ON b.a_id=a.id ORDER BY a.id, b.id FOR JSON AUTO`],
  ['auto left join include nulls', `SELECT a.id, a.name, b.x, b.n FROM dbo.a a LEFT JOIN dbo.b b ON b.a_id=a.id ORDER BY a.id, b.id FOR JSON AUTO, INCLUDE_NULL_VALUES`],
  ['auto three levels', `SELECT a.id, b.x, c.y FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id JOIN dbo.c c ON c.b_id=b.id ORDER BY a.id, b.id, c.id FOR JSON AUTO`],
  ['auto three levels left', `SELECT a.id, b.x, c.y FROM dbo.a a LEFT JOIN dbo.b b ON b.a_id=a.id LEFT JOIN dbo.c c ON c.b_id=b.id ORDER BY a.id, b.id, c.id FOR JSON AUTO`],
  ['auto reversed order', `SELECT a.id, b.x, c.y FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id JOIN dbo.c c ON c.b_id=b.id ORDER BY c.id DESC FOR JSON AUTO`],
  ['auto select order defines nesting', `SELECT b.x, a.id FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto later parent column', `SELECT a.id, b.x, a.name FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto expressions join deepest level', `SELECT a.id, b.x, 5 AS k, a.id+1 AS e FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto leading expression', `SELECT 5 AS k, a.id, b.x FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto expression of later table', `SELECT a.id+0 AS e, b.x FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto expressions not grouped', `SELECT a.id, b.id+0 AS e, b.x FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto aliased column', `SELECT a.id, b.x AS bx, (b.n) AS pn FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto unaliased tables', `SELECT dbo.a.id, dbo.b.x FROM dbo.a JOIN dbo.b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto unqualified columns', `SELECT name, x FROM dbo.a JOIN dbo.b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto star', `SELECT * FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto dotted aliases are flat', `SELECT a.id AS [x.y], b.x AS [p.q] FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto duplicate names', `SELECT a.id, b.id FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO`],
  ['auto consecutive grouping', `SELECT a.id, b.x FROM dbo.a a CROSS JOIN dbo.b b ORDER BY b.id, a.id FOR JSON AUTO`],
  ['auto unselected key columns', `SELECT d.k, b.x FROM dbo.d d CROSS JOIN (SELECT TOP 2 * FROM dbo.b ORDER BY id) b ORDER BY b.x, d.v FOR JSON AUTO`],
  ['auto case insensitive grouping', `SELECT e.s, b.x FROM dbo.e e CROSS JOIN (SELECT TOP 1 x FROM dbo.b ORDER BY id) b ORDER BY e.t FOR JSON AUTO`],
  ['auto derived table', `SELECT s.id, b.x FROM (SELECT id FROM dbo.a) s JOIN dbo.b b ON b.a_id=s.id ORDER BY b.id FOR JSON AUTO`],
  ['auto values table', `SELECT v.x, a.id FROM (VALUES (1)) v(x) CROSS JOIN dbo.a a ORDER BY a.id FOR JSON AUTO`],
  ['auto cte', `WITH q AS (SELECT id FROM dbo.a) SELECT q.id, b.x FROM q JOIN dbo.b b ON b.a_id=q.id ORDER BY b.id FOR JSON AUTO`],
  ['auto apply derived', `SELECT a.id, x.v FROM dbo.a a CROSS APPLY (SELECT TOP 1 b.x AS v FROM dbo.b b WHERE b.a_id=a.id ORDER BY b.id) x ORDER BY a.id FOR JSON AUTO`],
  ['auto string_split columns are flat', `SELECT a.id, s.value FROM dbo.a a CROSS APPLY STRING_SPLIT('u,v', ',') s WHERE a.id=1 FOR JSON AUTO`],
  ['auto union is flat', `SELECT a.id, b.x FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id UNION ALL SELECT 1, 'z' FOR JSON AUTO`],
  ['auto aggregate', `SELECT a.id, COUNT(*) AS cnt FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id GROUP BY a.id ORDER BY a.id FOR JSON AUTO`],
  ['auto json fragments', `SELECT a.id, b.x, JSON_QUERY('{"q":1}') AS j, '{"q":1}' AS s FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id WHERE b.id=10 FOR JSON AUTO`],
  ['auto value types', `SELECT CAST(1 AS bit) AS bt, CAST(1.50 AS decimal(5,2)) AS d, CAST('2024-01-02' AS date) AS dt, CAST(0x0102 AS varbinary(4)) AS bin, a.id FROM dbo.a a WHERE id=1 FOR JSON AUTO`],
  ['auto variable column', `DECLARE @v int = 4; SELECT @v AS v FROM dbo.a ORDER BY id FOR JSON AUTO`],
  ['auto empty', `SELECT a.id, b.x FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id WHERE 1=0 FOR JSON AUTO`],
  ['auto nested subquery', `SELECT a.id, (SELECT b.x FROM dbo.b b WHERE b.a_id=a.id ORDER BY b.id FOR JSON AUTO) AS bs FROM dbo.a a ORDER BY a.id`],
  ['auto nested in auto', `SELECT a.id, (SELECT b.x FROM dbo.b b WHERE b.a_id=a.id ORDER BY b.id FOR JSON AUTO) AS bs FROM dbo.a a ORDER BY a.id FOR JSON AUTO`],
  ['auto nested in path', `SELECT a.id AS [k.id], (SELECT b.x FROM dbo.b b WHERE b.a_id=a.id ORDER BY b.id FOR JSON AUTO) AS [k.bs] FROM dbo.a a ORDER BY a.id FOR JSON PATH`],
  ['auto nested options', `SELECT (SELECT a.id FROM dbo.a a WHERE 1=0 FOR JSON AUTO) AS j, (SELECT a.id FROM dbo.a a WHERE a.id=1 FOR JSON AUTO, WITHOUT_ARRAY_WRAPPER) AS u, (SELECT a.id, b.x FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id WHERE a.id=2 FOR JSON AUTO, ROOT('r')) AS r`],
  ['auto variable assignment', `DECLARE @j nvarchar(max) = (SELECT a.id, b.x FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO); SELECT @j AS j`],
  ['auto insert select', `CREATE TABLE dbo.log(j nvarchar(max)); INSERT dbo.log SELECT (SELECT a.id, b.x FROM dbo.a a JOIN dbo.b b ON b.a_id=a.id ORDER BY b.id FOR JSON AUTO); SELECT j FROM dbo.log; DROP TABLE dbo.log`],
  ['auto expression only no table', `SELECT 1 FOR JSON AUTO`],
  ['auto variable no table', `DECLARE @v int = 4; SELECT @v AS v FOR JSON AUTO`],
  ['auto function only no table', `SELECT value FROM STRING_SPLIT('a,b', ',') FOR JSON AUTO`],
  ['auto compile error skips batch', `SELECT 1 AS k; SELECT 2 AS v FOR JSON AUTO`],
  ['auto unnamed column', `SELECT a.id, a.id+1 FROM dbo.a a FOR JSON AUTO`],
  ['auto root and without wrapper', `SELECT a.id+1 FROM dbo.a a FOR JSON AUTO, ROOT('r'), WITHOUT_ARRAY_WRAPPER`],
  ['auto nested unnamed', `SELECT (SELECT 1 FROM dbo.a FOR JSON AUTO) AS j`],
  // JSON_MODIFY editing details.
  ['modify insert whitespace', `SELECT JSON_MODIFY(N'{ "a" : 1 }','$.b',2) AS i1, JSON_MODIFY(N'{ }','$.b',2) AS i2, JSON_MODIFY(N' {"a":1} ','$.b',2) AS i3, JSON_MODIFY(N'{"a":1 ,"b":2 }','$.c',3) AS i4`],
  ['modify delete whitespace', `SELECT JSON_MODIFY(N'{ "a" : 1 , "b" : 2 }','$.a',NULL) AS d1, JSON_MODIFY(N'{ "a" : 1 , "b" : 2 }','$.b',NULL) AS d2, JSON_MODIFY(N'{ "a" : 1 }','$.a',NULL) AS d3, JSON_MODIFY(N'{"a":1,"b":2,"c":3}','$.b',NULL) AS d4`],
  ['modify append whitespace', `SELECT JSON_MODIFY(N'{"arr":[ 1 , 2 ]}','append $.arr',3) AS a1, JSON_MODIFY(N'{"arr":[ ]}','append $.arr',3) AS a2, JSON_MODIFY(N'[1]','append $',3) AS a3, JSON_MODIFY(N'{"o":{"x":1}}','$.o.y',2) AS n1`],
  ['modify value kinds', `SELECT JSON_MODIFY(N'{"a":{"b":1}}','$.a',N'v') AS r1, JSON_MODIFY(N'{"a":[1,2]}','$.a',JSON_QUERY(N'[3]')) AS r2, JSON_MODIFY(N'{"a":1}','$.a',CAST(1.5 AS REAL)) AS r3, JSON_MODIFY(N'{"a":1}','$.a',CAST(0 AS BIT)) AS r4`],
  ['modify numbers', `SELECT JSON_MODIFY(N'{"a":1}','$.a',123456789012.5) AS q2, JSON_MODIFY(N'{"a":1}','$.a',CAST(1e300 AS FLOAT)) AS q3, JSON_MODIFY(N'{"a":1}','$.a',CAST(123.456 AS FLOAT)) AS q5, JSON_MODIFY(N'{"a":1}','$.a',CAST(5 AS TINYINT)) AS t3`],
  ['modify path spacing', `SELECT JSON_MODIFY(N'{"a":1}','strict$.a',2) AS s1, JSON_MODIFY(N'{"a":1}',' $.a ',2) AS s2, JSON_MODIFY(N'{"a":1}','append $.b',2) AS s5`],
  ['modify through non containers', `SELECT JSON_MODIFY(N'{"a":1}','$.a[0]',2) AS s6, JSON_MODIFY(N'[[1]]','$[0][0]',2) AS s7, JSON_MODIFY(N'{"a":[1]}','$.a[0].b',2) AS s8`],
  ['modify for json value', `SELECT JSON_MODIFY(N'{"a":1}','$.a', (SELECT 1 AS x FOR JSON PATH)) AS f1, JSON_MODIFY(N'{"a":1}','$.a', JSON_MODIFY(N'{}','$.z',1)) AS jm`],
  ['modify columns', `SELECT id, JSON_MODIFY(doc, 'append $.list', id) AS value FROM dbo.docs ORDER BY id`],
  ['modify strict in try', `BEGIN TRY SELECT JSON_MODIFY(N'{}', 'strict $.a', 1) AS v END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n, ERROR_STATE() AS st END CATCH`],
  ['modify variable', `DECLARE @d nvarchar(max) = N'{"a":1}'; SET @d = JSON_MODIFY(@d, '$.b', N'x'); SET @d = JSON_MODIFY(@d, '$.a', NULL); SELECT @d AS d`],
  // STRING_AGG, STRING_SPLIT and HASHBYTES in larger statements.
  ['string_agg ordered nvarchar column', `SELECT a_id, STRING_AGG(x, N'|') WITHIN GROUP (ORDER BY id DESC) AS value FROM dbo.b GROUP BY a_id ORDER BY a_id`],
  ['string_agg correlated', `SELECT a.id, (SELECT STRING_AGG(b.x, ',') WITHIN GROUP (ORDER BY b.x DESC) FROM dbo.b b WHERE b.a_id = a.id) AS xs FROM dbo.a a ORDER BY a.id`],
  ['string_agg over string_split', `SELECT STRING_AGG(value, '|') WITHIN GROUP (ORDER BY value DESC) AS value FROM STRING_SPLIT('c,a,b', ',')`],
  ['string_split apply nvarchar column', `SELECT d.id, s.value, s.ordinal FROM dbo.docs d CROSS APPLY STRING_SPLIT(d.csv, N'|', 1) s ORDER BY d.id, s.ordinal`],
  ['string_split variable', `DECLARE @s nvarchar(100) = N'x,y'; SELECT value FROM STRING_SPLIT(@s, ',') ORDER BY value`],
  ['string_split in subquery', `SELECT (SELECT COUNT(*) FROM STRING_SPLIT('a,b,c', ',')) AS n`],
  ['hashbytes algorithms', `SELECT HASHBYTES('MD2', 'abc') AS md2, HASHBYTES('MD4', 'abc') AS md4, HASHBYTES('MD5', 'abc') AS md5, HASHBYTES('SHA', 'abc') AS sha, HASHBYTES('SHA1', 'abc') AS sha1, HASHBYTES('SHA2_256', 'abc') AS s256, HASHBYTES('SHA2_512', 'abc') AS s512`],
  ['hashbytes encodings', `SELECT HASHBYTES('MD5', 'é') AS v, HASHBYTES('MD5', N'é') AS n, HASHBYTES('MD5', 0xE9) AS b, HASHBYTES('SHA2_256', N'') AS empty`],
  ['hashbytes columns', `SELECT id, HASHBYTES('SHA1', csv) AS n, HASHBYTES('SHA1', tag) AS v FROM dbo.docs ORDER BY id`],
  ['hashbytes concatenation', `SELECT HASHBYTES('MD5', 'a' + 'b') AS c1, HASHBYTES('MD5', N'a' + 'b') AS c2`],
  ['hashbytes variables', `DECLARE @v varbinary(10) = 0x01, @n nvarchar(10) = N'ab', @a varchar(10) = 'ab'; SELECT HASHBYTES('sha2_256', @v) AS b, HASHBYTES('MD5', @n) AS n, HASHBYTES('MD5', @a) AS a`],
  ['hashbytes integer input', `SELECT HASHBYTES('MD5', 1) AS h`],
]

async function observe(config, database) {
  const connection = await connect({ ...config, options: { ...config.options, database } })
  const observations = []
  try {
    for (const sql of setup) {
      const result = await run(connection, sql)
      assert.deepEqual(result.messages, [], `setup failed: ${JSON.stringify(result.messages)}`)
    }
    for (const [name, sql] of batches) observations.push({ name, sql, ...(await run(connection, sql)) })
  } finally {
    await close(connection)
  }
  return observations
}

function validate(observations) {
  const find = name => {
    const found = observations.find(o => o.name === name)
    assert(found, `${name}: missing observation`)
    return found
  }
  assert.equal(find('repro for json auto no table').messages[0]?.number, 13600, 'AUTO without a table changed')
  assert.deepEqual(find('auto join').sets[0].rows, [['[{"id":1,"name":"one","b":[{"x":"p"},{"x":"q","n":5}]},{"id":2,"name":"two","b":[{"x":"r","n":6}]}]']], 'AUTO nesting changed')
  assert.deepEqual(find('repro json_modify').sets[0].rows, [['{"a":2}']], 'JSON_MODIFY changed')
  assert.deepEqual(find('repro ordered string_agg desc').sets[0].rows, [['3,2,1']], 'ordered STRING_AGG changed')
}

const database = 'json_string_capture'
async function inContainer(config) {
  const admin = await connect(config)
  await run(admin, `CREATE DATABASE [${database}]`)
  await close(admin)
  return observe(config, database)
}

if (check) {
  const retained = JSON.parse(await readFile(fixture, 'utf8'))
  assert.equal(createHash('sha256').update(JSON.stringify(retained.runs[0])).digest('hex'), retained.captureSha256, 'capture checksum changed')
  for (const observations of retained.runs) validate(observations)
  assertSameCapture(retained.runs[0], retained.runs[1], 'independent runs differ')
  console.log(`Checked ${retained.runs[0].length} json_string observations in two retained runs (${retained.image})`)
} else if (comparePort !== null) {
  const retained = JSON.parse(await readFile(fixture, 'utf8'))
  const config = { server: '127.0.0.1', authentication: { type: 'default', options: { userName: 'sa', password: '' } }, options: { port: comparePort, encrypt: false, requestTimeout: 30000 } }
  const admin = await connect(config)
  const name = `${database}_${Date.now()}`
  await run(admin, `CREATE DATABASE [${name}]`)
  await close(admin)
  const actual = await observe(config, name)
  let differences = 0
  for (const expected of retained.runs[0]) {
    const found = actual.find(o => o.name === expected.name)
    if (found && isDeepStrictEqual(found, expected)) continue
    differences++
    console.log(`${expected.name}: ${found ? describeFirstDifference(found, expected) : 'missing'}`)
  }
  console.log(`${differences} of ${retained.runs[0].length} observations differ`)
  process.exitCode = differences ? 1 : 0
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    await withReferenceContainer(async (config, container) => {
      assert.equal(container.image, image, 'reference image must be the selected one')
      const observations = await inContainer(config)
      validate(observations)
      if (runs.length) assertSameCapture(observations, runs[0], 'independent reference containers differ')
      runs.push(observations)
    }, { image, docker: labelled })
  }
  const actual = { image, captureSha256: createHash('sha256').update(JSON.stringify(runs[0])).digest('hex'), runs }
  await writeFile(output, JSON.stringify(actual, null, 1) + '\n')
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} json_string observations in two fresh containers`)
}

// Containers carry this task's label so parallel workers can tell them apart.
async function labelled(args, env) {
  const { execFile } = await import('node:child_process')
  const { promisify } = await import('node:util')
  const command = args[0] === 'run' ? ['run', '--label', 'msduck.task=gaps-json-string-v1', ...args.slice(1)] : args
  return (await promisify(execFile)('docker', command, { env: { ...process.env, ...env }, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()
}
