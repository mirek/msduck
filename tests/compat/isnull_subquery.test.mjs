import assert from 'node:assert/strict'
import { test } from 'node:test'
import { readFileSync } from 'node:fs'
import { start } from '../support/client.mjs'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/openjson-isnull.json', import.meta.url)))
const entry = reference.cases.find(c => c.name === 'concat isnull aggregate subquery')

test('ISNULL aggregate scalar queries retain the original SQL Server deadline and result', async t => {
  // The shared helper's original 5000ms request deadline remains in force.
  const connection = await start(t)
  const raw = await capture(connection, entry.query)
  const actual = canonical({
    sets: raw.sets.map(set => ({columns: set.columns.map(c => [c.name, c.type, c.length]), rows: set.rows})),
    errors: raw.errors.map(e => ({number:e.number, class:e.class ?? null, state:e.state ?? null, message:e.message})),
    done: raw.done.filter(d => d.kind === 'done' || d.kind === 'doneInProc').map(d => d.rowCount),
  })
  assert.deepEqual(actual, entry.result)
  const reusable = await capture(connection, 'SELECT 1 AS reusable')
  assert.deepEqual(reusable.errors, [])
  assert.deepEqual(reusable.sets[0].rows, [[1]])
})

// Independent pinned SQL Server 2025 17.0.4065.4 capture. Keep isolated units
// in the expected JavaScript string; visual replacement glyphs are insufficient.
const carrierQuery = "SELECT ISNULL((SELECT j.[value] FROM OPENJSON(N'{\"s\":\"\\ud800\"}') j), N'') AS v, N'<' + ISNULL((SELECT j.[value] FROM OPENJSON(N'{\"s\":\"\\ud800\"}') j), N'') + N'>' AS c, CASE WHEN ISNULL((SELECT j.[value] FROM OPENJSON(N'{\"s\":\"\\ud800\"}') j), N'') = (SELECT j.[value] FROM OPENJSON(N'{\"s\":\"\\ud800\"}') j) THEN 1 ELSE 0 END AS hit"
const carrierExpected = {"sets": [{"columns": [["v", "NVarChar", 65535], ["c", "NVarChar", 65535], ["hit", "Int", null]], "rows": [["\ud800", "<\ud800>", 1]]}], "errors": [], "done": [1]}

test('ISNULL scalar-query carriers retain captured UTF-16 values and descriptors', async t => {
  const connection = await start(t)
  const raw = await capture(connection, carrierQuery)
  const actual = canonical({
    sets: raw.sets.map(set => ({columns: set.columns.map(c => [c.name,c.type,c.length]), rows:set.rows})),
    errors: raw.errors,
    done: raw.done.filter(d => d.kind === 'done' || d.kind === 'doneInProc').map(d => d.rowCount),
  })
  assert.deepEqual(actual, carrierExpected)
})

// Pinned SQL Server17.0.4065.4: character casts use padded collation equality.
const characterPeerCases = [
  {
    "name": "NVARCHAR",
    "query": "SELECT CASE WHEN ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'x') = CAST(N'x ' AS NVARCHAR(2)) THEN 1 ELSE 0 END AS v",
    "expected": {
      "sets": [
        {
          "columns": [
            [
              "v",
              "Int",
              null
            ]
          ],
          "rows": [
            [
              1
            ]
          ]
        }
      ],
      "errors": [],
      "done": [
        1
      ]
    }
  },
  {
    "name": "NCHAR",
    "query": "SELECT CASE WHEN ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'x') = CAST(N'x ' AS NCHAR(2)) THEN 1 ELSE 0 END AS v",
    "expected": {
      "sets": [
        {
          "columns": [
            [
              "v",
              "Int",
              null
            ]
          ],
          "rows": [
            [
              1
            ]
          ]
        }
      ],
      "errors": [],
      "done": [
        1
      ]
    }
  },
  {
    "name": "CHAR",
    "query": "SELECT CASE WHEN ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'x') = CAST(N'x ' AS CHAR(2)) THEN 1 ELSE 0 END AS v",
    "expected": {
      "sets": [
        {
          "columns": [
            [
              "v",
              "Int",
              null
            ]
          ],
          "rows": [
            [
              1
            ]
          ]
        }
      ],
      "errors": [],
      "done": [
        1
      ]
    }
  },
  {
    "name": "VARCHAR",
    "query": "SELECT CASE WHEN ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'x') = CAST(N'x ' AS VARCHAR(2)) THEN 1 ELSE 0 END AS v",
    "expected": {
      "sets": [
        {
          "columns": [
            [
              "v",
              "Int",
              null
            ]
          ],
          "rows": [
            [
              1
            ]
          ]
        }
      ],
      "errors": [],
      "done": [
        1
      ]
    }
  }
]

test('ISNULL scalar-query comparisons retain captured character peer equality', async t => {
  const connection = await start(t)
  for (const entry of characterPeerCases) {
    const raw = await capture(connection, entry.query)
    const actual = canonical({
      sets: raw.sets.map(set => ({columns: set.columns.map(c => [c.name,c.type,c.length]), rows:set.rows})),
      errors: raw.errors,
      done: raw.done.filter(d => d.kind === 'done' || d.kind === 'doneInProc').map(d => d.rowCount),
    })
    assert.deepEqual(actual, entry.expected, entry.name)
  }
})
