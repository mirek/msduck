#[path = "../src/datetrunc.rs"]
mod datetrunc;

use datetrunc::{Phase, RuleError, SourceType, TemporalType, Value};

const SECOND: i64 = 10_000_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;

fn sample(ty: TemporalType) -> Value {
    let (year, month, day, tick, offset) = match ty {
        TemporalType::Date => (2024, 5, 15, 0, 0),
        TemporalType::Time(s) => (1, 1, 1, quantized(s), 0),
        TemporalType::DateTime2(s) => (2024, 5, 15, quantized(s), 0),
        TemporalType::DateTimeOffset(s) => (2024, 5, 15, quantized(s), 330),
        TemporalType::DateTime => (
            2024,
            5,
            15,
            13 * HOUR + 47 * MINUTE + 39 * SECOND + 1_230_000,
            0,
        ),
        TemporalType::SmallDateTime => (2024, 5, 15, 13 * HOUR + 47 * MINUTE, 0),
    };
    Value::new(year, month, day, tick, offset).unwrap()
}

fn quantized(scale: u8) -> i64 {
    let tick = 13 * HOUR + 47 * MINUTE + 39 * SECOND + 1_234_567;
    let quantum = 10_i64.pow(u32::from(7 - scale));
    ((tick + quantum / 2) / quantum) * quantum
}

fn fixture_type(label: &str) -> Option<TemporalType> {
    let scaled = |prefix: &str| {
        label
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|digits| digits.parse::<u8>().ok())
    };
    if let Some(s) = scaled("time(") {
        return Some(TemporalType::Time(s));
    }
    if let Some(s) = scaled("datetime2(") {
        return Some(TemporalType::DateTime2(s));
    }
    if let Some(s) = scaled("datetimeoffset(") {
        return Some(TemporalType::DateTimeOffset(s));
    }
    match label {
        "date" => Some(TemporalType::Date),
        "datetime" => Some(TemporalType::DateTime),
        "smalldatetime" => Some(TemporalType::SmallDateTime),
        _ => None,
    }
}

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../reference/datetrunc-bucket.json")).unwrap()
}

#[test]
fn matrix_matches_all_four_reference_runs() {
    let fixture = fixture();
    let mut matched = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            let run = run.as_array().unwrap();
            assert_eq!(run[756]["name"], "datetrunc abbreviation yy");
            for case in run.iter().take(756) {
                let name = case["name"].as_str().unwrap();
                let (function, tail) = if let Some(tail) = name.strip_prefix("datetrunc ") {
                    ("datetrunc", tail)
                } else if let Some(tail) = name.strip_prefix("date_bucket ") {
                    ("date_bucket", tail)
                } else {
                    continue;
                };
                let Some((part, type_label)) = (if function == "datetrunc" {
                    tail.split_once(' ')
                } else {
                    tail.split_once(" 2 ")
                }) else {
                    continue;
                };
                let Some(ty) = fixture_type(type_label) else {
                    continue;
                };
                matched += 1;
                let binding = if function == "datetrunc" {
                    datetrunc::bind_trunc(part, SourceType::Temporal(ty))
                } else {
                    datetrunc::bind_bucket(part, ty, None)
                };
                let actual = binding.and_then(|bound| {
                    if function == "datetrunc" {
                        datetrunc::truncate(bound, Some(sample(ty)), 7)
                    } else {
                        datetrunc::bucket(bound, Some(2), Some(sample(ty)), None)
                    }
                    .and_then(|value| {
                        value
                            .map(|value| datetrunc::text(value, bound.result_type))
                            .transpose()
                    })
                });
                let errors = case["result"]["errors"].as_array().unwrap();
                let columns = case["result"]["sets"].as_array().unwrap();
                if let Some(column) = columns.first().map(|set| &set["columns"][0]) {
                    let expected_name = match ty {
                        TemporalType::Date => "Date",
                        TemporalType::Time(_) => "Time",
                        TemporalType::DateTime2(_) => "DateTime2",
                        TemporalType::DateTimeOffset(_) => "DateTimeOffset",
                        TemporalType::DateTime | TemporalType::SmallDateTime => "DateTimeN",
                    };
                    assert_eq!(column["type"], expected_name, "{name}: result type");
                    assert_eq!(column["flags"], 33, "{name}: result flags");
                    match ty {
                        TemporalType::Time(s)
                        | TemporalType::DateTime2(s)
                        | TemporalType::DateTimeOffset(s) => {
                            assert_eq!(column["scale"], s, "{name}: result scale");
                        }
                        TemporalType::DateTime => {
                            assert_eq!(column["length"], 8, "{name}: result length")
                        }
                        TemporalType::SmallDateTime => {
                            assert_eq!(column["length"], 4, "{name}: result length")
                        }
                        TemporalType::Date => {}
                    }
                }
                if let Some(error) = errors.first() {
                    let RuleError::Sql {
                        number,
                        state,
                        class,
                        phase,
                        message,
                    } = actual.expect_err(name)
                    else {
                        panic!("{name}: unexpected unsupported result")
                    };
                    assert_eq!(number, error["number"].as_i64().unwrap() as i32, "{name}");
                    assert_eq!(state, error["state"].as_u64().unwrap() as u8, "{name}");
                    assert_eq!(class, error["class"].as_u64().unwrap() as u8, "{name}");
                    assert_eq!(message, error["message"].as_str().unwrap(), "{name}");
                    let has_metadata = !case["result"]["sets"].as_array().unwrap().is_empty();
                    assert_eq!(
                        phase == Phase::Execution,
                        has_metadata,
                        "{name}: metadata phase"
                    );
                } else {
                    let actual = actual.unwrap().unwrap();
                    let expected = case["result"]["sets"][0]["rows"][0][1].as_str().unwrap();
                    assert_eq!(actual, expected, "{name}");
                }
            }
        }
    }
    assert_eq!(matched, 4 * (15 + 13) * 27);
}

#[test]
fn captured_abbreviations_and_untyped_bindings() {
    let fixture = fixture();
    let value = sample(TemporalType::DateTime2(7));
    let mut checked = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for case in run.as_array().unwrap() {
                let name = case["name"].as_str().unwrap();
                let actual = if let Some(keyword) = name.strip_prefix("datetrunc abbreviation ") {
                    datetrunc::bind_trunc(keyword, SourceType::Temporal(TemporalType::DateTime2(7)))
                        .and_then(|bound| {
                            datetrunc::truncate(bound, Some(value), 7).and_then(|v| {
                                v.map(|v| datetrunc::text(v, bound.result_type)).transpose()
                            })
                        })
                } else if let Some(keyword) = name.strip_prefix("date_bucket abbreviation ") {
                    datetrunc::bind_bucket(keyword, TemporalType::DateTime2(7), None).and_then(
                        |bound| {
                            datetrunc::bucket(bound, Some(1), Some(value), None).and_then(|v| {
                                v.map(|v| datetrunc::text(v, bound.result_type)).transpose()
                            })
                        },
                    )
                } else {
                    continue;
                };
                checked += 1;
                let errors = case["result"]["errors"].as_array().unwrap();
                if let Some(error) = errors.first() {
                    let RuleError::Sql {
                        number,
                        state,
                        class,
                        phase,
                        message,
                    } = actual.expect_err(name)
                    else {
                        panic!("{name}: unexpected unsupported result")
                    };
                    assert_eq!(number, error["number"].as_i64().unwrap() as i32, "{name}");
                    assert_eq!(state, error["state"].as_u64().unwrap() as u8, "{name}");
                    assert_eq!(class, error["class"].as_u64().unwrap() as u8, "{name}");
                    assert_eq!(message, error["message"].as_str().unwrap(), "{name}");
                    assert_eq!(
                        phase == Phase::Execution,
                        !case["result"]["sets"].as_array().unwrap().is_empty(),
                        "{name}"
                    );
                } else {
                    assert_eq!(
                        actual.unwrap().unwrap(),
                        case["result"]["sets"][0]["rows"][0][1].as_str().unwrap(),
                        "{name}"
                    );
                }
            }
        }
    }
    assert_eq!(checked, 4 * (25 + 15));

    assert_eq!(
        datetrunc::bind_trunc("day", SourceType::Character)
            .unwrap()
            .result_type,
        TemporalType::DateTime2(7)
    );
    assert_eq!(
        datetrunc::bind_trunc("day", SourceType::UntypedNull)
            .unwrap()
            .result_type,
        TemporalType::DateTime2(7)
    );
    assert_eq!(
        datetrunc::non_keyword_part(),
        RuleError::Sql {
            number: 1023,
            state: 1,
            class: 15,
            phase: Phase::Binding,
            message: "Invalid parameter 1 specified for datetrunc.".to_owned(),
        }
    );
    assert_eq!(
        datetrunc::non_keyword_part_for("Date_Bucket"),
        RuleError::Sql {
            number: 1023,
            state: 1,
            class: 15,
            phase: Phase::Binding,
            message: "Invalid parameter 1 specified for Date_Bucket.".to_owned(),
        }
    );
}

#[allow(clippy::too_many_arguments)] // Keep calendar fields visible in fixture probes.
fn value(
    year: i32,
    month: i32,
    day: i32,
    hour: i64,
    minute: i64,
    second: i64,
    frac: i64,
    offset: i16,
) -> Value {
    Value::new(
        year,
        month,
        day,
        hour * HOUR + minute * MINUTE + second * SECOND + frac,
        offset,
    )
    .unwrap()
}

#[test]
fn captured_origins_boundaries_and_datefirst() {
    let fixture = fixture();
    let mut checked = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for case in run.as_array().unwrap() {
                let name = case["name"].as_str().unwrap();
                if let Some(first) = name
                    .strip_prefix("datefirst ")
                    .and_then(|s| s.parse::<u8>().ok())
                {
                    let input = value(2024, 5, 15, 0, 0, 0, 0, 0);
                    let week = datetrunc::truncate(
                        datetrunc::bind_trunc("week", SourceType::Temporal(TemporalType::Date))
                            .unwrap(),
                        Some(input),
                        first,
                    )
                    .unwrap()
                    .unwrap();
                    let iso = datetrunc::truncate(
                        datetrunc::bind_trunc("iso_week", SourceType::Temporal(TemporalType::Date))
                            .unwrap(),
                        Some(input),
                        first,
                    )
                    .unwrap()
                    .unwrap();
                    let bucket = datetrunc::bucket(
                        datetrunc::bind_bucket("week", TemporalType::Date, None).unwrap(),
                        Some(1),
                        Some(input),
                        None,
                    )
                    .unwrap()
                    .unwrap();
                    let row = &case["result"]["sets"][0]["rows"][0];
                    for (index, actual) in [(1, week), (2, iso), (3, bucket)] {
                        assert_eq!(
                            format!(
                                "{}T00:00:00.000Z",
                                datetrunc::text(actual, TemporalType::Date).unwrap()
                            ),
                            row[index]["value"].as_str().unwrap(),
                            "{name} column {index}"
                        );
                    }
                    checked += 1;
                    continue;
                }
                let (function, part, ty, input, width, origin, datefirst) = match name {
                    "date_bucket year overflow width" => (
                        "bucket",
                        "year",
                        TemporalType::DateTime2(7),
                        sample(TemporalType::DateTime2(7)),
                        5000,
                        None,
                        7,
                    ),
                    "date_bucket millisecond large width" => (
                        "bucket",
                        "millisecond",
                        TemporalType::DateTime2(7),
                        sample(TemporalType::DateTime2(7)),
                        2147483647,
                        None,
                        7,
                    ),
                    "date_bucket default origin week" => (
                        "bucket",
                        "week",
                        TemporalType::Date,
                        value(2024, 5, 15, 0, 0, 0, 0, 0),
                        1,
                        None,
                        7,
                    ),
                    "date_bucket explicit default origin" => (
                        "bucket",
                        "day",
                        TemporalType::Date,
                        value(2024, 5, 15, 0, 0, 0, 0, 0),
                        3,
                        Some(value(1900, 1, 1, 0, 0, 0, 0, 0)),
                        7,
                    ),
                    "date_bucket origin after date" => (
                        "bucket",
                        "day",
                        TemporalType::Date,
                        value(2024, 5, 15, 0, 0, 0, 0, 0),
                        3,
                        Some(value(2024, 6, 1, 0, 0, 0, 0, 0)),
                        7,
                    ),
                    "date_bucket origin equals date" => (
                        "bucket",
                        "day",
                        TemporalType::Date,
                        value(2024, 5, 15, 0, 0, 0, 0, 0),
                        3,
                        Some(value(2024, 5, 15, 0, 0, 0, 0, 0)),
                        7,
                    ),
                    "date_bucket date lower bound" => (
                        "bucket",
                        "day",
                        TemporalType::Date,
                        value(1, 1, 1, 0, 0, 0, 0, 0),
                        7,
                        None,
                        7,
                    ),
                    "date_bucket date upper bound" => (
                        "bucket",
                        "year",
                        TemporalType::Date,
                        value(9999, 12, 31, 0, 0, 0, 0, 0),
                        3,
                        None,
                        7,
                    ),
                    "date_bucket datetime lower bound" => (
                        "bucket",
                        "week",
                        TemporalType::DateTime,
                        value(1753, 1, 1, 0, 0, 0, 0, 0),
                        1,
                        None,
                        7,
                    ),
                    "date_bucket smalldatetime lower bound" => (
                        "bucket",
                        "week",
                        TemporalType::SmallDateTime,
                        value(1900, 1, 1, 0, 0, 0, 0, 0),
                        1,
                        None,
                        7,
                    ),
                    "datetrunc iso_week date lower bound" => (
                        "trunc",
                        "iso_week",
                        TemporalType::Date,
                        value(1, 1, 1, 0, 0, 0, 0, 0),
                        0,
                        None,
                        7,
                    ),
                    "datetrunc year upper bound" => (
                        "trunc",
                        "year",
                        TemporalType::DateTime2(7),
                        value(9999, 12, 31, 23, 59, 59, 9_999_999, 0),
                        0,
                        None,
                        7,
                    ),
                    "datetrunc datetime millisecond rounding" => (
                        "trunc",
                        "millisecond",
                        TemporalType::DateTime,
                        value(2024, 5, 15, 13, 47, 39, 9_970_000, 0),
                        0,
                        None,
                        7,
                    ),
                    "datetrunc datetime second tick" => (
                        "trunc",
                        "second",
                        TemporalType::DateTime,
                        value(2024, 5, 15, 23, 59, 59, 9_970_000, 0),
                        0,
                        None,
                        7,
                    ),
                    "datetrunc datetimeoffset negative offset" => (
                        "trunc",
                        "day",
                        TemporalType::DateTimeOffset(0),
                        value(2024, 5, 15, 2, 0, 0, 0, -480),
                        0,
                        None,
                        7,
                    ),
                    "datetrunc datetimeoffset iso_week" => (
                        "trunc",
                        "iso_week",
                        TemporalType::DateTimeOffset(0),
                        value(2024, 5, 13, 2, 0, 0, 0, 840),
                        0,
                        None,
                        7,
                    ),
                    "date_bucket day width 1"
                    | "date_bucket day width 3"
                    | "date_bucket day width 7"
                    | "date_bucket day width 10"
                    | "date_bucket day width 100"
                    | "date_bucket day width 2147483647" => {
                        let width = name
                            .strip_prefix("date_bucket day width ")
                            .unwrap()
                            .parse()
                            .unwrap();
                        (
                            "bucket",
                            "day",
                            TemporalType::DateTime2(7),
                            sample(TemporalType::DateTime2(7)),
                            width,
                            None,
                            7,
                        )
                    }
                    "date_bucket date before default origin" => (
                        "bucket",
                        "day",
                        TemporalType::Date,
                        value(1899, 12, 30, 0, 0, 0, 0, 0),
                        7,
                        None,
                        7,
                    ),
                    "date_bucket date before origin month" => (
                        "bucket",
                        "month",
                        TemporalType::Date,
                        value(2024, 1, 15, 0, 0, 0, 0, 0),
                        5,
                        Some(value(2024, 5, 31, 0, 0, 0, 0, 0)),
                        7,
                    ),
                    "date_bucket month end origin" => (
                        "bucket",
                        "month",
                        TemporalType::Date,
                        value(2024, 2, 29, 0, 0, 0, 0, 0),
                        1,
                        Some(value(2024, 1, 31, 0, 0, 0, 0, 0)),
                        7,
                    ),
                    "date_bucket month end origin march" => (
                        "bucket",
                        "month",
                        TemporalType::Date,
                        value(2024, 3, 30, 0, 0, 0, 0, 0),
                        1,
                        Some(value(2024, 1, 31, 0, 0, 0, 0, 0)),
                        7,
                    ),
                    "date_bucket year origin leap day" => (
                        "bucket",
                        "year",
                        TemporalType::Date,
                        value(2025, 3, 1, 0, 0, 0, 0, 0),
                        1,
                        Some(value(2024, 2, 29, 0, 0, 0, 0, 0)),
                        7,
                    ),
                    "date_bucket quarter origin" => (
                        "bucket",
                        "quarter",
                        TemporalType::Date,
                        value(2024, 5, 15, 0, 0, 0, 0, 0),
                        1,
                        Some(value(2024, 2, 10, 0, 0, 0, 0, 0)),
                        7,
                    ),
                    "date_bucket hour origin fraction" => (
                        "bucket",
                        "hour",
                        TemporalType::DateTime2(7),
                        sample(TemporalType::DateTime2(7)),
                        1,
                        Some(value(2024, 5, 15, 0, 30, 0, 5_000_000, 0)),
                        7,
                    ),
                    "date_bucket datetimeoffset origin different offset" => (
                        "bucket",
                        "hour",
                        TemporalType::DateTimeOffset(0),
                        value(2024, 5, 15, 13, 47, 39, 0, 330),
                        1,
                        Some(value(2024, 5, 15, 0, 30, 0, 0, 0)),
                        7,
                    ),
                    "date_bucket datetimeoffset day" => (
                        "bucket",
                        "day",
                        TemporalType::DateTimeOffset(0),
                        value(2024, 5, 15, 2, 0, 0, 0, 330),
                        1,
                        None,
                        7,
                    ),
                    "date_bucket time origin" => (
                        "bucket",
                        "minute",
                        TemporalType::Time(0),
                        value(1, 1, 1, 13, 47, 39, 0, 0),
                        15,
                        Some(value(1, 1, 1, 0, 5, 0, 0, 0)),
                        7,
                    ),
                    "date_bucket date below minimum" => (
                        "bucket",
                        "day",
                        TemporalType::Date,
                        value(1, 1, 1, 0, 0, 0, 0, 0),
                        10,
                        None,
                        7,
                    ),
                    "date_bucket datetime below minimum" => (
                        "bucket",
                        "day",
                        TemporalType::DateTime,
                        value(1753, 1, 1, 0, 0, 0, 0, 0),
                        3,
                        None,
                        7,
                    ),
                    "datetrunc week date lower bound" => (
                        "trunc",
                        "week",
                        TemporalType::Date,
                        value(1, 1, 1, 0, 0, 0, 0, 0),
                        0,
                        None,
                        7,
                    ),
                    "datetrunc week datetime lower bound" => (
                        "trunc",
                        "week",
                        TemporalType::DateTime,
                        value(1753, 1, 1, 0, 0, 0, 0, 0),
                        0,
                        None,
                        7,
                    ),
                    "datetrunc week smalldatetime lower bound" => (
                        "trunc",
                        "week",
                        TemporalType::SmallDateTime,
                        value(1900, 1, 1, 0, 0, 0, 0, 0),
                        0,
                        None,
                        7,
                    ),
                    "datetrunc iso_week year boundary" => (
                        "trunc",
                        "iso_week",
                        TemporalType::Date,
                        value(2021, 1, 1, 0, 0, 0, 0, 0),
                        0,
                        None,
                        7,
                    ),
                    "datetrunc week year boundary" => (
                        "trunc",
                        "week",
                        TemporalType::Date,
                        value(2021, 1, 1, 0, 0, 0, 0, 0),
                        0,
                        None,
                        7,
                    ),
                    "date_bucket negative interval" => (
                        "bucket",
                        "hour",
                        TemporalType::DateTime2(0),
                        value(1899, 12, 31, 1, 0, 0, 0, 0),
                        5,
                        None,
                        7,
                    ),
                    _ => continue,
                };
                checked += 1;
                let actual = if function == "trunc" {
                    datetrunc::bind_trunc(part, SourceType::Temporal(ty))
                        .and_then(|bound| datetrunc::truncate(bound, Some(input), datefirst))
                } else {
                    datetrunc::bind_bucket(part, ty, None).and_then(|bound| {
                        datetrunc::bucket(bound, Some(width), Some(input), origin)
                    })
                };
                let errors = case["result"]["errors"].as_array().unwrap();
                if let Some(error) = errors.first() {
                    let RuleError::Sql {
                        number,
                        state,
                        class,
                        message,
                        ..
                    } = actual.expect_err(name)
                    else {
                        panic!("{name}: unexpected unsupported result")
                    };
                    assert_eq!(number, error["number"].as_i64().unwrap() as i32, "{name}");
                    assert_eq!(state, error["state"].as_u64().unwrap() as u8, "{name}");
                    assert_eq!(class, error["class"].as_u64().unwrap() as u8, "{name}");
                    assert_eq!(message, error["message"].as_str().unwrap(), "{name}");
                } else {
                    let actual = datetrunc::text(actual.unwrap().unwrap(), ty).unwrap();
                    assert_eq!(
                        actual,
                        case["result"]["sets"][0]["rows"][0][1].as_str().unwrap(),
                        "{name}"
                    );
                }
            }
        }
    }
    assert_eq!(checked, 4 * (7 + 24 + 16));
}

#[test]
fn captured_null_width_and_origin_binding() {
    use datetrunc::WidthInput;
    let fixture = fixture();
    let mut checked = 0;
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            for case in run.as_array().unwrap() {
                let name = case["name"].as_str().unwrap();
                let source = TemporalType::DateTime2(7);
                let mut result_type = source;
                let actual = match name {
                    "datetrunc integer input" => {
                        datetrunc::bind_trunc("day", SourceType::Integer).map(|_| None)
                    }
                    "datetrunc decimal input" => {
                        datetrunc::bind_trunc("day", SourceType::Numeric).map(|_| None)
                    }
                    "date_bucket integer input" => {
                        datetrunc::bind_bucket_source("day", SourceType::Integer, None)
                            .map(|_| None)
                    }
                    "date_bucket null date" => {
                        datetrunc::bind_bucket("day", TemporalType::DateTime2(3), None)
                            .and_then(|bound| datetrunc::bucket(bound, Some(1), None, None))
                            .map(|_| None)
                    }
                    "date_bucket untyped null date" => {
                        datetrunc::bind_bucket_source("day", SourceType::UntypedNull, None)
                            .map(|_| None)
                    }
                    "date_bucket null width"
                    | "date_bucket untyped null width"
                    | "date_bucket width string" => {
                        let width = match name {
                            "date_bucket null width" => WidthInput::Integer(None),
                            "date_bucket untyped null width" => WidthInput::UntypedNull,
                            _ => WidthInput::Character,
                        };
                        datetrunc::bind_width(width).and_then(|width| {
                            let bound = datetrunc::bind_bucket("day", source, None)?;
                            datetrunc::bucket(bound, width, Some(sample(source)), None).and_then(
                                |v| v.map(|v| datetrunc::text(v, bound.result_type)).transpose(),
                            )
                        })
                    }
                    "date_bucket null origin" | "date_bucket untyped null origin" => {
                        let origin = (name == "date_bucket null origin")
                            .then_some(SourceType::Temporal(source))
                            .or(Some(SourceType::UntypedNull));
                        datetrunc::bind_bucket_source("day", SourceType::Temporal(source), origin)
                            .and_then(|bound| {
                                datetrunc::bucket(bound, Some(1), Some(sample(source)), None)
                                    .and_then(|v| {
                                        v.map(|v| datetrunc::text(v, bound.result_type)).transpose()
                                    })
                            })
                    }
                    "date_bucket width zero" | "date_bucket width zero variable" => {
                        let bound = datetrunc::bind_bucket("day", source, None).unwrap();
                        datetrunc::bucket(bound, Some(0), Some(sample(source)), None).and_then(
                            |v| v.map(|v| datetrunc::text(v, bound.result_type)).transpose(),
                        )
                    }
                    "date_bucket width negative" => {
                        let bound = datetrunc::bind_bucket("day", source, None).unwrap();
                        datetrunc::bucket(bound, Some(-1), Some(sample(source)), None).and_then(
                            |v| v.map(|v| datetrunc::text(v, bound.result_type)).transpose(),
                        )
                    }
                    "date_bucket origin type mismatch" => {
                        datetrunc::bind_bucket("day", TemporalType::Date, Some(source))
                            .map(|_| None)
                    }
                    "date_bucket origin datetime for datetime2" => {
                        datetrunc::bind_bucket("day", source, Some(TemporalType::DateTime))
                            .map(|_| None)
                    }
                    "date_bucket origin string" => datetrunc::bind_bucket_source(
                        "day",
                        SourceType::Temporal(TemporalType::Date),
                        Some(SourceType::Character),
                    )
                    .map(|_| None),
                    "date_bucket origin scale mismatch" => {
                        let bound =
                            datetrunc::bind_bucket("day", TemporalType::DateTime2(3), Some(source))
                                .unwrap();
                        result_type = bound.result_type;
                        datetrunc::bucket(
                            bound,
                            Some(1),
                            Some(value(2024, 5, 15, 0, 0, 0, 0, 0)),
                            Some(value(2024, 5, 1, 0, 0, 0, 0, 0)),
                        )
                        .and_then(|v| v.map(|v| datetrunc::text(v, bound.result_type)).transpose())
                    }
                    _ => continue,
                };
                checked += 1;
                let errors = case["result"]["errors"].as_array().unwrap();
                if let Some(error) = errors.first() {
                    let RuleError::Sql {
                        number,
                        state,
                        class,
                        phase,
                        message,
                    } = actual.expect_err(name)
                    else {
                        panic!("{name}: unexpected unsupported result");
                    };
                    assert_eq!(number, error["number"].as_i64().unwrap() as i32, "{name}");
                    assert_eq!(state, error["state"].as_u64().unwrap() as u8, "{name}");
                    assert_eq!(class, error["class"].as_u64().unwrap() as u8, "{name}");
                    assert_eq!(message, error["message"].as_str().unwrap(), "{name}");
                    assert_eq!(
                        phase == Phase::Execution,
                        !case["result"]["sets"].as_array().unwrap().is_empty(),
                        "{name}"
                    );
                } else {
                    let expected = &case["result"]["sets"][0]["rows"][0][1];
                    match actual.unwrap() {
                        Some(text) => assert_eq!(text, expected.as_str().unwrap(), "{name}"),
                        None => assert!(expected.is_null(), "{name}"),
                    }
                    if name == "date_bucket origin scale mismatch" {
                        assert_eq!(result_type, TemporalType::DateTime2(7));
                        assert_eq!(case["result"]["sets"][0]["columns"][0]["scale"], 7);
                    }
                }
            }
        }
    }
    assert_eq!(checked, 4 * 17);
    assert_eq!(
        datetrunc::bind_bucket(
            "day",
            TemporalType::DateTimeOffset(3),
            Some(TemporalType::DateTimeOffset(7)),
        ),
        Err(RuleError::Unsupported(
            "uncaptured datetimeoffset scale combination"
        ))
    );
}

#[test]
fn pinned_reference_precision_grid_and_typed_overflow() {
    // Read-only probes against the pinned SQL Server 2025 reference image.
    let probes = [
        (
            TemporalType::Time(0),
            value(1, 1, 1, 13, 47, 39, 0, 0),
            "13:47:39",
        ),
        (
            TemporalType::Time(1),
            value(1, 1, 1, 13, 47, 39, 1_000_000, 0),
            "13:47:39.1",
        ),
        (
            TemporalType::DateTime2(0),
            value(2024, 5, 15, 13, 47, 39, 0, 0),
            "2024-05-15 13:47:39",
        ),
        (
            TemporalType::DateTimeOffset(0),
            value(2024, 5, 15, 13, 47, 39, 0, 330),
            "2024-05-15 13:47:39 +05:30",
        ),
    ];
    for (ty, input, expected) in probes {
        let bound = datetrunc::bind_bucket("millisecond", ty, None).unwrap();
        let output = datetrunc::bucket(bound, Some(7), Some(input), None)
            .unwrap()
            .unwrap();
        assert_eq!(datetrunc::text(output, ty).unwrap(), expected, "{ty:?}");
    }
    let ty = TemporalType::SmallDateTime;
    let bound = datetrunc::bind_bucket("second", ty, None).unwrap();
    let output = datetrunc::bucket(
        bound,
        Some(7),
        Some(value(2024, 5, 15, 13, 47, 0, 0, 0)),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        datetrunc::text(output, ty).unwrap(),
        "2024-05-15 13:47:00.000"
    );

    let ty = TemporalType::DateTime2(7);
    let bound = datetrunc::bind_bucket("day", ty, None).unwrap();
    let result = datetrunc::bucket(bound, Some(10), Some(value(1, 1, 1, 0, 0, 0, 0, 0)), None);
    assert_eq!(
        result,
        Err(RuleError::Sql {
            number: 9835,
            state: 1,
            class: 16,
            phase: Phase::Execution,
            message: "Calculating date bucket for 'datetime2' column caused an overflow."
                .to_owned(),
        })
    );
}

#[test]
fn pinned_reference_width_conversion_overflow() {
    let ty = TemporalType::Date;
    let bound = datetrunc::bind_bucket("day", ty, None).unwrap();
    let input = Some(value(2024, 5, 15, 0, 0, 0, 0, 0));
    let expected = Err(RuleError::Sql {
        number: 8115,
        state: 2,
        class: 16,
        phase: Phase::Execution,
        message: "Arithmetic overflow error converting expression to data type int.".to_owned(),
    });
    for width in [2_147_483_648, -2_147_483_649, i64::MAX] {
        assert_eq!(datetrunc::bucket(bound, Some(width), input, None), expected);
    }
    let valid = datetrunc::bucket(bound, Some(i64::from(i32::MAX)), input, None)
        .unwrap()
        .unwrap();
    assert_eq!(datetrunc::text(valid, ty).unwrap(), "1900-01-01");
}

#[test]
fn pinned_reference_time_origin_wrap_and_range() {
    let ty = TemporalType::Time(0);
    let bound = datetrunc::bind_bucket("hour", ty, Some(ty)).unwrap();
    let input = Some(value(1, 1, 1, 1, 0, 0, 0, 0));
    let origin = Some(value(1, 1, 1, 2, 0, 0, 0, 0));
    let previous_day = datetrunc::bucket(bound, Some(5), input, origin)
        .unwrap()
        .unwrap();
    assert_eq!(datetrunc::text(previous_day, ty).unwrap(), "21:00:00");
    let large = datetrunc::bucket(bound, Some(i64::from(i32::MAX)), input, origin);
    assert_eq!(
        large,
        Err(RuleError::Sql {
            number: 9835,
            state: 1,
            class: 16,
            phase: Phase::Execution,
            message: "Calculating date bucket for 'time' column caused an overflow.".to_owned(),
        })
    );
}
