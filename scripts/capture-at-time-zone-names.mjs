// Probe Windows-zone name matching on the pinned SQL Server 2025 image.
// --check validates the retained evidence without starting a container.
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { isDeepStrictEqual } from 'node:util'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, command, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const output = process.argv[2] === '--check'
  ? process.argv[3] ?? 'reference/at-time-zone-names.json'
  : process.argv[2] ?? 'reference/at-time-zone-names.json'
const source = "CAST('2024-07-01T12:34:56.1234567' AS DATETIME2(7))"
const literal = name => `N'${name.replaceAll("'", "''")}'`
const cases = [
  ['canonical UTC', literal('UTC')],
  ['lowercase UTC', literal('utc')],
  ['mixed-case UTC', literal('uTc')],
  ['canonical Pacific', literal('Pacific Standard Time')],
  ['lowercase Pacific', literal('pacific standard time')],
  ['uppercase Pacific', literal('PACIFIC STANDARD TIME')],
  ['leading space', literal(' UTC')],
  ['trailing space', literal('UTC ')],
  ['both spaces', literal(' UTC ')],
  ['empty name', literal('')],
  ['unknown name', literal('Not A Time Zone')],
  ['IANA UTC alias', literal('Etc/UTC')],
  ['IANA Zurich alias', literal('Europe/Zurich')],
  ['GMT alias', literal('GMT')],
  ['fullwidth UTC', literal('ＵＴＣ')],
  ['combining accent', literal('U\u0301TC')],
  ['embedded NUL', "CAST(N'UT'+NCHAR(0)+N'C' AS NVARCHAR(128))"],
  ['leading NUL', "CAST(NCHAR(0)+N'UTC' AS NVARCHAR(128))"],
  ['trailing NUL', "CAST(N'UTC'+NCHAR(0) AS NVARCHAR(128))"],
  ['NUL before suffix', "CAST(N'UTC'+NCHAR(0)+N'garbage' AS NVARCHAR(128))"],
  ['tab within name', "CAST(N'UT'+NCHAR(9)+N'C' AS NVARCHAR(128))"],
  ['dynamic lowercase', "CAST(N'utc' AS NVARCHAR(128))"],
  ['dynamic trailing space', "CAST(N'UTC ' AS NVARCHAR(128))"],
  ['dynamic NULL', 'CAST(NULL AS NVARCHAR(128))'],
].map(([name, zone]) => ({
  name,
  query: `SELECT ${source} AT TIME ZONE ${zone} AS value${name.includes('NUL') ? `,DATALENGTH(${zone}) AS input_bytes,CONVERT(VARBINARY(300),${zone}) AS input_raw` : ''}`,
}))
const retained = JSON.parse(await readFile(new URL('../reference/at-time-zone.json', import.meta.url), 'utf8'))
const pinnedCaptureSha256 = '14d60d7016a6d927342c193f2e8c7ea2e07ab4868961ae19e8311c007ae77bb3'

function validate(fixture, pin) {
  if (fixture.image !== referenceImage || !isDeepStrictEqual(fixture.version, retained.version)) throw new Error('reference image or SQL Server version changed')
  if (!isDeepStrictEqual(fixture.results.map(({ name, query }) => ({ name, query })), cases)) throw new Error('name-probe query list changed')
  for (const { name, reference } of fixture.results) {
    if (!Array.isArray(reference?.sets) || !Array.isArray(reference?.done) || !Array.isArray(reference?.errors) || !Array.isArray(reference?.info) || reference.info.length) throw new Error(`incomplete capture: ${name}`)
    if (reference.errors.some(error => !Number.isInteger(error.number) || typeof error.message !== 'string')) throw new Error(`malformed diagnostic: ${name}`)
    if (!reference.errors.length && reference.sets.length !== 1) throw new Error(`missing result: ${name}`)
  }
  if (pin && createHash('sha256').update(JSON.stringify(fixture)).digest('hex') !== pinnedCaptureSha256) throw new Error('retained name lookup evidence differs from pinned capture')
  return fixture.results.length
}

if (process.argv[2] === '--check') {
  const fixture = JSON.parse(await readFile(output, 'utf8'))
  console.log(`checked ${validate(fixture, true)} named-zone probes`)
} else {
  await refuseExistingFixture(output)
  await withReferenceContainer(async (config, container) => {
    const connection = await connect(config)
    try {
      const version = canonical(await command(connection, 'SELECT @@VERSION AS version'))
      const results = []
      for (const { name, query } of cases) {
        results.push({ name, query, reference: canonical(await capture(connection, query)) })
        console.log(name)
      }
      const fixture = { image: container.image, version, results }
      console.log(`captured ${validate(fixture, false)} probes; sha256 ${createHash('sha256').update(JSON.stringify(fixture)).digest('hex')}`)
      await writeNewFixture(output, fixture)
    } finally { connection.close() }
  })
}
