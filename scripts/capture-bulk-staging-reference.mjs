#!/usr/bin/env node
// Owner-controlled SQL Server evidence; retain post-login bytes, never LOGIN7.
import assert from 'node:assert/strict'
import {lstat, mkdir, readFile, realpath, writeFile} from 'node:fs/promises'
import {dirname, resolve} from 'node:path'
import {fileURLToPath} from 'node:url'
import {isDeepStrictEqual} from 'node:util'
import {TYPES} from 'tedious'
import {capture, canonical, differences} from './lib/compatibility.mjs'
import {isolatedReference} from './lib/reference.mjs'
import {withReferenceContainer} from './lib/reference-container.mjs'

const fixture = fileURLToPath(new URL('../reference/bulk-staging-reference.json', import.meta.url))
const LIMIT = 8 * 1024 * 1024
const MAX_PACKETS = 8192
export const cases = [
  ...[1, 499, 500, 501, 999, 1000, 1001, 1501].map(count => ({name: `defaults-${count}`, count, fireTriggers: true})),
  {name: 'keep-nulls-staged', count: 1001, fireTriggers: true, keepNulls: true},
  {name: 'no-triggers-staged', count: 1001, fireTriggers: false},
  ...[399, 400, 401, 1001].map(count => ({name: `listed-identity-${count}`, count, fireTriggers: true, listedIdentity: true})),
  {name: 'all-default-staged', count: 1001, fireTriggers: true, allDefault: true},
  ...['check', 'foreign-key'].flatMap(constraintKind => [false, true].flatMap(checkConstraints => [false, true].map(transaction => ({
    name: `trigger-${constraintKind}-check${Number(checkConstraints)}-tran${Number(transaction)}`,
    count: 3, fireTriggers: true, checkConstraints, transaction, constraintKind
  }))))
]

export function rowsFor(entry) {
  return Array.from({length: entry.count}, (_, index) => {
    if (entry.allDefault) return {a: null, b: null, c: null}
    const row = {marker: index + 1, a: index % 3 === 0 ? null : index + 100,
      b: index % 3 === 1 ? null : `row${index}`, c: index % 3 === 2 ? null : index + 200}
    if (entry.listedIdentity) row.id = 10000 + index * 2
    return row
  })
}

// Guards happen before Docker. Every existing path, including dangling links,
// is refused. Raw output and its comparison sidecar are independent new files.
export async function guardOutput(path) {
  for (let part = resolve(path);; part = dirname(part)) {
    let info
    try { info = await lstat(part) } catch (error) { if (error.code !== 'ENOENT') throw error }
    assert.ok(!info?.isSymbolicLink(), 'output path contains a symlink')
    if (part === resolve(path)) assert.ok(!info, 'refusing existing output')
    if (dirname(part) === part) break
  }
}

function trace(connection) {
  const outgoing = connection.messageIo.outgoingMessageStream
  const incoming = connection.messageIo.securePair?.cleartext ?? connection.messageIo.socket
  let pending = Buffer.alloc(0), bytes = 0
  const packets = []
  const retain = (direction, packet) => {
    bytes += packet.length
    assert.ok(bytes <= LIMIT && packets.length < MAX_PACKETS, 'bounded post-login exchange')
    assert.ok(packet.length >= 8 && packet.length <= 32767, 'bounded packet length')
    assert.equal(packet.readUInt16BE(2), packet.length, 'complete packet')
    assert.ok(direction === 'out' ? [1, 7].includes(packet[0]) : packet[0] === 4, 'controlled post-login packet type')
    packets.push({direction, rawHex: packet.toString('hex')})
  }
  const onOut = packet => retain('out', packet)
  const onIn = chunk => {
    assert.ok(chunk.length + pending.length <= LIMIT, 'bounded incoming buffer')
    pending = Buffer.concat([pending, chunk])
    while (pending.length >= 8) {
      const size = pending.readUInt16BE(2)
      assert.ok(size >= 8 && size <= 32767, 'bounded incoming frame')
      if (pending.length < size) break
      retain('in', pending.subarray(0, size))
      pending = pending.subarray(size)
    }
  }
  outgoing.on('data', onOut)
  incoming.on('data', onIn)
  return () => {
    outgoing.off('data', onOut)
    incoming.off('data', onIn)
    assert.equal(pending.length, 0, 'no unfinished captured packet')
    return packets
  }
}

async function exchange(connection, action) {
  const stop = trace(connection)
  let result, packets
  try { result = canonical(await action()) } finally { packets = stop() }
  return {result, packets}
}

async function sql(connection, text) {
  return {sql: text, ...await exchange(connection, () => capture(connection, text))}
}

function bulk(connection, entry, rows) {
  return new Promise((resolveResult, reject) => {
    const errors = [], info = []
    const fields = e => ({number: e.number, state: e.state, class: e.class,
      lineNumber: e.lineNumber, message: e.message, serverName: e.serverName, procName: e.procName})
    const onError = e => errors.push(fields(e)), onInfo = e => info.push(fields(e))
    const clean = () => { connection.off('errorMessage', onError); connection.off('infoMessage', onInfo) }
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    try {
      const load = connection.newBulkLoad('dbo.items', {
        fireTriggers: Boolean(entry.fireTriggers), keepNulls: Boolean(entry.keepNulls),
        checkConstraints: Boolean(entry.checkConstraints)
      }, (error, rowCount) => {
        clean()
        resolveResult({errors, info, rowCount: rowCount ?? null,
          error: error ? {number: error.number ?? null, code: error.code ?? null, message: error.message} : null})
      })
      if (entry.listedIdentity) load.addColumn('id', TYPES.Int, {nullable: false})
      if (!entry.allDefault) load.addColumn('marker', TYPES.Int, {nullable: false})
      load.addColumn('a', TYPES.Int, {nullable: true})
      load.addColumn('b', TYPES.NVarChar, {length: 20, nullable: true})
      load.addColumn('c', TYPES.Int, {nullable: true})
      connection.execBulkLoad(load, rows)
    } catch (error) { clean(); reject(error) }
  })
}

async function observe(connection) {
  const version = await sql(connection, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)) AS version, DB_NAME() AS database_name")
  assert.equal(version.result.errors.length, 0, 'version query succeeds')
  const observations = []
  for (const entry of cases) {
    const setupSql = `CREATE TABLE dbo.items (id int IDENTITY(1,1) NOT NULL, marker int NOT NULL DEFAULT 0, a int NULL DEFAULT 11 CONSTRAINT ck_items CHECK(a > 0), b nvarchar(20) NULL DEFAULT N'β', c int NULL DEFAULT 22);
CREATE TABLE dbo.audit (invocation int IDENTITY(1,1) NOT NULL, n int NOT NULL);
CREATE TABLE dbo.seen (invocation int NOT NULL, id int NOT NULL, marker int NULL);
CREATE TABLE dbo.sink (n int NOT NULL CONSTRAINT ck_sink CHECK(n > 0));
CREATE TABLE dbo.parent (id int NOT NULL PRIMARY KEY);
INSERT dbo.parent VALUES(1);
CREATE TABLE dbo.child (id int NOT NULL CONSTRAINT fk_child REFERENCES dbo.parent(id));`
    const setup = await sql(connection, setupSql)
    assert.equal(setup.result.errors.length, 0, 'setup succeeds')
    const trigger = await sql(connection, `CREATE TRIGGER tr_items ON dbo.items AFTER INSERT AS BEGIN
DECLARE @invocation int;
INSERT dbo.audit(n) SELECT COUNT(*) FROM inserted;
SET @invocation = SCOPE_IDENTITY();
INSERT dbo.seen SELECT @invocation, id, marker FROM inserted;
${entry.constraintKind === 'check' ? 'INSERT dbo.sink VALUES(-1);' : ''}
${entry.constraintKind === 'foreign-key' ? 'INSERT dbo.child VALUES(999);' : ''}
END`)
    assert.equal(trigger.result.errors.length, 0, 'trigger creation succeeds')
    const begin = entry.transaction ? await sql(connection, 'BEGIN TRAN') : null
    const input = rowsFor(entry)
    const execution = await exchange(connection, () => bulk(connection, entry, input))
    const readback = await sql(connection, `SELECT @@ERROR AS last_error, @@ROWCOUNT AS last_rowcount, @@IDENTITY AS last_identity, SCOPE_IDENTITY() AS scope_identity, IDENT_CURRENT('dbo.items') AS current_identity, XACT_STATE() AS transaction_state, @@TRANCOUNT AS transaction_count;
SELECT id, marker, a, b, c FROM dbo.items ORDER BY id;
SELECT invocation, n FROM dbo.audit ORDER BY invocation;
SELECT invocation, id, marker FROM dbo.seen ORDER BY invocation, id;
SELECT n FROM dbo.sink;
SELECT id FROM dbo.child;
SELECT name, is_not_trusted, is_disabled FROM sys.check_constraints ORDER BY name;
SELECT name, is_not_trusted, is_disabled FROM sys.foreign_keys ORDER BY name;`)
    const rollback = entry.transaction ? await sql(connection, 'IF @@TRANCOUNT > 0 ROLLBACK') : null
    const cleanup = await sql(connection, 'DROP TABLE dbo.items; DROP TABLE dbo.audit; DROP TABLE dbo.seen; DROP TABLE dbo.sink; DROP TABLE dbo.child; DROP TABLE dbo.parent;')
    assert.equal(cleanup.result.errors.length, 0, 'cleanup succeeds')
    observations.push({case: entry, input, setup, trigger, begin, execution, readback, rollback, cleanup})
  }
  return {version, observations}
}

export function compare(actual, expected) {
  // All differences remain visible. No hostname/SPID/error/row normalization.
  return differences(actual, expected).map(item => ({...item,
    category: /\/packets\/\d+\/rawHex$/.test(item.path) ? 'raw-packet' : 'observed-field'}))
}

export function validate(value) {
  assert.equal(value.format, 1)
  assert.equal(value.containers.length, 2)
  assert.equal(value.runs.length, 4)
  for (const run of value.runs) {
    assert.equal(run.observations.length, cases.length)
    for (const [index, observation] of run.observations.entries()) {
      assert.ok(isDeepStrictEqual(observation.case, cases[index]), 'exact case manifest')
      assert.ok(isDeepStrictEqual(observation.input, rowsFor(cases[index])), 'exact original input')
      if (!observation.case.constraintKind) {
        assert.equal(observation.execution.result.error, null, 'successful interaction reaches row insertion')
        assert.equal(observation.execution.result.errors.length, 0, 'no accidental metadata/setup diagnostic')
        assert.equal(observation.execution.result.rowCount, observation.case.count, 'complete successful bulk load')
      } else {
        assert.ok(observation.execution.result.errors.some(error => error.number === 547), 'other-table constraint probe reaches trigger violation')
      }
      for (const step of [run.version, observation.setup, observation.trigger, observation.begin,
        observation.execution, observation.readback, observation.rollback, observation.cleanup].filter(Boolean)) {
        assert.ok(Array.isArray(step.packets) && step.packets.length > 0 && step.packets.length <= MAX_PACKETS, 'bounded complete exchange')
        let bytes = 0, message = null
        for (const packet of step.packets) {
          assert.match(packet.rawHex, /^(?:[0-9a-f]{2})+$/)
          const raw = Buffer.from(packet.rawHex, 'hex')
          bytes += raw.length
          assert.ok(raw.length >= 8 && raw.length <= 32767 && raw.readUInt16BE(2) === raw.length, 'exact packet frame')
          assert.ok(packet.direction === 'out' ? [1, 7].includes(raw[0]) : packet.direction === 'in' && raw[0] === 4, 'post-login packet direction/type')
          if (message) {
            assert.equal(packet.direction, message.direction, 'one direction per complete message')
            assert.equal(raw[0], message.type, 'one packet type per complete message')
          } else message = {direction: packet.direction, type: raw[0]}
          if (raw[1] & 1) message = null
        }
        assert.ok(bytes <= LIMIT, 'bounded exchange bytes')
        assert.equal(message, null, 'complete EOM framing')
      }
    }
  }
  const expected = value.runs.slice(1).map(run => compare(run, value.runs[0]))
  assert.ok(isDeepStrictEqual(value.comparisons, expected), 'comparisons exactly reflect retained observations')
}

export async function main(args = process.argv.slice(2)) {
  const allowed = new Set(['--write-fixture', '--replay-fixture'])
  assert.ok(args.filter(arg => arg.startsWith('--')).every(arg => allowed.has(arg)), 'known flags only')
  const positional = args.filter(arg => !arg.startsWith('--'))
  assert.ok(positional.length <= 1, 'one output path')
  const write = args.includes('--write-fixture'), replay = args.includes('--replay-fixture')
  assert.ok(!(write && replay), 'choose writing or replay')
  if (replay) {
    validate(JSON.parse(await readFile(fixture, 'utf8')))
    console.log('Validated four retained BulkLoad staging reference runs and exact raw differences')
    return
  }
  const output = resolve(positional[0] ?? 'artifacts/compatibility/bulk-staging-reference/capture.json')
  await guardOutput(output)
  await guardOutput(`${output}.comparison.json`)
  if (write) await guardOutput(fixture)
  assert.notEqual(output, fixture, 'diagnostic output differs from fixture')
  const containers = [], runs = []
  for (let index = 0; index < 2; index++) {
    const result = await withReferenceContainer(async (config, container) => ({image: container.image,
      runs: [await isolatedReference(config, observe), await isolatedReference(config, observe)]}))
    containers.push({image: result.image})
    runs.push(...result.runs)
  }
  const actual = {format: 1, containers, runs, comparisons: runs.slice(1).map(run => compare(run, runs[0]))}
  await mkdir(dirname(output), {recursive: true})
  await writeFile(output, JSON.stringify(actual) + '\n', {flag: 'wx'})
  validate(actual)
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) } catch (error) { if (error.code !== 'ENOENT') throw error }
  const comparison = retained ? compare(actual, retained) : []
  await writeFile(`${output}.comparison.json`, JSON.stringify({retained: Boolean(retained), differences: comparison}) + '\n', {flag: 'wx'})
  if (write) await writeFile(fixture, JSON.stringify(actual) + '\n', {flag: 'wx'})
  console.log(`Captured ${cases.length} cases in four fresh databases; ${retained ? `${comparison.length} retained-fixture differences` : 'no prior retained baseline'}`)
  if (retained && comparison.length) throw Error('raw reference drift preserved; inspect output and comparison sidecar')
}

if (process.argv[1] && await realpath(process.argv[1]) === fileURLToPath(import.meta.url)) await main()
