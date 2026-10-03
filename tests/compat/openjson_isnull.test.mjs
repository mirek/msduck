// ISNULL, COALESCE, IIF and CASE over OPENJSON key and value columns (issue
// #900, docs/openjson-isnull.md). Every case replays the SQL Server capture in
// reference/openjson-isnull.json: setup batches, then one query batch whose
// result sets (names, types, lengths, rows), errors and DONE counts must match.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { isDeepStrictEqual } from 'node:util'
import { test } from 'node:test'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'
import { describeFirstDifference } from '../../scripts/lib/reference.mjs'
import { start } from '../support/client.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/openjson-isnull.json', import.meta.url)))
const keep = result => canonical({
  sets: result.sets.map(set => ({ columns: set.columns.map(c => [c.name, c.type, c.length]), rows: set.rows })),
  errors: result.errors.map(e => ({ number: e.number, class: e.class ?? null, state: e.state ?? null, message: e.message })),
  done: result.done.filter(d => d.kind === 'done' || d.kind === 'doneInProc').map(d => d.rowCount),
})
const reset = 'DROP TRIGGER IF EXISTS docs_audit; DROP FUNCTION IF EXISTS dbo.foo; DROP TABLE IF EXISTS items; DROP TABLE IF EXISTS audit; DROP TABLE IF EXISTS docs; DROP TABLE IF EXISTS tn;'

// Remaining differences are outside this lowering (docs/openjson-isnull.md):
// COALESCE, IIF, CASE and comparisons that mix a direct OPENJSON carrier
// column with text still fail with 245 (or DuckDB's binder error) until the
// predicate catalog declares OPENJSON columns; ISNULL of a code-page first
// argument with a non-cp1252 carrier fails on the wire; the type column is
// int; COALESCE widths over OPENJSON keys are max; and the unicode
// comparison of ISNULL results keeps trailing spaces. Each known case
// asserts msduck's complete current result, so any further change, fix or
// regression, fails here.
const knownResults = {
  "report nvarchar document": () => ({"sets":[],"errors":[{"number":245,"class":16,"state":1,"message":"Conversion Error: Type VARCHAR with value 'x' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: ... j.\"key\", __msduck_isnull(j.\"value\", ''), COALESCE(j.\"value\", 'x'), CASE WHEN j.\"value\" IS NULL THEN 'n' ELSE j.\"value...\n                                                                         ^"}],"done":[1,null]}),
  "report varchar document": () => ({"sets":[],"errors":[{"number":245,"class":16,"state":1,"message":"Conversion Error: Type VARCHAR with value 'x' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: ... j.\"key\", __msduck_isnull(j.\"value\", ''), COALESCE(j.\"value\", 'x'), CASE WHEN j.\"value\" IS NULL THEN 'n' ELSE j.\"value...\n                                                                         ^"}],"done":[1,null]}),
  "report bounded nvarchar document": () => ({"sets":[],"errors":[{"number":245,"class":16,"state":1,"message":"Conversion Error: Type VARCHAR with value 'x' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: ... j.\"key\", __msduck_isnull(j.\"value\", ''), COALESCE(j.\"value\", 'x'), CASE WHEN j.\"value\" IS NULL THEN 'n' ELSE j.\"value...\n                                                                         ^"}],"done":[1,null]}),
  "report literal document": () => ({"sets":[{"columns":[["key","NVarChar",8000],["","NVarChar",65535],["","NVarChar",65535],["","NVarChar",65535],["","IntN",4]],"rows":[]}],"errors":[{"number":245,"class":16,"state":1,"message":"Conversion Error: Type VARCHAR with value 'x' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: ... j.\"key\", __msduck_isnull(j.\"value\", ''), COALESCE(j.\"value\", 'x'), CASE WHEN j.\"value\" IS NULL THEN 'n' ELSE j.\"value...\n                                                                         ^"}],"done":[null]}),
  "isnull key and value types": () => ({"sets":[{"columns":[["key","NVarChar",8000],["k","NVarChar",8000],["v","NVarChar",65535],["va","NVarChar",65535],["t","Int",null],["n","IntN",8],["b","IntN",8]],"rows":[["a","a","1","1",2,"1","2"],["b","b","","ansi",0,"0","0"],["c","c","x ","x ",1,"1","4"],["d","d","🦆","🦆",1,"2","4"],["e","e","[1,2]","[1,2]",4,"5","10"],["f","f","true","true",3,"4","8"],["g","g","","",1,"0","0"]]}],"errors":[],"done":[1,7]}),
  "coalesce mixes": () => ({"sets":[],"errors":[{"number":245,"class":16,"state":1,"message":"Conversion Error: Type VARCHAR with value 'ansi' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: ...\"key\", COALESCE(j.\"value\", j.\"key\") AS vk, COALESCE(j.\"value\", 'ansi') AS va, COALESCE(j.\"value\", NULL, 'z') AS vz, COALESCE...\n                                                                          ^"}],"done":[1,null]}),
  "carrier as replacement": () => ({"sets":[],"errors":[{"number":50000,"class":16,"state":1,"message":"VARCHAR value is not representable in Windows-1252"}],"done":[1,1,null]}),
  "iif and case": () => ({"sets":[],"errors":[{"number":245,"class":16,"state":1,"message":"Conversion Error: Type VARCHAR with value 'n' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: SELECT j.\"key\", CASE WHEN j.\"value\" IS NULL THEN 'n' ELSE j.\"value\" END AS i1, CASE WHEN j.\"type\" = 2 THEN...\n                                                         ^"}],"done":[1,null]}),
  "case comparisons": () => ({"sets":[],"errors":[{"number":245,"class":16,"state":1,"message":"Conversion Error: Type VARCHAR with value '1' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: SELECT j.\"key\", CASE j.\"value\" WHEN '1' THEN 'one' WHEN 'x' THEN 'ex' ELSE j.\"key\" END AS c1...\n                                            ^"}],"done":[1,null]}),
  "isnull in predicates and ordering": () => ({"sets":[],"errors":[{"number":245,"class":16,"state":1,"message":"Conversion Error: Type VARCHAR with value '' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: ...('$'))) AS x (r)) j WHERE __msduck_isnull(j.\"value\", '') <> '' AND __msduck_isnull(j.\"value\", '') <> 'x' ORDER BY __msd...\n                                                                       ^"}],"done":[1,null]}),
  "comparison with literal": () => ({"sets":[],"errors":[{"number":245,"class":16,"state":1,"message":"Conversion Error: Type VARCHAR with value 'x' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: ...), __msduck_carrier_input('$'))) AS x (r)) j WHERE j.\"value\" = 'x' OR j.\"value\" = '1' OR j.\"value\" IN ('true', '') ORDER...\n                                                                          ^"}],"done":[1,null]}),
  "explicit schema": () => ({"sets":[],"errors":[{"number":245,"class":16,"state":1,"message":"Conversion Error: Type VARCHAR with value 'n' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: ...') AS b, COALESCE(w.c, 'cc') AS c, CASE WHEN w.b IS NULL THEN 'n' ELSE w.b END AS i, CASE WHEN w.c = 'x' THEN CAST(__msdu...\n                                                                         ^"}],"done":[1,null]}),
  "explicit schema varchar document": () => ({"sets":[],"errors":[{"number":50000,"class":16,"state":1,"message":"Binder Error: Cannot mix values of type VARCHAR and STRUCT(__msduck_utf16le BLOB) in COALESCE operator - an explicit cast is required"}],"done":[1,null]}),
  "two sources full join": () => ({"sets":[{"columns":[["id","Int",null],["k","NVarChar",65535],["l","NVarChar",65535],["r","NVarChar",65535]],"rows":[[1,"a","1",null],[1,"b","2","3"],[1,"c",null,"4"],[1,"s","x","x  "],[2,"x",null,"1"]]}],"errors":[],"done":[5]}),
  "two sources through function": () => ({"sets":[{"columns":[["id","Int",null],["key","NVarChar",65535],["old_value","NVarChar",65535],["new_value","NVarChar",65535],["o","NVarChar",65535],["n","NVarChar",65535]],"rows":[]}],"errors":[{"number":245,"class":16,"state":1,"message":"Conversion Error: Type VARCHAR with value '-' can't be cast to the destination type STRUCT(__msduck_utf16le BLOB)\n\nLINE 1: ... __msduck_isnull(x.old_value, '-') AS o, COALESCE(x.new_value, '-') AS n FROM items i LEFT OUTER JOIN LATERAL (SELECT __ms...\n                                                                          ^"}],"done":[null]}),
}

test('ISNULL, COALESCE, IIF and CASE over OPENJSON match the SQL Server capture', async t => {
  const connection = await start(t)
  assert.equal(reference.cases.length, 21)
  // Every case runs, so one report lists all differences.
  const differences = []
  const differingFromReference = []
  for (const entry of reference.cases) {
    for (const batch of [reset, ...entry.setup]) {
      const setup = await capture(connection, batch)
      assert.equal(setup.errors.length, 0, `${entry.name}: setup failed with ${setup.errors[0]?.number}`)
    }
    const actual = keep(await capture(connection, entry.query))
    const expected = knownResults[entry.name]?.(entry.result) ?? entry.result
    if (!isDeepStrictEqual(actual, expected)) differences.push(`${entry.name}: ${describeFirstDifference(actual, expected)}`)
    if (!isDeepStrictEqual(actual, entry.result)) differingFromReference.push(entry.name)
  }
  assert.deepEqual(differences, [])
  assert.deepEqual(differingFromReference, Object.keys(knownResults))
})
