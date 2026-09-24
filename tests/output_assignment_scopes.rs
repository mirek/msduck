//! Native joined-image planning against SQL Server scope reference programs.
//! This tests row values, readback and binding errors, not TDS/server dispatch.
use msduck::{engine::Session, output_join, server::Server};
use serde_json::{Value, json};
use sqlparser::{ast::*, parser::Parser};
use std::collections::HashMap;

fn rows(db: &duckdb::Connection, sql: &str) -> Vec<Value> {
    let mut statement = db.prepare(sql).unwrap();
    statement
        .query_map([], |row| {
            let count = row.as_ref().column_count();
            Ok(Value::Array(
                (0..count)
                    .map(|index| row.get::<_, Option<i32>>(index).map(|value| json!(value)))
                    .collect::<duckdb::Result<Vec<_>>>()?,
            ))
        })
        .unwrap()
        .collect::<duckdb::Result<Vec<_>>>()
        .unwrap()
}

#[test]
fn reference_assignment_scopes_preserve_native_values_and_prewrite_errors() {
    let reference: Value =
        serde_json::from_str(include_str!("../reference/output-assignment-scopes.json")).unwrap();
    for (index, case) in reference["results"].as_array().unwrap().iter().enumerate() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(
            session
                .batch_response(
                    case["setup"].as_str().unwrap(),
                    &HashMap::new(),
                    false,
                    None
                )
                .1
        );
        let Statement::Update(update) = Parser::parse_sql(
            &msduck_sql::dialect::ServerDialect,
            case["query"].as_str().unwrap(),
        )
        .unwrap()
        .remove(0) else {
            unreachable!()
        };
        let binding = output_join::bind(&session.db, &update, None, &HashMap::new()).unwrap();
        let plan = binding.images(
            &update,
            ObjectName::from(vec![Ident::new("candidates")]),
            ObjectName::from(vec![Ident::new("images")]),
            Ident::new("captured"),
        );
        if let Some(expected) = case["reference"]["errors"].as_array().unwrap().first() {
            let error = plan.err().unwrap();
            let actual = error
                .downcast_ref::<msduck_core::diagnostic::SqlError>()
                .unwrap();
            assert_eq!(
                json!([
                    actual.number,
                    actual.state,
                    actual.severity,
                    &actual.message
                ]),
                json!([
                    expected["number"],
                    expected["state"],
                    expected["class"],
                    expected["message"]
                ]),
                "case {index}"
            );
            println!(
                "{}",
                json!({"case":index,"error":{"number":actual.number,"state":actual.state,"class":actual.severity,"message":actual.message}})
            );
        } else {
            let plan = plan.unwrap();
            let Some(OutputClause::Output { select_items, .. }) = &update.output else {
                unreachable!()
            };
            let projection = plan.projection(select_items).unwrap();
            session.db.execute_batch("BEGIN TRANSACTION").unwrap();
            session
                .db
                .execute_batch(&format!(
                    "CREATE TEMP TABLE candidates AS {}",
                    binding.candidates.capture
                ))
                .unwrap();
            session
                .db
                .execute_batch(&format!("CREATE TEMP TABLE images AS {}", plan.capture))
                .unwrap();
            session.db.execute_batch(&plan.write.to_string()).unwrap();
            let actual = rows(&session.db, &projection.to_string());
            let expected = case["reference"]["sets"][0]["rows"].as_array().unwrap();
            // OUTPUT has no ordering contract. Preserve raw observed order in
            // the test log and check these uniquely keyed rows individually.
            println!(
                "{}",
                json!({"case":index,"rows":actual,"referenceRows":expected})
            );
            assert_eq!(actual.len(), expected.len(), "case {index}");
            for row in expected {
                assert!(actual.contains(row), "case {index}: {actual:?}");
            }
            session.db.execute_batch("COMMIT").unwrap();
        }
        let after = rows(&session.db, "SELECT id,n FROM output_ref ORDER BY id");
        let expected = case["after"]["sets"][0]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| Value::Array(row.as_array().unwrap()[..2].to_vec()))
            .collect::<Vec<_>>();
        assert_eq!(after, expected, "readback case {index}");
    }
}
