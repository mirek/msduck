import { spawn } from 'node:child_process'
import { Connection, Request } from 'tedious'

export async function start(t, { serverArgs = [], options = {}, authentication = { type: 'default', options: { userName: 'sa', password: 'development' } } } = {}) {
  const server = spawn('target/debug/msduck', ['--listen', '127.0.0.1:0', ...serverArgs], { stdio: ['ignore', 'ignore', 'pipe'] })
  t.after(() => server.kill())
  let logs = ''
  const port = await new Promise((resolve, reject) => {
    // A freshly installed native executable can take over ten seconds to
    // become ready on macOS; connection and request deadlines stay separate.
    const timeout = setTimeout(() => reject(new Error(`Server startup timed out: ${logs}`)), 20000)
    server.once('error', error => { clearTimeout(timeout); reject(error) })
    server.once('exit', code => { clearTimeout(timeout); reject(new Error(`Server exited (${code}): ${logs}`)) })
    server.stderr.on('data', data => {
      logs += data.toString()
      const match = logs.match(/listening on 127\.0\.0\.1:(\d+)/)
      if (match) { clearTimeout(timeout); resolve(Number(match[1])) }
    })
  })
  const connection = new Connection({
    server: '127.0.0.1',
    authentication,
    options: { port, encrypt: false, database: 'master', connectTimeout: 5000, requestTimeout: 5000, ...options }
  })
  t.after(() => connection.close())
  connection.on('error', () => {})
  await new Promise((resolve, reject) => connection.connect(error => error ? reject(error) : resolve()))
  return connection
}
export function query(connection, sql, parameters = []) {
  return new Promise((resolve, reject) => {
    const rows = [], columns = []
    let returnStatus
    const request = new Request(sql, (error, rowCount) => error ? reject(error) : resolve({ rows, columns, rowCount, returnStatus }))
    request.on('doneProc', (_count, _more, status) => { returnStatus = status })
    request.on('row', cells => rows.push(cells.map(cell => cell.value)))
    request.on('columnMetadata', metadata => columns.push(metadata))
    for (const [name, type, value, options] of parameters) request.addParameter(name, type, value, options)
    connection.execSql(request)
  })
}
