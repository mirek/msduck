#[path = "../src/sequence.rs"]
mod sequence;

use sequence::{IntegerType, RowAllocations, SequenceError, SequenceSpec};

fn captured_rows(name: &str) -> Vec<Vec<serde_json::Value>> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../reference/sequence-reference.json")).unwrap();
    fixture["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["name"] == name)
        .unwrap()["result"]["sets"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row.as_array().unwrap().clone())
        .collect()
}

#[test]
fn ascending_values_and_same_row_references_follow_retained_sql_server_capture() {
    let spec = SequenceSpec::new(IntegerType::BigInt, 10, 2, 10, 16, false).unwrap();
    assert_eq!(spec.kind(), IntegerType::BigInt);
    let mut state = spec.initial_state();
    assert_eq!(state.current_value(), 10);
    assert!(!state.has_allocated());

    let first = RowAllocations::default()
        .value_for(42, spec, &mut state)
        .unwrap();
    assert_eq!(
        first.to_string(),
        captured_rows("first value")[0][0].as_str().unwrap()
    );

    let mut second_row = RowAllocations::default();
    let left = second_row.value_for(42, spec, &mut state).unwrap();
    let right = second_row.value_for(42, spec, &mut state).unwrap();
    let repeated = captured_rows("repeated reference per row");
    assert_eq!(left.to_string(), repeated[0][0].as_str().unwrap());
    assert_eq!(right.to_string(), repeated[0][1].as_str().unwrap());
    assert_eq!(state.current_value(), 12);

    let ordered = captured_rows("ordered two rows");
    for row in ordered {
        let value = RowAllocations::default()
            .value_for(42, spec, &mut state)
            .unwrap();
        assert_eq!(value.to_string(), row[1].as_str().unwrap());
    }
    let terminal = state;
    assert_eq!(spec.advance(&mut state), Err(SequenceError::Exhausted));
    assert_eq!(state, terminal, "exhaustion cannot consume or alter state");
}

#[test]
fn descending_values_exhaust_then_cycling_wraps_to_maximum() {
    let spec = SequenceSpec::new(IntegerType::SmallInt, 0, -1, -2, 0, false).unwrap();
    let mut state = spec.initial_state();
    for row in captured_rows("descending rows") {
        let value = RowAllocations::default()
            .value_for(7, spec, &mut state)
            .unwrap();
        assert_eq!(value, row[1].as_i64().unwrap());
    }
    assert_eq!(spec.advance(&mut state), Err(SequenceError::Exhausted));
    let cycling = SequenceSpec::new(IntegerType::SmallInt, 0, -1, -2, 0, true).unwrap();
    let mut cycling_state = cycling.initial_state();
    assert_eq!(
        (0..4)
            .map(|_| cycling.advance(&mut cycling_state).unwrap())
            .collect::<Vec<_>>(),
        [0, -1, -2, 0]
    );
}

#[test]
fn restart_and_cycle_use_the_bounds_instead_of_the_start() {
    let spec = SequenceSpec::new(IntegerType::BigInt, 16, 2, 10, 16, true).unwrap();
    let mut state = spec.initial_state();
    assert_eq!(
        spec.advance(&mut state).unwrap().to_string(),
        captured_rows("cycle maximum value")[0][0].as_str().unwrap()
    );
    assert_eq!(
        spec.advance(&mut state).unwrap().to_string(),
        captured_rows("cycle wraps to minimum")[0][0]
            .as_str()
            .unwrap()
    );
    assert_eq!(spec.advance(&mut state).unwrap(), 12);
}

#[test]
fn type_edges_and_oversized_steps_do_not_overflow() {
    let tiny = SequenceSpec::new(IntegerType::TinyInt, 0, -1, 0, 255, true).unwrap();
    let mut state = tiny.initial_state();
    assert_eq!(tiny.advance(&mut state), Ok(0));
    assert_eq!(tiny.advance(&mut state), Ok(255));

    let small = SequenceSpec::new(
        IntegerType::SmallInt,
        i16::MAX.into(),
        1,
        i16::MIN.into(),
        i16::MAX.into(),
        false,
    )
    .unwrap();
    let mut state = small.initial_state();
    assert_eq!(small.advance(&mut state), Ok(i16::MAX.into()));
    assert_eq!(small.advance(&mut state), Err(SequenceError::Exhausted));

    let int = SequenceSpec::new(
        IntegerType::Int,
        i32::MIN.into(),
        -1,
        i32::MIN.into(),
        i32::MAX.into(),
        false,
    )
    .unwrap();
    let mut state = int.initial_state();
    assert_eq!(int.advance(&mut state), Ok(i32::MIN.into()));
    assert_eq!(int.advance(&mut state), Err(SequenceError::Exhausted));

    let big = SequenceSpec::new(
        IntegerType::BigInt,
        i64::MAX,
        i64::MAX,
        i64::MIN,
        i64::MAX,
        true,
    )
    .unwrap();
    let mut state = big.initial_state();
    assert_eq!(big.advance(&mut state), Ok(i64::MAX));
    assert_eq!(big.advance(&mut state), Ok(i64::MIN));
}

#[test]
fn definitions_and_caller_state_are_checked_before_mutation() {
    assert_eq!(
        SequenceSpec::new(IntegerType::Int, 1, 0, 0, 2, false),
        Err(SequenceError::ZeroIncrement)
    );
    assert_eq!(
        SequenceSpec::new(IntegerType::TinyInt, 1, 1, -1, 2, false),
        Err(SequenceError::InvalidBounds)
    );
    assert_eq!(
        SequenceSpec::new(IntegerType::TinyInt, 1, 1, 2, 1, false),
        Err(SequenceError::InvalidBounds)
    );
    assert_eq!(
        SequenceSpec::new(IntegerType::Int, 3, 1, 0, 2, false),
        Err(SequenceError::StartOutsideBounds)
    );

    let left = SequenceSpec::new(IntegerType::Int, 1, 1, 0, 2, false).unwrap();
    let right = SequenceSpec::new(IntegerType::Int, 2, 1, 2, 4, false).unwrap();
    let mut state = left.initial_state();
    let before = state;
    assert_eq!(right.advance(&mut state), Err(SequenceError::InvalidState));
    assert_eq!(state, before);
}

#[test]
fn one_row_shares_each_identity_but_distinct_rows_allocate_again() {
    let first = SequenceSpec::new(IntegerType::Int, 5, 1, 5, 6, false).unwrap();
    let second = SequenceSpec::new(IntegerType::Int, 20, 1, 20, 21, false).unwrap();
    let mut first_state = first.initial_state();
    let mut second_state = second.initial_state();
    let mut row = RowAllocations::default();
    assert_eq!(row.value_for(100, first, &mut first_state), Ok(5));
    assert_eq!(row.value_for(200, second, &mut second_state), Ok(20));
    assert_eq!(row.value_for(100, first, &mut first_state), Ok(5));
    assert_eq!(row.value_for(200, second, &mut second_state), Ok(20));
    assert_eq!(first_state.current_value(), 5);
    assert_eq!(second_state.current_value(), 20);

    let mut next_row = RowAllocations::default();
    assert_eq!(next_row.value_for(100, first, &mut first_state), Ok(6));
    assert_eq!(next_row.value_for(200, second, &mut second_state), Ok(21));
    let mut final_row = RowAllocations::default();
    assert_eq!(
        final_row.value_for(100, first, &mut first_state),
        Err(SequenceError::Exhausted)
    );
    assert_eq!(first_state.current_value(), 6);
}
