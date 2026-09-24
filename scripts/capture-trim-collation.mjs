// Node preserves isolated UTF-16 units in captures. Run on a Docker-capable host.
import assert from 'node:assert/strict'
import { writeFile } from 'node:fs/promises'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { connect, command } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const output = process.argv[2] ?? 'reference/trim-collation.json'
const collations = [
  'SQL_Latin1_General_CP1_CI_AS', 'Latin1_General_100_CI_AS',
  'Latin1_General_100_CS_AS', 'Latin1_General_100_CI_AI',
  'Latin1_General_100_CS_AI', 'Latin1_General_100_BIN2',
]
const samples = [
  ['case', "N'AaxAa'", "N'a'"],
  ['accent', "N'éxÉe'", "N'e'"],
  ['nonbreaking space', "NCHAR(160)+N' x '+NCHAR(160)", "N' '"],
  ['tab', "NCHAR(9)+N'x'+NCHAR(9)", 'NCHAR(9)'],
  ['combining accent', "N'e'+NCHAR(769)+N'x'+NCHAR(769)", 'NCHAR(769)'],
  ['composed versus decomposed', "N'éxé'", "N'e'+NCHAR(769)"],
  ['decomposed versus composed', "N'e'+NCHAR(769)+N'x'+N'e'+NCHAR(769)", "N'é'"],
  ['sharp s expansion', "N'ßxß'", "N'ss'"],
  ['ligature expansion', "N'œxœ'", "N'oe'"],
  ['fullwidth letter', "N'ＡxＡ'", "N'A'"],
  ['null code unit', "NCHAR(0)+N'x'+NCHAR(0)", 'NCHAR(0)'],
  ['soft hyphen', "NCHAR(173)+N'x'+NCHAR(173)", 'NCHAR(173)'],
  ['surrogate pair and high set', "N'🦆x🦆'", 'NCHAR(55358)'],
  ['surrogate pair and low set', "N'🦆x🦆'", 'NCHAR(56710)'],
  ['isolated high and low set', "NCHAR(55358)+N'x'+NCHAR(55358)", 'NCHAR(56710)'],
  ['isolated surrogate default spaces', "N' '+NCHAR(55358)+N' '", "N' '"],
  ['supplementary set', "N'🦆x🦆'", "N'🦆'"],
  ['empty source', "N''", "N'x'"],
  ['empty set', "N' x '", "N''"],
  ['null source', 'CAST(NULL AS NVARCHAR(8))', "N'x'"],
  ['null set', "N'x'", 'CAST(NULL AS NVARCHAR(8))'],
]
const programs = []
const collate = (expr, name) => `(${expr}) COLLATE ${name}`
function explicit(name, source, characters) {
  return { name, query: `SELECT LTRIM(${source},${characters}) AS l,RTRIM(${source},${characters}) AS r,TRIM(${characters} FROM ${source}) AS b,TRIM(LEADING ${characters} FROM ${source}) AS tl,TRIM(TRAILING ${characters} FROM ${source}) AS tr` }
}
for (const collation of collations) {
  for (const [sample, source, characters] of samples) {
    programs.push({ ...explicit(`${collation}: ${sample}`, collate(source,collation), collate(characters,collation)), collation, sample })
  }
  const source = collate("NCHAR(160)+N' x '+NCHAR(160)",collation)
  programs.push({ name:`${collation}: omitted characters`, collation, sample:'omitted characters', query:`SELECT LTRIM(${source}) AS l,RTRIM(${source}) AS r,TRIM(${source}) AS b` })
}
for (const sourceCollation of ['Latin1_General_100_CI_AI','Latin1_General_100_CS_AS']) {
  for (const charsCollation of ['Latin1_General_100_CI_AI','Latin1_General_100_CS_AS']) {
    const source = collate("N'AéxÉa'",sourceCollation)
    const chars = collate("N'ae'",charsCollation)
    if (sourceCollation === charsCollation) {
      programs.push({ ...explicit(`precedence ${sourceCollation} / ${charsCollation}`,source,chars), sourceCollation, charsCollation })
    } else {
      for (const [operation,expression] of [
        ['ltrim',`LTRIM(${source},${chars})`], ['rtrim',`RTRIM(${source},${chars})`],
        ['trim',`TRIM(${chars} FROM ${source})`],
        ['trim leading',`TRIM(LEADING ${chars} FROM ${source})`],
        ['trim trailing',`TRIM(TRAILING ${chars} FROM ${source})`],
      ]) programs.push({name:`conflict ${operation}: ${sourceCollation} / ${charsCollation}`,query:`SELECT ${expression} AS n`,sourceCollation,charsCollation,expectError:468})
    }
  }
  programs.push({ ...explicit(`explicit source ${sourceCollation}`,collate("N'AéxÉa'",sourceCollation),"N'ae'"), sourceCollation })
  programs.push({ ...explicit(`explicit set ${sourceCollation}`,"N'AéxÉa'",collate("N'ae'",sourceCollation)), charsCollation:sourceCollation })
}
assert.equal(new Set(programs.map(p=>p.name)).size,programs.length)

await withReferenceContainer(async (config, container) => {
  const c = await connect(config)
  let tokens = []
  const debug = c.debug.token.bind(c.debug)
  c.debug.token = token => { if (token.name.startsWith('DONE')) tokens.push({...token}); debug(token) }
  try {
    const version = await command(c,'SELECT @@VERSION AS version')
    const results = []
    for (const program of programs) {
      tokens = []
      const reference = canonical(await capture(c,program.query))
      assert.ok(tokens.length > 0,program.name)
      if (program.expectError) {
        assert.deepEqual(reference.errors.map(e=>e.number),[program.expectError],program.name)
        assert.equal(reference.sets.length,0,program.name)
      } else {
        assert.equal(reference.errors.length,0,program.name)
        assert.equal(reference.sets.length,1,program.name)
        assert.equal(reference.sets[0].rows.length,1,program.name)
        assert.equal(reference.sets[0].columns.length,program.sample==='omitted characters'?3:5,program.name)
      }
      results.push({...program,reference,tokens:canonical(tokens)})
    }
    assert.equal(results.length,148)
    await writeFile(output,JSON.stringify({image:container.image,version,collations,results},null,2)+'\n')
    console.log(JSON.stringify({output,cases:results.length,errorCases:results.filter(r=>r.reference.errors.length).length}))
  } finally { c.close() }
})
