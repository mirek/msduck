#!/usr/bin/env node
// Retain SQL Server T-SQL cursor rows, descriptors, diagnostics, @@FETCH_STATUS,
// @@CURSOR_ROWS, return statuses and completion tokens (issue #337).
//
// Conventions follow docs/reference-captures.md from owner PR #312. That PR's
// shared helpers (capturePrepared, useUtcTimeZone and the per-request return
// status rule) are not on main yet, so the needed logic is replicated below and
// should be replaced by the shared helpers once #312 merges.
process.env.TZ = 'UTC'
import assert from 'node:assert/strict'
import { mkdir, readFile, realpath, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Request, TYPES } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { assertSameCapture, connect, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

if (new Date(0).getTimezoneOffset() !== 0) throw new Error('could not switch the process time zone to UTC')

// Per-request bounds; overflow is counted in `truncated` instead of retained.
const LIMITS = Object.freeze({ rows: 1000, messages: 200 })

// Token hooks on tedious's stream parser (capture-local; tedious itself is not
// modified).
// - SQL Server sends browse-mode TABNAME (0xA4) and COLINFO (0xA5) tokens with
//   the rows of every cursor FETCH that returns rows to the client. tedious 20
//   has no parser for either and fails the connection with "Unknown type: 164".
//   Both carry a USHORT byte length; the hook decodes them as data for the
//   outstanding request and continues with the next token. Every nested length
//   is checked against the remaining payload; a payload that does not decode
//   exactly is kept as bounded hex instead of as metadata.
// - tedious's DONE/DONEPROC/DONEINPROC events expose only the row count (when
//   DONE_COUNT is set) and MORE, and drop the other status bits (ERROR, INXACT,
//   ATTN, SRVERROR) and CurCmd. The hook records each DONE body (TDS 7.2+:
//   USHORT status, USHORT CurCmd, ULONGLONG row count) verbatim before tedious
//   parses it unchanged.
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const browseTypes = new Map([[0xA4, 'TABNAME'], [0xA5, 'COLINFO']])
const doneTypes = new Map([[0xFD, 'done'], [0xFE, 'doneProc'], [0xFF, 'doneInProc']])
let tokenSink = null
let strayBrowseTokens = 0
function decodeBrowse(type, bytes) {
  let at = 0
  const take = n => {
    if (!Number.isInteger(n) || n < 0 || at + n > bytes.length) throw new RangeError('field exceeds token payload')
    const start = at
    at += n
    return start
  }
  const u8 = () => bytes.readUInt8(take(1))
  const u16 = () => bytes.readUInt16LE(take(2))
  const utf16 = chars => { const start = take(chars * 2); return bytes.toString('utf16le', start, start + chars * 2) }
  try {
    if (type === 0xA4) {
      const tables = []
      while (at < bytes.length) {
        const count = u8()
        const parts = []
        for (let i = 0; i < count; i++) parts.push(utf16(u16()))
        tables.push(parts)
      }
      return { token: 'TABNAME', tables }
    }
    const columns = []
    while (at < bytes.length) {
      const column = { column: u8(), table: u8(), status: u8() }
      if (column.status & 0x20) column.name = utf16(u8())
      columns.push(column)
    }
    return { token: 'COLINFO', columns }
  } catch (error) {
    if (!(error instanceof RangeError)) throw error
    return { token: browseTypes.get(type), undecoded: true, hex: bytes.subarray(0, 512).toString('hex'), length: bytes.length }
  }
}
const readToken = StreamParser.prototype.readToken
StreamParser.prototype.readToken = function (type) {
  if (browseTypes.has(type)) return skipBrowse(this, type)
  if (doneTypes.has(type)) return recordDone(this, type)
  return readToken.call(this, type)
}
function recordDone(parser, type) {
  if (!(parser.options.tdsVersion >= '7_2')) throw new Error(`unexpected TDS version ${parser.options.tdsVersion}`)
  const at = parser.position
  if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordDone(parser, type))
  if (tokenSink) {
    if (tokenSink.done.length < LIMITS.messages) tokenSink.done.push({
      kind: doneTypes.get(type),
      status: parser.buffer.readUInt16LE(at),
      curCmd: parser.buffer.readUInt16LE(at + 2),
      rowCount: parser.buffer.readBigUInt64LE(at + 4).toString(),
    })
    else tokenSink.overflow++
  }
  return readToken.call(parser, type)
}
function skipBrowse(parser, type) {
  const at = parser.position
  if (parser.buffer.length < at + 2 || parser.buffer.length < at + 2 + parser.buffer.readUInt16LE(at)) return parser.waitForChunk().then(() => skipBrowse(parser, type))
  const length = parser.buffer.readUInt16LE(at)
  const token = decodeBrowse(type, Buffer.from(parser.buffer.subarray(at + 2, at + 2 + length)))
  parser.position = at + 2 + length
  if (!tokenSink) strayBrowseTokens++
  else if (tokenSink.browse.length < LIMITS.messages) tokenSink.browse.push(token)
  else tokenSink.overflow++
  return nextToken(parser)
}
// Attaches the hooked tokens to a finished request or phase. `done` holds the
// verbatim DONE bodies; tedious's own done events are only counted, as a
// consistency check.
function attachTokens(result, sink, doneEvents) {
  result.done = sink.done
  if (sink.browse.length) result.browse = sink.browse
  if (sink.overflow) { result.truncated ??= { rows: 0, messages: 0 }; result.truncated.messages += sink.overflow }
  if (doneEvents !== sink.done.length + sink.overflow) result.doneEventMismatch = { events: doneEvents, tokens: sink.done.length + sink.overflow }
}
const newSink = () => ({ browse: [], done: [], overflow: 0 })
function nextToken(parser) {
  if (parser.buffer.length < parser.position + 1) return parser.waitForChunk().then(() => nextToken(parser))
  const type = parser.buffer.readUInt8(parser.position)
  parser.position += 1
  return parser.readToken(type)
}

const fixture = new URL('../reference/cursor.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/cursor-reference-v1/capture.json')

// serverName is omitted: it is the container host name and differs per container.
const messageFields = m => ({ number: m.number, state: m.state, class: m.class, lineNumber: m.lineNumber, procName: m.procName ?? null, message: m.message })
const columnFields = c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })

// One batch, sp_executesql RPC or stored procedure RPC on a fresh Request.
// tedious keeps the latest RETURNSTATUS on connection.procReturnStatusValue and
// clears it only on DONEPROC, so a status left by an EXEC inside an earlier
// batch would be reported by this request's doneProc. The carried value is
// cleared before and after the request; returnStatus records only a status that
// arrives with a DONEPROC during this request and otherwise stays null.
function execute(connection, sql, { kind = 'batch', parameters = [] } = {}) {
  return new Promise(done => {
    const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
    const overflow = what => { result.truncated ??= { rows: 0, messages: 0 }; result.truncated[what]++ }
    const message = list => token => {
      if (result.errors.length + result.info.length >= LIMITS.messages) overflow('messages')
      else result[list].push(messageFields(token))
    }
    const onError = message('errors')
    const onInfo = message('info')
    const sink = newSink()
    let doneEvents = 0
    const finish = (error, rowCount) => {
      tokenSink = null
      attachTokens(result, sink, doneEvents)
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      connection.procReturnStatusValue = undefined
      result.rowCount = rowCount
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      done(result)
    }
    const request = new Request(sql, finish)
    for (const [name, type, value] of parameters) request.addParameter(name, type, value)
    request.on('columnMetadata', columns => result.sets.push({ columns: columns.map(columnFields), rows: [] }))
    request.on('row', columns => {
      const set = result.sets.at(-1)
      if (!set || set.rows.length >= LIMITS.rows) overflow('rows')
      else set.rows.push(columns.map(c => c.value))
    })
    for (const name of ['done', 'doneInProc', 'doneProc']) request.on(name, () => { doneEvents++ })
    request.on('doneProc', (_count, _more, status) => { if (status !== undefined) result.returnStatus = status })
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    connection.procReturnStatusValue = undefined
    tokenSink = sink
    try {
      if (kind === 'batch') connection.execSqlBatch(request)
      else if (kind === 'rpc') connection.execSql(request)
      else if (kind === 'procedure') connection.callProcedure(request)
      else throw new Error(`unknown request kind ${kind}`)
    } catch (error) { finish(error, undefined) }
  })
}

// Replica of capturePrepared from owner PR #312 (scripts/lib/reference.mjs on
// work/prepared-capture-helper-v1), with procName added to messages. One
// reusable Request runs sp_prepare, one sp_execute per value set and
// sp_unprepare. sp_prepare completes through the 'prepared'/'error' events, not
// the callback. request.error is cleared before each phase and an error object
// already attributed to an earlier phase is never reported again, so only
// errors raised during each execution are recorded. The carried return status
// is cleared before each phase and recorded only when it arrives in the phase.
async function capturePrepared(connection, sql, declarations, valueSets) {
  let result
  let complete = () => {}
  const reported = new Set()
  const fresh = () => ({ sets: [], done: [], errors: [], info: [], returnStatus: null })
  const overflow = what => { result.truncated ??= { rows: 0, messages: 0 }; result.truncated[what]++ }
  const message = list => token => {
    if (!result) return
    if (result.errors.length + result.info.length >= LIMITS.messages) overflow('messages')
    else result[list].push(messageFields(token))
  }
  const onError = message('errors')
  const onInfo = message('info')
  const request = new Request(sql, (error, rowCount) => complete(error, rowCount))
  for (const [name, type] of declarations) request.addParameter(name, type)
  request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(columnFields), rows: [] }))
  request.on('row', columns => {
    if (!result) return
    const set = result.sets.at(-1)
    if (!set || set.rows.length >= LIMITS.rows) overflow('rows')
    else set.rows.push(columns.map(c => c.value))
  })
  let sink = newSink()
  let doneEvents = 0
  for (const name of ['done', 'doneInProc', 'doneProc']) request.on(name, () => { if (result) doneEvents++ })
  request.on('doneProc', (_count, _more, status) => { if (result && status !== undefined) result.returnStatus = status })
  const phase = start => new Promise(settle => {
    result = fresh()
    sink = newSink()
    doneEvents = 0
    tokenSink = sink
    complete = (error, rowCount) => {
      complete = () => {}
      const finished = result
      result = undefined
      tokenSink = null
      attachTokens(finished, sink, doneEvents)
      finished.rowCount = rowCount
      if (error && !reported.has(error) && !finished.errors.length) finished.errors.push({ message: error.message, number: error.number ?? null })
      if (error) reported.add(error)
      settle(finished)
    }
    request.error = undefined
    connection.procReturnStatusValue = undefined
    start()
  })
  connection.on('errorMessage', onError)
  connection.on('infoMessage', onInfo)
  const onPrepared = () => complete(undefined, undefined)
  const onPrepareError = error => complete(error, undefined)
  try {
    const prepare = await phase(() => {
      request.once('prepared', onPrepared)
      request.once('error', onPrepareError)
      connection.prepare(request)
    })
    request.off('prepared', onPrepared)
    request.off('error', onPrepareError)
    if (request.handle === undefined) return { prepare, prepared: false, executions: [], unprepare: null }
    const executions = []
    for (const values of valueSets) executions.push({ values, result: await phase(() => connection.execute(request, values)) })
    const unprepare = await phase(() => connection.unprepare(request))
    return { prepare, prepared: true, executions, unprepare }
  } finally {
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
    connection.procReturnStatusValue = undefined
  }
}

const cursors = 'SELECT name, properties, is_open FROM sys.dm_exec_cursors(@@SPID) ORDER BY name'
const status = s => `SELECT @@FETCH_STATUS AS fetch_status /*${s}*/`
const itemsBaseline = "DELETE dbo.cur_items; INSERT dbo.cur_items(id,name,qty) VALUES(1,'a',10),(2,'b',20),(3,'c',30),(4,'d',40),(5,'e',50)"

// Visibility of another session's committed changes after OPEN and one FETCH.
const visibilityTypes = [
  ['static', 'STATIC'],
  ['keyset', 'KEYSET'],
  ['dynamic', 'DYNAMIC'],
  ['fast forward', 'FAST_FORWARD'],
  ['default forward only', 'FORWARD_ONLY'],
]

async function observe(connection, config) {
  const records = []
  let secondary
  // Every batch gets a unique trailing comment so no two steps share statement
  // text (and plan cache entries). The two 'rpc local cursor' executions share
  // text intentionally; procedure calls send only the procedure name.
  // variesByContainer: fields retained raw but excluded (only) from the
  // reproducibility comparison; see VARYING below.
  async function record(name, sql, { session = 'primary', kind = 'batch', parameters, shared = false, variesByContainer } = {}) {
    assert(!name.includes('*/'))
    const target = session === 'primary' ? connection : secondary
    const text = kind === 'procedure' || shared ? sql : `${sql} /*${name}*/`
    const typed = parameters?.map(([p, type, value]) => [p, TYPES[type], value])
    const result = canonical(await execute(target, text, { kind, parameters: typed }))
    records.push({ name, session, kind, sql: text, ...(parameters ? { parameters: canonical(parameters) } : {}), ...(variesByContainer ? { variesByContainer } : {}), result })
    return result
  }
  async function prepared(name, sql, declarations, valueSets) {
    const text = `${sql} /*${name}*/`
    const outcome = await capturePrepared(connection, text, declarations.map(([p, type]) => [p, TYPES[type]]), valueSets)
    records.push({ name, session: 'primary', kind: 'prepared', sql: text, declarations, result: canonical(outcome) })
  }
  try {
    await record('initial globals', 'SELECT @@CURSOR_ROWS AS cursor_rows, @@FETCH_STATUS AS fetch_status')
    await record('database cursor options', 'SELECT is_local_cursor_default, is_cursor_close_on_commit_on FROM sys.databases WHERE database_id = DB_ID()')
    await record('create items', 'CREATE TABLE dbo.cur_items(id INT NOT NULL CONSTRAINT pk_cur_items PRIMARY KEY CLUSTERED, name VARCHAR(10) NOT NULL, qty INT NOT NULL)')
    await record('fill items', itemsBaseline)
    await record('create heap', "CREATE TABLE dbo.cur_heap(id INT NOT NULL, name VARCHAR(10) NOT NULL); INSERT dbo.cur_heap(id,name) VALUES(1,'x'),(2,'y')")
    await record('create visibility table', 'CREATE TABLE dbo.cur_vis(id INT NOT NULL CONSTRAINT pk_cur_vis PRIMARY KEY CLUSTERED, val VARCHAR(10) NOT NULL)')

    // Default declaration and forward-only lifecycle on one global cursor.
    await record('default declare', `DECLARE c_default CURSOR FOR SELECT id, name FROM dbo.cur_items ORDER BY id; SELECT @@CURSOR_ROWS AS cursor_rows, @@FETCH_STATUS AS fetch_status, CURSOR_STATUS('global','c_default') AS global_status, CURSOR_STATUS('local','c_default') AS local_status; ${cursors}`)
    await record('default open fetch without into', `OPEN c_default; SELECT @@CURSOR_ROWS AS cursor_rows, CURSOR_STATUS('global','c_default') AS global_status; FETCH NEXT FROM c_default; ${status(1)}; FETCH c_default; ${status(2)}; ${cursors}`)
    await record('default prior rejected', `FETCH PRIOR FROM c_default; ${status(1)}`)
    await record('default loop into variables', `DECLARE @id INT, @name VARCHAR(10), @seen VARCHAR(100) = '';
FETCH NEXT FROM c_default INTO @id, @name;
WHILE @@FETCH_STATUS = 0
BEGIN
  SET @seen += CONCAT(@id, ':', @name, ';');
  FETCH NEXT FROM c_default INTO @id, @name;
END
SELECT @seen AS seen, @@FETCH_STATUS AS fetch_status, @id AS last_id, @name AS last_name, CURSOR_STATUS('global','c_default') AS global_status`)
    await record('default fetch past end', `FETCH NEXT FROM c_default; ${status(1)}; FETCH NEXT FROM c_default; ${status(2)}`)
    await record('default close', `CLOSE c_default; SELECT CURSOR_STATUS('global','c_default') AS global_status, @@CURSOR_ROWS AS cursor_rows, @@FETCH_STATUS AS fetch_status`)
    await record('default fetch closed', `FETCH NEXT FROM c_default; ${status(1)}`)
    await record('default close closed', 'CLOSE c_default')
    await record('default reopen', `OPEN c_default; FETCH NEXT FROM c_default; SELECT @@CURSOR_ROWS AS cursor_rows; ${status(1)}`)
    await record('default open open', `OPEN c_default; ${status(1)}`)
    await record('default deallocate open', `DEALLOCATE c_default; SELECT CURSOR_STATUS('global','c_default') AS global_status, @@CURSOR_ROWS AS cursor_rows, @@FETCH_STATUS AS fetch_status; ${cursors}`)
    await record('default fetch deallocated', 'FETCH NEXT FROM c_default')
    await record('default deallocate missing', 'DEALLOCATE c_default')
    await record('default open missing', 'OPEN c_default')
    await record('duplicate declare', "DECLARE c_dup CURSOR GLOBAL FOR SELECT 1 AS one; DECLARE c_dup CURSOR GLOBAL FOR SELECT 2 AS two; SELECT CURSOR_STATUS('global','c_dup') AS global_status")
    await record('duplicate after', `SELECT CURSOR_STATUS('global','c_dup') AS global_status; OPEN c_dup; FETCH NEXT FROM c_dup; DEALLOCATE c_dup`)

    // Scroll navigation.
    const nav = ['LAST', 'PRIOR', 'FIRST', 'PRIOR', 'NEXT', 'ABSOLUTE 3', 'ABSOLUTE -2', 'RELATIVE -1', 'RELATIVE 0', 'RELATIVE 10', 'PRIOR', 'ABSOLUTE 0', 'NEXT', 'ABSOLUTE 99', 'ABSOLUTE -99', 'NEXT']
    await record('scroll static navigation', `DECLARE c_scroll CURSOR LOCAL SCROLL STATIC FOR SELECT id, name FROM dbo.cur_items ORDER BY id;
OPEN c_scroll;
SELECT @@CURSOR_ROWS AS cursor_rows;
${nav.map((f, i) => `FETCH ${f} FROM c_scroll; ${status(`${i} ${f}`)};`).join('\n')}`)
    await record('scroll keyset into variables', `DECLARE c_vars CURSOR LOCAL SCROLL KEYSET FOR SELECT id, name, qty FROM dbo.cur_items ORDER BY id;
DECLARE @id INT = -1, @name VARCHAR(10) = 'unset', @qty INT = -1, @n INT = 2, @back SMALLINT = -1;
OPEN c_vars;
SELECT @@CURSOR_ROWS AS cursor_rows;
FETCH ABSOLUTE @n FROM c_vars INTO @id, @name, @qty; SELECT 'absolute @n' AS step, @@FETCH_STATUS AS fetch_status, @id AS id, @name AS name, @qty AS qty;
FETCH RELATIVE @back FROM c_vars INTO @id, @name, @qty; SELECT 'relative @back' AS step, @@FETCH_STATUS AS fetch_status, @id AS id, @name AS name, @qty AS qty;
FETCH LAST FROM c_vars INTO @id, @name, @qty; SELECT 'last' AS step, @@FETCH_STATUS AS fetch_status, @id AS id, @name AS name, @qty AS qty;
FETCH ABSOLUTE 99 FROM c_vars INTO @id, @name, @qty; SELECT 'absolute 99' AS step, @@FETCH_STATUS AS fetch_status, @id AS id, @name AS name, @qty AS qty;
CLOSE c_vars; DEALLOCATE c_vars`)
    await record('into count mismatch', `DECLARE c_mismatch CURSOR LOCAL FOR SELECT id, name FROM dbo.cur_items ORDER BY id; DECLARE @id INT = -1; OPEN c_mismatch; FETCH NEXT FROM c_mismatch INTO @id; SELECT @@FETCH_STATUS AS fetch_status, @id AS id, CURSOR_STATUS('local','c_mismatch') AS local_status`)
    await record('into conversion failure', `DECLARE c_convert CURSOR LOCAL FOR SELECT id, name FROM dbo.cur_items ORDER BY id; DECLARE @id INT = -1, @bad INT = -1; OPEN c_convert; FETCH NEXT FROM c_convert INTO @id, @bad; SELECT @@FETCH_STATUS AS fetch_status, @id AS id, @bad AS bad`)
    await record('fast forward last rejected', `DECLARE c_ff CURSOR LOCAL FAST_FORWARD FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_ff; FETCH LAST FROM c_ff; ${status(1)}; FETCH NEXT FROM c_ff; ${status(2)}`)
    await record('forward only absolute rejected', `DECLARE c_fo CURSOR LOCAL FORWARD_ONLY STATIC FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_fo; FETCH ABSOLUTE 2 FROM c_fo; ${status(1)}; FETCH NEXT FROM c_fo; ${status(2)}`)
    await record('dynamic absolute rejected', `DECLARE c_dyn CURSOR LOCAL SCROLL DYNAMIC FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_dyn; FETCH ABSOLUTE 2 FROM c_dyn; ${status(1)}; FETCH RELATIVE 2 FROM c_dyn; ${status(2)}; FETCH LAST FROM c_dyn; ${status(3)}`)
    await record('fast forward scroll conflict', 'DECLARE c_conflict CURSOR LOCAL FAST_FORWARD SCROLL FOR SELECT id FROM dbo.cur_items')
    await record('forward only scroll conflict', 'DECLARE c_conflict2 CURSOR LOCAL FORWARD_ONLY SCROLL FOR SELECT id FROM dbo.cur_items')
    await record('static for update conflict', 'DECLARE c_conflict3 CURSOR LOCAL STATIC FOR SELECT id FROM dbo.cur_items FOR UPDATE')
    await record('read only for update conflict', 'DECLARE c_conflict4 CURSOR LOCAL READ_ONLY FOR SELECT id FROM dbo.cur_items FOR UPDATE')
    await record('iso insensitive scroll', `DECLARE c_iso INSENSITIVE SCROLL CURSOR FOR SELECT id, name FROM dbo.cur_items ORDER BY id FOR READ ONLY; OPEN c_iso; SELECT @@CURSOR_ROWS AS cursor_rows, CURSOR_STATUS('global','c_iso') AS global_status; FETCH LAST FROM c_iso; ${cursors}; CLOSE c_iso; DEALLOCATE c_iso`)
    await record('iso syntax with tsql option', 'DECLARE c_iso2 INSENSITIVE CURSOR LOCAL FOR SELECT id FROM dbo.cur_items', { variesByContainer: ['errors[].lineNumber'] })

    // Declared and effective cursor models, @@CURSOR_ROWS and CURSOR_STATUS per type.
    const models = [
      ['k_default', '', 'dbo.cur_items ORDER BY id'],
      ['k_static', 'STATIC', 'dbo.cur_items ORDER BY id'],
      ['k_keyset', 'KEYSET', 'dbo.cur_items ORDER BY id'],
      ['k_dynamic', 'DYNAMIC', 'dbo.cur_items ORDER BY id'],
      ['k_fast', 'FAST_FORWARD', 'dbo.cur_items ORDER BY id'],
      ['k_scroll', 'SCROLL', 'dbo.cur_items ORDER BY id'],
      ['k_for_update', 'SCROLL', 'dbo.cur_items ORDER BY id FOR UPDATE OF qty'],
      ['k_scroll_locks', 'SCROLL SCROLL_LOCKS', 'dbo.cur_items ORDER BY id'],
      ['k_keyset_heap', 'KEYSET TYPE_WARNING', 'dbo.cur_heap ORDER BY id'],
      ['k_dynamic_sort', 'DYNAMIC TYPE_WARNING', 'dbo.cur_items ORDER BY name'],
      ['k_static_empty', 'STATIC', 'dbo.cur_items WHERE id < 0'],
      ['k_dynamic_empty', 'DYNAMIC', 'dbo.cur_items WHERE id < 0'],
    ]
    await record('cursor models', `DECLARE @r TABLE(seq INT NOT NULL, cursor_name SYSNAME NOT NULL, declared_status SMALLINT, open_status SMALLINT, cursor_rows INT);
${models.map(([n, options, from], i) => `DECLARE ${n} CURSOR LOCAL ${options} FOR SELECT id, name FROM ${from};
INSERT @r VALUES(${i}, '${n}', CURSOR_STATUS('local','${n}'), NULL, NULL);
OPEN ${n};
UPDATE @r SET open_status = CURSOR_STATUS('local','${n}'), cursor_rows = @@CURSOR_ROWS WHERE seq = ${i};`).join('\n')}
SELECT seq, cursor_name, declared_status, open_status, cursor_rows FROM @r ORDER BY seq;
${cursors}`)

    // Positioned UPDATE and DELETE.
    await record('current of declare', `DECLARE c_upd CURSOR GLOBAL SCROLL KEYSET FOR SELECT id, name, qty FROM dbo.cur_items WHERE id <= 3 ORDER BY id FOR UPDATE OF qty; OPEN c_upd; ${cursors}`)
    await record('current of before fetch', 'UPDATE dbo.cur_items SET qty = -1 WHERE CURRENT OF c_upd')
    await record('current of update', `FETCH NEXT FROM c_upd; UPDATE dbo.cur_items SET qty = qty + 1 WHERE CURRENT OF c_upd; SELECT @@ROWCOUNT AS row_count; FETCH RELATIVE 0 FROM c_upd; ${status(1)}`)
    await record('current of column outside list', 'UPDATE dbo.cur_items SET name = \'z\' WHERE CURRENT OF c_upd')
    await record('current of other table', 'UPDATE dbo.cur_heap SET name = \'q\' WHERE CURRENT OF c_upd')
    await record('current of delete', `FETCH NEXT FROM c_upd; DELETE dbo.cur_items WHERE CURRENT OF c_upd; SELECT @@ROWCOUNT AS row_count; FETCH RELATIVE 0 FROM c_upd; ${status(1)}; FETCH NEXT FROM c_upd; ${status(2)}; FETCH PRIOR FROM c_upd; ${status(3)}`)
    await record('current of deleted row', 'UPDATE dbo.cur_items SET qty = 0 WHERE CURRENT OF c_upd')
    await record('current of global qualifier', `FETCH FIRST FROM GLOBAL c_upd; DELETE dbo.cur_items WHERE CURRENT OF GLOBAL c_upd; SELECT @@ROWCOUNT AS row_count`)
    await record('current of table state', `SELECT id, name, qty FROM dbo.cur_items ORDER BY id; CLOSE c_upd; DEALLOCATE c_upd`)
    await record('restore items after positioned', itemsBaseline)
    await record('current of read only', `DECLARE c_ro CURSOR LOCAL KEYSET READ_ONLY FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_ro; FETCH NEXT FROM c_ro; UPDATE dbo.cur_items SET qty = 0 WHERE CURRENT OF c_ro; SELECT @@ROWCOUNT AS row_count`)
    await record('current of static', `DECLARE c_st CURSOR LOCAL STATIC FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_st; FETCH NEXT FROM c_st; DELETE dbo.cur_items WHERE CURRENT OF c_st; SELECT @@ROWCOUNT AS row_count`)
    await record('current of fast forward', `DECLARE c_fw CURSOR LOCAL FAST_FORWARD FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_fw; FETCH NEXT FROM c_fw; DELETE dbo.cur_items WHERE CURRENT OF c_fw; SELECT @@ROWCOUNT AS row_count`)
    await record('current of dynamic update', `DECLARE c_du CURSOR LOCAL DYNAMIC FOR SELECT id, qty FROM dbo.cur_items ORDER BY id; OPEN c_du; FETCH NEXT FROM c_du; FETCH NEXT FROM c_du; UPDATE dbo.cur_items SET qty = 200 WHERE CURRENT OF c_du; SELECT @@ROWCOUNT AS row_count; SELECT id, qty FROM dbo.cur_items ORDER BY id`)
    await record('restore items after dynamic', itemsBaseline)

    // Cursor variables.
    await record('cursor variable lifecycle', `DECLARE @cv CURSOR;
SELECT CURSOR_STATUS('variable','@cv') AS unassigned;
SET @cv = CURSOR LOCAL SCROLL STATIC FOR SELECT id, name FROM dbo.cur_items WHERE id <= 2 ORDER BY id;
SELECT CURSOR_STATUS('variable','@cv') AS assigned;
OPEN @cv;
SELECT CURSOR_STATUS('variable','@cv') AS opened, @@CURSOR_ROWS AS cursor_rows;
FETCH LAST FROM @cv; ${status(1)};
CLOSE @cv;
SELECT CURSOR_STATUS('variable','@cv') AS closed;
DEALLOCATE @cv;
SELECT CURSOR_STATUS('variable','@cv') AS deallocated`)
    await record('cursor variable aliases named', `DECLARE c_named CURSOR LOCAL FOR SELECT id FROM dbo.cur_items WHERE id <= 2 ORDER BY id;
DECLARE @alias CURSOR;
SET @alias = c_named;
OPEN c_named;
FETCH NEXT FROM @alias;
SELECT CURSOR_STATUS('variable','@alias') AS alias_status, CURSOR_STATUS('local','c_named') AS named_status;
DEALLOCATE @alias;
SELECT CURSOR_STATUS('variable','@alias') AS alias_status, CURSOR_STATUS('local','c_named') AS named_status;
FETCH NEXT FROM c_named;
DEALLOCATE c_named;
SELECT CURSOR_STATUS('local','c_named') AS named_status`)
    await record('cursor variable unassigned open', "DECLARE @cv CURSOR; OPEN @cv; SELECT CURSOR_STATUS('variable','@cv') AS variable_status")
    await record('cursor variable positioned update', `DECLARE @cv CURSOR; SET @cv = CURSOR LOCAL KEYSET FOR SELECT id, qty FROM dbo.cur_items WHERE id = 5 FOR UPDATE; OPEN @cv; FETCH NEXT FROM @cv; UPDATE dbo.cur_items SET qty = 55 WHERE CURRENT OF @cv; SELECT @@ROWCOUNT AS row_count; SELECT id, qty FROM dbo.cur_items WHERE id = 5; DEALLOCATE @cv`)
    await record('restore items after variable', itemsBaseline)

    // Scope across batches.
    await record('global across batches first', "DECLARE c_across CURSOR GLOBAL FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_across; FETCH NEXT FROM c_across")
    await record('global across batches second', `FETCH NEXT FROM c_across; SELECT @@FETCH_STATUS AS fetch_status, CURSOR_STATUS('global','c_across') AS global_status; CLOSE c_across; DEALLOCATE c_across`)
    await record('local across batches first', "DECLARE c_local CURSOR LOCAL FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_local; FETCH NEXT FROM c_local; SELECT CURSOR_STATUS('local','c_local') AS local_status")
    await record('local across batches second', "SELECT CURSOR_STATUS('local','c_local') AS local_status, CURSOR_STATUS('global','c_local') AS global_status; FETCH NEXT FROM c_local")
    await record('local and global same name', `DECLARE c_both CURSOR GLOBAL FOR SELECT 'global' AS origin;
DECLARE c_both CURSOR LOCAL FOR SELECT 'local' AS origin;
OPEN c_both; FETCH NEXT FROM c_both;
SELECT CURSOR_STATUS('local','c_both') AS local_status, CURSOR_STATUS('global','c_both') AS global_status;
OPEN GLOBAL c_both; FETCH NEXT FROM GLOBAL c_both;
SELECT CURSOR_STATUS('local','c_both') AS local_status, CURSOR_STATUS('global','c_both') AS global_status;
DEALLOCATE GLOBAL c_both`)
    await record('cursor default local declare', "ALTER DATABASE CURRENT SET CURSOR_DEFAULT LOCAL; DECLARE c_default_local CURSOR FOR SELECT 1 AS one; SELECT CURSOR_STATUS('local','c_default_local') AS local_status, CURSOR_STATUS('global','c_default_local') AS global_status")
    await record('cursor default local next batch', "SELECT CURSOR_STATUS('local','c_default_local') AS local_status, CURSOR_STATUS('global','c_default_local') AS global_status; DECLARE c_default_local2 CURSOR FOR SELECT 2 AS two; SELECT CURSOR_STATUS('local','c_default_local2') AS local_status, CURSOR_STATUS('global','c_default_local2') AS global_status")
    await record('cursor default local after', "SELECT CURSOR_STATUS('local','c_default_local2') AS local_status, CURSOR_STATUS('global','c_default_local2') AS global_status, CURSOR_STATUS('global','c_default_local') AS first_global_status")
    await record('cursor default restore', 'ALTER DATABASE CURRENT SET CURSOR_DEFAULT GLOBAL; SELECT is_local_cursor_default FROM sys.databases WHERE database_id = DB_ID()')
    await record('cursor default local gone after batch', 'DEALLOCATE c_default_local')

    // Scope across procedures and dynamic SQL.
    await record('create local procedure', `CREATE PROCEDURE dbo.cur_local_proc AS
BEGIN
  DECLARE c_proc_local CURSOR LOCAL FOR SELECT id FROM dbo.cur_items WHERE id <= 2 ORDER BY id;
  OPEN c_proc_local;
  FETCH NEXT FROM c_proc_local;
  SELECT CURSOR_STATUS('local','c_proc_local') AS in_procedure, @@FETCH_STATUS AS fetch_status;
  RETURN 7;
END`)
    await record('create global procedure', `CREATE PROCEDURE dbo.cur_global_proc AS
BEGIN
  DECLARE c_proc_global CURSOR GLOBAL FOR SELECT id FROM dbo.cur_items WHERE id <= 2 ORDER BY id;
  OPEN c_proc_global;
  RETURN 3;
END`)
    await record('create output procedure', `CREATE PROCEDURE dbo.cur_output_proc @out CURSOR VARYING OUTPUT AS
BEGIN
  SET @out = CURSOR FORWARD_ONLY STATIC FOR SELECT id, name FROM dbo.cur_items WHERE id BETWEEN 2 AND 3 ORDER BY id;
  OPEN @out;
END`)
    await record('create probe procedure', `CREATE PROCEDURE dbo.cur_probe_proc AS
BEGIN
  FETCH NEXT FROM c_scope_probe;
  SELECT @@FETCH_STATUS AS fetch_status, CURSOR_STATUS('global','c_scope_probe') AS global_status, CURSOR_STATUS('local','c_scope_probe') AS local_status;
END`)
    await record('local procedure cursor rpc', 'dbo.cur_local_proc', { kind: 'procedure' })
    await record('local procedure cursor after', "SELECT CURSOR_STATUS('local','c_proc_local') AS local_status, CURSOR_STATUS('global','c_proc_local') AS global_status, @@FETCH_STATUS AS fetch_status; FETCH NEXT FROM c_proc_local")
    await record('global procedure cursor batch', "EXEC dbo.cur_global_proc; SELECT CURSOR_STATUS('global','c_proc_global') AS global_status; FETCH NEXT FROM c_proc_global; FETCH NEXT FROM c_proc_global; CLOSE c_proc_global; DEALLOCATE c_proc_global")
    await record('global procedure cursor rpc', 'dbo.cur_global_proc', { kind: 'procedure' })
    await record('global procedure cursor rpc after', "SELECT CURSOR_STATUS('global','c_proc_global') AS global_status; DEALLOCATE c_proc_global")
    await record('output cursor parameter', `DECLARE @c CURSOR; EXEC dbo.cur_output_proc @out = @c OUTPUT; SELECT CURSOR_STATUS('variable','@c') AS variable_status, @@CURSOR_ROWS AS cursor_rows; FETCH NEXT FROM @c; FETCH NEXT FROM @c; FETCH NEXT FROM @c; ${status(1)}; DEALLOCATE @c`)
    await record('output cursor without output keyword', "DECLARE @c CURSOR; EXEC dbo.cur_output_proc @out = @c; SELECT CURSOR_STATUS('variable','@c') AS variable_status")
    await record('caller local cursor hidden from procedure', "DECLARE c_scope_probe CURSOR LOCAL FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_scope_probe; EXEC dbo.cur_probe_proc; SELECT CURSOR_STATUS('local','c_scope_probe') AS local_status")
    await record('caller global cursor visible to procedure', "DECLARE c_scope_probe CURSOR GLOBAL FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_scope_probe; EXEC dbo.cur_probe_proc; FETCH NEXT FROM c_scope_probe; CLOSE c_scope_probe; DEALLOCATE c_scope_probe")
    await record('dynamic sql local cursor', "EXEC sp_executesql N'DECLARE c_dyn_local CURSOR LOCAL FOR SELECT 1 AS one; OPEN c_dyn_local; FETCH NEXT FROM c_dyn_local'; SELECT CURSOR_STATUS('local','c_dyn_local') AS local_status, CURSOR_STATUS('global','c_dyn_local') AS global_status")
    await record('dynamic sql global cursor', "EXEC sp_executesql N'DECLARE c_dyn_global CURSOR GLOBAL FOR SELECT 2 AS two; OPEN c_dyn_global'; SELECT CURSOR_STATUS('global','c_dyn_global') AS global_status; FETCH NEXT FROM c_dyn_global; DEALLOCATE c_dyn_global")
    await record('dynamic sql sees outer local', "DECLARE c_outer_local CURSOR LOCAL FOR SELECT 3 AS three; OPEN c_outer_local; EXEC sp_executesql N'FETCH NEXT FROM c_outer_local'")
    const rpcLocal = 'DECLARE c_rpc CURSOR LOCAL STATIC FOR SELECT id FROM dbo.cur_items WHERE id >= @min ORDER BY id; OPEN c_rpc; SELECT @@CURSOR_ROWS AS cursor_rows; FETCH NEXT FROM c_rpc /*rpc local cursor*/'
    await record('rpc local cursor first', rpcLocal, { kind: 'rpc', parameters: [['min', 'Int', 4]], shared: true })
    await record('rpc local cursor second', rpcLocal, { kind: 'rpc', parameters: [['min', 'Int', 2]], shared: true })
    await record('rpc local cursor after', "SELECT CURSOR_STATUS('local','c_rpc') AS local_status, CURSOR_STATUS('global','c_rpc') AS global_status")
    await prepared('prepared local cursor', 'DECLARE c_prep_local CURSOR LOCAL STATIC FOR SELECT id FROM dbo.cur_items WHERE id >= @min ORDER BY id; OPEN c_prep_local; SELECT @@CURSOR_ROWS AS cursor_rows; FETCH NEXT FROM c_prep_local', [['min', 'Int']], [{ min: 4 }, { min: 2 }])
    await prepared('prepared global cursor', 'DECLARE c_prep_global CURSOR GLOBAL STATIC FOR SELECT id FROM dbo.cur_items WHERE id >= @min ORDER BY id; OPEN c_prep_global; SELECT @@CURSOR_ROWS AS cursor_rows; FETCH NEXT FROM c_prep_global', [['min', 'Int']], [{ min: 4 }, { min: 2 }, { min: 1 }])
    await prepared('prepared control batch', 'DECLARE @copy INT = @min; SELECT @copy AS copy', [['min', 'Int']], [{ min: 4 }])
    await record('prepared global cursor after', "SELECT CURSOR_STATUS('global','c_prep_global') AS global_status, @@CURSOR_ROWS AS cursor_rows; FETCH NEXT FROM c_prep_global; DEALLOCATE c_prep_global")

    // Transactions with CURSOR_CLOSE_ON_COMMIT OFF.
    await record('static cursor across commit and rollback', `DECLARE c_tx CURSOR LOCAL STATIC FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_tx;
BEGIN TRANSACTION; FETCH NEXT FROM c_tx; COMMIT TRANSACTION; SELECT CURSOR_STATUS('local','c_tx') AS after_commit;
FETCH NEXT FROM c_tx; ${status(1)};
BEGIN TRANSACTION; FETCH NEXT FROM c_tx; ROLLBACK TRANSACTION; SELECT CURSOR_STATUS('local','c_tx') AS after_rollback;
FETCH NEXT FROM c_tx; ${status(2)}`)
    await record('keyset cursor across rollback', `DECLARE c_tx2 CURSOR LOCAL KEYSET FOR SELECT id FROM dbo.cur_items ORDER BY id; OPEN c_tx2;
BEGIN TRANSACTION; FETCH NEXT FROM c_tx2; ROLLBACK TRANSACTION; SELECT CURSOR_STATUS('local','c_tx2') AS after_rollback;
FETCH NEXT FROM c_tx2; ${status(1)}`)

    // Visibility of another session's committed modifications, one cursor type
    // at a time. Steps alternate strictly between the two sessions; each step
    // completes before the next starts, and the secondary session uses a lock
    // timeout so an unexpected block is recorded as an error, not a hang.
    const db = (await execute(connection, 'SELECT DB_NAME() AS db')).sets[0].rows[0][0]
    secondary = await connect({ ...config, options: { ...config.options, database: db } })
    await record('secondary lock timeout', 'SET LOCK_TIMEOUT 5000; SELECT @@LOCK_TIMEOUT AS lock_timeout', { session: 'secondary' })
    for (const [label, options] of visibilityTypes) {
      const name = `c_vis_${label.replaceAll(' ', '_')}`
      await record(`visibility ${label} reset`, "DELETE dbo.cur_vis; INSERT dbo.cur_vis(id,val) VALUES(10,'a'),(20,'b'),(30,'c'),(40,'d')", { session: 'secondary' })
      await record(`visibility ${label} open`, `DECLARE ${name} CURSOR GLOBAL ${options} FOR SELECT id, val FROM dbo.cur_vis ORDER BY id; OPEN ${name}; SELECT @@CURSOR_ROWS AS cursor_rows; FETCH NEXT FROM ${name}; ${status(1)}`)
      await record(`visibility ${label} modify`, "UPDATE dbo.cur_vis SET val = 'B' WHERE id = 20; DELETE dbo.cur_vis WHERE id = 30; UPDATE dbo.cur_vis SET id = 45 WHERE id = 40; INSERT dbo.cur_vis(id,val) VALUES(35,'new'),(5,'before')", { session: 'secondary' })
      await record(`visibility ${label} fetch`, `${[1, 2, 3, 4, 5].map(i => `FETCH NEXT FROM ${name}; ${status(i)};`).join('\n')}
SELECT @@CURSOR_ROWS AS cursor_rows; CLOSE ${name}; DEALLOCATE ${name}`)
    }
    await record('visibility final table', 'SELECT id, val FROM dbo.cur_vis ORDER BY id', { session: 'secondary' })
    await record('final cursors', cursors)
    return records
  } finally {
    if (secondary && !secondary.closed) await new Promise(done => { secondary.once('end', done); secondary.close() })
  }
}

function validate(run) {
  assert(Array.isArray(run) && run.length > 0)
  assert.equal(strayBrowseTokens, 0, 'browse tokens arrived outside a request')
  const byName = new Map(run.map(record => [record.name, record]))
  assert.equal(byName.size, run.length, 'duplicate step names')
  const texts = run.filter(r => r.kind === 'batch' || r.kind === 'prepared').map(r => r.sql)
  assert.equal(new Set(texts).size, texts.length, 'batch or prepared statement text repeated')
  const get = name => { const r = byName.get(name); assert(r, `missing ${name}`); return r.result }
  const rows = (name, index = 0) => get(name).sets[index].rows
  const errors = result => result.errors.map(e => e.number)
  const fetchStatuses = name => get(name).sets.filter(s => s.columns.length === 1 && s.columns[0].name === 'fetch_status').map(s => s.rows[0][0])
  for (const record of run) {
    const phases = record.kind === 'prepared'
      ? [record.result.prepare, ...record.result.executions.map(e => e.result), ...(record.result.unprepare ? [record.result.unprepare] : [])]
      : [record.result]
    for (const phase of phases) {
      assert(phase.done.length > 0, `${record.name}: no completion`)
      assert(!phase.truncated, `${record.name}: truncated`)
      assert(!phase.doneEventMismatch, `${record.name}: DONE tokens and tedious done events disagree`)
      for (const b of phase.browse ?? []) assert(!b.undecoded, `${record.name}: undecoded ${b.token}`)
    }
  }

  assert.deepEqual(rows('initial globals'), [[0, 0]])
  assert.deepEqual(rows('database cursor options'), [[false, false]])
  // A default cursor is a global optimistic dynamic cursor.
  assert.deepEqual(rows('default declare'), [[0, 0, -1, -3]])
  assert.deepEqual(rows('default declare', 1), [['c_default', 'TSQL | Dynamic | Optimistic | Global (0)', false]])
  // FETCH without INTO returns one row set per FETCH with a hidden ROWSTAT column and browse tokens.
  const firstFetch = get('default open fetch without into')
  assert.deepEqual(firstFetch.sets[0].rows, [[-1, 1]])
  assert.deepEqual(firstFetch.sets[1].columns.map(c => c.name), ['id', 'name', 'ROWSTAT'])
  assert.deepEqual(firstFetch.sets[1].rows, [[1, 'a', 1]])
  assert.deepEqual(firstFetch.browse[0], { token: 'TABNAME', tables: [['dbo', 'cur_items']] })
  assert.deepEqual(firstFetch.browse[1].columns.at(-1), { column: 3, table: 0, status: 0x14 })
  assert.deepEqual(errors(get('default prior rejected')), [16911])
  assert.deepEqual(fetchStatuses('default prior rejected'), [-1])
  assert.deepEqual(rows('default loop into variables'), [['3:c;4:d;5:e;', -1, 5, 'e', 1]])
  assert.deepEqual(fetchStatuses('default fetch past end'), [-1, -1])
  assert.deepEqual(get('default fetch past end').sets[0].rows, [])
  assert.deepEqual(rows('default close'), [[-1, 0, -1]])
  assert.deepEqual(errors(get('default fetch closed')), [16917])
  assert.deepEqual(errors(get('default close closed')), [16917])
  assert.deepEqual(errors(get('default open open')), [16905])
  assert.deepEqual(rows('default deallocate open'), [[-3, 0, 0]])
  for (const name of ['default fetch deallocated', 'default deallocate missing', 'default open missing']) assert.deepEqual(errors(get(name)), [16916], name)
  assert.deepEqual(errors(get('duplicate declare')), [16915])
  assert.deepEqual(rows('duplicate after', 1), [[1, 1]])

  // Scroll navigation: LAST PRIOR FIRST PRIOR NEXT ABS3 ABS-2 REL-1 REL0 REL10 PRIOR ABS0 NEXT ABS99 ABS-99 NEXT.
  assert.deepEqual(rows('scroll static navigation'), [[5]])
  assert.deepEqual(fetchStatuses('scroll static navigation'), [0, 0, 0, -1, 0, 0, 0, 0, 0, -1, 0, -1, 0, -1, -1, 0])
  const navigated = get('scroll static navigation').sets.filter(s => s.columns[0].name === 'id').map(s => s.rows[0]?.[0] ?? null)
  assert.deepEqual(navigated, [5, 4, 1, null, 1, 3, 4, 3, 3, null, 5, null, 1, null, null, 1])
  assert.deepEqual(get('scroll keyset into variables').sets.slice(1).map(s => s.rows[0]), [
    ['absolute @n', 0, 2, 'b', 20], ['relative @back', 0, 1, 'a', 10], ['last', 0, 5, 'e', 50], ['absolute 99', -1, 5, 'e', 50]])
  assert.deepEqual(errors(get('into count mismatch')), [16924])
  assert.deepEqual(rows('into count mismatch'), [[-1, -1, 1]])
  assert.deepEqual(errors(get('into conversion failure')), [245])
  assert.deepEqual(errors(get('fast forward last rejected')), [16911])
  assert.deepEqual(errors(get('forward only absolute rejected')), [16911])
  assert.deepEqual(errors(get('dynamic absolute rejected')), [16925])
  for (const name of ['fast forward scroll conflict', 'forward only scroll conflict', 'static for update conflict', 'read only for update conflict']) assert.deepEqual(errors(get(name)), [1048], name)
  assert.deepEqual(errors(get('iso syntax with tsql option')), [1049])
  assert(Number.isInteger(get('iso syntax with tsql option').errors[0].lineNumber))
  assert.deepEqual(rows('iso insensitive scroll', 2), [['c_iso', 'TSQL | Snapshot | Read Only | Global (0)', true]])

  // Declared versus effective model and @@CURSOR_ROWS.
  assert.deepEqual(rows('cursor models').map(r => [r[1], r[3], r[4]]), [
    ['k_default', 1, -1], ['k_static', 1, 5], ['k_keyset', 1, 5], ['k_dynamic', 1, -1], ['k_fast', 1, -1], ['k_scroll', 1, 5],
    ['k_for_update', 1, 5], ['k_scroll_locks', 1, 5], ['k_keyset_heap', 1, 2], ['k_dynamic_sort', 1, 5], ['k_static_empty', 0, 0], ['k_dynamic_empty', 1, -1]])
  const models = Object.fromEntries(rows('cursor models', 1).map(r => [r[0], r[1]]))
  assert.equal(models.k_keyset_heap, 'TSQL | Snapshot | Read Only | Local (0)')
  assert.equal(models.k_dynamic_sort, 'TSQL | Keyset | Optimistic | Local (0)')
  assert.equal(models.k_scroll, 'TSQL | Keyset | Optimistic | Local (0)')
  assert.equal(models.k_scroll_locks, 'TSQL | Keyset | Scroll Locks | Local (0)')
  assert.equal(models.k_fast, 'TSQL | Fast_Forward | Read Only | Local (0)')
  assert.deepEqual(get('cursor models').info.map(i => i.number), [16956, 16956])

  // Positioned modification.
  assert.deepEqual(errors(get('current of before fetch')), [16931])
  assert.deepEqual(get('current of update').sets[2].rows, [[1, 'a', 11, 1]])
  assert.deepEqual(errors(get('current of column outside list')), [16932])
  assert.deepEqual(errors(get('current of other table')), [16933])
  assert.deepEqual(get('current of delete').sets[2].rows, [[0, '          ', 0, 2]])
  assert.deepEqual(fetchStatuses('current of delete'), [-2, 0, -2])
  assert.deepEqual(errors(get('current of deleted row')), [16947])
  assert.deepEqual(rows('current of table state'), [[3, 'c', 30], [4, 'd', 40], [5, 'e', 50]])
  for (const name of ['current of read only', 'current of static', 'current of fast forward']) assert.deepEqual(errors(get(name)), [16929], name)
  assert.deepEqual(rows('current of dynamic update', 3)[1], [2, 200])

  // Cursor variables.
  assert.deepEqual(get('cursor variable lifecycle').sets.filter(s => s.columns[0].name !== 'id' && s.columns[0].name !== 'fetch_status').map(s => s.rows[0]), [[-2], [-1], [1, 2], [-1], [-2]])
  assert.deepEqual(get('cursor variable aliases named').sets.filter(s => s.columns[0].name !== 'id').map(s => s.rows[0]), [[1, 1], [-2, 1], [-3]])
  assert.deepEqual(errors(get('cursor variable unassigned open')), [16950])
  assert.deepEqual(rows('cursor variable positioned update', 2), [[5, 55]])

  // Scope.
  assert.deepEqual(rows('global across batches second'), [[2, 1]])
  assert.deepEqual(rows('local across batches second'), [[-3, -3]])
  assert.deepEqual(errors(get('local across batches second')), [16916])
  assert.deepEqual(get('local and global same name').sets.map(s => s.rows[0]), [['local', 1], [1, -1], ['global', 1], [1, 1]])
  assert.deepEqual(rows('cursor default local declare'), [[-1, -3]])
  assert.deepEqual(get('cursor default local next batch').sets.map(s => s.rows[0]), [[-3, -3], [-1, -3]])
  assert.deepEqual(errors(get('cursor default local gone after batch')), [16916])
  const localProc = get('local procedure cursor rpc')
  assert.equal(localProc.returnStatus, 7)
  assert.deepEqual(localProc.sets[1].rows, [[1, 0]])
  assert.deepEqual(errors(get('local procedure cursor after')), [16916])
  assert.equal(get('global procedure cursor batch').returnStatus, 3)
  assert.deepEqual(get('global procedure cursor batch').sets.map(s => s.rows[0]), [[1], [1, 1], [2, 1]])
  assert.equal(get('global procedure cursor rpc').returnStatus, 3)
  assert.equal(get('global procedure cursor rpc after').returnStatus, null)
  assert.deepEqual(get('output cursor parameter').sets.map(s => s.rows[0] ?? null), [[1, 2], [2, 'b', 1], [3, 'c', 1], null, [-1]])
  assert.deepEqual(rows('output cursor without output keyword'), [[-2]])
  const hidden = get('caller local cursor hidden from procedure')
  assert.deepEqual(hidden.errors.map(e => [e.number, e.procName]), [[16916, 'dbo.cur_probe_proc']])
  assert.equal(hidden.returnStatus, -6)
  assert.deepEqual(get('caller global cursor visible to procedure').sets.map(s => s.rows[0]), [[1, 1], [0, 1, -3], [2, 1]])
  assert.deepEqual(rows('dynamic sql local cursor', 1), [[-3, -3]])
  assert.deepEqual(rows('dynamic sql global cursor'), [[1]])
  assert.deepEqual(errors(get('dynamic sql sees outer local')), [16916])
  assert.equal(get('dynamic sql sees outer local').returnStatus, 16916)
  assert.deepEqual([rows('rpc local cursor first'), rows('rpc local cursor second')], [[[2]], [[4]]])
  assert.deepEqual(rows('rpc local cursor after'), [[-3, -3]])
  const preparedLocal = get('prepared local cursor')
  assert.equal(preparedLocal.prepared, true)
  assert.deepEqual(preparedLocal.executions.map(e => [errors(e.result), e.result.sets[0].rows[0][0], e.result.returnStatus]), [[[], 2, 0], [[], 4, 0]])
  const preparedGlobal = get('prepared global cursor')
  assert.deepEqual(preparedGlobal.executions.map(e => [errors(e.result), e.result.sets[1].rows, e.result.returnStatus]), [
    [[], [[4, 1]], 0], [[16915, 16905], [[5, 1]], -6], [[16915, 16905], [], -6]])
  assert.equal(preparedGlobal.unprepare.errors.length, 0)
  assert.deepEqual(rows('prepared global cursor after'), [[1, 2]])

  // CURSOR_CLOSE_ON_COMMIT OFF keeps static and keyset cursors open over COMMIT and ROLLBACK.
  assert.deepEqual(fetchStatuses('static cursor across commit and rollback'), [0, 0])
  assert.deepEqual(fetchStatuses('keyset cursor across rollback'), [0])

  // Another session's committed UPDATE 20, DELETE 30, key UPDATE 40->45, INSERT 35 and 5 after the first FETCH.
  const visible = label => get(`visibility ${label} fetch`).sets.filter(s => s.columns[0].name === 'id').map(s => s.rows[0] ?? null)
  assert.deepEqual(visible('static'), [[20, 'b', 1], [30, 'c', 1], [40, 'd', 1], null, null])
  assert.deepEqual(visible('keyset'), [[20, 'B', 1], [0, '          ', 2], [0, '          ', 2], null, null])
  assert.deepEqual(fetchStatuses('visibility keyset fetch'), [0, -2, -2, -1, -1])
  for (const label of ['dynamic', 'fast forward', 'default forward only']) {
    assert.deepEqual(visible(label), [[20, 'B', 1], [35, 'new', 1], [45, 'd', 1], null, null], label)
    assert.deepEqual(fetchStatuses(`visibility ${label} fetch`), [0, 0, 0, -1, -1], label)
  }
  for (const record of run) if (record.session === 'secondary') assert.equal(record.result.errors.length, 0, record.name)
  assert.deepEqual(rows('final cursors'), [])
}

// Resolves symlinks in the existing part of a path, so an alias of the fixture
// (or of its directory) compares equal even before the file exists. Same logic
// as refuseFixtureOutput in owner PR #312, which is not on main yet.
async function canonicalPath(path) {
  const absolute = resolve(path instanceof URL ? fileURLToPath(path) : path)
  try { return await realpath(absolute) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return resolve(await canonicalPath(dirname(absolute)), basename(absolute))
  }
}
// The scratch output is written unconditionally, so it must never alias the
// retained fixture. Both checks run before any container starts.
if (await canonicalPath(output) === await canonicalPath(fixture)) throw new Error('refusing to write capture output over retained fixture ' + fileURLToPath(fixture))
if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
      const run = await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 60000 } }, connection => observe(connection, config))
      validate(run)
      runs.push(run)
    }
    containers.push({ image: container.image, runs })
  })
}
// Every captured field is retained raw. A step may declare fields that vary
// between containers (variesByContainer); `varying` lists their raw values per
// container and database as evidence. Databases within one container are
// compared strictly on the raw capture; comparisons across containers and with
// the retained fixture exclude exactly the declared fields and nothing else.
const errorLineField = 'errors[].lineNumber'
function varyingValues(run) {
  return run.filter(r => r.variesByContainer).map(r => {
    assert.deepEqual(r.variesByContainer, [errorLineField], `${r.name}: unsupported varying field`)
    return { step: r.name, field: errorLineField, value: r.result.errors.map(e => e.lineNumber) }
  })
}
function comparable(run) {
  return run.map(r => r.variesByContainer
    ? { ...r, result: { ...r.result, errors: r.result.errors.map(e => ({ ...e, lineNumber: { kind: 'excluded from comparison' } })) } }
    : r)
}
const comparableCapture = capture => capture.containers.map(c => ({ image: c.image, runs: c.runs.map(comparable) }))
const varying = varyingValues(containers[0].runs[0]).map(({ step, field }) => ({
  step, field,
  values: containers.map(c => c.runs.map(run => varyingValues(run).find(v => v.step === step).value)),
}))
// The raw capture is written before comparison so a difference can be inspected.
const actual = { containers, varying }
await writeFile(output, JSON.stringify(actual) + '\n')
if (!oneDatabase) {
  for (const [index, { runs }] of containers.entries()) assertSameCapture(runs[0], runs[1], `cursor observations differ across fresh databases in container ${index}`)
  assertSameCapture(comparable(containers[0].runs[0]), comparable(containers[1].runs[0]), 'cursor observations differ across containers')
}
let retained
if (!oneDatabase) {
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(comparableCapture(actual), comparableCapture(retained), 'cursor observations differ from retained fixture')
  if (writeFixture) await writeNewFixture(fixture, actual)
}
for (const v of varying) console.log(`${v.step} ${v.field} per container/database: ${JSON.stringify(v.values)}`)
console.log(`Captured ${containers[0].runs[0].length} cursor observations` +
  (oneDatabase ? ' in one diagnostic database' : ' in four fresh databases across two containers') +
  (retained ? ' and matched retained fixture' : ''))
