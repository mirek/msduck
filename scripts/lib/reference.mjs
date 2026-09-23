import { randomUUID } from 'node:crypto'
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
