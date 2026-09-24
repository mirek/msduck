import { Request } from 'tedious'

export function capture(connection, sql) {
  return new Promise(resolve => {
    const result = { sets: [], done: [], errors: [], info: [], returnStatus: null }
    const request = new Request(sql, (error, rowCount) => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.rowCount = rowCount
      if (error && !result.errors.length) result.errors.push({ message: error.message, number: error.number ?? null })
      resolve(result)
    })
    const onError = error => result.errors.push({ number: error.number, state: error.state, class: error.class, lineNumber: error.lineNumber, message: error.message })
    const onInfo = info => result.info.push({ number: info.number, state: info.state, class: info.class, lineNumber: info.lineNumber, message: info.message })
    connection.on('infoMessage', onInfo)
    connection.on('errorMessage', onError)
    request.on('columnMetadata', metadata => result.sets.push({
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: canonical(c.collation ?? null) })), rows: []
    }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => c.value)))
    for (const name of ['done', 'doneInProc', 'doneProc']) request.on(name, (rowCount, more) => result.done.push({ kind: name, rowCount: rowCount ?? null, more }))
    request.on('doneProc', (_count, _more, status) => { result.returnStatus = status })
    try { connection.execSqlBatch(request) }
    catch (error) {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.errors.push({ message: error.message, number: error.number ?? null })
      resolve(result)
    }
  })
}

export function canonical(value) {
  if (value === undefined) return { kind: 'missing' }
  if (typeof value === 'bigint') return { kind: 'bigint', value: value.toString() }
  if (Buffer.isBuffer(value)) return { kind: 'binary', value: value.toString('hex') }
  if (value instanceof Date) return {
    kind: 'date', value: value.toISOString(),
    ...(value.nanosecondsDelta === undefined ? {} : { nanosecondsDelta: value.nanosecondsDelta })
  }
  if (typeof value === 'number' && !Number.isFinite(value)) return { kind: 'number', value: String(value) }
  if (Array.isArray(value)) return value.map(canonical)
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k,v]) => [k,canonical(v)]))
  return value
}

export function differences(local, reference, path = '') {
  if (Object.is(local, reference)) return []
  if (local && reference && typeof local === 'object' && typeof reference === 'object' && Array.isArray(local) === Array.isArray(reference)) {
    const keys = new Set([...Object.keys(local), ...Object.keys(reference)])
    return [...keys].flatMap(key => differences(local[key], reference[key], `${path}/${key.replaceAll('~','~0').replaceAll('/','~1')}`))
  }
  return [{ path: path || '/', local: canonical(local), reference: canonical(reference) }]
}

export async function runCase(connection, entry) {
  const result = {}
  try {
    if (entry.setup) result.setup = await capture(connection, entry.setup)
    if (!result.setup?.errors.length) {
      result.execution = await capture(connection, entry.query)
      result.reuse = await capture(connection, 'SELECT @@TRANCOUNT AS transaction_count, XACT_STATE() AS transaction_state; SELECT 1 AS reusable')
    } else result.skipped = 'setup failed'
  } catch (error) { result.transportError = error.message }
  finally {
    if (entry.cleanup) {
      try { result.cleanup = await capture(connection, entry.cleanup) }
      catch (error) { result.cleanupTransportError = error.message }
    }
  }
  return canonical(result)
}

export function complete(result) {
  return Boolean(result.execution && result.reuse && !result.transportError && !result.cleanupTransportError && !result.skipped && !result.reuse.errors.length && !result.cleanup?.errors.length)
}
