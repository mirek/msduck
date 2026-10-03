#[path = "../src/temporal_guid_text.rs"]
mod rules;
use msduck_core::{
    datetime2::{DateTime2, Parts},
    datetimeoffset::DateTimeOffset,
    types::{Scale, Type},
};
use rules::{Contract, Domain, Error, Language, Profile, Stored, Text};
use serde_json::{Value, json};
use std::collections::HashMap;
const DAY: i64 = 864_000_000_000;
fn source(text: &str) -> Type {
    let scale = || {
        Scale::new(
            text.split_once('(')
                .unwrap()
                .1
                .trim_end_matches(')')
                .parse()
                .unwrap(),
        )
        .unwrap()
    };
    match text {
        "DATE" => Type::Date,
        "DATETIME" => Type::DateTime,
        "SMALLDATETIME" => Type::SmallDateTime,
        "UNIQUEIDENTIFIER" => Type::UniqueIdentifier,
        _ if text.starts_with("TIME(") => Type::Time(scale()),
        _ if text.starts_with("DATETIME2(") => Type::DateTime2(scale()),
        _ if text.starts_with("DATETIMEOFFSET(") => Type::DateTimeOffset(scale()),
        _ => panic!("uncaptured declaration {text}"),
    }
}
fn language(text: &str) -> Language {
    match text {
        "us_english" => Language::UsEnglish,
        "French" => Language::French,
        "German" => Language::German,
        _ => panic!("unknown language"),
    }
}
fn bytes(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    text.as_bytes()
        .chunks_exact(2)
        .map(|chunk| u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap())
        .collect()
}
fn unsigned_le(bytes: &[u8]) -> u64 {
    assert!(bytes.len() <= 8);
    let mut full = [0; 8];
    full[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(full)
}
fn decode(kind: Type, bytes: &[u8], sql_storage: bool) -> Stored {
    match kind {
        Type::Date => {
            assert_eq!(bytes.len(), 3);
            Stored::Date {
                days: unsigned_le(bytes) as u32,
            }
        }
        Type::Time(scale) => {
            let payload = if sql_storage {
                assert_eq!(bytes[0], scale.get());
                &bytes[1..]
            } else {
                bytes
            };
            assert_eq!(payload.len(), usize::from(scale.time_bytes()));
            Stored::Time {
                ticks: unsigned_le(payload) * 10u64.pow(u32::from(7 - scale.get())),
            }
        }
        Type::DateTime2(scale) => {
            let payload = if sql_storage {
                assert_eq!(bytes[0], scale.get());
                &bytes[1..]
            } else {
                bytes
            };
            Stored::DateTime2(DateTime2::decode(payload, scale.get()).unwrap())
        }
        Type::DateTimeOffset(scale) => {
            let payload = if sql_storage {
                assert_eq!(bytes[0], scale.get());
                &bytes[1..]
            } else {
                bytes
            };
            Stored::DateTimeOffset(DateTimeOffset::decode(payload, scale.get()).unwrap())
        }
        Type::DateTime => {
            assert_eq!(bytes.len(), 8);
            let days = bytes[..4].try_into().unwrap();
            let ticks = bytes[4..].try_into().unwrap();
            Stored::DateTime {
                days: if sql_storage {
                    i32::from_be_bytes(days)
                } else {
                    i32::from_le_bytes(days)
                },
                ticks_300: if sql_storage {
                    u32::from_be_bytes(ticks)
                } else {
                    u32::from_le_bytes(ticks)
                },
            }
        }
        Type::SmallDateTime => {
            assert_eq!(bytes.len(), 4);
            let days = bytes[..2].try_into().unwrap();
            let minutes = bytes[2..].try_into().unwrap();
            Stored::SmallDateTime {
                days: u32::from(if sql_storage {
                    u16::from_be_bytes(days)
                } else {
                    u16::from_le_bytes(days)
                }),
                minutes: if sql_storage {
                    u16::from_be_bytes(minutes)
                } else {
                    u16::from_le_bytes(minutes)
                },
            }
        }
        Type::UniqueIdentifier => Stored::Guid(bytes.try_into().unwrap()),
        _ => panic!("unsupported decoder declaration"),
    }
}
fn native_value(kind: Type, result: &Value) -> Option<Stored> {
    let raw = &result["sets"][0]["rows"][0][1];
    if raw.is_null() {
        None
    } else {
        assert_eq!(raw["kind"], "binary");
        Some(decode(kind, &bytes(raw["value"].as_str().unwrap()), true))
    }
}
fn native_parts(value: Stored) -> Vec<u32> {
    let (parts, offset, day_only, time_only, nanosecond) = match value {
        Stored::Guid(_) => return vec![],
        Stored::Date { days } => (
            DateTime2::from_ticks(i64::from(days) * DAY)
                .unwrap()
                .parts(),
            None,
            true,
            false,
            None,
        ),
        Stored::Time { ticks } => (
            DateTime2::from_ticks(ticks as i64).unwrap().parts(),
            None,
            false,
            true,
            None,
        ),
        Stored::DateTime2(value) => (value.parts(), None, false, false, None),
        Stored::DateTimeOffset(value) => (
            value.local().parts(),
            Some(value.offset_minutes()),
            false,
            false,
            None,
        ),
        Stored::DateTime { days, ticks_300 } => {
            let mut p = DateTime2::from_ticks((693595 + i64::from(days)) * DAY)
                .unwrap()
                .parts();
            let seconds = ticks_300 / 300;
            p.hour = (seconds / 3600) as u8;
            p.minute = (seconds / 60 % 60) as u8;
            p.second = (seconds % 60) as u8;
            (
                p,
                None,
                false,
                false,
                Some((u64::from(ticks_300 % 300) * 1_000_000_000 / 300) as u32),
            )
        }
        Stored::SmallDateTime { days, minutes } => {
            let mut p = DateTime2::from_ticks((693595 + i64::from(days)) * DAY)
                .unwrap()
                .parts();
            p.hour = (minutes / 60) as u8;
            p.minute = (minutes % 60) as u8;
            (p, None, false, false, None)
        }
    };
    let mut fields = vec![];
    if !time_only {
        fields.extend([
            u32::from(parts.year),
            u32::from(parts.month),
            u32::from(parts.day),
        ]);
    }
    if !day_only {
        fields.extend([
            u32::from(parts.hour),
            u32::from(parts.minute),
            u32::from(parts.second),
            nanosecond.unwrap_or(parts.fraction * 100),
        ]);
    }
    // Signed offsets are compared separately below, not narrowed to unsigned.
    assert!(offset.is_none() || matches!(value, Stored::DateTimeOffset(_)));
    fields
}
fn assert_native(kind: Type, value: Option<Stored>, result: &Value) {
    let expected = match kind {
        Type::Date => "Date",
        Type::Time(_) => "Time",
        Type::DateTime2(_) => "DateTime2",
        Type::DateTimeOffset(_) => "DateTimeOffset",
        Type::DateTime | Type::SmallDateTime => "DateTimeN",
        Type::UniqueIdentifier => "UniqueIdentifier",
        _ => unreachable!(),
    };
    let columns = &result["sets"][0]["columns"];
    assert_eq!(columns[0]["type"], expected);
    assert_eq!(columns[0]["userType"], 0);
    if matches!(kind, Type::DateTime | Type::SmallDateTime) {
        assert_eq!(
            columns[0]["length"],
            if kind == Type::DateTime { 8 } else { 4 }
        );
    }
    let row = result["sets"][0]["rows"][0].as_array().unwrap();
    match value {
        None => assert!(row.iter().all(Value::is_null)),
        Some(value) => {
            let fields = native_parts(value);
            for (index, expected) in fields.iter().enumerate() {
                assert_eq!(row[index + 2], *expected);
            }
            if let Stored::DateTimeOffset(value) = value {
                assert_eq!(row.last().unwrap(), &json!(value.offset_minutes()));
            }
        }
    }
}
fn key(input: &Value) -> String {
    format!(
        "{}/{}/{}",
        input["language"], input["declaration"], input["originalText"]
    )
}
fn plan(kind: Type, input: &Value) -> Contract {
    let role = input["role"].as_str().unwrap();
    let profile = if role.starts_with("default cast") {
        Profile::DefaultCast
    } else if role.starts_with("explicit style0") {
        Profile::ExplicitStyle(0)
    } else if role.starts_with("explicit style121") {
        Profile::ExplicitStyle(121)
    } else if role.starts_with("translate") {
        Profile::Translate
    } else if role.starts_with("concat") {
        Profile::ConcatWs
    } else {
        panic!("not a formatter role")
    };
    rules::contract(
        kind,
        profile,
        language(input["language"].as_str().unwrap()),
        if role.ends_with("unicode") {
            Domain::Utf16
        } else {
            Domain::Cp1252
        },
    )
    .unwrap()
}
fn rendered(plan: Contract, value: Option<Stored>, role: &str) -> Option<String> {
    let formatted = rules::format(plan, value).unwrap().map(|text| match text {
        Text::Ansi(text) => text,
        Text::Unicode(units) => String::from_utf16(&units).unwrap(),
    });
    // Compose the fixed captured companions; keep the oracle's full row intact.
    if role.starts_with("concat separator") {
        Some(format!("a{}b", formatted.unwrap_or_default()))
    } else if role.starts_with("concat") {
        Some(formatted.unwrap_or_default())
    } else {
        formatted
    }
}
#[test]
fn all_four_captures_replay_from_exact_storage_and_actual_tds_bindings() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/temporal-guid-format.json")).unwrap();
    let (mut formatted, mut failures, mut native, mut prepared_native) = (0, 0, 0, 0);
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            let records = run.as_array().unwrap();
            assert_eq!(records.len(), 5550);
            let source_records: HashMap<_, _> = records
                .iter()
                .filter(|record| {
                    record["input"]["kind"] == "typed SQL source"
                        && record["input"]["role"] == "native stored parts"
                })
                .map(|record| (key(&record["input"]), record))
                .collect();
            assert_eq!(source_records.len(), 366);
            for record in records {
                let input = &record["input"];
                if input["kind"] == "typed SQL source" {
                    let original = source_records[&key(input)];
                    let kind = source(input["declaration"].as_str().unwrap());
                    let result = &record["result"];
                    if !original["result"]["errors"].as_array().unwrap().is_empty() {
                        // Construction failures precede formatting and remain whole errors.
                        assert_eq!(result["errors"], original["result"]["errors"]);
                        // SQL Server can emit descriptors before construction fails.
                        for set in result["sets"].as_array().unwrap() {
                            assert!(set["rows"].as_array().unwrap().is_empty());
                        }
                        failures += 1;
                        continue;
                    }
                    assert!(result["errors"].as_array().unwrap().is_empty());
                    let stored = native_value(kind, &original["result"]);
                    let role = input["role"].as_str().unwrap();
                    if role == "native stored parts" {
                        assert_native(kind, stored, result);
                        native += 1;
                    } else {
                        assert_eq!(
                            json!([[rendered(plan(kind, input), stored, role)]]),
                            result["sets"][0]["rows"],
                            "{}",
                            record["name"]
                        );
                        formatted += 1;
                    }
                } else if input["kind"] == "prepared typed source" {
                    let kind = source(input["declaration"].as_str().unwrap());
                    let role = input["role"].as_str().unwrap();
                    for execution in record["prepared"]["executions"].as_array().unwrap() {
                        let wire = &execution["wire"][0];
                        let stored = if wire["length"] == "00" {
                            assert_eq!(wire["payload"], "");
                            None
                        } else {
                            Some(decode(
                                kind,
                                &bytes(wire["payload"].as_str().unwrap()),
                                false,
                            ))
                        };
                        let result = &execution["result"];
                        assert!(result["errors"].as_array().unwrap().is_empty());
                        assert_eq!(
                            result["sets"][0]["columns"],
                            record["prepared"]["prepare"]["sets"][0]["columns"]
                        );
                        if role == "native stored parts" {
                            assert_eq!(stored, native_value(kind, result));
                            assert_native(kind, stored, result);
                            prepared_native += 1;
                        } else {
                            assert_eq!(
                                json!([[rendered(plan(kind, input), stored, role)]]),
                                result["sets"][0]["rows"],
                                "{}",
                                record["name"]
                            );
                            formatted += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        (formatted, failures, native, prepared_native),
        (20920, 300, 1444, 176)
    );
}
#[test]
fn declarations_profiles_contexts_and_null_are_checked_before_values() {
    assert_eq!(
        rules::contract(
            Type::Int,
            Profile::DefaultCast,
            Language::UsEnglish,
            Domain::Cp1252
        ),
        Err(Error::UnknownSource)
    );
    assert_eq!(
        rules::contract(
            Type::Date,
            Profile::ExplicitStyle(126),
            Language::UsEnglish,
            Domain::Cp1252
        ),
        Err(Error::UnknownProfile)
    );
    assert_eq!(
        rules::contract(
            Type::Date,
            Profile::DefaultCast,
            Language::Unknown,
            Domain::Cp1252
        ),
        Err(Error::UnknownLanguage)
    );
    assert_eq!(
        rules::contract(
            Type::Date,
            Profile::DefaultCast,
            Language::French,
            Domain::Cp1252
        ),
        Err(Error::UnknownLanguage)
    );
    assert_eq!(
        rules::contract(
            Type::Date,
            Profile::DefaultCast,
            Language::UsEnglish,
            Domain::Unknown
        ),
        Err(Error::UnknownDomain)
    );
    let mut plan = rules::contract(
        Type::Date,
        Profile::DefaultCast,
        Language::UsEnglish,
        Domain::Cp1252,
    )
    .unwrap();
    assert_eq!(rules::format(plan, None), Ok(None));
    plan.profile = Profile::ExplicitStyle(9);
    assert_eq!(rules::format(plan, None), Err(Error::UnknownProfile));
    plan.profile = Profile::DefaultCast;
    assert_eq!(
        rules::format(plan, Some(Stored::Time { ticks: 0 })),
        Err(Error::TypeMismatch)
    );
}
#[test]
fn stored_ranges_and_scale_alignment_are_not_repaired() {
    let cases = [
        (Type::Date, Stored::Date { days: u32::MAX }),
        (Type::Date, Stored::Date { days: 3_652_059 }),
        (
            Type::Time(Scale::new(3).unwrap()),
            Stored::Time { ticks: 1 },
        ),
        (
            Type::Time(Scale::new(7).unwrap()),
            Stored::Time { ticks: DAY as u64 },
        ),
        (
            Type::DateTime,
            Stored::DateTime {
                days: i32::MIN,
                ticks_300: 0,
            },
        ),
        (
            Type::DateTime,
            Stored::DateTime {
                days: -53691,
                ticks_300: 0,
            },
        ),
        (
            Type::DateTime,
            Stored::DateTime {
                days: 0,
                ticks_300: 25_920_000,
            },
        ),
        (
            Type::SmallDateTime,
            Stored::SmallDateTime {
                days: 65536,
                minutes: 0,
            },
        ),
        (
            Type::SmallDateTime,
            Stored::SmallDateTime {
                days: 0,
                minutes: 1440,
            },
        ),
    ];
    for (kind, value) in cases {
        let plan = rules::contract(
            kind,
            Profile::DefaultCast,
            Language::UsEnglish,
            Domain::Utf16,
        )
        .unwrap();
        assert_eq!(
            rules::format(plan, Some(value)),
            Err(Error::InvalidStoredValue)
        );
    }
    let tiny = DateTime2::from_ticks(1).unwrap();
    for (kind, value) in [
        (
            Type::DateTime2(Scale::new(6).unwrap()),
            Stored::DateTime2(tiny),
        ),
        (
            Type::DateTimeOffset(Scale::new(6).unwrap()),
            Stored::DateTimeOffset(DateTimeOffset::from_utc(tiny, 0).unwrap()),
        ),
    ] {
        let plan = rules::contract(
            kind,
            Profile::DefaultCast,
            Language::UsEnglish,
            Domain::Cp1252,
        )
        .unwrap();
        assert_eq!(
            rules::format(plan, Some(value)),
            Err(Error::InvalidStoredValue)
        );
    }
    assert!(DateTimeOffset::from_local(DateTime2::from_ticks(0).unwrap(), 840).is_err());
    assert!(DateTimeOffset::from_utc(DateTime2::from_ticks(0).unwrap(), -840).is_err());
}
#[test]
fn locale_guid_and_bounded_output_contracts_are_explicit() {
    let epoch = DateTime2::from_parts(Parts {
        year: 2024,
        month: 3,
        day: 2,
        hour: 0,
        minute: 0,
        second: 0,
        fraction: 0,
    })
    .unwrap();
    let value = Stored::DateTime {
        days: (epoch.ticks() / DAY - 693595) as i32,
        ticks_300: 0,
    };
    let plan = rules::contract(
        Type::DateTime,
        Profile::DefaultCast,
        Language::German,
        Domain::Utf16,
    )
    .unwrap();
    assert_eq!(
        rules::format(plan, Some(value)),
        Ok(Some(Text::Unicode(
            "Mär  2 2024 12:00AM".encode_utf16().collect()
        )))
    );
    let guid = Stored::Guid([
        0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ]);
    let plan = rules::contract(
        Type::UniqueIdentifier,
        Profile::Translate,
        Language::UsEnglish,
        Domain::Cp1252,
    )
    .unwrap();
    assert_eq!(
        rules::format(plan, Some(guid)),
        Ok(Some(Text::Ansi(
            "00112233-4455-6677-8899-AABBCCDDEEFF".into()
        )))
    );
    let value = DateTimeOffset::from_local(
        DateTime2::parse_iso("9999-12-31T23:59:59.9999999").unwrap(),
        840,
    )
    .unwrap();
    let plan = rules::contract(
        Type::DateTimeOffset(Scale::new(7).unwrap()),
        Profile::DefaultCast,
        Language::UsEnglish,
        Domain::Utf16,
    )
    .unwrap();
    let Some(Text::Unicode(units)) =
        rules::format(plan, Some(Stored::DateTimeOffset(value))).unwrap()
    else {
        panic!("Unicode contract")
    };
    assert!(units.len() <= 36);
    assert_eq!(
        String::from_utf16(&units).unwrap(),
        "9999-12-31 23:59:59.9999999 +14:00"
    );
}
