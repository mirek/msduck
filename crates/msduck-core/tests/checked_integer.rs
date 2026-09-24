use msduck_core::checked_integer::{Integer, Operation, calculate};
use serde_json::Value;

fn operand(width: &str, value: &Value) -> Integer {
    match width {
        "INT" => Integer::Int(value.as_str().map(|s| s.parse().unwrap())),
        "BIGINT" => Integer::BigInt(value.as_str().map(|s| s.parse().unwrap())),
        _ => panic!("unexpected reference width"),
    }
}

#[test]
fn checked_arithmetic_matches_all_captured_values_nulls_and_diagnostics() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../reference/checked-integer.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 240);
    let mut errors = 0;
    let mut nulls = 0;
    let mut values = 0;
    for case in cases {
        let width = case["width"].as_str().unwrap();
        let left = operand(case["leftWidth"].as_str().unwrap_or(width), &case["left"]);
        let right = operand(case["rightWidth"].as_str().unwrap_or(width), &case["right"]);
        let operation = match case["operation"].as_str().unwrap() {
            "+" => Operation::Add,
            "-" => Operation::Subtract,
            "*" => Operation::Multiply,
            "/" => Operation::Divide,
            "%" => Operation::Modulo,
            _ => panic!("unexpected reference operation"),
        };
        let actual = calculate(operation, left, right);
        let expected_errors = case["result"]["errors"].as_array().unwrap();
        if let Some(expected) = expected_errors.first() {
            errors += 1;
            assert_eq!(expected_errors.len(), 1);
            let error = actual.expect_err(case["sql"].as_str().unwrap());
            assert_eq!(
                error.number,
                expected["number"].as_i64().unwrap() as i32,
                "{case}"
            );
            assert_eq!(
                error.state,
                expected["state"].as_u64().unwrap() as u8,
                "{case}"
            );
            assert_eq!(
                error.severity,
                expected["class"].as_u64().unwrap() as u8,
                "{case}"
            );
            assert_eq!(
                error.message,
                expected["message"].as_str().unwrap(),
                "{case}"
            );
        } else {
            let sets = case["result"]["sets"].as_array().unwrap();
            assert_eq!(sets.len(), 1);
            let rows = sets[0]["rows"].as_array().unwrap();
            assert_eq!(rows.len(), 1);
            let value = &rows[0][0];
            let expected = match width {
                "INT" => Integer::Int(if value.is_null() {
                    None
                } else {
                    Some(i32::try_from(value.as_i64().unwrap()).unwrap())
                }),
                "BIGINT" => Integer::BigInt(if value.is_null() {
                    None
                } else {
                    Some(value.as_str().unwrap().parse().unwrap())
                }),
                _ => unreachable!(),
            };
            if value.is_null() {
                nulls += 1;
            } else {
                values += 1;
            }
            assert_eq!(actual.unwrap(), expected, "{case}");
        }
    }
    assert!(errors > 0 && nulls > 0 && values > 0);
    assert_eq!(errors + nulls + values, 240);
}
