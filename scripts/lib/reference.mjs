import { randomUUID } from 'node:crypto'
import { access, realpath, stat, writeFile } from 'node:fs/promises'
import { dirname, basename, resolve as resolvePath } from 'node:path'
import { fileURLToPath } from 'node:url'
import { isDeepStrictEqual } from 'node:util'
import { Connection, Request } from 'tedious'
import { canonical, capture } from './compatibility.mjs'

export function referenceConfig(env = process.env) {
  for (const name of ['MSSQL_REFERENCE_HOST','MSSQL_REFERENCE_USER','MSSQL_REFERENCE_PASSWORD']) {
    if (!env[name]) throw new Error(`${name} is required for --compare`)
  }
  const port = Number(env.MSSQL_REFERENCE_PORT ?? 1433)
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('MSSQL_REFERENCE_PORT must be an integer between 1 and 65535')
  const boolean = (name, fallback) => {
    if (env[name] === undefined) return fallback
    if (!['true','false'].includes(env[name])) throw new Error(`${name} must be true or false`)
    return env[name] === 'true'
  }
  return {
    server: env.MSSQL_REFERENCE_HOST,
    authentication: { type: 'default', options: { userName: env.MSSQL_REFERENCE_USER, password: env.MSSQL_REFERENCE_PASSWORD } },
    options: { port, database: 'master', encrypt: boolean('MSSQL_REFERENCE_ENCRYPT', true), trustServerCertificate: boolean('MSSQL_REFERENCE_TRUST_CERTIFICATE', false), connectTimeout: 15000, requestTimeout: 30000 }
  }
}

export async function connect(config) {
  const connection = new Connection(config)
  connection.on('error', () => {})
  try {
    await new Promise((resolve, reject) => connection.connect(error => error ? reject(error) : resolve()))
    return connection
  } catch (error) { connection.close(); throw error }
}

export async function command(connection, sql) {
  const result = await capture(connection, sql)
  if (result.errors.length) throw new Error(result.errors.map(e => e.message).join('; '))
  return result
}

// tedious derives a bound DATETIMEOFFSET parameter's offset (and Date values
// generally) from the client process time zone, so a capture taken on a
// Europe/Warsaw host differs from one taken on a UTC host. Call this at the top
// of a capture script, before creating connections or Date values. Node applies
// a runtime assignment to process.env.TZ to subsequent Date operations.
export function useUtcTimeZone(env = process.env) {
  env.TZ = 'UTC'
  if (env === process.env && new Date(0).getTimezoneOffset() !== 0) throw new Error('could not switch the process time zone to UTC')
}

export async function isolatedReference(config, work, operations = { connect, command }) {
  const admin = await operations.connect(config)
  const name = `msduck_audit_${randomUUID().replaceAll('-','')}`
  let created = false
  let connection
  try {
    await operations.command(admin, `CREATE DATABASE [${name}]`)
    created = true
    connection = await operations.connect({ ...config, options: { ...config.options, database: name } })
    return await work(connection)
  } finally {
    if (connection && !connection.closed) {
      await new Promise(resolve => { connection.once('end', resolve); connection.close() })
    }
    try {
      // Only the freshly generated, successfully created database is dropped.
      if (created) await operations.command(admin, `DROP DATABASE [${name}]`)
    } finally { admin.close() }
  }
}

// Never hand whole captures or fixtures to node:assert. On Node 24, building
// the AssertionError for a failed equal/deepEqual inspects its operands without
// a practical bound, even with a custom message and even when caught; a
// multi-MB capture drove capture processes to ~123 GB RSS and OOM kills.
// These helpers compare with isDeepStrictEqual and raise plain, bounded errors.

const DETAIL_LIMIT = 500

function displayPath(path) { return path instanceof URL ? fileURLToPath(path) : String(path) }

// Refuses to overwrite a retained fixture using an existence check only; the
// fixture is never read or parsed. Call before starting any container.
export async function refuseExistingFixture(path) {
  try { await access(path) } catch (error) {
    if (error.code === 'ENOENT') return
    throw error
  }
  throw new Error('refusing to overwrite retained fixture ' + displayPath(path))
}

// Resolves symlinks in the existing part of a path, so an alias of the
// fixture (or of its directory) compares equal even before the file exists.
async function canonicalPath(path) {
  const absolute = resolvePath(path instanceof URL ? fileURLToPath(path) : path)
  try { return await realpath(absolute) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    return resolvePath(await canonicalPath(dirname(absolute)), basename(absolute))
  }
}

// Scratch capture output must never alias the retained fixture: an
// unconditional write would replace the ground truth and then compare the
// capture with itself. Call before any container starts.
export async function refuseFixtureOutput(output, fixture) {
  const refuse = () => { throw new Error('refusing to write capture output over retained fixture ' + displayPath(fixture)) }
  if (await canonicalPath(output) === await canonicalPath(fixture)) refuse()
  // Hard links share an inode but not a path.
  const identity = async path => { try { const s = await stat(path); return `${s.dev}:${s.ino}` } catch (error) { if (error.code === 'ENOENT') return null; throw error } }
  const target = await identity(fixture)
  if (target !== null && target === await identity(output)) refuse()
}

// Writes a new fixture in the retained compact JSON format; never overwrites.
export async function writeNewFixture(path, value) {
  await writeFile(path, JSON.stringify(value) + '\n', { flag: 'wx' })
}

function kind(value) {
  if (value === null) return 'null'
  if (Array.isArray(value)) return 'array'
  return typeof value
}

function preview(value) {
  const type = kind(value)
  if (type === 'array') return `array(${value.length})`
  if (type === 'object') {
    let keys = 0
    for (const _ in value) if (++keys > 1000) break
    return `object(${keys > 1000 ? '>1000' : keys} keys)`
  }
  if (type === 'string') return JSON.stringify(value.length > 80 ? value.slice(0, 80) : value) + (value.length > 80 ? `...(${value.length} chars)` : '')
  if (type === 'bigint') return `${value}n`
  if (type === 'undefined') return 'undefined'
  return String(value).slice(0, 80)
}

function segment(key, record) {
  const name = record !== null && typeof record === 'object' && typeof record.name === 'string' ? record.name : undefined
  const base = typeof key === 'number' ? `[${key}]` : /^[A-Za-z_$][\w$]*$/.test(key) ? `.${key}` : `[${JSON.stringify(key.slice(0, 40))}]`
  return name === undefined ? base : `${base}(${JSON.stringify(name.slice(0, 60))})`
}

// Locates the first differing path between two structured-clone/JSON values
// without serializing either whole value. Returns a bounded description.
export function describeFirstDifference(actual, expected) {
  let path = ''
  let note = ''
  let a = actual
  let b = expected
  for (let depth = 0; depth < 64; depth++) {
    const ka = kind(a), kb = kind(b)
    if (ka !== kb || (ka !== 'array' && ka !== 'object')) break
    if (ka === 'array') {
      const length = Math.min(a.length, b.length)
      let index = 0
      while (index < length && isDeepStrictEqual(a[index], b[index])) index++
      if (index === length) {
        if (a.length !== b.length) {
          note = ` (array length ${a.length} vs ${b.length})`
          a = a.length > length ? a[length] : undefined
          b = b.length > length ? b[length] : undefined
          path += segment(length, a ?? b)
        }
        break
      }
      path += segment(index, a[index])
      a = a[index]; b = b[index]
      continue
    }
    let key
    for (const k in a) if (!Object.hasOwn(b, k) || !isDeepStrictEqual(a[k], b[k])) { key = k; break }
    if (key === undefined) for (const k in b) if (!Object.hasOwn(a, k)) { key = k; break }
    if (key === undefined) break
    path += segment(key, a[key])
    a = a[key]; b = b[key]
  }
  const detail = `first difference at ${path || '<root>'}${note}: actual ${preview(a)}, expected ${preview(b)}`
  return detail.length > DETAIL_LIMIT ? detail.slice(0, DETAIL_LIMIT - 3) + '...' : detail
}

// Throws a plain Error naming only the first differing record/path.
export function assertSameCapture(actual, expected, label) {
  if (isDeepStrictEqual(actual, expected)) return
  throw new Error(`${String(label).slice(0, 300)} (${describeFirstDifference(actual, expected)})`)
}

// sp_prepare/sp_execute/sp_unprepare capture on one reusable tedious Request.
//
// tedious quirks this helper absorbs:
// - sp_prepare completion arrives through the Request's 'prepared' or 'error'
//   event, never through its callback; waiting on the callback hangs.
// - tedious assigns request.error for each server error but never clears it,
//   and passes request.error to every later callback of the same Request. A
//   successful execution after a failed one therefore "completes" with the
//   earlier error object. The helper clears request.error before each phase
//   and never reports an error object it already attributed to an earlier phase.
// - tedious keeps the latest RETURNSTATUS token on
//   connection.procReturnStatusValue and clears it only on a DONEPROC token, so
//   a status left by an earlier request (for example an EXEC inside a batch,
//   which ends with DONEINPROC/DONE) is reported by the next request's
//   'doneProc' event as if that request had returned it. The helper clears the
//   carried value before each phase and records returnStatus only when a
//   status actually arrives during the phase; otherwise it stays null.
// Only errorMessage/infoMessage tokens raised while a phase is outstanding are
// recorded for it; a callback error is recorded only when the phase received
// no server error (client-side validation, cancellation, socket failure).
// Rows and messages are bounded per phase; overflow is counted in `truncated`
// instead of retained.
export const PREPARED_LIMITS = Object.freeze({ rows: 10000, messages: 200 })

const messageFields = m => ({ number: m.number, state: m.state, class: m.class, lineNumber: m.lineNumber, message: m.message })
const columnFields = c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })

// declarations: [name, TYPES.X, options?][]; valueSets: {name: value}[].
// Returns { prepare, prepared, executions: [{ values, result }], unprepare }.
// Each phase result is { sets, done, errors, info, returnStatus, rowCount }
// plus `truncated: { rows, messages }` only when a bound was exceeded. When
// the prepare step reports an error or no valid handle (a positive integer)
// arrives, prepared is false, skipped is 'prepare failed', each value set is
// recorded as { values, skipped: 'prepare failed' } without a result, and
// unprepare is null. No sp_execute or sp_unprepare request is sent: tedious
// keeps a handle after a failed sp_prepare, and executing it only yields
// client-caused server errors (8009 malformed request, 8179 no statement for
// handle 0) that are not behavior under test.
// Values are returned uncanonicalized; callers apply canonical() as needed.
export async function capturePrepared(connection, sql, declarations, valueSets, options = {}) {
  const limits = { ...PREPARED_LIMITS, ...(options.limits ?? {}) }
  const RequestType = options.Request ?? Request
  let result
  let complete = () => {}
  const reported = new Set()
  const fresh = () => ({ sets: [], done: [], errors: [], info: [], returnStatus: null })
  const overflow = kind => { result.truncated ??= { rows: 0, messages: 0 }; result.truncated[kind]++ }
  // Server errors that arrive after the message bound are still errors: the
  // callback fallback below must never stand in for a truncated diagnostic.
  let serverError = false
  const message = list => token => {
    if (!result) return
    if (list === 'errors') serverError = true
    if (result.errors.length + result.info.length >= limits.messages) overflow('messages')
    else result[list].push(messageFields(token))
  }
  const onError = message('errors')
  const onInfo = message('info')
  const request = new RequestType(sql, (error, rowCount) => complete(error, rowCount))
  for (const [name, type, parameterOptions] of declarations) request.addParameter(name, type, undefined, parameterOptions)
  request.on('columnMetadata', columns => result?.sets.push({ columns: columns.map(columnFields), rows: [] }))
  request.on('row', columns => {
    if (!result) return
    const set = result.sets.at(-1)
    if (!set || set.rows.length >= limits.rows) overflow('rows')
    else set.rows.push(columns.map(c => c.value))
  })
  for (const kind of ['done', 'doneInProc', 'doneProc']) request.on(kind, (rowCount, more) => {
    if (!result) return
    if (result.done.length >= limits.messages) overflow('messages')
    else result.done.push({ kind, rowCount: rowCount ?? null, more })
  })
  request.on('doneProc', (_count, _more, status) => { if (result && status !== undefined) result.returnStatus = status })
  const phase = start => new Promise(resolve => {
    result = fresh()
    serverError = false
    complete = (error, rowCount) => {
      complete = () => {}
      const finished = result
      result = undefined
      finished.rowCount = rowCount
      if (error && !reported.has(error) && !serverError) {
        if (finished.errors.length + finished.info.length >= limits.messages) { result = finished; overflow('messages'); result = undefined }
        else finished.errors.push({ message: error.message, number: error.number ?? null })
      }
      if (error) reported.add(error)
      resolve(finished)
    }
    request.error = undefined
    if ('procReturnStatusValue' in connection) connection.procReturnStatusValue = undefined
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
    if (prepare.errors.length || !(Number.isInteger(request.handle) && request.handle > 0)) {
      const skipped = 'prepare failed'
      return { prepare, prepared: false, skipped, executions: valueSets.map(values => ({ values, skipped })), unprepare: null }
    }
    const executions = []
    for (const values of valueSets) executions.push({ values, result: await phase(() => connection.execute(request, values)) })
    const unprepare = await phase(() => connection.unprepare(request))
    return { prepare, prepared: true, executions, unprepare }
  } finally {
    connection.off('errorMessage', onError)
    connection.off('infoMessage', onInfo)
  }
}
