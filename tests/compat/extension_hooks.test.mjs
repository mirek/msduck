// Extension hook scaffold (docs/extension-hooks.md): procedure-call argument
// names are not caller variables, and ordinary batches are unchanged. Feature
// tasks add their own tests/compat/<feature>.test.mjs files next to this one.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { start, query } from '../support/client.mjs'

test('named EXEC arguments are not undeclared variables', async t => {
  const connection = await start(t)
  await assert.rejects(
    query(connection, "EXEC dbo.no_such_procedure @Resource = N'foo', @Value = 1"),
    error => !/Must declare the scalar variable/.test(error.message)
  )
  // Values that are variables must still be declared.
  await assert.rejects(
    query(connection, 'EXEC dbo.no_such_procedure @Resource = @undeclared'),
    /Must declare the scalar variable @undeclared/
  )
  const { rows } = await query(connection, 'SELECT 1 AS one')
  assert.deepEqual(rows, [[1]])
})
