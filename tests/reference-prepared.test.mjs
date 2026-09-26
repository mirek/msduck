import assert from 'node:assert/strict'
import { EventEmitter } from 'node:events'
import test from 'node:test'
import { TYPES } from 'tedious'
import { capturePrepared, useUtcTimeZone, refuseFixtureOutput } from '../scripts/lib/reference.mjs'

// A fake connection that replays scripted server responses to real tedious
// Request objects, reproducing tedious' bookkeeping: prepare() sets
// request.preparing (so completion is signalled through 'prepared'/'error'),
// each server error overwrites request.error and nothing ever clears it, and
// completion calls request.callback(request.error, rowCount). A RETURNSTATUS
// token is kept on connection.procReturnStatusValue and cleared only by the
// DONEPROC that reports it.
class FakeConnection extends EventEmitter {
  constructor(script) {
    super()
    this.script = script
    this.calls = []
    this.procReturnStatusValue = undefined
  }
  prepare(request) {
    request.preparing = true
    this.calls.push({ kind: 'prepare' })
    this.respond(request, this.script.prepare)
  }
  execute(request, values) {
    this.calls.push({ kind: 'execute', values })
    this.respond(request, this.script.executions[this.calls.filter(c => c.kind === 'execute').length - 1])
  }
  unprepare(request) {
    this.calls.push({ kind: 'unprepare' })
    this.respond(request, this.script.unprepare ?? {})
  }
  respond(request, step) {
    setImmediate(() => {
      if (step.clientError) {
        request.error = step.clientError
        request.callback(step.clientError)
        return
      }
      if (step.handle !== undefined) request.handle = step.handle
      if (step.columns) request.emit('columnMetadata', step.columns.map(name => ({ colName: name, type: { name: 'IntN' }, dataLength: 4, flags: 33 })))
      for (const row of step.rows ?? []) request.emit('row', row.map(value => ({ value })))
      for (const info of step.info ?? []) this.emit('infoMessage', { number: info, state: 1, class: 0, lineNumber: 1, message: 'info ' + info })
      for (const number of step.errors ?? []) {
        const token = { number, state: 2, class: 16, lineNumber: 1, message: 'error ' + number }
        this.emit('errorMessage', token)
        request.error = Object.assign(new Error(token.message), token)
      }
      request.emit('doneInProc', step.rowCount ?? 0, true)
      if (!step.noStatus) this.procReturnStatusValue = step.returnStatus ?? (step.errors?.length ? -6 : 0)
      request.emit('doneProc', undefined, false, this.procReturnStatusValue)
      this.procReturnStatusValue = undefined
      request.callback(request.error, step.rowCount ?? 0)
    })
  }
}

const declarations = [['find', TYPES.VarChar, { length: 10 }], ['start', TYPES.BigInt]]

test('a failing execution does not leak its error into later executions', async () => {
  const connection = new FakeConnection({
    prepare: { handle: 7, columns: ['value'] },
    executions: [
      { columns: ['value'], errors: [8115] },
      { columns: ['value'], rows: [[4]], rowCount: 1 },
      { columns: ['value'], errors: [8134] },
      { columns: ['value'], rows: [[2]], rowCount: 1 },
    ],
  })
  const valueSets = [{ find: 'b', start: '3000000000' }, { find: 'b', start: '3' }, { find: 'b', start: '0' }, { find: 'b', start: '1' }]
  const captured = await capturePrepared(connection, 'SELECT CHARINDEX(@find,\'abcb\',@start) AS value', declarations, valueSets)
  assert.equal(captured.prepared, true)
  assert.deepEqual(captured.prepare.errors, [])
  assert.equal(captured.prepare.rowCount, undefined)
  assert.deepEqual(captured.executions.map(e => e.values), valueSets)
  assert.deepEqual(captured.executions.map(e => e.result.errors.map(x => x.number)), [[8115], [], [8134], []])
  assert.deepEqual(captured.executions[0].result.errors[0], { number: 8115, state: 2, class: 16, lineNumber: 1, message: 'error 8115' })
  assert.deepEqual(captured.executions.map(e => e.result.sets.map(s => s.rows)), [[[]], [[[4]]], [[]], [[[2]]]])
  assert.deepEqual(captured.executions.map(e => e.result.returnStatus), [-6, 0, -6, 0])
  assert.deepEqual(captured.executions.map(e => e.result.rowCount), [0, 1, 0, 1])
  assert.deepEqual(captured.unprepare.errors, [])
  assert.deepEqual(connection.calls.map(c => c.kind), ['prepare', 'execute', 'execute', 'execute', 'execute', 'unprepare'])
  assert.equal(connection.listenerCount('errorMessage'), 0)
  assert.equal(connection.listenerCount('infoMessage'), 0)
})

test('a callback error object reported earlier is never attributed again', async () => {
  // Stricter than tedious: this fake re-delivers the first error object on
  // every callback even though the helper clears request.error.
  const connection = new FakeConnection({ prepare: { handle: 1 }, executions: [] })
  let retained
  connection.respond = function (request, step) {
    setImmediate(() => {
      if (step?.handle !== undefined) request.handle = step.handle
      if (step?.fail) {
        const token = { number: 245, state: 1, class: 16, lineNumber: 1, message: 'conversion' }
        this.emit('errorMessage', token)
        retained ??= Object.assign(new Error(token.message), token)
      }
      request.emit('doneProc', undefined, false, 0)
      request.callback(retained, 0)
    })
  }
  connection.script.executions = [{ fail: true }, {}, {}]
  const captured = await capturePrepared(connection, 'SELECT @find AS v', declarations, [{ find: 'x', start: '1' }, { find: 'y', start: '1' }, { find: 'z', start: '1' }])
  assert.deepEqual(captured.executions.map(e => e.result.errors.map(x => x.number)), [[245], [], []])
  assert.deepEqual(captured.unprepare.errors, [])
})

test('client-side execution errors are recorded once without a server token', async () => {
  const clientError = new TypeError('Value must be between -9223372036854775808 and 9223372036854775807, inclusive.')
  const connection = new FakeConnection({
    prepare: { handle: 3 },
    executions: [{ clientError }, { columns: ['value'], rows: [[1]], rowCount: 1 }],
  })
  const captured = await capturePrepared(connection, 'SELECT @start AS value', declarations, [{ find: 'b', start: '1e30' }, { find: 'b', start: '1' }])
  assert.deepEqual(captured.executions[0].result.errors, [{ message: clientError.message, number: null }])
  assert.deepEqual(captured.executions[0].result.done, [])
  assert.deepEqual(captured.executions[1].result.errors, [])
  assert.deepEqual(captured.executions[1].result.sets[0].rows, [[1]])
})

test('preparation completes through tedious prepared/error events', async () => {
  const failing = new FakeConnection({ prepare: { errors: [102] }, executions: [] })
  const failed = await capturePrepared(failing, 'SELECT FROM', declarations, [{ find: 'b', start: '1' }])
  assert.equal(failed.prepared, false)
  assert.deepEqual(failed.prepare.errors.map(e => e.number), [102])
  assert.equal(failed.skipped, 'prepare failed')
  assert.deepEqual(failed.executions, [{ values: { find: 'b', start: '1' }, skipped: 'prepare failed' }])
  assert.equal(failed.unprepare, null)
  assert.deepEqual(failing.calls.map(c => c.kind), ['prepare'])
  assert.equal(failing.listenerCount('errorMessage'), 0)

  const informational = new FakeConnection({ prepare: { handle: 9, info: [5701] }, executions: [{ info: [5703], rowCount: 0 }] })
  const passed = await capturePrepared(informational, 'SELECT 1', [], [{}])
  assert.deepEqual(passed.prepare.info.map(i => i.number), [5701])
  assert.deepEqual(passed.executions[0].result.info.map(i => i.number), [5703])
  assert.deepEqual(passed.unprepare.info, [])
})

test('rows and messages are bounded per execution and overflow is counted', async () => {
  const connection = new FakeConnection({
    prepare: { handle: 4 },
    executions: [
      { columns: ['value'], rows: Array.from({ length: 25 }, (_, i) => [i]), info: Array.from({ length: 6 }, (_, i) => 5000 + i), rowCount: 25 },
      { columns: ['value'], rows: [[1]], rowCount: 1 },
    ],
  })
  const captured = await capturePrepared(connection, 'SELECT value FROM t', [], [{}, {}], { limits: { rows: 10, messages: 4 } })
  const [first, second] = captured.executions.map(e => e.result)
  assert.equal(first.sets[0].rows.length, 10)
  assert.equal(first.info.length, 4)
  assert.equal(first.done.length, 2)
  assert.deepEqual(first.truncated, { rows: 15, messages: 2 })
  assert.equal(first.rowCount, 25)
  assert.equal(second.truncated, undefined)
  assert.deepEqual(second.sets[0].rows, [[1]])
})

test('useUtcTimeZone pins client-side Date handling to UTC', () => {
  const saved = process.env.TZ
  try {
    process.env.TZ = 'Europe/Warsaw'
    assert.equal(new Date('2024-07-01T00:00:00Z').getTimezoneOffset(), -120)
    useUtcTimeZone()
    assert.equal(process.env.TZ, 'UTC')
    assert.equal(new Date('2024-07-01T00:00:00Z').getTimezoneOffset(), 0)
    assert.equal(new Date('2024-01-01T00:00:00Z').getTimezoneOffset(), 0)
    const env = { TZ: 'America/New_York' }
    useUtcTimeZone(env)
    assert.deepEqual(env, { TZ: 'UTC' })
  } finally {
    if (saved === undefined) delete process.env.TZ
    else process.env.TZ = saved
  }
})

test('a return status carried from an earlier request is not attributed to a step', async () => {
  const connection = new FakeConnection({
    prepare: { handle: 5, noStatus: true },
    executions: [{ columns: ['value'], rows: [[1]], rowCount: 1, noStatus: true }, { columns: ['value'], rows: [[2]], rowCount: 1, returnStatus: 3 }],
    unprepare: { noStatus: true },
  })
  // e.g. a batch EXEC whose RETURNSTATUS was followed by DONEINPROC, not DONEPROC
  connection.procReturnStatusValue = 5
  const captured = await capturePrepared(connection, 'SELECT @find AS value', declarations, [{ find: 'a', start: '1' }, { find: 'b', start: '1' }])
  assert.equal(captured.prepare.returnStatus, null)
  assert.deepEqual(captured.prepare.done.map(d => d.kind), ['doneInProc', 'doneProc'])
  assert.deepEqual(captured.executions.map(e => e.result.returnStatus), [null, 3])
  assert.equal(captured.unprepare.returnStatus, null)
})

test('a genuine sp_prepare return status arriving during the step is kept', async () => {
  // SQL Server's sp_prepare response carries its own RETURNSTATUS (observed as
  // the session's most recent error number); it replaces any carried value.
  const connection = new FakeConnection({
    prepare: { handle: 6, returnStatus: 8115 },
    executions: [{ columns: ['value'], rows: [[1]], rowCount: 1 }],
  })
  connection.procReturnStatusValue = 5
  const captured = await capturePrepared(connection, 'SELECT @find AS value', declarations, [{ find: 'a', start: '1' }])
  assert.equal(captured.prepare.returnStatus, 8115)
  assert.equal(captured.executions[0].result.returnStatus, 0)
  assert.equal(captured.unprepare.returnStatus, 0)
})

test('a failed prepare that still leaves a handle sends no execute or unprepare', async () => {
  // tedious keeps a handle output even when sp_prepare raised; executing it
  // would only produce client-caused 8009/8179 errors.
  for (const prepare of [{ handle: 0, errors: [8180, 207] }, { handle: 11, errors: [207] }, { handle: 0 }, {}]) {
    const connection = new FakeConnection({ prepare, executions: [{ errors: [8009] }, { errors: [8009] }], unprepare: { errors: [8179] } })
    const valueSets = [{ find: 'a', start: '1' }, { find: 'b', start: '2' }]
    const captured = await capturePrepared(connection, 'SELECT missing_column', declarations, valueSets)
    assert.equal(captured.prepared, false, JSON.stringify(prepare))
    assert.equal(captured.skipped, 'prepare failed')
    assert.deepEqual(captured.prepare.errors.map(e => e.number), prepare.errors ?? [])
    assert.deepEqual(captured.executions, valueSets.map(values => ({ values, skipped: 'prepare failed' })))
    assert.equal(captured.unprepare, null)
    assert.deepEqual(connection.calls.map(c => c.kind), ['prepare'])
    assert.equal(connection.listenerCount('errorMessage'), 0)
  }
})

test('capture output may not alias the retained fixture, including via symlinks', async () => {
  const { mkdtemp, mkdir, writeFile, symlink, rm } = await import('node:fs/promises')
  const { join, relative } = await import('node:path')
  const { tmpdir } = await import('node:os')
  const { pathToFileURL } = await import('node:url')
  const root = await mkdtemp(join(tmpdir(), 'msduck-fixture-guard-'))
  try {
    await mkdir(join(root, 'reference'))
    const fixture = join(root, 'reference', 'x.json')
    await writeFile(fixture, '{}\n')
    await symlink(fixture, join(root, 'link.json'))
    await symlink(join(root, 'reference'), join(root, 'refdir'))
    const { link } = await import('node:fs/promises')
    await link(fixture, join(root, 'hard.json'))
    const refused = /refusing to write capture output over retained fixture/
    for (const output of [fixture, join(root, 'reference', '.', 'x.json'), join(root, 'link.json'), join(root, 'refdir', 'x.json'), relative(process.cwd(), fixture), join(root, 'hard.json')])
      await assert.rejects(refuseFixtureOutput(output, fixture), refused, output)
    await assert.rejects(refuseFixtureOutput(join(root, 'refdir', 'y.json'), pathToFileURL(join(root, 'reference', 'y.json'))), refused)
    await refuseFixtureOutput(join(root, 'artifacts', 'capture.json'), fixture)
    await refuseFixtureOutput(join(root, 'reference', 'other.json'), fixture)
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('a server error beyond the message bound is counted, never replaced by the callback error', async () => {
  const connection = new FakeConnection({
    prepare: { handle: 1 },
    executions: [{ info: [5701, 5703], errors: [8115] }],
  })
  const result = await capturePrepared(connection, 'select 1', declarations, [{ find: 'b', start: 1 }], { limits: { messages: 2 } })
  const run = result.executions[0].result
  assert.deepEqual(run.info.map(m => m.number), [5701, 5703])
  assert.deepEqual(run.errors, [])
  assert.equal(run.truncated.messages, 1)
})
