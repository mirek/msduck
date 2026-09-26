import { randomUUID } from 'node:crypto'
import { access, writeFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
import { isDeepStrictEqual } from 'node:util'
import { Connection } from 'tedious'
import { capture } from './compatibility.mjs'

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
