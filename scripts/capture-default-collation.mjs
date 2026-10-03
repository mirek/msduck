#!/usr/bin/env node
// Capture SQL Server's default (SQL_Latin1_General_CP1_CI_AS) and column
// collation behavior for comparisons, ordering, grouping and keys (issue #868).
//
//   node scripts/capture-default-collation.mjs [output]
//
// Each case is one batch in a fresh database of one reference container and
// keeps its result descriptors, rows and diagnostics raw. Cases that create
// tables use their own table names. MSSQL_REFERENCE_IMAGE selects another
// image; the fixture records which image produced it.
import { resolve } from 'node:path'
import { canonical, capture } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const output = resolve(process.argv[2] ?? 'reference/default-collation.json')
await refuseExistingFixture(output)

export const cases = []
const add = (group, name, sql) => cases.push({ group, name, sql })
const yes = condition => `CASE WHEN ${condition} THEN 1 ELSE 0 END`

// Literals and variables under the database default.
add('literals', 'unicode equality', `SELECT ${yes("N'A' = N'a'")} AS eq, ${yes("N'a' = N'A  '")} AS padded, ${yes("N'a' = N'á'")} AS accent, ${yes("N'a' <> N'A'")} AS ne`)
add('literals', 'ansi equality', `SELECT ${yes("'A' = 'a'")} AS eq, ${yes("'a' = 'A  '")} AS padded, ${yes("'a' = 'á'")} AS accent, ${yes("'a' <> 'A'")} AS ne`)
add('literals', 'ordering operators', `SELECT ${yes("N'a' < N'B'")} AS lt, ${yes("'a' < 'B'")} AS alt, ${yes("N'B' > N'a'")} AS gt, ${yes("N'a' <= N'A'")} AS le, ${yes("N'A' >= N'a'")} AS ge, ${yes("N'Z' > N'a'")} AS z`)
add('literals', 'in between like case', `SELECT ${yes("N'a' IN (N'A', N'B')")} AS i, ${yes("'b' BETWEEN 'A' AND 'C'")} AS b, ${yes("N'abc' LIKE N'A%'")} AS l, ${yes("'ABC' LIKE '%b%'")} AS al, CASE N'a' WHEN N'A' THEN 1 ELSE 0 END AS c, ${yes("NULLIF(N'A', N'a') IS NULL")} AS n`)
add('literals', 'variables', `DECLARE @a nvarchar(10) = N'Foo', @b varchar(10) = 'FOO', @c nchar(5) = N'foo', @d char(5) = 'fOo'; SELECT ${yes('@a = @b')} AS ab, ${yes('@a = @c')} AS ac, ${yes('@b = @d')} AS bd, ${yes("@a = 'foo'")} AS al, ${yes('@c LIKE N\'F%\'')} AS l`)
add('literals', 'explicit collation overrides', `SELECT ${yes("N'A' = N'a' COLLATE Latin1_General_CS_AS")} AS cs, ${yes("'A' = 'a' COLLATE Latin1_General_BIN2")} AS bin2, ${yes("N'A' = N'a' COLLATE Latin1_General_CI_AS")} AS ci, ${yes("N'a' = N'á' COLLATE Latin1_General_CI_AI")} AS ai`)

// Code units that the default collation ignores in Unicode and ANSI
// comparisons, and the order of accented letters.
const units = 'WITH n AS (SELECT 0 AS u UNION ALL SELECT u + 1 FROM n WHERE u < 255) SELECT u FROM (SELECT u FROM n UNION ALL SELECT v FROM (VALUES (173), (256), (257), (383), (768), (769), (8203), (8204), (8205), (8206), (8232), (65279), (65534), (65535), (57344), (55357), (56966)) x(v)) a'
add('weights', 'ignorable unicode units', `${units} WHERE N'a' + NCHAR(u) + N'b' = N'ab' ORDER BY u OPTION (MAXRECURSION 300)`)
add('weights', 'ignorable ansi units', `${units} WHERE u < 256 AND 'a' + CHAR(u) + 'b' = 'ab' ORDER BY u OPTION (MAXRECURSION 300)`)
add('weights', 'surrogate pair', `SELECT ${yes("N'🦆' = N''")} AS e, ${yes("N'a🦆b' = N'ab'")} AS m, ${yes("NCHAR(55357) = N''")} AS l, ${yes("N'🦆' < N'a'")} AS lt`)
add('weights', 'accented letter order', "SELECT c FROM (VALUES (N'a'), (N'b'), (N'z'), (N'á'), (N'Á'), (N'ā'), (N'Ā'), (N'à'), (N'ab'), (N'ä'), (N'é'), (N'e'), (N'f'), (N'ñ'), (N'n'), (N'o'), (N'ö'), (N'ç'), (N'c'), (N'd')) v(c) ORDER BY c, CAST(c AS varbinary(8))")

// Table data under the default.
const data = "CREATE TABLE dbo.words (id int NOT NULL, n nvarchar(20) NULL, v varchar(20) NULL, c nchar(6) NULL, ch char(6) NULL); INSERT dbo.words VALUES (1, N'Foo', 'Foo', N'Foo', 'Foo'), (2, N'foo', 'foo', N'foo', 'foo'), (3, N'FOO ', 'FOO ', N'FOO', 'FOO'), (4, N'bar', 'bar', N'bar', 'bar'), (5, N'Bär', 'Bär', N'Bär', 'Bär'), (6, N'_x', '_x', N'_x', '_x'), (7, N'Zed', 'Zed', N'Zed', 'Zed'), (8, NULL, NULL, NULL, NULL); "
add('table', 'where equality', data + "SELECT id FROM dbo.words WHERE n = N'FOO' ORDER BY id; SELECT id FROM dbo.words WHERE v = 'fOO' ORDER BY id; SELECT id FROM dbo.words WHERE c = N'foo' ORDER BY id; SELECT id FROM dbo.words WHERE ch = 'FOO' ORDER BY id")
add('table', 'where ranges', data + "SELECT id FROM dbo.words WHERE n < N'C' ORDER BY id; SELECT id FROM dbo.words WHERE v BETWEEN 'B' AND 'G' ORDER BY id; SELECT id FROM dbo.words WHERE n IN (N'ZED', N'BAR') ORDER BY id; SELECT id FROM dbo.words WHERE v NOT IN ('FOO') ORDER BY id")
add('table', 'like', data + "SELECT id FROM dbo.words WHERE n LIKE N'f%' ORDER BY id; SELECT id FROM dbo.words WHERE v LIKE 'F_O' ORDER BY id; SELECT id FROM dbo.words WHERE c LIKE N'%O%' ORDER BY id; SELECT id FROM dbo.words WHERE n LIKE N'[a-c]%' ORDER BY id")
add('table', 'join', data + "SELECT a.id, b.id FROM dbo.words a JOIN dbo.words b ON a.n = b.n AND a.id < b.id ORDER BY a.id, b.id; SELECT a.id, b.id FROM dbo.words a JOIN dbo.words b ON a.v = b.c AND a.id <> b.id ORDER BY a.id, b.id")
add('table', 'group by', data + "SELECT count(*) AS n, count(DISTINCT n) AS dn, count(DISTINCT v) AS dv, count(DISTINCT c) AS dc FROM dbo.words; SELECT count(*) FROM (SELECT n FROM dbo.words GROUP BY n) g; SELECT count(*) FROM (SELECT DISTINCT v FROM dbo.words) d; SELECT upper(n) AS k, count(*) AS c FROM dbo.words GROUP BY upper(n) ORDER BY k")
add('table', 'min max', data + "SELECT min(n) AS a, max(n) AS b, min(v) AS c, max(v) AS d, min(c) AS e, max(ch) AS f FROM dbo.words WHERE id <> 6")
add('table', 'order by', data + "SELECT id FROM dbo.words WHERE id IN (4, 5, 6, 7) ORDER BY n; SELECT id FROM dbo.words WHERE id IN (4, 5, 6, 7) ORDER BY v DESC")
add('table', 'case', data + "SELECT id, CASE n WHEN N'FOO' THEN 1 WHEN N'BAR' THEN 2 ELSE 0 END AS k FROM dbo.words ORDER BY id")
add('table', 'ordering weights', "CREATE TABLE dbo.weights (id int, n nvarchar(10), v varchar(10)); INSERT dbo.weights VALUES (1, N'a', 'a'), (2, N'B', 'B'), (3, N'b', 'b'), (4, N'_', '_'), (5, N'{', '{'), (6, N'á', 'á'), (7, N'z', 'z'), (8, N'1', '1'), (9, N'-a', '-a'), (10, N'ab', 'ab'), (11, N'A', 'A'); SELECT id FROM dbo.weights ORDER BY n, id; SELECT id FROM dbo.weights ORDER BY v, id")

// Keys under the default.
add('keys', 'unique nvarchar', "CREATE TABLE dbo.items (value nvarchar(100) UNIQUE); INSERT dbo.items VALUES (N'Foo'); INSERT dbo.items VALUES (N'foo'); INSERT dbo.items VALUES (N'FOO  '); INSERT dbo.items VALUES (N'Fóo'); SELECT value FROM dbo.items ORDER BY value")
add('keys', 'primary key varchar', "CREATE TABLE dbo.codes (code varchar(10) NOT NULL CONSTRAINT pk_codes PRIMARY KEY, n int); INSERT dbo.codes VALUES ('abc', 1); INSERT dbo.codes VALUES ('ABC', 2); UPDATE dbo.codes SET code = 'ABD' WHERE n = 1; INSERT dbo.codes VALUES ('abd', 3); SELECT code, n FROM dbo.codes")
add('keys', 'nchar and char keys', "CREATE TABLE dbo.fixed (a nchar(4) NOT NULL PRIMARY KEY, b char(4) NULL UNIQUE); INSERT dbo.fixed VALUES (N'x', 'y'); INSERT dbo.fixed VALUES (N'X', 'z'); INSERT dbo.fixed VALUES (N'w', 'Y'); SELECT a, b FROM dbo.fixed")
add('keys', 'unique index', "CREATE TABLE dbo.people (id int, email nvarchar(100)); CREATE UNIQUE INDEX ux_people_email ON dbo.people(email); INSERT dbo.people VALUES (1, N'a@x.com'); INSERT dbo.people VALUES (2, N'A@X.COM'); INSERT dbo.people VALUES (3, N'b@x.com'), (4, N'B@x.com'); SELECT id FROM dbo.people")
add('keys', 'unique index over existing duplicates', "CREATE TABLE dbo.tags (t varchar(20)); INSERT dbo.tags VALUES ('Red'), ('RED'); CREATE UNIQUE INDEX ux_tags ON dbo.tags(t); SELECT count(*) FROM sys.indexes WHERE name = 'ux_tags'")
add('keys', 'alter table add unique', "CREATE TABLE dbo.names (n nvarchar(20)); INSERT dbo.names VALUES (N'Ann'); ALTER TABLE dbo.names ADD CONSTRAINT uq_names UNIQUE (n); INSERT dbo.names VALUES (N'ANN'); ALTER TABLE dbo.names ADD m varchar(10) NULL; CREATE TABLE dbo.dups (n nvarchar(20)); INSERT dbo.dups VALUES (N'x'), (N'X'); ALTER TABLE dbo.dups ADD CONSTRAINT uq_dups UNIQUE (n)")
add('keys', 'composite key', "CREATE TABLE dbo.pairs (a nvarchar(10) NOT NULL, b varchar(10) NOT NULL, PRIMARY KEY (a, b)); INSERT dbo.pairs VALUES (N'k', 'v'); INSERT dbo.pairs VALUES (N'K', 'V'); INSERT dbo.pairs VALUES (N'K', 'w'); SELECT a, b FROM dbo.pairs ORDER BY b")

// Column collations.
const columns = "CREATE TABLE dbo.mixed (id int, ci nvarchar(10) COLLATE Latin1_General_CI_AS, cs nvarchar(10) COLLATE Latin1_General_CS_AS, scs varchar(10) COLLATE SQL_Latin1_General_CP1_CS_AS, b2 nvarchar(10) COLLATE Latin1_General_BIN2, u8 varchar(10) COLLATE Latin1_General_100_CI_AS_SC_UTF8, ai nvarchar(10) COLLATE Latin1_General_CI_AI); INSERT dbo.mixed VALUES (1, N'Foo', N'Foo', 'Foo', N'Foo', 'Foo', N'Fóo'), (2, N'foo', N'foo', 'foo', N'foo', 'foo', N'foo'); "
add('columns', 'catalog', columns + "SELECT name, collation_name FROM sys.columns WHERE object_id = OBJECT_ID('dbo.mixed') ORDER BY column_id; SELECT ci, cs, scs, b2, u8, ai FROM dbo.mixed WHERE id = 1")
add('columns', 'comparisons', columns + "SELECT id FROM dbo.mixed WHERE ci = N'FOO' ORDER BY id; SELECT id FROM dbo.mixed WHERE cs = N'FOO' ORDER BY id; SELECT id FROM dbo.mixed WHERE cs = N'foo' ORDER BY id; SELECT id FROM dbo.mixed WHERE scs = 'FOO' ORDER BY id; SELECT id FROM dbo.mixed WHERE b2 = N'foo' ORDER BY id; SELECT id FROM dbo.mixed WHERE u8 = 'FOO' ORDER BY id; SELECT id FROM dbo.mixed WHERE ai = N'FOO' ORDER BY id")
add('columns', 'like and in', columns + "SELECT id FROM dbo.mixed WHERE cs LIKE N'f%' ORDER BY id; SELECT id FROM dbo.mixed WHERE scs IN ('FOO', 'foo') ORDER BY id; SELECT id FROM dbo.mixed WHERE b2 BETWEEN N'a' AND N'z' ORDER BY id")
add('columns', 'grouping and order', columns + "SELECT count(DISTINCT cs) AS a, count(DISTINCT ci) AS b, count(DISTINCT b2) AS c FROM dbo.mixed; SELECT id FROM dbo.mixed ORDER BY cs, id")
add('columns', 'conflict', columns + "SELECT id FROM dbo.mixed WHERE ci = cs")
add('columns', 'conflict between case-insensitive names', "CREATE TABLE dbo.names2 (id int, ci nvarchar(10) COLLATE Latin1_General_CI_AS, d nvarchar(10)); INSERT dbo.names2 VALUES (1, N'a', N'A'); SELECT id FROM dbo.names2 WHERE ci = d")
add('columns', 'conflict in dml', columns + "UPDATE dbo.mixed SET id = id + 10 WHERE ci = cs; DELETE FROM dbo.mixed WHERE scs <> ai; SELECT id FROM dbo.mixed ORDER BY id")
add('columns', 'conflict in like', columns + "SELECT id FROM dbo.mixed WHERE cs LIKE ci")
add('columns', 'conflict in list', columns + "SELECT id FROM dbo.mixed WHERE ci IN (cs, N'x')")
add('columns', 'conflict in between', columns + "SELECT id FROM dbo.mixed WHERE ci BETWEEN cs AND N'z'")
add('columns', 'conflict in simple case', columns + "SELECT CASE ci WHEN cs THEN 1 ELSE 0 END FROM dbo.mixed")
add('columns', 'conflict resolved', columns + "SELECT id FROM dbo.mixed WHERE ci = cs COLLATE Latin1_General_CS_AS ORDER BY id")
add('columns', 'case sensitive keys', "CREATE TABLE dbo.cskeys (a nvarchar(10) COLLATE Latin1_General_CS_AS NOT NULL PRIMARY KEY, b varchar(10) COLLATE SQL_Latin1_General_CP1_CS_AS NULL UNIQUE, c nvarchar(10) COLLATE Latin1_General_BIN2 NULL); CREATE UNIQUE INDEX ux_cskeys_c ON dbo.cskeys(c); INSERT dbo.cskeys VALUES (N'k', 'v', N'w'); INSERT dbo.cskeys VALUES (N'K', 'V', N'W'); INSERT dbo.cskeys VALUES (N'k', 'x', N'y'); INSERT dbo.cskeys VALUES (N'k2', 'v', N'y2'); INSERT dbo.cskeys VALUES (N'k3', 'v3', N'w  '); SELECT a, b, c FROM dbo.cskeys ORDER BY a COLLATE Latin1_General_BIN2")
add('columns', 'add column', "CREATE TABLE dbo.later (id int); ALTER TABLE dbo.later ADD tag nvarchar(10) COLLATE Latin1_General_CS_AS NULL; INSERT dbo.later VALUES (1, N'A'), (2, N'a'); SELECT id FROM dbo.later WHERE tag = N'a'; SELECT collation_name FROM sys.columns WHERE object_id = OBJECT_ID('dbo.later') AND name = 'tag'")
add('columns', 'unknown name', "CREATE TABLE dbo.bad (a nvarchar(10) COLLATE Not_A_Collation)")
add('columns', 'unknown name add column', "CREATE TABLE dbo.bad2 (id int); ALTER TABLE dbo.bad2 ADD a nvarchar(10) COLLATE Latin1_General_XX_AS")
add('columns', 'non character', "CREATE TABLE dbo.bad3 (a int COLLATE Latin1_General_CI_AS)")
add('columns', 'descriptors', columns + "SELECT cs, b2, scs FROM dbo.mixed WHERE id = 2")

async function run(config, selected) {
  return isolatedReference(config, async connection => {
    const version = (await capture(connection, "SELECT CONVERT(nvarchar(128), SERVERPROPERTY('ProductVersion')) AS v")).sets[0].rows[0][0]
    const captured = []
    for (const entry of selected) {
      const result = await capture(connection, entry.sql)
      captured.push({
        ...entry,
        sets: result.sets.map(set => ({
          columns: set.columns.map(c => ({ name: c.name, type: c.type, length: c.length, collation: c.collation ?? null })),
          rows: canonical(set.rows),
        })),
        errors: result.errors.map(e => ({ number: e.number, state: e.state, class: e.class, message: e.message })),
      })
      // Cases reuse table names; drop every user table before the next one.
      await capture(connection, "DECLARE @s nvarchar(max) = N''; SELECT @s = @s + N'DROP TABLE ' + QUOTENAME(SCHEMA_NAME(schema_id)) + N'.' + QUOTENAME(name) + N';' FROM sys.tables; EXEC (@s)")
    }
    return { version, captured }
  })
}

const docker = async (args, env) => {
  const { execFile } = await import('node:child_process')
  const { promisify } = await import('node:util')
  const command = args[0] === 'run' ? ['run', '--label', 'msduck.task=v025-default-collation-v1', ...args.slice(1)] : args
  return (await promisify(execFile)('docker', command, { env: { ...process.env, ...env }, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()
}

const main = await withReferenceContainer(async (config, container) => ({ image: container.image, ...await run(config, cases) }), { docker })
await writeNewFixture(output, { image: main.image, version: main.version, cases: main.captured })
console.log('Captured', main.captured.length, 'cases from', main.version)
