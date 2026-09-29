#!/usr/bin/env node
// Capture SQL Server 2025's LOGIN7 response when the login names a database
// that does not exist, plus related name and user variants. Only incoming
// bytes after TLS are recorded; the outgoing LOGIN7 and password are not.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Connection, Request } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { observeLogin, comparable } from './lib/login-response.mjs'

const fixture = new URL('../reference/login-database-error.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/login-database-error/capture.json')

// `user` names a login created for the case; the password is the sa password.
const cases = [
  { name: 'nonexistent database', database: 'foo' },
  { name: 'nonexistent quoted name', database: 'fo"o]' },
  { name: 'nonexistent unicode name', database: 'dbé🦆' },
  { name: 'nonexistent database, differently cased login name', database: 'foo', userName: 'SA' },
  { name: 'nonexistent database, login name with quote', database: 'foo', user: "o'brien" },
  { name: 'wrong password and nonexistent database', database: 'foo', wrongPassword: true },
  { name: 'master', database: 'master', batch: 'select 42' },
  { name: 'master, different case', database: 'MASTER', batch: 'SELECT DB_NAME()' },
  { name: 'empty database', database: '', batch: 'SELECT DB_NAME()' },
  { name: 'user database, different case', database: 'casedb', batch: 'SELECT DB_NAME()' },
  { name: 'user database, trailing space', database: 'CaseDb ', batch: 'SELECT DB_NAME()' }
]

function admin(config) {
  return new Promise((resolveConnection, reject) => {
    const connection = new Connection(config)
    connection.on('error', () => {})
    connection.connect(error => error ? reject(error) : resolveConnection(connection))
  })
}
function scalar(connection, sql) {
  return new Promise((resolveValue, reject) => {
    let value
    const request = new Request(sql, error => error ? reject(error) : resolveValue(value))
    request.on('row', cells => { value = cells[0]?.value })
    connection.execSqlBatch(request)
  })
}

async function observe(config, serverName) {
  const records = []
  for (const item of cases) {
    const observation = await observeLogin(config, {
      database: item.database,
      userName: item.user ?? item.userName,
      password: item.wrongPassword ? 'wrong password' : undefined,
      batch: item.batch
    })
    records.push({ name: item.name, database: item.database, userName: item.user ?? item.userName ?? 'sa', ...comparable(observation, serverName) })
  }
  return records
}

function validate(run) {
  const get = name => run.find(record => record.name === name)
  const foo = get('nonexistent database')
  assert.deepEqual(foo.tokens.map(token => [token.token, token.number ?? token.status]), [['ERROR', 4060], ['ERROR', 18456], ['DONE', 2]])
  assert.equal(foo.tokens[0].message, 'Cannot open database "foo" requested by the login. The login failed.')
  assert.equal(foo.tokens[1].message, "Login failed for user 'sa'.")
  assert.deepEqual(foo.events.find(event => event[0] === 'connect')[1], { code: 'ELOGIN', message: "Login failed for user 'sa'." })
  assert.deepEqual(get('master').rows, [[42]])
  assert.equal(get('master').events.find(event => event[0] === 'connect')[1], null)
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const connection = await admin(config)
  let serverName
  try {
    serverName = await scalar(connection, 'SELECT @@SERVERNAME')
    const password = config.authentication.options.password.replaceAll("'", "''")
    // Fixed names are safe: the container is fresh and owned by this run.
    await scalar(connection, `CREATE DATABASE CaseDb; CREATE LOGIN [o'brien] WITH PASSWORD = N'${password}', CHECK_POLICY = OFF`)
  } finally { connection.close() }
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    const run = await observe(config, serverName)
    validate(run)
    runs.push(run)
  }
  assertSameCapture(runs[0], runs[1], 'login responses differ across repeats')
  const actual = { image: container.image, serverNameMapping: '@@SERVERNAME is replaced by <serverName>; packet SPID is reduced to spidNonZero', runs }
  await writeFile(output, JSON.stringify(actual, null, 1) + '\n')
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'login responses differ from retained fixture')
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} login observations twice${retained ? ' and matched retained fixture' : ''}`)
})
