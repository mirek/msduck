// Probe UTC conversions at SQL Server's temporal boundaries on the pinned image.
// --check validates retained evidence without starting a container.
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { isDeepStrictEqual } from 'node:util'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, command, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const output = process.argv[2] === '--check'
  ? process.argv[3] ?? 'reference/at-time-zone-utc-range.json'
  : process.argv[2] ?? 'reference/at-time-zone-utc-range.json'
const cases = [
  ['local minimum', "CAST('0001-01-01T00:00:00.0000000' AS DATETIME2(7))", 'UTC'],
  ['local before snapshot', "CAST('1899-12-31T23:59:59.9999999' AS DATETIME2(7))", 'UTC'],
  ['local snapshot start', "CAST('1900-01-01T00:00:00.0000000' AS DATETIME2(7))", 'UTC'],
  ['local snapshot end', "CAST('2050-12-31T23:59:59.9999999' AS DATETIME2(7))", 'UTC'],
  ['local after snapshot', "CAST('2051-01-01T00:00:00.0000000' AS DATETIME2(7))", 'UTC'],
  ['local maximum', "CAST('9999-12-31T23:59:59.9999999' AS DATETIME2(7))", 'UTC'],
  ['instant minimum', "CAST('0001-01-01T00:00:00.0000000+00:00' AS DATETIMEOFFSET(7))", 'UTC'],
  ['instant before snapshot', "CAST('1899-12-31T23:59:59.9999999+00:00' AS DATETIMEOFFSET(7))", 'UTC'],
  ['instant after snapshot', "CAST('2051-01-01T00:00:00.0000000+00:00' AS DATETIMEOFFSET(7))", 'UTC'],
  ['instant maximum', "CAST('9999-12-31T23:59:59.9999999+00:00' AS DATETIMEOFFSET(7))", 'UTC'],
  ['instant offset before snapshot', "CAST('1899-12-31T23:59:59.9999999+02:00' AS DATETIMEOFFSET(7))", 'UTC'],
  ['instant offset after snapshot', "CAST('2051-01-01T00:00:00.0000000+02:00' AS DATETIMEOFFSET(7))", 'UTC'],
  ['local Pacific before snapshot', "CAST('1899-12-31T12:00:00.0000000' AS DATETIME2(7))", 'Pacific Standard Time'],
  ['local Pacific after snapshot', "CAST('2051-01-01T12:00:00.0000000' AS DATETIME2(7))", 'Pacific Standard Time'],
  ['instant Pacific before snapshot', "CAST('1899-12-31T12:00:00.0000000+00:00' AS DATETIMEOFFSET(7))", 'Pacific Standard Time'],
  ['instant Pacific after snapshot', "CAST('2051-01-01T12:00:00.0000000+00:00' AS DATETIMEOFFSET(7))", 'Pacific Standard Time'],
].map(([name, source, zone]) => ({ name, query: `SELECT ${source} AT TIME ZONE '${zone}' AS value` }))
const retained = JSON.parse(await readFile(new URL('../reference/at-time-zone.json', import.meta.url), 'utf8'))
const pinnedCaptureSha256 = 'c2e01031921b15cf13a19ba7c9b45a3357f9cd3ea57c82889017d226d52853b3'

function validate(fixture, pin) {
  if (fixture.image !== referenceImage || !isDeepStrictEqual(fixture.version, retained.version)) throw new Error('reference image or SQL Server version changed')
  if (!isDeepStrictEqual(fixture.results.map(({ name, query }) => ({ name, query })), cases)) throw new Error('UTC-range query list changed')
  for (const { name, reference } of fixture.results) {
    if (!Array.isArray(reference?.sets) || !Array.isArray(reference?.done) || !Array.isArray(reference?.errors) || !Array.isArray(reference?.info) || reference.info.length) throw new Error(`incomplete capture: ${name}`)
    if (reference.errors.some(error => !Number.isInteger(error.number) || typeof error.message !== 'string')) throw new Error(`malformed diagnostic: ${name}`)
    if (!reference.errors.length && reference.sets.length !== 1) throw new Error(`missing result: ${name}`)
  }
  if (pin && createHash('sha256').update(JSON.stringify(fixture)).digest('hex') !== pinnedCaptureSha256) throw new Error('retained UTC-range evidence differs from pinned capture')
  return fixture.results.length
}

if (process.argv[2] === '--check') {
  const fixture = JSON.parse(await readFile(output, 'utf8'))
  console.log(`checked ${validate(fixture, true)} UTC-range probes`)
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
