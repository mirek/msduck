// The module is staged until its public export can be claimed without
// colliding with the worker currently editing msduck-core/src/lib.rs.
#[path = "../src/at_time_zone.rs"]
mod at_time_zone;

use at_time_zone::{Resolution, Rules, Transition};
use msduck_core::datetime2::DateTime2;
use msduck_core::datetimeoffset::DateTimeOffset;
use serde_json::Value;

fn ticks(text: &str) -> i64 {
    DateTime2::parse_iso(text).unwrap().ticks()
}

fn central_europe() -> Rules<'static> {
    static TRANSITIONS: std::sync::LazyLock<Vec<Transition>> = std::sync::LazyLock::new(|| {
        vec![
            Transition {
                utc_ticks: ticks("2022-03-27T01:00:00"),
                offset_before_minutes: 60,
                offset_after_minutes: 120,
            },
            Transition {
                utc_ticks: ticks("2022-10-30T01:00:00"),
                offset_before_minutes: 120,
                offset_after_minutes: 60,
            },
        ]
    });
    Rules::new(60, &TRANSITIONS).unwrap()
}

fn pacific() -> Rules<'static> {
    static TRANSITIONS: std::sync::LazyLock<Vec<Transition>> = std::sync::LazyLock::new(|| {
        vec![
            Transition {
                utc_ticks: ticks("2024-03-10T10:00:00"),
                offset_before_minutes: -480,
                offset_after_minutes: -420,
            },
            Transition {
                utc_ticks: ticks("2024-11-03T09:00:00"),
                offset_before_minutes: -420,
                offset_after_minutes: -480,
            },
        ]
    });
    Rules::new(-480, &TRANSITIONS).unwrap()
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../reference/at-time-zone.json")).unwrap()
}

fn expected(fixture: &Value, name: &str) -> DateTimeOffset {
    let case = fixture["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap();
    let rendered = case["reference"]["sets"][0]["rows"][0][1].as_str().unwrap();
    DateTimeOffset::parse_iso(rendered).unwrap()
}

fn assert_match(actual: Resolution, expected: DateTimeOffset) {
    assert_eq!(actual.utc_ticks, expected.utc().ticks());
    assert_eq!(actual.local_ticks, expected.local().ticks());
    assert_eq!(actual.offset_minutes, expected.offset_minutes());
}

#[test]
fn central_europe_reference_gap_overlap_and_normal_times() {
    let fixture = fixture();
    let rules = central_europe();
    assert_match(
        rules
            .resolve_local(ticks("2024-01-02T03:04:05.1234567"))
            .unwrap(),
        expected(&fixture, "datetime2 named zone"),
    );
    for date in [
        "2022-03-27T01:59:59",
        "2022-03-27T02:00:00",
        "2022-03-27T02:30:00",
        "2022-03-27T02:59:59",
        "2022-03-27T03:00:00",
    ] {
        assert_match(
            rules.resolve_local(ticks(date)).unwrap(),
            expected(&fixture, &format!("Central Europe spring {date}")),
        );
    }
    for date in [
        "2022-10-30T01:59:59",
        "2022-10-30T02:00:00",
        "2022-10-30T02:30:00",
        "2022-10-30T02:59:59",
        "2022-10-30T03:00:00",
    ] {
        assert_match(
            rules.resolve_local(ticks(date)).unwrap(),
            expected(&fixture, &format!("Central Europe autumn {date}")),
        );
    }
}

#[test]
fn pacific_and_offset_aware_reference_cases() {
    let fixture = fixture();
    let pacific = pacific();
    assert_match(
        pacific.resolve_local(ticks("2024-03-10T02:30:00")).unwrap(),
        expected(&fixture, "Pacific spring gap"),
    );
    assert_match(
        pacific.resolve_local(ticks("2024-11-03T01:30:00")).unwrap(),
        expected(&fixture, "Pacific autumn overlap"),
    );
    let utc = Rules::new(0, &[]).unwrap();
    let source = DateTimeOffset::parse_iso("2024-01-02T03:04:05+02:00").unwrap();
    assert_match(
        utc.resolve_utc(source.utc().ticks()).unwrap(),
        expected(&fixture, "datetimeoffset changes zone"),
    );
    assert_match(
        central_europe().resolve_utc(source.utc().ticks()).unwrap(),
        expected(&fixture, "datetimeoffset same zone"),
    );
    let first = pacific.resolve_local(ticks("2024-01-02T03:04:05")).unwrap();
    assert_match(
        central_europe().resolve_utc(first.utc_ticks).unwrap(),
        expected(&fixture, "chained zone conversion"),
    );
}

#[test]
fn transition_snapshot_validation_and_range_boundaries() {
    let spring = Transition {
        utc_ticks: ticks("2022-03-27T01:00:00"),
        offset_before_minutes: 60,
        offset_after_minutes: 120,
    };
    let autumn = Transition {
        utc_ticks: ticks("2022-10-30T01:00:00"),
        offset_before_minutes: 120,
        offset_after_minutes: 60,
    };
    assert!(Rules::new(841, &[]).is_err());
    assert!(Rules::new(60, &[autumn, spring]).is_err());
    assert!(Rules::new(60, &[spring, spring]).is_err());
    assert!(Rules::new(0, &[spring]).is_err());
    assert!(
        Rules::new(
            60,
            &[Transition {
                offset_after_minutes: -841,
                ..spring
            }]
        )
        .is_err()
    );
    assert!(
        Rules::new(
            60,
            &[Transition {
                offset_after_minutes: 60,
                ..spring
            }]
        )
        .is_err()
    );
    assert!(
        Rules::new(
            60,
            &[Transition {
                utc_ticks: -1,
                ..spring
            }]
        )
        .is_err()
    );
    let rules = Rules::new(840, &[]).unwrap();
    assert!(rules.resolve_local(ticks("0001-01-01T00:00:00")).is_err());
    assert!(rules.resolve_utc(-1).is_err());
    assert_eq!(
        rules
            .resolve_utc(ticks("0001-01-01T00:00:00"))
            .unwrap()
            .offset_minutes,
        840
    );
    let negative = Rules::new(-840, &[]).unwrap();
    assert!(negative.resolve_utc(ticks("0001-01-01T00:00:00")).is_err());
    assert!(
        negative
            .resolve_local(ticks("9999-12-31T23:59:59.9999999"))
            .is_err()
    );
}

#[test]
fn long_history_and_nearby_transitions_use_the_correct_segment() {
    const DAY: i64 = 864_000_000_000;
    let base = ticks("2000-01-01T00:00:00");
    let mut transitions = Vec::new();
    let mut before = 0;
    for index in 0..20_000 {
        let after = if before == 0 { 60 } else { 0 };
        transitions.push(Transition {
            utc_ticks: base + i64::from(index) * 2 * DAY,
            offset_before_minutes: before,
            offset_after_minutes: after,
        });
        before = after;
    }
    let rules = Rules::new(0, &transitions).unwrap();
    let instant = transitions.last().unwrap().utc_ticks + DAY;
    let expected = rules.resolve_utc(instant).unwrap();
    assert_eq!(rules.resolve_local(expected.local_ticks).unwrap(), expected);

    // Two close changes can make a wall time lie inside a nominal spring gap
    // while a later UTC segment still supplies a valid occurrence.
    let close = [
        Transition {
            utc_ticks: ticks("2024-01-01T10:00:00"),
            offset_before_minutes: 0,
            offset_after_minutes: 60,
        },
        Transition {
            utc_ticks: ticks("2024-01-01T10:10:00"),
            offset_before_minutes: 60,
            offset_after_minutes: 0,
        },
    ];
    let rules = Rules::new(0, &close).unwrap();
    let local = ticks("2024-01-01T10:20:00");
    assert_eq!(rules.resolve_local(local).unwrap().utc_ticks, local);
}
