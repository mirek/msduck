// Capture minute-resolved SQL Server Windows-zone changes from 0001 through 0499.
// --check validates the retained fixture without a container.
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { isDeepStrictEqual } from 'node:util'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { connect, command, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const checking = process.argv[2] === '--check'
const output = (checking ? process.argv[3] : process.argv[2]) ?? 'reference/at-time-zone-history-0001-0499.json'
const catalog = JSON.parse(await readFile(new URL('../reference/at-time-zone-catalog.json', import.meta.url), 'utf8'))
const following = JSON.parse(await readFile(new URL('../src/at_time_zone_rules_0500_0999.json', import.meta.url), 'utf8'))
const names = catalog.zoneNames.reference.sets[0].rows.map(row => row[0])
const pinnedNamesSha256 = '28653b74ec07656b5b49341691a0e37f8c08cdcd1c0238e2200266d7ecd66879'
const pinnedCaptureSha256 = '84b3382e1056aa72463e78947430c3ff422dadc35e2c46c0e08f39ae501f8b0a'
const sampleDates = ['0001-01-15', '0001-07-15', '0250-01-15', '0250-07-15', '0499-01-15', '0499-07-15']
const years = Array.from({ length: 50 }, (_, index) => 1 + index * 10)
const batchSize = 100
const utc = date => `CAST('${date}' AS DATETIMEOFFSET(7))`
const offset = (value, zone) => `DATEPART(TZOFFSET,${value} AT TIME ZONE ${zone})`
const yearStart = year => `${String(year).padStart(4, '0')}-01-01T00:00:00+00:00`
const yearMs = year => { const date = new Date(0); date.setUTCFullYear(year, 0, 1); date.setUTCHours(0, 0, 0, 0); return date.getTime() }
const endYear = start => Math.min(start + 10, 500)
const daysInChunk = start => (yearMs(endYear(start)) - yearMs(start)) / 86400000
const baselineQuery = `SELECT z.name AS zone_name,${offset(utc(yearStart(1)), 'z.name')} AS offset_minutes FROM sys.time_zone_info z ORDER BY z.name`
const sampleQuery = date => `SELECT z.name AS zone_name,${offset(`CAST('${date}T12:00:00' AS DATETIME2(7))`, 'z.name')} AS offset_minutes FROM sys.time_zone_info z ORDER BY z.name`
const boundaryCases = [
  ['utc-midnight', utc('0001-01-01T00:00:00+00:00')],
  ['utc-1400', utc('0001-01-01T14:00:00+00:00')],
  ['local-midnight', "CAST('0001-01-01T00:00:00' AS DATETIME2(7))"],
  ['local-noon', "CAST('0001-01-01T12:00:00' AS DATETIME2(7))"]
]
const boundaryQuery = expression => `SELECT z.name AS zone_name,CONVERT(VARCHAR(40),${expression} AT TIME ZONE z.name) AS converted,${offset(expression, 'z.name')} AS offset_minutes FROM sys.time_zone_info z ORDER BY z.name`
const localBoundaryTimes = ['00:00:00', '12:00:00', '14:00:00']
const localBoundaryQuery = (name, time) => {
  const literal = name.replaceAll("'", "''")
  const expression = `CAST('0001-01-01T${time}' AS DATETIME2(7))`
  return `SELECT z.name AS zone_name,CONVERT(VARCHAR(40),${expression} AT TIME ZONE z.name) AS converted,${offset(expression, 'z.name')} AS offset_minutes FROM (VALUES(N'${literal}')) z(name)`
}

function dailyQuery(start) {
  // Year 0001 starts at the first representable instant, so it has no -1 day.
  // The last chunk samples 0500-01-01 to detect a final-day change.
  const last = daysInChunk(start) - (endYear(start) === 500 ? 0 : 1)
  const first = start === 1 ? 0 : -1
  return `WITH sampled AS (SELECT z.name AS zone_name,g.value AS day_index,${offset(`DATEADD(day,CONVERT(int,g.value),${utc(yearStart(start))})`, 'z.name')} AS offset_minutes FROM sys.time_zone_info z CROSS JOIN GENERATE_SERIES(${first},${last}) g), sequenced AS (SELECT zone_name,day_index,LAG(offset_minutes) OVER(PARTITION BY zone_name ORDER BY day_index) AS previous_offset,offset_minutes FROM sampled) SELECT zone_name,day_index,previous_offset,offset_minutes FROM sequenced WHERE previous_offset IS NOT NULL AND previous_offset<>offset_minutes ORDER BY zone_name,day_index`
}

function values(rows, baseYear, withHour = false) {
  return rows.map(row => {
    const [name, day, a, b, c] = row
    if (!names.includes(name) || !Number.isInteger(day) || day < 0 || day > daysInChunk(baseYear) || !Number.isInteger(a) || !Number.isInteger(b) || (withHour && !Number.isInteger(c))) throw new Error('invalid captured transition row')
    return withHour
      ? `(N'${name.replaceAll("'", "''")}',${day},${a},${b},${c})`
      : `(N'${name.replaceAll("'", "''")}',${day},${a},${b})`
  }).join(',')
}

function hourlyQuery(start, dailyRows) {
  const records = values(dailyRows, start)
  const instant = `DATEADD(hour,CONVERT(int,g.value),DATEADD(day,c.day_index-1,${utc(yearStart(start))}))`
  return `WITH changes(zone_name,day_index,previous_offset,next_offset) AS (SELECT * FROM (VALUES ${records}) v(zone_name,day_index,previous_offset,next_offset)), hours AS (SELECT c.*,g.value AS hour_index,${instant} AS utc_instant FROM changes c CROSS JOIN GENERATE_SERIES(0,24) g), observed AS (SELECT h.*,${offset('h.utc_instant', 'h.zone_name')} AS observed_offset FROM hours h), first_change AS (SELECT zone_name,day_index,MIN(hour_index) AS hour_index FROM observed WHERE observed_offset=next_offset GROUP BY zone_name,day_index) SELECT c.zone_name,c.day_index,f.hour_index,c.previous_offset,c.next_offset,DATEADD(hour,CONVERT(int,f.hour_index),DATEADD(day,c.day_index-1,${utc(yearStart(start))})) AS utc_changed_hour,${offset(`DATEADD(hour,CONVERT(int,f.hour_index)-1,DATEADD(day,c.day_index-1,${utc(yearStart(start))}))`, 'c.zone_name')} AS before_actual,${offset(`DATEADD(hour,CONVERT(int,f.hour_index),DATEADD(day,c.day_index-1,${utc(yearStart(start))}))`, 'c.zone_name')} AS after_actual FROM changes c JOIN first_change f ON f.zone_name=c.zone_name AND f.day_index=c.day_index ORDER BY c.zone_name,c.day_index`
}

function minuteQuery(start, hourlyRows) {
  const records = values(hourlyRows.map(([name, day, hour, before, after]) => [name, day, hour, before, after]), start, true)
  const hour = `DATEADD(hour,c.hour_index,DATEADD(day,c.day_index-1,${utc(yearStart(start))}))`
  const before = 'DATEADD(minute,-1,selected.utc_instant) AT TIME ZONE c.zone_name'
  const after = 'selected.utc_instant AT TIME ZONE c.zone_name'
  return `WITH changes(zone_name,day_index,hour_index,previous_offset,next_offset) AS (SELECT * FROM (VALUES ${records}) v(zone_name,day_index,hour_index,previous_offset,next_offset)), minutes AS (SELECT c.*,g.value AS minute_index,DATEADD(minute,CONVERT(int,g.value),DATEADD(hour,-1,${hour})) AS utc_instant FROM changes c CROSS JOIN GENERATE_SERIES(0,60) g), observed AS (SELECT m.*,${offset('m.utc_instant', 'm.zone_name')} AS observed_offset FROM minutes m), first_change AS (SELECT zone_name,day_index,MIN(minute_index) AS minute_index FROM observed WHERE observed_offset=next_offset GROUP BY zone_name,day_index) SELECT c.zone_name,c.day_index,c.hour_index,f.minute_index,selected.utc_instant AS utc_transition,c.previous_offset,c.next_offset,CONVERT(VARCHAR(40),${before}) AS before_rendered,${offset('DATEADD(minute,-1,selected.utc_instant)', 'c.zone_name')} AS before_actual,CONVERT(VARCHAR(40),${after}) AS after_rendered,${offset('selected.utc_instant', 'c.zone_name')} AS after_actual FROM changes c JOIN first_change f ON f.zone_name=c.zone_name AND f.day_index=c.day_index JOIN minutes selected ON selected.zone_name=c.zone_name AND selected.day_index=c.day_index AND selected.minute_index=f.minute_index AND selected.hour_index=c.hour_index ORDER BY c.zone_name,c.day_index`
}

function rows(result, columnCount) {
  if (!Array.isArray(result?.sets) || result.sets.length !== 1 || !Array.isArray(result.sets[0]?.rows) || result.sets[0].columns.length !== columnCount || !Array.isArray(result.done) || !Array.isArray(result.errors) || !Array.isArray(result.info) || result.errors.length) throw new Error('incomplete SQL Server capture')
  return result.sets[0].rows
}

function batches(array) {
  return Array.from({ length: Math.ceil(array.length / batchSize) }, (_, index) => array.slice(index * batchSize, (index + 1) * batchSize))
}

function validate(fixture, pin = false) {
  if (fixture.image !== referenceImage || !fixture.version?.sets?.length || createHash('sha256').update(JSON.stringify(names)).digest('hex') !== pinnedNamesSha256 || fixture.baseline.query !== baselineQuery || !Array.isArray(fixture.chunks) || fixture.chunks.length !== years.length) throw new Error('reference image, names or shape differ')
  if (pin && !isDeepStrictEqual(fixture.version, catalog.version)) throw new Error('SQL Server version differs from retained catalog')
  if (!Array.isArray(fixture.boundaries) || fixture.boundaries.length !== boundaryCases.length) throw new Error('first-century boundary probes missing')
  for (const [index, [name, expression]] of boundaryCases.entries()) {
    const boundary = fixture.boundaries[index]
    const reference = boundary?.reference
    const expectedRows = [141, 141, 0, 31][index]
    const expectedErrors = index < 2 ? 0 : 1
    if (boundary.name !== name || boundary.query !== boundaryQuery(expression) || reference?.sets?.length !== 1 || reference.sets[0].columns.length !== 3 || reference.sets[0].rows.length !== expectedRows || reference.errors?.length !== expectedErrors || !Array.isArray(reference.info) || !Array.isArray(reference.done) || (expectedErrors && reference.errors[0].number !== 9813)) throw new Error(`first-century boundary differs for ${name}`)
    if (!isDeepStrictEqual(reference.sets[0].rows.map(row => row[0]), names.slice(0, expectedRows))) throw new Error(`boundary name order differs for ${name}`)
  }
  const boundaryOffset = (index, name) => fixture.boundaries[index].reference.sets[0].rows.find(row => row[0] === name)?.[2]
  if (boundaryOffset(0, 'Pacific Standard Time') !== 0 || boundaryOffset(0, 'Samoa Standard Time') !== 0 || boundaryOffset(1, 'Pacific Standard Time') !== -480 || boundaryOffset(1, 'Samoa Standard Time') !== -660) throw new Error('year-one negative-zone boundary differs')
  if (!Array.isArray(fixture.localBoundaries) || fixture.localBoundaries.length !== names.length * localBoundaryTimes.length) throw new Error('individual local boundary probes missing')
  for (const [index, boundary] of fixture.localBoundaries.entries()) {
    const name = names[Math.floor(index / localBoundaryTimes.length)]
    const time = localBoundaryTimes[index % localBoundaryTimes.length]
    const reference = boundary?.reference
    const result = reference?.sets?.[0]
    const rowCount = result?.rows?.length
    const errorCount = reference?.errors?.length
    if (boundary.name !== name || boundary.time !== time || boundary.query !== localBoundaryQuery(name, time) || reference.sets.length !== 1 || result.columns.length !== 3 || !Array.isArray(reference.info) || !Array.isArray(reference.done) || ![0, 1].includes(rowCount) || ![0, 1].includes(errorCount) || rowCount + errorCount !== 1 || (errorCount && reference.errors[0].number !== 9813) || (rowCount && result.rows[0][0] !== name) || (time === '14:00:00' && errorCount)) throw new Error(`individual local boundary differs for ${name} ${time}`)
  }
  const baselineRows = rows(fixture.baseline.reference, 2)
  if (!isDeepStrictEqual(baselineRows.map(row => row[0]), names)) throw new Error('baseline zone coverage differs')
  if (!isDeepStrictEqual(baselineRows, fixture.boundaries[0].reference.sets[0].rows.map(([name, , minutes]) => [name, minutes]))) throw new Error('first-instant baseline differs from boundary probe')
  const baseline = new Map(baselineRows)
  const histories = new Map(names.map(name => [name, []]))
  let transitions = 0
  for (const [index, chunk] of fixture.chunks.entries()) {
    const start = years[index]
    if (chunk.start !== start || chunk.daily.query !== dailyQuery(start)) throw new Error(`daily query differs for ${start}`)
    const daily = rows(chunk.daily.reference, 4)
    const groups = batches(daily)
    if (!Array.isArray(chunk.refinements) || chunk.refinements.length !== groups.length) throw new Error(`refinement count differs for ${start}`)
    for (const [groupIndex, group] of groups.entries()) {
      const refinement = chunk.refinements[groupIndex]
      if (refinement.hourly.query !== hourlyQuery(start, group)) throw new Error(`hourly query differs for ${start} group ${groupIndex}`)
      const hourly = rows(refinement.hourly.reference, 8)
      if (hourly.length !== group.length) throw new Error(`missing hourly boundary for ${start} group ${groupIndex}`)
      if (refinement.minute.query !== minuteQuery(start, hourly)) throw new Error(`minute query differs for ${start} group ${groupIndex}`)
      const minute = rows(refinement.minute.reference, 11)
      if (minute.length !== group.length) throw new Error(`missing minute boundary for ${start} group ${groupIndex}`)
      const dailyByKey = new Map(group.map(([name, day, before, after]) => [`${name}|${day}`, { before, after }]))
      const hourlyByKey = new Map(hourly.map(([name, day, hour, before, after, scanned, beforeActual, afterActual]) => {
        const key = `${name}|${day}`
        if (hour < 0 || hour > 24 || beforeActual !== before || afterActual !== after || scanned?.kind !== 'date') throw new Error(`invalid hourly boundary for ${key}`)
        const dailyRow = dailyByKey.get(key)
        if (!dailyRow || dailyRow.before !== before || dailyRow.after !== after) throw new Error(`hourly offset differs from daily for ${key}`)
        return [key, { hour, before, after }]
      }))
      const seen = new Set()
      for (const [name, day, hour, minuteIndex, instant, before, after, beforeText, beforeActual, afterText, afterActual] of minute) {
        const key = `${name}|${day}`
        const preceding = hourlyByKey.get(key)
        const epoch = Date.parse(instant?.value)
        const dayEnd = yearMs(start) + day * 86400000
        const hourEnd = dayEnd - 86400000 + hour * 3600000
        if (seen.has(key) || !preceding || hour !== preceding.hour || before !== preceding.before || after !== preceding.after || beforeActual !== before || afterActual !== after || !Number.isInteger(minuteIndex) || minuteIndex < 0 || minuteIndex > 60 || instant?.kind !== 'date' || !Number.isFinite(epoch) || epoch <= hourEnd - 3600000 || epoch > hourEnd || epoch >= yearMs(500) || epoch % 60000 !== 0 || typeof beforeText !== 'string' || typeof afterText !== 'string') throw new Error(`invalid minute boundary for ${key}`)
        seen.add(key)
        histories.get(name).push({ utc: epoch, before, after })
        transitions++
      }
    }
  }
  for (const [name, initial] of baseline) {
    if (!Number.isInteger(initial) || initial < -840 || initial > 840) throw new Error(`invalid initial offset for ${name}`)
    let current = initial
    let prior = -Infinity
    for (const transition of histories.get(name).sort((a, b) => a.utc - b.utc)) {
      if (transition.utc <= prior || transition.before < -840 || transition.before > 840 || transition.after < -840 || transition.after > 840 || transition.before === transition.after) throw new Error(`invalid transition sequence for ${name}`)
      if (transition.before !== current) throw new Error(`discontinuous offsets for ${name}`)
      current = transition.after
      prior = transition.utc
    }
  }
  const firstDay = new Map()
  for (const [name, history] of histories) {
    const early = history.filter(transition => transition.utc < yearMs(1) + 86400000)
    if (early.length > 1) throw new Error(`multiple first-day changes for ${name}`)
    if (early.length) firstDay.set(name, early[0])
  }
  const firstDayExpected = fixture.boundaries[1].reference.sets[0].rows.filter(([name, , minutes]) => baseline.get(name) === 0 && minutes < 0)
  if (firstDay.size !== 47 || firstDayExpected.length !== firstDay.size) throw new Error('year-one edge transition coverage differs')
  for (const [name, , minutes] of firstDayExpected) {
    const transition = firstDay.get(name)
    if (!transition || transition.utc !== yearMs(1) - minutes * 60000 || transition.before !== 0 || transition.after !== minutes) throw new Error(`year-one edge transition differs for ${name}`)
  }
  if (!Array.isArray(fixture.samples) || fixture.samples.length !== sampleDates.length) throw new Error('sample count differs')
  for (const [index, sample] of fixture.samples.entries()) {
    if (sample.query !== sampleQuery(sampleDates[index])) throw new Error('sample query differs')
    const actual = rows(sample.reference, 2)
    if (!isDeepStrictEqual(actual.map(row => row[0]), names)) throw new Error('sample zone coverage differs')
    const localNoon = Date.parse(`${sampleDates[index]}T12:00:00Z`)
    for (let row = 0; row < names.length; row++) {
      const [name, capturedOffset] = actual[row]
      let estimate = baseline.get(name)
      for (let attempt = 0; attempt < 3; attempt++) {
        const utcInstant = localNoon - estimate * 60000
        const observed = histories.get(name).reduce((offsetMinutes, transition) => transition.utc <= utcInstant ? transition.after : offsetMinutes, baseline.get(name))
        if (estimate === observed) break
        estimate = observed
      }
      if (estimate !== capturedOffset) throw new Error(`history disagrees with ${sampleDates[index]} ${name}`)
    }
  }
  if (following.utcStart !== '157469184000000000' || !isDeepStrictEqual(following.zones.map(zone => zone.name), names)) throw new Error('following rule snapshot differs')
  for (const [index, name] of names.entries()) {
    const zone = following.zones[index]
    const terminal = histories.get(name).at(-1)?.after ?? baseline.get(name)
    if (terminal !== zone.initial) throw new Error(`0500 boundary differs from following rules for ${name}`)
  }
  if (pin && createHash('sha256').update(JSON.stringify(fixture)).digest('hex') !== pinnedCaptureSha256) throw new Error('retained history capture differs from pinned evidence')
  return { transitions }
}

if (checking) {
  const fixture = JSON.parse(await readFile(output, 'utf8'))
  console.log(`checked ${validate(fixture, true).transitions} detected historical transitions across ${names.length} zones`)
} else {
  await refuseExistingFixture(output)
  await withReferenceContainer(async (config, container) => {
    const connection = await connect({ ...config, options: { ...config.options, requestTimeout: 180000 } })
    try {
      const version = canonical(await command(connection, 'SELECT @@VERSION AS version'))
      const boundaries = []
      for (const [name, expression] of boundaryCases) {
        const query = boundaryQuery(expression)
        boundaries.push({ name, query, reference: canonical(await capture(connection, query)) })
      }
      const localBoundaries = []
      for (const name of names) {
        for (const time of localBoundaryTimes) {
          const query = localBoundaryQuery(name, time)
          localBoundaries.push({ name, time, query, reference: canonical(await capture(connection, query)) })
        }
      }
      const baseline = { query: baselineQuery, reference: canonical(await capture(connection, baselineQuery)) }
      const samples = []
      for (const date of sampleDates) {
        const query = sampleQuery(date)
        samples.push({ query, reference: canonical(await capture(connection, query)) })
      }
      const chunks = []
      for (const start of years) {
        const query = dailyQuery(start)
        const daily = { query, reference: canonical(await capture(connection, query)) }
        const refinements = []
        for (const group of batches(rows(daily.reference, 4))) {
          const hourSql = hourlyQuery(start, group)
          const hourly = { query: hourSql, reference: canonical(await capture(connection, hourSql)) }
          const hourlyRows = rows(hourly.reference, 8)
          if (hourlyRows.length !== group.length) throw new Error(`incomplete hourly refinement for ${start}`)
          const minuteSql = minuteQuery(start, hourlyRows)
          const minute = { query: minuteSql, reference: canonical(await capture(connection, minuteSql)) }
          if (rows(minute.reference, 11).length !== group.length) throw new Error(`incomplete minute refinement for ${start}`)
          refinements.push({ hourly, minute })
        }
        chunks.push({ start, daily, refinements })
        console.log(`${start}-${endYear(start) - 1}: ${rows(daily.reference, 4).length} daily changes`)
      }
      const fixture = { image: container.image, version, boundaries, localBoundaries, baseline, samples, chunks }
      const checked = validate(fixture)
      console.log(`captured ${checked.transitions} detected historical changes`)
      await writeNewFixture(output, fixture)
    } finally { connection.close() }
  })
}
