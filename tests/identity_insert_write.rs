#[path = "../src/identity_insert_write.rs"]
mod identity_insert_write;

use identity_insert_write::{Permit, PreflightError, preflight, session};
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

fn assert_error(actual: PreflightError, case: &Value) {
    let PreflightError::Diagnostic {
        error,
        done_command,
    } = actual
    else {
        panic!("expected captured error, got {actual:?}")
    };
    let expected = &case["result"]["errors"][0];
    assert_eq!(error.number as i64, expected["number"].as_i64().unwrap());
    assert_eq!(error.state as u64, expected["state"].as_u64().unwrap());
    assert_eq!(error.severity as u64, expected["class"].as_u64().unwrap());
    assert_eq!(error.message, expected["message"].as_str().unwrap());
    assert_eq!(
        done_command as u64,
        case["result"]["doneTokens"][0]["command"].as_u64().unwrap()
    );
    assert_eq!(case["result"]["doneTokens"][0]["status"], 2);
}

#[test]
fn captured_null_identity_preflight_preserves_live_rows_allocator_and_setting() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(
        &mut session,
        "CREATE TABLE dbo.conversion(id INT IDENTITY(10,2) PRIMARY KEY, v INT NOT NULL)",
    );
    run(&mut session, "INSERT dbo.conversion(v) VALUES(1)");
    let mut state = session::State::default();
    session::apply(
        &session.db,
        1,
        "logical_db",
        &mut state,
        &statement("SET IDENTITY_INSERT dbo.conversion ON"),
    )
    .unwrap();
    let active = state.active().unwrap().clone();
    let before: (i64, i64) = session
        .db
        .query_row(
            "SELECT (SELECT count(*) FROM dbo.conversion), \
                    (SELECT last_value FROM duckdb_sequences() \
                     WHERE sequence_name LIKE '__msduck_identity_%')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(before, (1, 10));
    let fixture: Value =
        serde_json::from_str(include_str!("../reference/identity-insert-conversion.json")).unwrap();
    for run in fixture["runs"].as_array().unwrap() {
        let case = captured(run, "NULL identity");
        assert_error(
            preflight(
                &session.db,
                1,
                "logical_db",
                &state,
                &statement(case["sql"].as_str().unwrap()),
            )
            .unwrap_err(),
            case,
        );
        assert_eq!(state.active(), Some(&active));
        let after: (i64, i64) = session
            .db
            .query_row(
                "SELECT (SELECT count(*) FROM dbo.conversion), \
                        (SELECT last_value FROM duckdb_sequences() \
                         WHERE sequence_name LIKE '__msduck_identity_%')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(after, before);
    }
}

#[test]
fn captured_insert_shapes_use_live_columns_and_session_key() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(
        &mut session,
        "CREATE TABLE dbo.alpha(id INT IDENTITY(1,1),v INT)",
    );
    let mut state = session::State::default();
    let shapes: Value =
        serde_json::from_str(include_str!("../reference/identity-insert-shapes.json")).unwrap();
    let off_errors = [
        "OFF positional VALUES",
        "OFF positional DEFAULT",
        "OFF explicit conversion",
    ];
    let on_errors = [
        "ON positional VALUES",
        "ON positional DEFAULT",
        "ON omitted INSERT SELECT",
        "ON omitted conversion",
        "ON duplicate identity columns",
        "ON invalid column",
    ];
    for run in shapes["runs"].as_array().unwrap() {
        for name in off_errors {
            let case = captured(run, name);
            assert_error(
                preflight(
                    &session.db,
                    1,
                    "logical_db",
                    &state,
                    &statement(case["sql"].as_str().unwrap()),
                )
                .unwrap_err(),
                case,
            );
        }
    }
    assert!(state.active().is_none());
    assert_eq!(
        preflight(
            &session.db,
            1,
            "logical_db",
            &state,
            &statement("INSERT dbo.alpha(v) VALUES(1)"),
        )
        .unwrap(),
        Some(Permit::Generated)
    );
    let enabled = session::apply(
        &session.db,
        1,
        "logical_db",
        &mut state,
        &statement("SET IDENTITY_INSERT [DbO].[ALPHA] ON"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(enabled.transition, session::Transition::Enabled);
    assert_eq!(enabled.done_command, 183);
    assert_eq!(enabled.target.table.table, "alpha");
    let active = state.active().unwrap().clone();
    for run in shapes["runs"].as_array().unwrap() {
        for name in on_errors {
            let case = captured(run, name);
            assert_error(
                preflight(
                    &session.db,
                    1,
                    "logical_db",
                    &state,
                    &statement(case["sql"].as_str().unwrap()),
                )
                .unwrap_err(),
                case,
            );
        }
        let case = captured(run, "ON explicit INSERT SELECT");
        assert_eq!(
            preflight(
                &session.db,
                1,
                "logical_db",
                &state,
                &statement(case["sql"].as_str().unwrap()),
            )
            .unwrap(),
            Some(Permit::Explicit { source_column: 0 })
        );
    }
    assert_eq!(state.active(), Some(&active));
    assert!(matches!(
        preflight(
            &session.db,
            2,
            "other_database",
            &state,
            &statement("INSERT dbo.alpha(id,v) VALUES(50,3)"),
        ),
        Err(PreflightError::Diagnostic { error, done_command: 195 }) if error.number == 544
    ));
}

#[test]
fn captured_defaults_and_other_active_table_replay_without_source_execution() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(
        &mut session,
        "CREATE TABLE dbo.alpha(id INT IDENTITY(10,2),v INT)",
    );
    run(
        &mut session,
        "CREATE TABLE dbo.beta(id INT IDENTITY(1,1),v INT)",
    );
    let mut state = session::State::default();
    session::apply(
        &session.db,
        1,
        "logical_db",
        &mut state,
        &statement("SET IDENTITY_INSERT dbo.alpha ON"),
    )
    .unwrap();
    let fixture: Value =
        serde_json::from_str(include_str!("../reference/identity-insert-errors.json")).unwrap();
    for run in fixture["runs"].as_array().unwrap() {
        let default = captured(run, "DEFAULT VALUES while ON");
        assert_error(
            preflight(
                &session.db,
                1,
                "logical_db",
                &state,
                &statement(default["sql"].as_str().unwrap()),
            )
            .unwrap_err(),
            default,
        );
    }
    let batch: Value =
        serde_json::from_str(include_str!("../reference/identity-insert.json")).unwrap();
    let off = session::State::default();
    for run in batch["runs"].as_array().unwrap() {
        let other = captured(run, "B explicit while off");
        assert_error(
            preflight(
                &session.db,
                1,
                "logical_db",
                &off,
                &statement(other["sql"].as_str().unwrap()),
            )
            .unwrap_err(),
            other,
        );
    }
    assert!(matches!(
        preflight(
            &session.db,
            1,
            "logical_db",
            &state,
            &statement("INSERT dbo.beta(id,v) VALUES(200,4)"),
        ),
        Err(PreflightError::Diagnostic { error, done_command: 195 }) if error.number == 544
    ));
    assert_eq!(
        preflight(
            &session.db,
            1,
            "logical_db",
            &state,
            &statement("INSERT dbo.alpha(id,v) VALUES(50,CONVERT(INT,'bad'))"),
        )
        .unwrap(),
        Some(Permit::Explicit { source_column: 0 })
    );
    assert_eq!(
        preflight(
            &session.db,
            1,
            "logical_db",
            &state,
            &statement("INSERT dbo.beta(v) VALUES(1)"),
        )
        .unwrap(),
        Some(Permit::Generated)
    );
}

#[test]
fn physical_position_nonidentity_and_unknown_targets_are_distinct() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(
        &mut session,
        "CREATE TABLE dbo.second(v INT,id INT IDENTITY(1,1))",
    );
    run(&mut session, "CREATE TABLE dbo.plain(v INT)");
    let mut state = session::State::default();
    assert_eq!(
        preflight(
            &session.db,
            1,
            "logical_db",
            &state,
            &statement("INSERT dbo.second(v) VALUES(3)"),
        )
        .unwrap(),
        Some(Permit::Generated)
    );
    session::apply(
        &session.db,
        1,
        "logical_db",
        &mut state,
        &statement("SET IDENTITY_INSERT dbo.second ON"),
    )
    .unwrap();
    for (sql, expected) in [
        (
            "INSERT [DbO].[SECOND](v,id) VALUES(3,50)",
            Permit::Explicit { source_column: 1 },
        ),
        (
            "INSERT dbo.second(id,v) VALUES(50,3)",
            Permit::Explicit { source_column: 0 },
        ),
    ] {
        assert_eq!(
            preflight(&session.db, 1, "logical_db", &state, &statement(sql)).unwrap(),
            Some(expected)
        );
    }
    for sql in [
        "INSERT dbo.plain(v) VALUES(1)",
        "INSERT dbo.missing(v) VALUES(1)",
    ] {
        assert_eq!(
            preflight(&session.db, 1, "logical_db", &state, &statement(sql)).unwrap(),
            Some(Permit::NotApplicable)
        );
    }
    for sql in [
        "INSERT master.dbo.second(v,id) VALUES(1,2)",
        "INSERT dbo.[#temporary](v,id) VALUES(1,2)",
        "INSERT dbo.second(v,v,id) VALUES(1,2,3)",
        "INSERT dbo.second(v,v,missing,id) VALUES(1,2,3,4)",
        "INSERT dbo.second(missing,v) VALUES(1,2)",
    ] {
        assert!(matches!(
            preflight(&session.db, 1, "logical_db", &state, &statement(sql)),
            Err(PreflightError::Unsupported(_))
        ));
    }
    assert_eq!(
        preflight(&session.db, 1, "logical_db", &state, &statement("SELECT 1"),).unwrap(),
        None
    );
    assert_eq!(state.active().unwrap().table.table, "second");
    assert_eq!(state.fork_rpc().active(), state.active());
    assert!(matches!(
        session::apply(
            &session.db,
            1,
            "logical_db",
            &mut state,
            &statement("SET IDENTITY_INSERT dbo.missing ON"),
        ),
        Err(session::ApplyError::Diagnostic {
            done_command: 253,
            ..
        })
    ));
}
