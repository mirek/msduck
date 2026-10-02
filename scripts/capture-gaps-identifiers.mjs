// First-party SQL Server evidence for contextual identifiers and
// database-qualified object names in error messages (task gaps-identifiers-v1).
//
//   node scripts/capture-gaps-identifiers.mjs            capture the reference
//   node scripts/capture-gaps-identifiers.mjs --local P  replay against msduck on
//                                                       127.0.0.1:P and diff
//
// The reference run starts a labelled, owned container and writes
// reference/gaps-identifiers.json. Every case runs in a fresh user database
// whose name is replaced with `<db>` in the retained messages.
import { writeFile, readFile } from 'node:fs/promises'
import { randomUUID } from 'node:crypto'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect } from './lib/reference.mjs'
import { capture } from './lib/compatibility.mjs'

const fixturePath = new URL('../reference/gaps-identifiers.json', import.meta.url)
const image = process.env.MSSQL_REFERENCE_IMAGE ?? 'mcr.microsoft.com/mssql/server:2022-latest'

// Words DuckDB reserves (reserved, type/function-name and column-name keyword
// categories), plus T-SQL reserved words used as controls.
export const words = [
  'offset', 'limit', 'qualify', 'at', 'pivot', 'unpivot', 'using', 'returning', 'window',
  'lateral', 'only', 'array', 'both', 'leading', 'trailing', 'placing', 'symmetric',
  'asymmetric', 'variadic', 'deferrable', 'initially', 'analyse', 'analyze', 'describe',
  'show', 'summarize', 'do', 'lambda', 'cast', 'true', 'false', 'pivot_longer', 'pivot_wider',
  'anti', 'asof', 'semi', 'positional', 'binary', 'collation', 'columns', 'concurrently',
  'freeze', 'generated', 'glob', 'ilike', 'isnull', 'notnull', 'map', 'struct', 'natural',
  'overlaps', 'similar', 'try_cast', 'unpack', 'verbose', 'tablesample',
  'position', 'row', 'time', 'timestamp', 'interval', 'extract', 'overlay', 'trim',
  'substring', 'grouping', 'grouping_id', 'none', 'setof', 'treat', 'out', 'inout', 'values'
]

function cases(word) {
  const w = word
  return [
    ['create', `CREATE TABLE ${w}(${w} int NOT NULL, other int NULL)`],
    ['insert', `INSERT INTO ${w}(${w}, other) VALUES (1, 10); INSERT ${w} VALUES (2, 20)`],
    ['select', `SELECT ${w} FROM ${w} WHERE ${w} = 1 ORDER BY ${w}`],
    ['qualified', `SELECT ${w}.${w} FROM dbo.${w} WHERE dbo.${w}.${w} > 0 ORDER BY ${w}.${w}`],
    ['alias_as', `SELECT x.${w} AS ${w} FROM ${w} AS x WHERE x.${w} = 2`],
    ['alias_bare', `SELECT other ${w} FROM ${w} ${w} WHERE ${w}.${w} = 2`],
    ['update', `UPDATE ${w} SET ${w} = ${w} + 10 WHERE ${w} = 2`],
    ['parameter', `DECLARE @${w} int = 12; SELECT ${w} FROM ${w} WHERE ${w} = @${w}`],
    ['cte', `WITH ${w}(${w}) AS (SELECT ${w} FROM dbo.${w}) SELECT ${w} FROM ${w} ORDER BY ${w}`],
    ['group', `SELECT ${w}, count(*) AS n FROM ${w} GROUP BY ${w} ORDER BY ${w}`],
    ['delete', `DELETE FROM ${w} WHERE ${w} = 12`],
    ['index', `CREATE INDEX ix_${w} ON ${w}(${w})`],
    ['view', `CREATE VIEW v_${w} AS SELECT ${w} FROM ${w}`],
    ['view_select', `SELECT ${w} FROM v_${w}`],
    ['catalog', `SELECT c.name FROM sys.columns c WHERE c.object_id = OBJECT_ID('dbo.${w}') ORDER BY c.column_id`],
    ['drop', `DROP VIEW v_${w}; DROP TABLE ${w}`]
  ]
}

const diagnostics = [
  ['create', 'CREATE TABLE items(id int NOT NULL PRIMARY KEY, name nvarchar(3) NULL, code varchar(2) NULL, tag int NULL)'],
  ['unique', 'CREATE UNIQUE INDEX ux_items_tag ON items(tag)'],
  ['seed', "INSERT INTO items(id, name, code, tag) VALUES (1, N'abc', 'ab', 1)"],
  ['nvarchar_truncation', "INSERT INTO items(id, name) VALUES (2, N'abcdef')"],
  ['varchar_truncation', "INSERT INTO items(id, code) VALUES (3, 'abcdef')"],
  ['update_truncation', "UPDATE items SET name = N'wxyz' WHERE id = 1"],
  ['primary_key', "INSERT INTO items(id) VALUES (1)"],
  ['unique_index', "INSERT INTO items(id, tag) VALUES (4, 1)"],
  ['not_null', 'INSERT INTO items(id, name) VALUES (NULL, NULL)'],
  ['current', 'SELECT DB_NAME() AS db']
]

async function run(connection, database, sql) {
  const result = await capture(connection, sql)
  const clean = text => typeof text === 'string' ? text.replaceAll(database, '<db>') : text
  return {
    errors: result.errors.map(e => ({ number: e.number, state: e.state, class: e.class, message: clean(e.message) })),
    sets: result.sets.map(s => ({ columns: s.columns.map(c => c.name), rows: s.rows.map(r => r.map(clean)) }))
  }
}

async function observe(config) {
  const admin = await connect(config)
  const database = `ident_${randomUUID().replaceAll('-', '').slice(0, 12)}`
  try {
    await capture(admin, `CREATE DATABASE [${database}]`)
    const connection = await connect({ ...config, options: { ...config.options, database } })
    try {
      const identifiers = {}
      for (const word of words) {
        identifiers[word] = {}
        for (const [id, sql] of cases(word)) identifiers[word][id] = await run(connection, database, sql)
        await capture(connection, `DROP VIEW IF EXISTS v_${word}; DROP TABLE IF EXISTS ${word}; DROP TABLE IF EXISTS [${word}]`)
      }
      const errors = {}
      for (const [id, sql] of diagnostics) errors[id] = await run(connection, database, sql)
      return { identifiers, errors }
    } finally { connection.close() }
  } finally {
    await capture(admin, `DROP DATABASE IF EXISTS [${database}]`)
    admin.close()
  }
}

const local = process.argv.indexOf('--local')
if (local >= 0) {
  const port = Number(process.argv[local + 1])
  const config = {
    server: '127.0.0.1',
    authentication: { type: 'default', options: { userName: 'sa', password: 'development' } },
    options: { port, encrypt: false, database: 'master', connectTimeout: 5000, requestTimeout: 30000 }
  }
  const actual = await observe(config)
  const fixture = JSON.parse(await readFile(fixturePath, 'utf8'))
  let differences = 0
  const compare = (path, left, right) => {
    if (JSON.stringify(left) === JSON.stringify(right)) return
    if (right && typeof right === 'object' && !Array.isArray(right) && !('sets' in right)) {
      for (const key of Object.keys(right)) compare(`${path}/${key}`, left?.[key], right[key])
      return
    }
    differences++
    console.log(`${path}\n  msduck:    ${JSON.stringify(left)}\n  reference: ${JSON.stringify(right)}`)
  }
  compare('', actual, fixture.observations)
  console.log(`${differences} differing entries`)
} else {
  // Label the owned container with this task; the helper removes it.
  const docker = async (args, env) => {
    const { execFile } = await import('node:child_process')
    const { promisify } = await import('node:util')
    const full = args[0] === 'run' ? ['run', '--label', 'msduck.task=gaps-identifiers-v1', ...args.slice(1)] : args
    return (await promisify(execFile)('docker', full, { env: { ...process.env, ...env }, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()
  }
  await withReferenceContainer(async (config, container) => {
    const observations = await observe(config)
    await writeFile(fixturePath, JSON.stringify({ image: container.image, observations }, null, 2) + '\n')
    console.log(`Captured ${Object.keys(observations.identifiers).length} identifiers and ${Object.keys(observations.errors).length} diagnostics`)
  }, { image, docker })
}
