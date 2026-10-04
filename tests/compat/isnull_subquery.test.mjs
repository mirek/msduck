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

// Captured on pinned SQL Server 17.0.4065.4; raw evidence retained privately.
const declaredPredicateCases = [
  {
    "name": "order scope",
    "query": "SELECT j.value FROM OPENJSON(N'[\"x\"]') j ORDER BY CASE WHEN j.value=N'x' THEN 0 ELSE 1 END",
    "expected": {
      "sets": [
        {
          "columns": [
            [
              "value",
              "NVarChar",
              65535
            ]
          ],
          "rows": [
            [
              "x"
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
    "name": "scalar like",
    "query": "SELECT CASE WHEN ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'x') LIKE N'x' THEN 1 ELSE 0 END AS v",
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
    "name": "scalar list",
    "query": "SELECT CASE WHEN ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'x') IN (N'x ',NULL) THEN 1 ELSE 0 END AS v",
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
    "name": "scalar range",
    "query": "SELECT CASE WHEN ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'x') BETWEEN N'w' AND N'x ' THEN 1 ELSE 0 END AS v",
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
    "name": "varchar peer",
    "query": "CREATE TABLE predicate_peer(v VARCHAR(2)); INSERT INTO predicate_peer VALUES('x '); SELECT CASE WHEN ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'x')=v THEN 1 ELSE 0 END AS v FROM predicate_peer",
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
        null,
        1,
        1
      ]
    }
  },
  {
    "name": "parameter peer",
    "query": "DECLARE @p VARCHAR(2)='x '; SELECT CASE WHEN ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'x')=@p THEN 1 ELSE 0 END AS v",
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
        1,
        1
      ]
    }
  }
]

declaredPredicateCases.push(...[
  {
    "name": "order parameter",
    "query": "DECLARE @p NVARCHAR(2)=N'x '; SELECT j.value FROM OPENJSON(N'[\"z\",\"x\"]') j ORDER BY CASE WHEN j.value=@p THEN 0 ELSE 1 END",
    "expected": {
      "sets": [
        {
          "columns": [
            [
              "value",
              "NVarChar",
              65535
            ]
          ],
          "rows": [
            [
              "x"
            ],
            [
              "z"
            ]
          ]
        }
      ],
      "errors": [],
      "done": [
        1,
        2
      ]
    }
  },
  {
    "name": "numeric coalesce",
    "query": "SELECT CASE WHEN ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'02')=COALESCE(N'2',2) THEN 1 ELSE 0 END AS hit",
    "expected": {
      "sets": [
        {
          "columns": [
            [
              "hit",
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
    "name": "quoted parameter column",
    "query": "DECLARE @p INT=2; CREATE TABLE quoted_peer([@p] VARCHAR(2)); INSERT quoted_peer VALUES('x '); SELECT CASE WHEN ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'x')=[@p] THEN 1 ELSE 0 END AS hit FROM quoted_peer",
    "expected": {
      "sets": [
        {
          "columns": [
            [
              "hit",
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
        1,
        null,
        1,
        1
      ]
    }
  },
  {
    "name": "control flow membership",
    "query": "DECLARE @hit INT=0; IF ISNULL((SELECT CAST(NULL AS NVARCHAR(2))),N'x') IN (N'x ',NULL) SET @hit=1; SELECT @hit AS hit",
    "expected": {
      "sets": [
        {
          "columns": [
            [
              "hit",
              "IntN",
              4
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
        1,
        null,
        1,
        1
      ]
    }
  }
])

test('declared scalar character predicates match captured SQL Server results', async t => {
  const connection = await start(t)
  for (const entry of declaredPredicateCases) {
    const raw = await capture(connection, entry.query)
    const actual = canonical({
      sets: raw.sets.map(set => ({columns: set.columns.map(c => [c.name,c.type,c.length]), rows:set.rows})),
      errors: raw.errors,
      done: raw.done.filter(d => d.kind === 'done' || d.kind === 'doneInProc').map(d => d.rowCount),
    })
    assert.deepEqual(actual, entry.expected, entry.name)
  }
})
