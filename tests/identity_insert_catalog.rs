#[path = "../src/identity_insert_catalog.rs"]
mod identity_insert_catalog;

use identity_insert_catalog::{ResolveError, ResolvedTable, resolve};
use msduck::{engine::Session, server::Server};
use serde_json::Value;
use sqlparser::ast::{Ident, Set, SetSessionParamKind, Statement};

fn parts(sql: &str) -> Vec<Ident> {
    let statements = msduck_sql::batch::parse(sql).unwrap();
    let Statement::Set(Set::SetSessionParam(SetSessionParamKind::IdentityInsert(setting))) =
        &statements[0]
    else {
        panic!("expected parsed IDENTITY_INSERT setting: {sql}")
    };
    setting
        .obj
        .0
        .iter()
        .map(|part| part.as_ident().unwrap().clone())
        .collect()
}

fn run(session: &mut Session, sql: &str) {
    let (tokens, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok, "{sql}: {tokens:?}");
}

fn captured(run: &Value, name: &str) -> Value {
    run.as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap()
        .clone()
}

fn assert_diagnostic(actual: ResolveError, case: &Value) {
    let ResolveError::Diagnostic {
        error,
        done_command,
    } = actual
    else {
        panic!("expected captured diagnostic, got {actual:?}")
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
fn missing_and_nonidentity_targets_replay_captured_diagnostics() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(&mut session, "CREATE TABLE dbo.plain(v INT)");
    let fixture: Value =
        serde_json::from_str(include_str!("../reference/identity-insert-errors.json")).unwrap();
    for run in fixture["runs"].as_array().unwrap() {
        for name in ["plain ON", "missing ON"] {
            let case = captured(run, name);
            let target = parts(case["sql"].as_str().unwrap());
            assert_diagnostic(resolve(&session.db, &target).unwrap_err(), &case);
        }
    }
}

#[test]
fn aliases_retain_object_id_and_identity_physical_position() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(
        &mut session,
        "CREATE TABLE dbo.alpha(v INT,id INT IDENTITY(10,2))",
    );
    let plain = resolve(&session.db, &parts("SET IDENTITY_INSERT alpha ON")).unwrap();
    let quoted = resolve(&session.db, &parts("SET IDENTITY_INSERT [DbO].[ALPHA] ON")).unwrap();
    assert_eq!(plain, quoted);
    assert_eq!(
        plain,
        ResolvedTable {
            object_id: plain.object_id,
            schema: "dbo".into(),
            table: "alpha".into(),
            identity_position: 1,
        }
    );
    let catalog_id: i32 = session
        .db
        .query_row(
            "SELECT object_id FROM sys.tables WHERE name='alpha'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(plain.object_id, catalog_id);
    run(&mut session, "DROP TABLE dbo.alpha");
    assert!(matches!(
        resolve(&session.db, &parts("SET IDENTITY_INSERT alpha ON")),
        Err(ResolveError::Diagnostic { error, .. }) if error.number == 1088
    ));
    run(
        &mut session,
        "CREATE TABLE dbo.alpha(v INT,id INT IDENTITY(10,2))",
    );
    let recreated = resolve(&session.db, &parts("SET IDENTITY_INSERT alpha ON")).unwrap();
    assert_ne!(plain.object_id, recreated.object_id);
}

#[test]
fn transactional_visibility_and_sql_shaped_names_do_not_escape_bound_lookup() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(&mut session, "CREATE TABLE dbo.alpha(id INT IDENTITY(1,1))");
    run(&mut session, "BEGIN TRANSACTION");
    run(
        &mut session,
        "CREATE TABLE dbo.[odd'; DROP TABLE dbo.alpha;--](id INT IDENTITY(1,1))",
    );
    let odd = parts("SET IDENTITY_INSERT dbo.[odd'; DROP TABLE dbo.alpha;--] ON");
    let resolved = resolve(&session.db, &odd).unwrap();
    assert_eq!(resolved.table, "odd'; DROP TABLE dbo.alpha;--");
    assert_eq!(resolved.identity_position, 0);
    assert!(resolve(&session.db, &parts("SET IDENTITY_INSERT alpha ON")).is_ok());
    run(&mut session, "ROLLBACK TRANSACTION");
    assert!(matches!(
        resolve(&session.db, &odd),
        Err(ResolveError::Diagnostic { error, .. }) if error.number == 1088
    ));
    assert!(resolve(&session.db, &parts("SET IDENTITY_INSERT alpha ON")).is_ok());
    assert!(matches!(
        resolve(
            &session.db,
            &parts("SET IDENTITY_INSERT master.dbo.alpha ON")
        ),
        Err(ResolveError::Unsupported(_))
    ));
}
