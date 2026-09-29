#[path = "../src/identity_insert_session.rs"]
mod identity_insert_session;

use identity_insert_session::{ApplyError, State, Transition, apply};
use msduck::{engine::Session, server::Server};
use serde_json::Value;
use sqlparser::ast::Statement;

fn statement(sql: &str) -> Statement {
    msduck_sql::batch::parse(sql).unwrap().remove(0)
}

fn run(session: &mut Session, sql: &str) {
    let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{sql}: {tokens:?}");
}

fn captured<'a>(run: &'a Value, name: &str) -> &'a Value {
    run.as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap()
}

fn assert_error(actual: ApplyError, case: &Value, logical_database: Option<&str>) {
    let ApplyError::Diagnostic {
        error,
        done_command,
    } = actual
    else {
        panic!("expected SQL diagnostic, got {actual:?}")
    };
    let expected = &case["result"]["errors"][0];
    let mut message = expected["message"].as_str().unwrap().to_owned();
    if let Some(database) = logical_database {
        let prefix = "IDENTITY_INSERT is already ON for table '";
        let captured_database = message
            .strip_prefix(prefix)
            .unwrap()
            .split_once(".dbo.alpha'")
            .unwrap()
            .0;
        message = message.replacen(captured_database, database, 1);
    }
    assert_eq!(error.number as i64, expected["number"].as_i64().unwrap());
    assert_eq!(error.state as u64, expected["state"].as_u64().unwrap());
    assert_eq!(error.severity as u64, expected["class"].as_u64().unwrap());
    assert_eq!(error.message, message);
    assert_eq!(
        done_command as u64,
        case["result"]["doneTokens"][0]["command"].as_u64().unwrap()
    );
    assert_eq!(case["result"]["doneTokens"][0]["status"], 2);
}

#[test]
fn two_sessions_keep_stable_catalog_keys_and_captured_conflicts() {
    let server = Server::open(":memory:").unwrap();
    let mut first = Session::new(server.connection().unwrap()).unwrap();
    let second = Session::new(server.connection().unwrap()).unwrap();
    run(&mut first, "CREATE TABLE dbo.alpha(id INT IDENTITY(1,1))");
    run(&mut first, "CREATE TABLE dbo.beta(id INT IDENTITY(10,2))");
    let mut a = State::default();
    let mut b = State::default();
    let alpha = statement("SET IDENTITY_INSERT dbo.alpha ON");
    let beta = statement("SET IDENTITY_INSERT dbo.beta ON");

    let enabled = apply(&first.db, 7, "logical_db", &mut a, &alpha)
        .unwrap()
        .unwrap();
    assert_eq!(enabled.transition, Transition::Enabled);
    assert_eq!(enabled.done_command, 183);
    let alias = apply(
        &first.db,
        7,
        "logical_db",
        &mut a,
        &statement("SET IDENTITY_INSERT [DbO].[ALPHA] ON"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(alias.transition, Transition::AlreadyEnabled);
    assert_eq!(alias.target, enabled.target);
    assert_eq!(
        apply(
            &second.db,
            7,
            "logical_db",
            &mut b,
            &statement("SET IDENTITY_INSERT [dbo].[alpha] ON"),
        )
        .unwrap()
        .unwrap()
        .transition,
        Transition::Enabled
    );
    assert_eq!(a.active(), b.active());

    let other_off = apply(
        &first.db,
        7,
        "logical_db",
        &mut a,
        &statement("SET IDENTITY_INSERT dbo.beta OFF"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(other_off.transition, Transition::OtherTableUnaffected);
    assert_eq!(other_off.done_command, 184);
    let fixture: Value =
        serde_json::from_str(include_str!("../reference/identity-insert.json")).unwrap();
    for run in fixture["runs"].as_array().unwrap() {
        let case = captured(run, "A beta on conflicts");
        assert_error(
            apply(&first.db, 7, "logical_db", &mut a, &beta).unwrap_err(),
            case,
            Some("logical_db"),
        );
    }
    assert_eq!(a.active(), Some(&enabled.target));
    assert_eq!(b.active(), Some(&enabled.target));

    let disabled = apply(
        &first.db,
        7,
        "logical_db",
        &mut a,
        &statement("SET IDENTITY_INSERT dbo.alpha OFF"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(disabled.transition, Transition::Disabled);
    assert!(a.active().is_none());
    assert_eq!(
        apply(&first.db, 7, "logical_db", &mut a, &beta)
            .unwrap()
            .unwrap()
            .transition,
        Transition::Enabled
    );
    assert_eq!(b.active(), Some(&enabled.target));
}

#[test]
fn resolution_errors_and_unsupported_shapes_never_change_the_setting() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(&mut session, "CREATE TABLE dbo.alpha(id INT IDENTITY(1,1))");
    run(&mut session, "CREATE TABLE dbo.plain(v INT)");
    let mut state = State::default();
    let active = apply(
        &session.db,
        1,
        "logical_db",
        &mut state,
        &statement("SET IDENTITY_INSERT dbo.alpha ON"),
    )
    .unwrap()
    .unwrap()
    .target;
    let fixture: Value = serde_json::from_str(include_str!(
        "../reference/identity-insert-name-errors.json"
    ))
    .unwrap();
    for run in fixture["runs"].as_array().unwrap() {
        for name in [
            "unqualified plain ON",
            "unqualified missing ON",
            "bracketed qualified plain ON",
            "case-varied missing ON",
        ] {
            let case = captured(run, name);
            assert_error(
                apply(
                    &session.db,
                    1,
                    "logical_db",
                    &mut state,
                    &statement(case["sql"].as_str().unwrap()),
                )
                .unwrap_err(),
                case,
                None,
            );
            assert_eq!(state.active(), Some(&active));
        }
    }
    for sql in [
        "SET IDENTITY_INSERT master.dbo.alpha ON",
        "SET IDENTITY_INSERT dbo.#temporary ON",
    ] {
        assert!(matches!(
            apply(&session.db, 1, "logical_db", &mut state, &statement(sql)),
            Err(ApplyError::Unsupported(_))
        ));
        assert_eq!(state.active(), Some(&active));
    }
    assert!(
        apply(
            &session.db,
            1,
            "logical_db",
            &mut state,
            &statement("SELECT 1")
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(state.active(), Some(&active));
}

#[test]
fn rollback_keeps_setting_and_rpc_fork_is_explicit() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(&mut session, "CREATE TABLE dbo.alpha(id INT IDENTITY(1,1))");
    run(&mut session, "CREATE TABLE dbo.beta(id INT IDENTITY(1,1))");
    let mut state = State::default();
    run(&mut session, "BEGIN TRANSACTION");
    apply(
        &session.db,
        1,
        "logical_db",
        &mut state,
        &statement("SET IDENTITY_INSERT dbo.alpha ON"),
    )
    .unwrap();
    run(&mut session, "ROLLBACK TRANSACTION");
    let parent = state.active().unwrap().clone();
    let mut rpc = state.fork_rpc();
    assert_eq!(
        apply(
            &session.db,
            1,
            "logical_db",
            &mut rpc,
            &statement("SET IDENTITY_INSERT dbo.alpha OFF"),
        )
        .unwrap()
        .unwrap()
        .transition,
        Transition::Disabled
    );
    apply(
        &session.db,
        1,
        "logical_db",
        &mut rpc,
        &statement("SET IDENTITY_INSERT dbo.beta ON"),
    )
    .unwrap();
    assert_eq!(state.active(), Some(&parent));
    assert_ne!(rpc.active(), Some(&parent));
}
