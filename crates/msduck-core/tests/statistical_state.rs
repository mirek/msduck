use msduck_core::bounded_aggregate::{FloatStatsState, Statistic};
use serde_json::Value;

const FUNCTIONS: [Statistic; 4] = [
    Statistic::Stdev,
    Statistic::Stdevp,
    Statistic::Var,
    Statistic::Varp,
];

fn fixture(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn input(values: &[Option<f64>]) -> FloatStatsState {
    let mut state = FloatStatsState::default();
    for value in values.iter().flatten() {
        state.push(*value);
    }
    state
}

fn compare(
    fixture: &Value,
    name: &str,
    row: usize,
    values: &[Option<f64>],
    offset: usize,
) -> usize {
    let record = fixture["runs"][0]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["name"] == name)
        .unwrap_or_else(|| panic!("missing capture: {name}"));
    let captured = record["bits"][0][row].as_array().unwrap();
    let state = input(values);
    for (index, statistic) in FUNCTIONS.into_iter().enumerate() {
        let actual = state
            .value(statistic)
            .unwrap()
            .map(|value| format!("{:016x}", value.to_bits()));
        let expected = captured[index + offset].as_str().map(str::to_owned);
        assert_eq!(actual, expected, "{name}: row {row}, {statistic:?}");
    }
    FUNCTIONS.len()
}

fn some(values: &[f64]) -> Vec<Option<f64>> {
    values.iter().copied().map(Some).collect()
}

#[test]
fn long_transitions_and_ordered_windows_match_all_retained_bits() {
    let capture = fixture(include_str!(
        "../../../reference/statistical-transition.json"
    ));
    assert_eq!(capture["runs"][0], capture["runs"][1]);
    let alternating: Vec<_> = (0..96)
        .map(|index| {
            Some(if index % 2 == 0 {
                1e12 - 1.0
            } else {
                1e12 + 1.0
            })
        })
        .collect();
    let mut clustered = vec![Some(1e12 - 1.0); 48];
    clustered.extend(vec![Some(1e12 + 1.0); 48]);
    let mut reversed_clustered = clustered.clone();
    reversed_clustered.reverse();
    let mut reversed_alternating = alternating.clone();
    reversed_alternating.reverse();
    let mut cells = 0;
    for (name, values) in [
        ("long alternating decimal", &alternating),
        ("long clustered decimal", &clustered),
        ("long reversed decimal", &reversed_clustered),
    ] {
        cells += compare(&capture, name, 0, values, 0);
    }
    for (name, values) in [
        ("long ordered window decimal", &alternating),
        ("long reverse window decimal", &reversed_alternating),
    ] {
        for row in 0..values.len() {
            cells += compare(&capture, name, row, &values[..=row], 1);
        }
    }
    for (name, values) in [
        ("signed zero float", vec![Some(-0.0), Some(0.0), Some(-0.0)]),
        ("signed zero real", vec![Some(-0.0), Some(0.0), Some(-0.0)]),
        ("large finite float", some(&[1e150, -1e150, 1e150, -1e150])),
        ("near clamp decimal 1e8", some(&[1e8, 1e8 + 1.0, 1e8 + 2.0])),
        (
            "near clamp decimal 1e10",
            some(&[1e10, 1e10 + 1.0, 1e10 + 2.0]),
        ),
        (
            "near clamp decimal 1e11",
            some(&[1e11, 1e11 + 1.0, 1e11 + 2.0]),
        ),
        (
            "near clamp decimal 1e12",
            some(&[1e12, 1e12 + 1.0, 1e12 + 2.0]),
        ),
        (
            "near clamp decimal 1e13",
            some(&[1e13, 1e13 + 1.0, 1e13 + 2.0]),
        ),
    ] {
        cells += compare(&capture, name, 0, &values, 0);
    }
    assert_eq!(cells, 812);
}

#[test]
fn earlier_precision_capture_matches_sequential_state() {
    let capture = fixture(include_str!(
        "../../../reference/statistical-precision.json"
    ));
    let large = [9007199254740992.0, 9007199254740993.0, 9007199254740994.0];
    let shifted = [1e12, 1e12 + 1.0, 1e12 + 2.0];
    let cases: Vec<(&str, Vec<Option<f64>>)> = vec![
        ("small repeated int", some(&[1.0, 2.0, 2.0])),
        ("small reversed int", some(&[2.0, 2.0, 1.0])),
        ("small mixed signs int", some(&[-9.0, -2.0, 1.0, 7.0])),
        ("large adjacent bigint", some(&large)),
        ("large adjacent decimal", some(&large)),
        ("large shifted decimal", some(&shifted)),
        (
            "balanced magnitudes decimal",
            some(&[1e12, -1e12, 3.0, -3.0]),
        ),
        ("mixed exponents float", some(&[1e20, 1e-20, -1e20, 3.0])),
        (
            "adjacent float",
            some(&[1.0, 1.0000000000000002, 1.0000000000000004]),
        ),
        ("real source", some(&[1.125, 2.25, 2.25, 4.5])),
        ("fractional decimal", some(&[0.1, 0.2, 0.3])),
        ("tiny decimal", some(&[1e-10, 2e-10, 3e-10])),
        ("nulls int", vec![Some(1.0), None, Some(2.0), None]),
        ("singleton int", some(&[42.0])),
        ("all null int", vec![None, None]),
        ("empty int", vec![]),
        // SQL Server deduplicates the original DECIMAL values first, then both
        // distinct values round to the same binary64 input.
        ("distinct adjacent decimal", some(&[large[0], large[1]])),
        ("distinct float with null", some(&[1.0, 2.0])),
    ];
    let frames: Vec<(&str, Vec<Vec<Option<f64>>>)> = vec![
        (
            "grouped decimal",
            vec![some(&[1.0, 2.0, 2.0]), some(&shifted)],
        ),
        (
            "ordered ascending int",
            vec![
                some(&[1.0]),
                some(&[1.0, 2.0]),
                some(&[1.0, 2.0, 2.0]),
                some(&[1.0, 2.0, 2.0, 9.0]),
            ],
        ),
        (
            "ordered reversed int",
            vec![
                some(&[9.0]),
                some(&[9.0, 2.0]),
                some(&[9.0, 2.0, 2.0]),
                some(&[9.0, 2.0, 2.0, 1.0]),
            ],
        ),
        (
            "bounded window with null",
            vec![
                vec![None],
                vec![None, Some(1.0)],
                vec![None, Some(1.0), Some(2.0)],
                some(&[1.0, 2.0, 2.0]),
                some(&[2.0, 2.0, 9.0]),
            ],
        ),
        (
            "partitioned decimal",
            vec![
                some(&[1.0]),
                some(&[1.0, 2.0]),
                some(&[1.0, 2.0, 2.0]),
                some(&[1e12]),
                some(&[1e12, 1e12 + 1.0]),
                some(&shifted),
            ],
        ),
    ];
    let mut cells = 0;
    for (name, values) in cases {
        cells += compare(&capture, name, 0, &values, 0);
    }
    for (name, rows) in frames {
        for (row, values) in rows.iter().enumerate() {
            cells += compare(&capture, name, row, values, 1);
        }
    }
    assert_eq!(cells, 156);
}

#[test]
fn failures_are_sticky_and_empty_and_singleton_shapes_are_explicit() {
    let empty = FloatStatsState::default();
    for statistic in FUNCTIONS {
        assert_eq!(empty.value(statistic), Ok(None));
    }
    let one = input(&[Some(7.0)]);
    assert_eq!(one.value(Statistic::Stdev), Ok(None));
    assert_eq!(one.value(Statistic::Var), Ok(None));
    assert_eq!(one.value(Statistic::Stdevp).unwrap().unwrap().to_bits(), 0);
    assert_eq!(one.value(Statistic::Varp).unwrap().unwrap().to_bits(), 0);
    for values in [[1e308, -1e308], [1e154, -1e154]] {
        let mut state = input(&some(&values));
        assert!(state.failed);
        state.push(1.0);
        assert_eq!(
            state.value(Statistic::Varp),
            Err(msduck_core::bounded_aggregate::Overflow)
        );
    }
    // Each square and their sum are finite, but S*S overflows at finalization.
    let final_overflow = input(&some(&[9e153, 9e153]));
    assert!(!final_overflow.failed);
    assert_eq!(
        final_overflow.value(Statistic::Varp),
        Err(msduck_core::bounded_aggregate::Overflow)
    );
    let mut state = input(&[Some(1.0)]);
    state.push(f64::INFINITY);
    assert!(state.failed);
    state.push(2.0);
    assert_eq!(
        state.value(Statistic::Var),
        Err(msduck_core::bounded_aggregate::Overflow)
    );
    let mut count_overflow = FloatStatsState {
        count: u64::MAX,
        ..FloatStatsState::default()
    };
    count_overflow.push(1.0);
    assert_eq!(
        count_overflow.value(Statistic::Var),
        Err(msduck_core::bounded_aggregate::Overflow)
    );
}
