// The core export file belongs to a separate claim. Compile the standalone
// module against the same public dependencies until its owner exports it.
pub use msduck_core::{datetime2, diagnostic};
#[path = "../src/legacy_datetime.rs"]
mod legacy_datetime;
use datetime2::DateTime2;
use legacy_datetime::{Target, Value, from_datetime2, from_iso, try_from_iso};

fn driver_date(value: Value) -> String {
    let (days, units) = value.storage_parts();
    let milliseconds = match value.target() {
        Target::DateTime => (i64::from(units) * 1_000 + 150) / 300,
        Target::SmallDateTime => i64::from(units) * 60_000,
    };
    let ticks = (i64::from(days) + 693_595) * 864_000_000_000 + milliseconds * 10_000;
    format!(
        "{}Z",
        DateTime2::from_ticks(ticks).unwrap().format_iso(3).unwrap()
    )
}

#[test]
fn scalar_conversions_match_retained_sql_server_values_and_errors() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../reference/legacy-datetime.json")).unwrap();
    let mut checked = 0;
    for case in fixture["results"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let sql = case["sql"].as_str().unwrap();
        // Parameter codecs and compound expression/assignment observations
        // belong to integration. Only direct captured CAST/TRY_CAST here.
        if !sql.starts_with("SELECT CAST(") && !sql.starts_with("SELECT TRY_CAST(") {
            continue;
        }
        let target = if id.starts_with("SMALLDATETIME-") {
            Target::SmallDateTime
        } else {
            Target::DateTime
        };
        let text = sql.split('\'').nth(1).unwrap();
        let actual = if sql.starts_with("SELECT TRY_CAST(") {
            Ok(try_from_iso(target, Some(text)))
        } else if sql.contains(" AS DATETIME2(7)") {
            from_datetime2(target, Some(DateTime2::parse_iso(text).unwrap()))
        } else {
            from_iso(target, Some(text))
        };
        let errors = case["result"]["errors"].as_array().unwrap();
        if let Some(error) = errors.first() {
            let actual = actual.expect_err(id);
            assert_eq!(
                actual.number,
                error["number"].as_i64().unwrap() as i32,
                "{id}"
            );
            assert_eq!(actual.state, error["state"].as_u64().unwrap() as u8, "{id}");
            assert_eq!(
                actual.severity,
                error["class"].as_u64().unwrap() as u8,
                "{id}"
            );
            assert_eq!(actual.message, error["message"].as_str().unwrap(), "{id}");
        } else {
            let expected = &case["result"]["sets"][0]["rows"][0][0];
            match actual.expect(id) {
                Some(value) => assert_eq!(
                    driver_date(value),
                    expected["value"].as_str().unwrap(),
                    "{id}"
                ),
                None => assert!(expected.is_null(), "{id}"),
            }
        }
        checked += 1;
    }
    assert_eq!(checked, 48);
}

#[test]
fn values_preserve_nulls_and_exact_storage_units() {
    for target in [Target::DateTime, Target::SmallDateTime] {
        assert_eq!(from_iso(target, None).unwrap(), None);
        assert_eq!(from_datetime2(target, None).unwrap(), None);
        assert_eq!(try_from_iso(target, Some("invalid")), None);
    }
    for (text, parts) in [
        ("1899-12-31T23:59:59.997", (-1, 25_919_999)),
        ("1900-01-01T00:00:00.003", (0, 1)),
        ("1900-01-01T00:00:00.007", (0, 2)),
        ("1753-01-01T00:00:00", (-53_690, 0)),
    ] {
        assert_eq!(
            from_iso(Target::DateTime, Some(text))
                .unwrap()
                .unwrap()
                .storage_parts(),
            parts
        );
    }
    let text = "1900-01-01T00:00:29.999";
    assert_eq!(
        from_iso(Target::SmallDateTime, Some(text))
            .unwrap()
            .unwrap()
            .storage_parts(),
        (0, 1)
    );
    assert_eq!(
        from_datetime2(
            Target::SmallDateTime,
            Some(DateTime2::parse_iso(text).unwrap())
        )
        .unwrap()
        .unwrap()
        .storage_parts(),
        (0, 0)
    );
}
