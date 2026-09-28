// Capture catalog-wide SQL Server time-zone evidence on the Docker-capable
// reference host. --check validates the retained fixture without a container.
import { readFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { isDeepStrictEqual } from 'node:util'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, command, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const checking = process.argv[2] === '--check'
const output = (checking ? process.argv[3] : process.argv[2]) ?? 'reference/at-time-zone-catalog.json'
const namesQuery = 'SELECT name FROM sys.time_zone_info ORDER BY name'
const pinnedNameCount = 141
const pinnedNamesSha256 = '28653b74ec07656b5b49341691a0e37f8c08cdcd1c0238e2200266d7ecd66879'
const pinnedCaptureSha256 = 'aa7f646d73662bd0934f9174a566cdb34e381a7acb34f0bfaedb183193dc2fc1'
const dates = [
  '1900-01-15T12:00:00',
  '1970-07-15T12:00:00',
  '2000-01-15T12:00:00',
  '2024-01-15T12:00:00',
  '2024-07-15T12:00:00',
  '2030-01-15T12:00:00',
  '2050-07-15T12:00:00',
]
const queryFor = expression => `SELECT z.name AS zone_name,v.value,CONVERT(VARCHAR(40),v.value) AS rendered,DATEPART(TZOFFSET,v.value) AS offset_minutes FROM sys.time_zone_info z CROSS APPLY (VALUES (${expression} AT TIME ZONE z.name)) v(value) ORDER BY z.name`
const cases = [
  ...dates.map(date => ({ name: `local ${date}`, query: queryFor(`CAST('${date}' AS DATETIME2(7))`) })),
  { name: 'UTC instant 2024-06-15T12:00:00', query: queryFor("CAST('2024-06-15T12:00:00+00:00' AS DATETIMEOFFSET(7))") },
]

function namesFrom(result, column) {
  if (!Array.isArray(result?.errors) || !Array.isArray(result?.info) || !Array.isArray(result?.done) || !Array.isArray(result?.sets) || result.errors.length || result.sets.length !== 1 || !Array.isArray(result.sets[0].rows)) throw new Error(`incomplete ${column} capture`)
  const names = result.sets[0].rows.map(row => row[column])
  if (names.some(name => typeof name !== 'string') || new Set(names).size !== names.length) throw new Error(`invalid ${column} zone names`)
  return names
}

function validate(fixture, pinNames = false) {
  if (fixture.image !== referenceImage || !fixture.version?.sets?.length) throw new Error('reference image or version is missing')
  if (fixture.zoneNames.query !== namesQuery || !isDeepStrictEqual(fixture.results.map(({ name, query }) => ({ name, query })), cases)) throw new Error('queries differ from source')
  const names = namesFrom(fixture.zoneNames.reference, 0)
  if (!names.length) throw new Error('empty SQL Server time-zone catalog')
  if (pinNames && (names.length !== pinnedNameCount || createHash('sha256').update(JSON.stringify(names)).digest('hex') !== pinnedNamesSha256)) throw new Error('zone-name set differs from the retained SQL Server capture')
  if (fixture.zoneNames.reference.sets[0].columns.length !== 1) throw new Error('incomplete zone-name descriptor')
  for (const entry of fixture.results) {
    const reference = entry.reference
    if (!Array.isArray(reference?.done) || !Array.isArray(reference?.info) || !Array.isArray(reference?.errors)) throw new Error(`incomplete events for ${entry.name}`)
    if (!isDeepStrictEqual(namesFrom(reference, 0), names)) throw new Error(`zone coverage differs for ${entry.name}`)
    if (reference.sets[0].columns.length !== 4 || reference.sets[0].rows.some(row => row.length !== 4 || typeof row[2] !== 'string' || !Number.isInteger(row[3]))) throw new Error(`incomplete offset evidence for ${entry.name}`)
  }
  return names.length
}

if (checking) {
  const fixture = JSON.parse(await readFile(output, 'utf8'))
  const count = validate(fixture, true)
  if (createHash('sha256').update(JSON.stringify(fixture)).digest('hex') !== pinnedCaptureSha256) throw new Error('retained SQL Server capture differs from the pinned evidence')
  console.log(`checked ${count} zones across ${cases.length} conversions`)
} else {
  await refuseExistingFixture(output)
  await withReferenceContainer(async (config, container) => {
    const connection = await connect(config)
    try {
      const version = canonical(await command(connection, 'SELECT @@VERSION AS version'))
      const zoneNames = { query: namesQuery, reference: canonical(await capture(connection, namesQuery)) }
      const results = []
      for (const { name, query } of cases) {
        const reference = canonical(await capture(connection, query))
        results.push({ name, query, reference })
        console.log(`${name}: ${reference.sets[0]?.rows.length ?? 0} rows, ${reference.errors.length} errors`)
      }
      const fixture = { image: container.image, version, zoneNames, results }
      console.log(`captured ${validate(fixture)} zones across ${cases.length} conversions`)
      await writeNewFixture(output, fixture)
    } finally { connection.close() }
  })
}
