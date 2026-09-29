// Capture SQL Server's observed 2024 UTC offset changes for every catalog zone.
// --check validates the retained evidence without starting a container.
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { isDeepStrictEqual } from 'node:util'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, command, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const checking = process.argv[2] === '--check'
const output = (checking ? process.argv[3] : process.argv[2]) ?? 'reference/at-time-zone-transitions-2024.json'
const catalog = JSON.parse(await readFile(new URL('../reference/at-time-zone-catalog.json', import.meta.url), 'utf8'))
const names = catalog.zoneNames.reference.sets[0].rows.map(row => row[0])
const pinnedCatalogNameSha256 = '28653b74ec07656b5b49341691a0e37f8c08cdcd1c0238e2200266d7ecd66879'
const pinnedCaptureSha256 = 'ff2f56e2af969412341d4f74e2a98859ea0a68536d038522fe18bce3f84404f2'
const stamp = date => `CAST('${date}' AS DATETIMEOFFSET(7))`
const offset = (value, zone) => `DATEPART(TZOFFSET, ${value} AT TIME ZONE ${zone})`
const base = month => `2024-${String(month).padStart(2, '0')}-01T00:00:00+00:00`
const monthHours = month => (Date.UTC(2024, month, 1) - Date.UTC(2024, month - 1, 1)) / 3600000
const baselineQuery = `SELECT z.name AS zone_name,${offset(stamp(base(1)), 'z.name')} AS offset_minutes FROM sys.time_zone_info z ORDER BY z.name`
const sampleQuery = date => `SELECT z.name AS zone_name,${offset(`CAST('${date}T12:00:00' AS DATETIME2(7))`, 'z.name')} AS offset_minutes FROM sys.time_zone_info z ORDER BY z.name`
const sampleDates = ['2024-01-15', '2024-07-15']

function hourlyQuery(month) {
  // The next month's -1/0 pair covers every earlier month-end. December
  // also needs the following midnight to detect a change in its final hour.
  const last = monthHours(month) - (month === 12 ? 0 : 1)
  return `WITH sampled AS (SELECT z.name AS zone_name,g.value AS hour_index,${offset(`DATEADD(hour,CONVERT(int,g.value),${stamp(base(month))})`, 'z.name')} AS offset_minutes FROM sys.time_zone_info z CROSS JOIN GENERATE_SERIES(-1,${last}) g), sequenced AS (SELECT zone_name,hour_index,LAG(offset_minutes) OVER(PARTITION BY zone_name ORDER BY hour_index) AS previous_offset,offset_minutes FROM sampled) SELECT zone_name,hour_index,previous_offset,offset_minutes FROM sequenced WHERE previous_offset IS NOT NULL AND previous_offset<>offset_minutes ORDER BY zone_name,hour_index`
}

function minuteQuery(month, hourlyRows) {
  if (!hourlyRows.length) return null
  const values = hourlyRows.map(([name, hour, before, after]) => {
    if (!names.includes(name) || !Number.isInteger(hour) || !Number.isInteger(before) || !Number.isInteger(after)) throw new Error('invalid hourly transition row')
    const instant = new Date(Date.UTC(2024, month - 1, 1) + hour * 3600000).toISOString().slice(0, 19) + '+00:00'
    return `(N'${name.replaceAll("'", "''")}',${stamp(instant)},${before},${after})`
  }).join(',')
  const before = 'DATEADD(minute,-1,f.utc_instant) AT TIME ZONE c.zone_name'
  const after = 'f.utc_instant AT TIME ZONE c.zone_name'
  return `WITH changes(zone_name,utc_hour,previous_offset,next_offset) AS (SELECT zone_name,utc_hour,previous_offset,next_offset FROM (VALUES ${values}) v(zone_name,utc_hour,previous_offset,next_offset)), minutes AS (SELECT c.zone_name,c.utc_hour,c.previous_offset,c.next_offset,DATEADD(minute,CONVERT(int,g.value),DATEADD(hour,-1,c.utc_hour)) AS utc_instant FROM changes c CROSS JOIN GENERATE_SERIES(0,60) g), observed AS (SELECT m.*,${offset('m.utc_instant', 'm.zone_name')} AS observed_offset FROM minutes m), first_change AS (SELECT zone_name,utc_hour,MIN(utc_instant) AS utc_instant FROM observed WHERE observed_offset=next_offset GROUP BY zone_name,utc_hour) SELECT c.zone_name,c.utc_hour AS scanned_utc_hour,f.utc_instant AS utc_transition,c.previous_offset,c.next_offset,CONVERT(VARCHAR(40),${before}) AS before_rendered,${offset('DATEADD(minute,-1,f.utc_instant)', 'c.zone_name')} AS before_actual,CONVERT(VARCHAR(40),${after}) AS after_rendered,${offset('f.utc_instant', 'c.zone_name')} AS after_actual FROM changes c JOIN first_change f ON f.zone_name=c.zone_name AND f.utc_hour=c.utc_hour ORDER BY c.zone_name,c.utc_hour`
}

function rows(result, expectedColumns) {
  if (!Array.isArray(result?.sets) || result.sets.length !== 1 || !Array.isArray(result.done) || !Array.isArray(result.errors) || !Array.isArray(result.info) || result.errors.length || result.sets[0].columns.length !== expectedColumns) throw new Error('incomplete SQL Server capture')
  return result.sets[0].rows
}

function validate(fixture, pin = false) {
  if (fixture.image !== referenceImage || !fixture.version?.sets?.length || names.length !== 141 || createHash('sha256').update(JSON.stringify(names)).digest('hex') !== pinnedCatalogNameSha256) throw new Error('reference version or catalog names differ')
  if (pin && !isDeepStrictEqual(fixture.version, catalog.version)) throw new Error('retained SQL Server version differs from catalog capture')
  if (fixture.baseline.query !== baselineQuery || !isDeepStrictEqual(fixture.samples.map(item => item.query), sampleDates.map(sampleQuery)) || fixture.months.length !== 12) throw new Error('capture query set differs')
  const orderedNames = rows(fixture.baseline.reference, 2).map(row => row[0])
  if (!isDeepStrictEqual(orderedNames, names)) throw new Error('baseline zone coverage differs')
  const baseline = new Map(rows(fixture.baseline.reference, 2).map(([name, minutes]) => [name, minutes]))
  const catalogDrifts = []
  for (const [index, sample] of fixture.samples.entries()) {
    const actual = rows(sample.reference, 2)
    const captured = catalog.results.find(result => result.name === `local ${sampleDates[index]}T12:00:00`).reference.sets[0].rows
    if (!isDeepStrictEqual(actual.map(row => row[0]), names)) throw new Error('sample zone coverage differs')
    for (let row = 0; row < names.length; row++) if (actual[row][1] !== captured[row][3]) catalogDrifts.push(`${sampleDates[index]} ${names[row]}: ${captured[row][3]} -> ${actual[row][1]}`)
  }
  let transitions = 0
  const byName = new Map(names.map(name => [name, []]))
  for (let month = 1; month <= 12; month++) {
    const entry = fixture.months[month - 1]
    if (entry.month !== month || entry.hourly.query !== hourlyQuery(month)) throw new Error(`hourly query differs for month ${month}`)
    const hourly = rows(entry.hourly.reference, 4)
    const expectedMinuteQuery = minuteQuery(month, hourly)
    if (expectedMinuteQuery === null) {
      if (entry.minute !== null) throw new Error('unexpected minute capture')
      continue
    }
    if (entry.minute?.query !== expectedMinuteQuery) throw new Error(`minute query differs for month ${month}`)
    const refined = rows(entry.minute.reference, 9)
    if (refined.length !== hourly.length) throw new Error(`incomplete minute refinement for month ${month}`)
    const hourlyByKey = new Map()
    for (const [name, hour, before, after] of hourly) {
      const key = `${name}|${hour}`
      if (hourlyByKey.has(key) || !names.includes(name) || hour < 0 || hour > monthHours(month) - (month === 12 ? 0 : 1) || !Number.isInteger(before) || !Number.isInteger(after) || before === after) throw new Error(`invalid hourly change for ${name}`)
      hourlyByKey.set(key, { hour, before, after })
    }
    const refinedKeys = new Set()
    for (const [name, scannedHour, instant, before, after, beforeText, beforeActual, afterText, afterActual] of refined) {
      const scanned = Date.parse(scannedHour?.value)
      const hour = (scanned - Date.UTC(2024, month - 1, 1)) / 3600000
      const key = `${name}|${hour}`
      const row = hourlyByKey.get(key)
      if (refinedKeys.has(key)) throw new Error(`duplicate minute boundary for ${name}`)
      refinedKeys.add(key)
      if (!row || before !== row.before || after !== row.after || beforeActual !== before || afterActual !== after || typeof beforeText !== 'string' || typeof afterText !== 'string') throw new Error(`invalid minute boundary for ${name}`)
      const utc = Date.parse(instant?.value)
      const hourStart = Date.UTC(2024, month - 1, 1) + row.hour * 3600000
      if (scannedHour?.kind !== 'date' || instant?.kind !== 'date' || !Number.isFinite(utc) || utc <= hourStart - 3600000 || utc > hourStart || utc >= Date.UTC(2025, 0, 1) || utc % 60000 !== 0) throw new Error(`out-of-window minute boundary for ${name}`)
      byName.get(name).push({ utc, before, after })
    }
    transitions += refined.length
  }
  for (const [name, minutes] of baseline) if (!Number.isInteger(minutes) || minutes < -840 || minutes > 840) throw new Error(`invalid baseline offset for ${name}`)
  for (const name of names) {
    let current = baseline.get(name)
    let previousUtc = -Infinity
    for (const transition of byName.get(name).sort((a, b) => a.utc - b.utc)) {
      if (transition.utc <= previousUtc) throw new Error(`duplicate or unordered transition for ${name}`)
      if (transition.utc === Date.UTC(2024, 0, 1)) {
        if (transition.after !== current) throw new Error(`baseline offset differs from midnight transition for ${name}`)
      } else if (transition.before !== current) throw new Error(`discontinuous transition offsets for ${name}`)
      current = transition.after
      previousUtc = transition.utc
    }
  }
  for (const [index, sample] of fixture.samples.entries()) {
    const localNoon = Date.parse(`${sampleDates[index]}T12:00:00Z`)
    for (const [name, capturedOffset] of rows(sample.reference, 2)) {
      const history = byName.get(name)
      let estimate = baseline.get(name)
      for (let attempt = 0; attempt < 3; attempt++) {
        const utc = localNoon - estimate * 60000
        const observed = history.reduce((minutes, transition) => transition.utc <= utc ? transition.after : minutes, baseline.get(name))
        if (observed === estimate) break
        estimate = observed
      }
      if (estimate !== capturedOffset) throw new Error(`transition history differs from local-noon offset for ${sampleDates[index]} ${name}`)
    }
  }
  if (pin && catalogDrifts.length) throw new Error(`retained catalog offsets differ: ${catalogDrifts.join('; ')}`)
  if (pin && createHash('sha256').update(JSON.stringify(fixture)).digest('hex') !== pinnedCaptureSha256) throw new Error('retained transition capture differs from pinned evidence')
  return { transitions, catalogDrifts }
}

if (checking) {
  const fixture = JSON.parse(await readFile(output, 'utf8'))
  console.log(`checked ${validate(fixture, true).transitions} detected transitions across ${names.length} zones`)
} else {
  await refuseExistingFixture(output)
  await withReferenceContainer(async (config, container) => {
    const connection = await connect({ ...config, options: { ...config.options, requestTimeout: 180000 } })
    try {
      const version = canonical(await command(connection, 'SELECT @@VERSION AS version'))
      const baseline = { query: baselineQuery, reference: canonical(await capture(connection, baselineQuery)) }
      const samples = []
      for (const date of sampleDates) {
        const query = sampleQuery(date)
        samples.push({ query, reference: canonical(await capture(connection, query)) })
      }
      const months = []
      for (let month = 1; month <= 12; month++) {
        const query = hourlyQuery(month)
        const hourly = { query, reference: canonical(await capture(connection, query)) }
        const minuteSql = minuteQuery(month, rows(hourly.reference, 4))
        const minute = minuteSql === null ? null : { query: minuteSql, reference: canonical(await capture(connection, minuteSql)) }
        months.push({ month, hourly, minute })
        console.log(`month ${month}: ${rows(hourly.reference, 4).length} hourly changes, ${minute ? rows(minute.reference, 9).length : 0} refined`)
      }
      const fixture = { image: container.image, version, baseline, samples, months }
      const checked = validate(fixture)
      console.log(`captured ${checked.transitions} detected transitions; ${checked.catalogDrifts.length} catalog offset differences`)
      for (const drift of checked.catalogDrifts) console.log(`difference: ${drift}`)
      await writeNewFixture(output, fixture)
    } finally { connection.close() }
  })
}
