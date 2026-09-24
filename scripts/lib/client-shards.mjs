// Deterministic planning and result accounting over explicit inputs.
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0
export const key = test => JSON.stringify([test.file, test.name])

export function partition(tests, count) {
  if (!Number.isInteger(count) || count < 1 || count > 16) throw Error('jobs must be an integer from 1 to 16')
  if (!tests.length) throw Error('No tests discovered')
  const seen = new Set()
  for (const test of tests) {
    if (typeof test.file !== 'string' || !test.file || typeof test.name !== 'string' || !test.name) throw Error('Invalid test identity')
    if (seen.has(key(test))) throw Error(`Duplicate test identity: ${key(test)}`)
    seen.add(key(test))
  }
  const ordered = [...tests].sort((a, b) => compare(a.file, b.file) || compare(a.name, b.name))
  const shards = Array.from({length: count}, () => [])
  ordered.forEach((test, i) => shards[i % count].push({...test}))
  // Each process selects names within one file. Equal names in different files
  // must not cause another shard's tests to execute.
  return shards.flatMap((shard, index) => [...new Set(shard.map(t => t.file))].map(file => ({
    shard: index, file, tests: shard.filter(t => t.file === file),
  })))
}

export function selection(tests) {
  if (!tests.length) throw Error('Empty selection')
  const escape = text => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  // Unlike $, the final assertion does not accept a trailing newline.
  return `^(?:${tests.map(t => escape(t.name)).join('|')})(?![\\s\\S])`
}

export function assess(tests, events, exitCode) {
  const expected = new Map(tests.map(test => [key(test), test]))
  const observed = new Map(), problems = []
  const counts = {passed: 0, failed: 0, skipped: 0, todo: 0}
  for (const event of events) {
    if (!['test:pass', 'test:fail'].includes(event.type) || event.nesting !== 0) continue
    const id = key(event)
    if (!expected.has(id)) {
      if (!event.skip) problems.push(`Unexpected executed test: ${id}`)
      continue
    }
    if (observed.has(id)) problems.push(`Repeated test: ${id}`)
    const status = event.skip ? 'skipped' : event.todo ? 'todo' : event.type === 'test:pass' ? 'passed' : 'failed'
    observed.set(id, status)
    counts[status]++
  }
  for (const id of expected.keys()) if (!observed.has(id)) problems.push(`Missing test: ${id}`)
  if (exitCode !== 0) problems.push(`Process exit: ${exitCode}`)
  return {ok: !problems.length && counts.failed === 0, expected: tests.length, ...counts, problems}
}

// Node's second reporter writes accounting separately from the complete TAP log.
export default async function* reporter(source) {
  for await (const event of source) {
    if (['test:pass', 'test:fail'].includes(event.type)) {
      const {name, file, nesting, skip, todo} = event.data
      yield JSON.stringify({type: event.type, name, file, nesting, skip, todo}) + '\n'
    }
  }
}
