// Compile the pure module here until the separately claimed core export is free.
#[path = "../src/identity_scope.rs"]
mod identity_scope;

use identity_scope::{IdentityScopes, MAX_NESTED_SCOPES, ScopeError};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../reference/identity-retrieval.json")).unwrap()
}

fn observation<'a>(run: &'a [Value], name: &str) -> &'a Value {
    &run.iter()
        .find(|item| item["name"] == name)
        .unwrap_or_else(|| panic!("missing {name}"))["result"]
}

fn decoded(value: &Value) -> Option<i128> {
    if value.is_null() {
        None
    } else {
        Some(i128::from(value.as_i64().unwrap()))
    }
}

fn assert_result(state: &IdentityScopes, result: &Value, set: usize) {
    let row = &result["sets"][set]["rows"][0];
    assert_eq!(state.scope_last(), decoded(&row[0]));
    assert_eq!(state.session_last(), decoded(&row[1]));
}

fn assert_last(state: &IdentityScopes, run: &[Value], name: &str) {
    let result = observation(run, name);
    let set = result["sets"].as_array().unwrap().len() - 1;
    assert_result(state, result, set);
}

#[test]
fn replay_captured_session_rpc_trigger_and_procedure_scopes() {
    let fixture = fixture();
    let run = fixture["runs"][0].as_array().unwrap();
    assert_eq!(run.len(), 31);
    let mut primary = IdentityScopes::new();
    let mut secondary = IdentityScopes::new();

    assert_last(&primary, run, "initial session values");
    assert_last(&primary, run, "before allocation");
    primary.publish_success(10).unwrap();
    assert_last(&primary, run, "first batch insert");
    assert_last(&primary, run, "after first batch");

    assert_last(&secondary, run, "second session before allocation");
    secondary.publish_success(12).unwrap();
    assert_last(&secondary, run, "second session insert");
    assert_last(&primary, run, "first session after second");

    let rpc = primary.enter().unwrap();
    assert_eq!(primary.scope_last(), None);
    assert_eq!(primary.session_last(), Some(10));
    primary.publish_success(14).unwrap();
    assert_last(&primary, run, "rpc insert 3");
    primary.leave(rpc).unwrap();
    assert_last(&primary, run, "after rpc insert");
    let rpc = primary.enter().unwrap();
    primary.publish_success(16).unwrap();
    assert_last(&primary, run, "rpc insert 4");
    primary.leave(rpc).unwrap();
    assert_last(&secondary, run, "second session after rpc");

    // The duplicate attempt allocated table ID 18 but published no session ID.
    assert_eq!(
        observation(run, "duplicate insert failure")["errors"][0]["number"],
        2627
    );
    assert_last(&primary, run, "after duplicate failure");
    primary.publish_success(20).unwrap();
    let rolled_back = observation(run, "rollback in same batch");
    assert_result(&primary, rolled_back, 0);
    assert_result(&primary, rolled_back, 1);
    assert_last(&primary, run, "after rollback");

    primary.publish_success(22).unwrap();
    let trigger = primary.enter().unwrap();
    assert_eq!(primary.scope_last(), None);
    primary.publish_success(100).unwrap();
    primary.leave(trigger).unwrap();
    assert_last(&primary, run, "triggered insert");
    assert_last(&primary, run, "after triggered batch");
    assert_last(&secondary, run, "second session after trigger");

    let procedure = primary.enter().unwrap();
    primary.publish_success(24).unwrap();
    let trigger = primary.enter().unwrap();
    primary.publish_success(103).unwrap();
    primary.leave(trigger).unwrap();
    let procedure_result = observation(run, "procedure scope");
    assert_result(&primary, procedure_result, 0);
    primary.leave(procedure).unwrap();
    assert_result(&primary, procedure_result, 1);
    assert_last(&primary, run, "after procedure batch");

    primary.publish_success(50).unwrap();
    let trigger = primary.enter().unwrap();
    primary.publish_success(106).unwrap();
    primary.leave(trigger).unwrap();
    assert_last(&primary, run, "explicit identity insert");
    assert_last(&primary, run, "after explicit identity");

    let big = 9_223_372_036_854_775_800_i128;
    primary.publish_success(big).unwrap();
    let exact = big.to_string();
    for name in ["big identity insert", "after big identity batch"] {
        let result = observation(run, name);
        let row = &result["sets"][result["sets"].as_array().unwrap().len() - 1]["rows"][0];
        assert_eq!(primary.scope_last(), Some(big));
        assert_eq!(primary.session_last(), Some(big));
        assert_eq!(row[3].as_str(), Some(exact.as_str()));
        assert_eq!(row[4].as_str(), Some(exact.as_str()));
    }
    // TRUNCATE resets the table allocator without clearing either session value.
    assert_eq!(
        observation(run, "after truncate")["sets"][0]["rows"][0][2],
        10
    );
    assert_eq!(primary.scope_last(), Some(big));
    assert_eq!(primary.session_last(), Some(big));
}

#[test]
fn decimal_38_bounds_and_invalid_publications_leave_state_intact() {
    const LIMIT: i128 = 100_000_000_000_000_000_000_000_000_000_000_000_000;
    let mut state = IdentityScopes::new();
    state.publish_success(LIMIT - 1).unwrap();
    assert_eq!(state.scope_last(), Some(LIMIT - 1));
    assert_eq!(state.session_last(), Some(LIMIT - 1));
    let saved = state.clone();
    for value in [LIMIT, -LIMIT, i128::MIN, i128::MAX] {
        assert_eq!(
            state.publish_success(value),
            Err(ScopeError::IdentityOutOfRange)
        );
        assert_eq!(state, saved);
    }
    state.publish_success(-LIMIT + 1).unwrap();
    assert_eq!(state.scope_last(), Some(-LIMIT + 1));
    assert_eq!(state.session_last(), Some(-LIMIT + 1));
}

#[test]
fn depth_and_lifo_errors_do_not_corrupt_scope_or_session() {
    let mut state = IdentityScopes::new();
    let first = state.enter().unwrap();
    state.leave(first).unwrap();
    state.publish_success(5).unwrap();
    let parent = state.enter().unwrap();
    state.publish_success(6).unwrap();
    let child = state.enter().unwrap();
    state.publish_success(7).unwrap();
    let saved = state.clone();
    assert_eq!(state.leave(parent), Err(ScopeError::WrongScope));
    assert_eq!(state, saved);
    state.leave(child).unwrap();
    assert_eq!(state.scope_last(), Some(6));
    assert_eq!(state.session_last(), Some(7));
    assert_eq!(state.leave(child), Err(ScopeError::WrongScope));
    state.leave(parent).unwrap();
    assert_eq!(state.scope_last(), Some(5));
    assert_eq!(state.session_last(), Some(7));
    assert_eq!(state.leave(parent), Err(ScopeError::NoNestedScope));

    let mut receipts = Vec::new();
    for _ in 0..MAX_NESTED_SCOPES {
        receipts.push(state.enter().unwrap());
    }
    let saved = state.clone();
    assert_eq!(state.enter(), Err(ScopeError::MaximumNesting));
    assert_eq!(state, saved);
    for receipt in receipts.into_iter().rev() {
        state.leave(receipt).unwrap();
    }
    assert_eq!(state.nested_depth(), 0);
    assert_eq!(state.scope_last(), Some(5));
    assert_eq!(state.session_last(), Some(7));
}
