//! Native execution against an explicit SQL Server 2025 Windows-zone snapshot.
//! Public AT TIME ZONE syntax is bound separately; these functions are internal.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use duckdb::{
    core::{DataChunkHandle, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};

#[path = "../crates/msduck-core/src/at_time_zone.rs"]
mod transition_core;
use transition_core::{Rules, Transition};

const INVALID_ZONE: &str = "The time zone ID provided to AT TIME ZONE clause is invalid.";
const START: i64 = 473_038_272_000_000_000; // 1500-01-01 UTC
const HISTORICAL_START: i64 = 567_709_344_000_000_000; // 1800-01-01 UTC
const BASE_START: i64 = 599_266_080_000_000_000; // 1900-01-01 UTC
const FUTURE_START: i64 = 646_917_408_000_000_000; // 2051-01-01 UTC
const END: i64 = 662_695_776_000_000_000; // 2101-01-01 UTC, exclusive

struct Zone {
    initial: i16,
    transitions: Vec<Transition>,
}

#[derive(Clone)]
struct Catalog(Arc<HashMap<String, Zone>>);

struct HistorySnapshot<'a> {
    json: &'a str,
    source_sha: &'a str,
    following_sha: &'a str,
    start: i64,
    end: i64,
    expected_count: usize,
}

fn prepend_history(
    zones: &mut HashMap<String, Zone>,
    image: &serde_json::Value,
    snapshot: HistorySnapshot<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let historical: serde_json::Value = serde_json::from_str(snapshot.json)?;
    if historical["version"] != 1
        || historical["image"] != *image
        || historical["sourceSha256"] != snapshot.source_sha
        || historical["followingSourceSha256"] != snapshot.following_sha
        || historical["utcStart"]
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            != Some(snapshot.start)
        || historical["utcEndExclusive"]
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            != Some(snapshot.end)
    {
        return Err("invalid historical AT TIME ZONE rule snapshot provenance".into());
    }
    let records = historical["zones"]
        .as_array()
        .ok_or("missing historical zone table")?;
    if records.len() != zones.len() {
        return Err("incomplete historical AT TIME ZONE zone table".into());
    }
    let mut seen = HashSet::with_capacity(records.len());
    let mut count = 0;
    for record in records {
        let name = record["name"]
            .as_str()
            .ok_or("invalid historical zone name")?;
        let key = name.to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            return Err("duplicate historical time-zone name".into());
        }
        let zone = zones
            .get_mut(&key)
            .ok_or("unknown historical time-zone name")?;
        let initial = i16::try_from(
            record["initial"]
                .as_i64()
                .ok_or("invalid historical baseline")?,
        )?;
        let entries = record["transitions"]
            .as_array()
            .ok_or("missing historical transitions")?;
        count += entries.len();
        let mut transitions = Vec::with_capacity(entries.len() + zone.transitions.len());
        for entry in entries {
            let fields = entry.as_array().ok_or("invalid historical transition")?;
            if fields.len() != 3 {
                return Err("invalid historical transition".into());
            }
            let transition = Transition {
                utc_ticks: fields[0]
                    .as_str()
                    .ok_or("invalid historical instant")?
                    .parse()?,
                offset_before_minutes: i16::try_from(
                    fields[1].as_i64().ok_or("invalid historical offset")?,
                )?,
                offset_after_minutes: i16::try_from(
                    fields[2].as_i64().ok_or("invalid historical offset")?,
                )?,
            };
            if !(snapshot.start..snapshot.end).contains(&transition.utc_ticks) {
                return Err("historical time-zone transition outside captured range".into());
            }
            transitions.push(transition);
        }
        if transitions
            .last()
            .map_or(initial, |entry| entry.offset_after_minutes)
            != zone.initial
        {
            return Err("historical time-zone baseline differs from following rules".into());
        }
        transitions.extend(std::mem::take(&mut zone.transitions));
        Rules::new(initial, &transitions)?;
        zone.initial = initial;
        zone.transitions = transitions;
    }
    if count != snapshot.expected_count {
        return Err("incomplete historical time-zone transitions".into());
    }
    Ok(())
}

fn load() -> Result<Catalog, Box<dyn std::error::Error>> {
    let document: serde_json::Value =
        serde_json::from_str(include_str!("at_time_zone_rules_1900_2050.json"))?;
    if document["version"] != 1
        || document["sourceSha256"]
            != "2ecb9841cb43d0c0251f99e7e3ebd70c9f259473d12232fa6f6fb24e3dc226b9"
        || document["utcStart"]
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            != Some(BASE_START)
        || document["utcEndExclusive"]
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            != Some(FUTURE_START)
    {
        return Err("invalid AT TIME ZONE rule snapshot provenance".into());
    }
    let records = document["zones"].as_array().ok_or("missing zone table")?;
    if records.len() != 141 {
        return Err("incomplete AT TIME ZONE zone table".into());
    }
    let mut zones = HashMap::with_capacity(records.len());
    for record in records {
        let name = record["name"].as_str().ok_or("invalid zone name")?;
        if name.len() > 256 || !name.is_ascii() {
            return Err("invalid zone name".into());
        }
        let initial = i16::try_from(record["initial"].as_i64().ok_or("invalid baseline")?)?;
        let entries = record["transitions"]
            .as_array()
            .ok_or("missing transitions")?;
        let mut transitions = Vec::with_capacity(entries.len());
        for entry in entries {
            let fields = entry.as_array().ok_or("invalid transition")?;
            if fields.len() != 3 {
                return Err("invalid transition".into());
            }
            transitions.push(Transition {
                utc_ticks: fields[0].as_str().ok_or("invalid instant")?.parse()?,
                offset_before_minutes: i16::try_from(fields[1].as_i64().ok_or("invalid offset")?)?,
                offset_after_minutes: i16::try_from(fields[2].as_i64().ok_or("invalid offset")?)?,
            });
        }
        Rules::new(initial, &transitions)?;
        if zones
            .insert(
                name.to_ascii_lowercase(),
                Zone {
                    initial,
                    transitions,
                },
            )
            .is_some()
        {
            return Err("duplicate time-zone name".into());
        }
    }
    prepend_history(
        &mut zones,
        &document["image"],
        HistorySnapshot {
            json: include_str!("at_time_zone_rules_1800_1899.json"),
            source_sha: "e6683618f8d80f58dd48edfb7042dbe4296402b1f68e5d0a626ec7c4232ff3d1",
            following_sha: "2ecb9841cb43d0c0251f99e7e3ebd70c9f259473d12232fa6f6fb24e3dc226b9",
            start: HISTORICAL_START,
            end: BASE_START,
            expected_count: 15_200,
        },
    )?;
    prepend_history(
        &mut zones,
        &document["image"],
        HistorySnapshot {
            json: include_str!("at_time_zone_rules_1500_1799.json"),
            source_sha: "60aba524ce29c1199b54c99e32bcb0616a59fff99fa8927049b9aaab1173af79",
            following_sha: "e6683618f8d80f58dd48edfb7042dbe4296402b1f68e5d0a626ec7c4232ff3d1",
            start: START,
            end: HISTORICAL_START,
            expected_count: 45_600,
        },
    )?;
    let future: serde_json::Value =
        serde_json::from_str(include_str!("at_time_zone_rules_2051_2100.json"))?;
    if future["version"] != 1
        || future["image"] != document["image"]
        || future["sourceSha256"]
            != "e7f7798940c1664048626907a4e9b52357053ae9c72e41582e29c58a1d2d3f6c"
        || future["priorSourceSha256"] != document["sourceSha256"]
        || future["utcStart"]
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            != Some(FUTURE_START)
        || future["utcEndExclusive"]
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            != Some(END)
    {
        return Err("invalid future AT TIME ZONE rule snapshot provenance".into());
    }
    let future_records = future["zones"]
        .as_array()
        .ok_or("missing future zone table")?;
    if future_records.len() != zones.len() {
        return Err("incomplete future AT TIME ZONE zone table".into());
    }
    let mut seen = HashSet::with_capacity(future_records.len());
    let mut future_count = 0;
    for record in future_records {
        let name = record["name"].as_str().ok_or("invalid future zone name")?;
        let key = name.to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            return Err("duplicate future time-zone name".into());
        }
        let zone = zones.get_mut(&key).ok_or("unknown future time-zone name")?;
        let terminal = zone
            .transitions
            .last()
            .map_or(zone.initial, |entry| entry.offset_after_minutes);
        if i16::try_from(
            record["initial"]
                .as_i64()
                .ok_or("invalid future baseline")?,
        )? != terminal
        {
            return Err("future time-zone baseline differs from prior rules".into());
        }
        let entries = record["transitions"]
            .as_array()
            .ok_or("missing future transitions")?;
        future_count += entries.len();
        for entry in entries {
            let fields = entry.as_array().ok_or("invalid future transition")?;
            if fields.len() != 3 {
                return Err("invalid future transition".into());
            }
            let transition = Transition {
                utc_ticks: fields[0]
                    .as_str()
                    .ok_or("invalid future instant")?
                    .parse()?,
                offset_before_minutes: i16::try_from(
                    fields[1].as_i64().ok_or("invalid future offset")?,
                )?,
                offset_after_minutes: i16::try_from(
                    fields[2].as_i64().ok_or("invalid future offset")?,
                )?,
            };
            if !(FUTURE_START..END).contains(&transition.utc_ticks) {
                return Err("future time-zone transition outside captured range".into());
            }
            zone.transitions.push(transition);
        }
        Rules::new(zone.initial, &zone.transitions)?;
    }
    if future_count != 4000 {
        return Err("incomplete future time-zone transitions".into());
    }
    Ok(Catalog(Arc::new(zones)))
}

fn zone_name(
    vector: &duckdb::core::FlatVector<'_>,
    unicode: bool,
    row: usize,
    len: usize,
    stored: Option<&duckdb::core::FlatVector<'_>>,
) -> Result<String, Box<dyn std::error::Error>> {
    if unicode {
        let stored = stored.ok_or(INVALID_ZONE)?;
        if stored.row_is_null(row as u64) {
            return Err("invalid Unicode carrier: NULL payload".into());
        }
        let bytes = crate::unicode_carrier::bytes(stored, row, len)?;
        if bytes.len() > 512 || bytes.len() % 2 != 0 {
            return Err(INVALID_ZONE.into());
        }
        let units: Vec<_> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return Ok(String::from_utf16(&units).map_err(|_| INVALID_ZONE)?);
    }
    if vector.logical_type().id() != Id::Varchar {
        return Err(INVALID_ZONE.into());
    }
    let mut value = unsafe { vector.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
    let size = unsafe { duckdb::ffi::duckdb_string_t_length(value) as usize };
    if size > 256 {
        return Err(INVALID_ZONE.into());
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(
            duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
            size,
        )
    };
    Ok(std::str::from_utf8(bytes)
        .map_err(|_| INVALID_ZONE)?
        .to_owned())
}

fn lookup_key(name: &str) -> String {
    // SQL Server's Windows-zone lookup ignores NUL code units anywhere in the
    // supplied name. Retain the original string for diagnostics; do not treat
    // NUL as a terminator, since a suffix after NUL still affects lookup.
    name.chars()
        .filter(|character| *character != '\0')
        .collect::<String>()
        .to_ascii_lowercase()
}

struct Convert<const SCALE: u8, const INSTANT: bool>;
impl<const SCALE: u8, const INSTANT: bool> VScalar for Convert<SCALE, INSTANT> {
    type State = Catalog;

    fn volatile() -> bool {
        // SQL Server marks AT TIME ZONE nondeterministic because external rule
        // updates can change its results. Keep that declaration at the boundary.
        true
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        let source = if INSTANT {
            super::kind(SCALE)
        } else {
            LogicalTypeHandle::struct_type(&[(
                &format!("__msduck_datetime2_{SCALE}"),
                Id::Bigint.into(),
            )])
        };
        vec![ScalarFunctionSignature::exact(
            vec![source, Id::Any.into()],
            super::kind(SCALE),
        )]
    }

    fn invoke(
        catalog: &Catalog,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let name = input.flat_vector(1);
        let unicode = crate::unicode_carrier::is_logical(&name.logical_type());
        let stored = unicode.then(|| input.struct_vector(1).child(0, len));
        let structure = input.struct_vector(0);
        let ticks = structure.child(0, len);
        let old_offset = INSTANT.then(|| structure.child(1, len));
        let mut result = output.struct_vector();
        let mut utc_result = result.child(0, len);
        let mut offset_result = result.child(1, len);
        for row in 0..len {
            if source.row_is_null(row as u64) || name.row_is_null(row as u64) {
                result.set_null(row);
                utc_result.set_null(row);
                offset_result.set_null(row);
                continue;
            }
            if ticks.row_is_null(row as u64)
                || old_offset
                    .as_ref()
                    .is_some_and(|offset| offset.row_is_null(row as u64))
            {
                return Err("invalid temporal payload".into());
            }
            let zone_name = zone_name(&name, unicode, row, len, stored.as_ref())?;
            let key = lookup_key(&zone_name);
            let zone = catalog.0.get(&key).ok_or(INVALID_ZONE)?;
            let rules = Rules::new(zone.initial, &zone.transitions)?;
            let value = unsafe { ticks.as_slice_with_len::<i64>(len)[row] };
            if INSTANT {
                let offset = unsafe {
                    old_offset
                        .as_ref()
                        .ok_or("missing source offset")?
                        .as_slice_with_len::<i16>(len)[row]
                };
                let timestamp = crate::datetime2::DateTime2::from_ticks(value)?;
                crate::datetimeoffset::DateTimeOffset::from_utc(timestamp, offset)?;
            } else {
                crate::datetime2::DateTime2::from_ticks(value)?;
            }
            let resolved = if INSTANT {
                if key != "utc" && !(START..END).contains(&value) {
                    return Err("AT TIME ZONE instant outside captured rule range".into());
                }
                rules.resolve_utc(value)?
            } else {
                let resolved = rules.resolve_local(value)?;
                if key != "utc" && !(START..END).contains(&resolved.utc_ticks) {
                    return Err("AT TIME ZONE instant outside captured rule range".into());
                }
                resolved
            };
            let utc = crate::datetime2::DateTime2::from_ticks(resolved.utc_ticks)?;
            crate::datetimeoffset::DateTimeOffset::from_utc(utc, resolved.offset_minutes)?;
            unsafe {
                utc_result.as_mut_slice_with_len::<i64>(len)[row] = resolved.utc_ticks;
                offset_result.as_mut_slice_with_len::<i16>(len)[row] = resolved.offset_minutes;
            }
        }
        Ok(())
    }
}

pub(super) fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    let catalog = load().map_err(|_| duckdb::Error::InvalidQuery)?;
    macro_rules! register {($($s:literal),*)=>{$(
        db.register_scalar_function_with_state::<Convert<$s, false>>(
            concat!("__msduck_at_time_zone_local_", stringify!($s)), &catalog)?;
        db.register_scalar_function_with_state::<Convert<$s, true>>(
            concat!("__msduck_at_time_zone_instant_", stringify!($s)), &catalog)?;
    )*};}
    register!(0, 1, 2, 3, 4, 5, 6, 7);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> duckdb::Connection {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        db
    }

    fn converted(db: &duckdb::Connection, local: &str, zone: &str) -> (i64, i16) {
        let sql = "SELECT d.__msduck_datetimeoffset_7,d.__msduck_offset_minutes FROM (SELECT __msduck_at_time_zone_local_7(__msduck_datetime2_cast_7(?1),?2) d)";
        db.query_row(sql, [local, zone], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
    }

    #[test]
    fn snapshot_is_complete_and_transition_rules_match_captured_cases() {
        let catalog = load().unwrap();
        assert_eq!(catalog.0.len(), 141);
        assert_eq!(
            catalog
                .0
                .values()
                .map(|zone| zone.transitions.len())
                .sum::<usize>(),
            85_214
        );
        let db = db();
        assert_eq!(
            converted(&db, "2024-01-02T03:04:05.1234567", "utc"),
            converted(&db, "2024-01-02T03:04:05.1234567", "UTC")
        );
        for (local, zone, expected_utc, expected_offset) in [
            (
                "2022-03-27T02:30:00",
                "Central European Standard Time",
                "2022-03-27T01:30:00",
                120,
            ),
            (
                "2022-10-30T02:30:00",
                "Central European Standard Time",
                "2022-10-30T00:30:00",
                120,
            ),
            (
                "2024-03-10T02:30:00",
                "Pacific Standard Time",
                "2024-03-10T10:30:00",
                -420,
            ),
            (
                "2024-11-03T01:30:00",
                "Pacific Standard Time",
                "2024-11-03T08:30:00",
                -420,
            ),
            (
                "2011-12-31T12:00:00",
                "Samoa Standard Time",
                "2011-12-31T22:00:00",
                840,
            ),
            (
                "2051-01-15T12:00:00",
                "Pacific Standard Time",
                "2051-01-15T20:00:00",
                -480,
            ),
            (
                "2051-07-15T12:00:00",
                "Pacific Standard Time",
                "2051-07-15T19:00:00",
                -420,
            ),
            (
                "2075-07-15T12:00:00",
                "Central European Standard Time",
                "2075-07-15T10:00:00",
                120,
            ),
            (
                "2100-07-15T12:00:00",
                "Samoa Standard Time",
                "2100-07-14T23:00:00",
                780,
            ),
        ] {
            assert_eq!(
                converted(&db, local, zone),
                (
                    crate::datetime2::DateTime2::parse_iso(expected_utc)
                        .unwrap()
                        .ticks(),
                    expected_offset
                ),
                "{local} in {zone}"
            );
        }
    }

    #[test]
    fn name_lookup_ignores_nul_without_truncating_or_trimming() {
        let db = db();
        let local = "2024-07-01T12:34:56.1234567";
        let utc = converted(&db, local, "UTC");
        for name in ["uTc", "UT\0C", "\0UTC", "UTC\0"] {
            assert_eq!(converted(&db, local, name), utc, "{name:?}");
        }
        for name in ["UTC\0garbage", " UTC", "UTC ", "Etc/UTC", "ＵＴＣ"] {
            let sql = "SELECT __msduck_at_time_zone_local_7(__msduck_datetime2_cast_7(?1),?2)";
            assert!(
                db.query_row(sql, [local, name], |_| Ok(())).is_err(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn instant_nulls_bounds_and_vectors() {
        let db = db();
        let source = "__msduck_datetimeoffset_cast_7('2024-01-02T03:04:05.1234567+02:00')";
        let sql = format!(
            "SELECT d.__msduck_datetimeoffset_7,d.__msduck_offset_minutes FROM (SELECT __msduck_at_time_zone_instant_7({source},'UTC') d)"
        );
        let actual: (i64, i16) = db
            .query_row(&sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(
            actual,
            (
                crate::datetime2::DateTime2::parse_iso("2024-01-02T01:04:05.1234567")
                    .unwrap()
                    .ticks(),
                0
            )
        );
        for sql in [
            "SELECT __msduck_at_time_zone_local_7(__msduck_datetime2_cast_7(NULL),'UTC') IS NULL",
            "SELECT __msduck_at_time_zone_local_7(__msduck_datetime2_cast_7('2024-01-01'),NULL) IS NULL",
            "SELECT __msduck_at_time_zone_instant_7(__msduck_datetimeoffset_cast_7(NULL),'UTC') IS NULL",
        ] {
            assert!(db.query_row(sql, [], |row| row.get::<_, bool>(0)).unwrap());
        }
        for (stamp, zone) in [
            ("2024-01-01", "Not A Time Zone"),
            ("1499-12-31", "Pacific Standard Time"),
            ("2101-01-01", "Pacific Standard Time"),
        ] {
            let sql = format!(
                "SELECT __msduck_at_time_zone_local_7(__msduck_datetime2_cast_7('{stamp}'),'{zone}')"
            );
            assert!(db.execute_batch(&sql).is_err(), "{stamp} {zone}");
        }
        let sql = "SELECT count(*) FROM (SELECT __msduck_at_time_zone_local_7(__msduck_datetime2_cast_7('2024-01-01'),CASE WHEN i%17=0 THEN NULL ELSE 'UTC' END) d FROM range(6000) r(i)) WHERE d IS NOT NULL";
        assert_eq!(
            db.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap(),
            6000 - 353
        );
    }
}
