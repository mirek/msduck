//! Native execution of the deterministic paired-image plan, before wiring it
//! into the server. This does not establish OUTPUT protocol compatibility.
use duckdb::Connection;
use sqlparser::ast::{Ident, ObjectName, OutputClause, Statement};

fn rows(db: &Connection, sql: &str) -> Vec<Vec<i32>> {
    let mut statement = db.prepare(sql).unwrap();
    statement
        .query_map([], |row| {
            (0..row.as_ref().column_count())
                .map(|i| row.get(i))
                .collect()
        })
        .unwrap()
        .collect::<duckdb::Result<_>>()
        .unwrap()
}

#[test]
fn paired_update_plan_preserves_key_changes_swaps_and_single_evaluation() {
    for fail in [false, true] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER); INSERT INTO t VALUES(10,1,2),(20,3,4); CREATE SEQUENCE calls START 1; BEGIN TRANSACTION").unwrap();
        let sql = if fail {
            "UPDATE t SET id=10,a=b,b=a OUTPUT deleted.id,inserted.id"
        } else {
            "UPDATE t SET id=id+100,a=b,b=CAST(a+nextval('calls') AS INT) OUTPUT deleted.id,inserted.id,deleted.a,inserted.a,inserted.b"
        };
        let Statement::Update(update) =
            sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::MsSqlDialect {}, sql)
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        let plan = msduck_sql::output_update::plan(
            &update,
            &[Ident::new("id"), Ident::new("a"), Ident::new("b")],
            ObjectName::from(vec![Ident::new("images")]),
            Ident::new("captured"),
        )
        .unwrap();
        db.execute_batch(&format!("CREATE TEMP TABLE images AS {}", plan.capture))
            .unwrap();
        let written = db.execute_batch(&plan.write.to_string());
        if fail {
            assert!(written.is_err());
            db.execute_batch("ROLLBACK").unwrap();
            assert_eq!(
                rows(&db, "SELECT id,a,b FROM t ORDER BY id"),
                vec![vec![10, 1, 2], vec![20, 3, 4]]
            );
            assert!(db.prepare("SELECT * FROM images").is_err());
        } else {
            written.unwrap();
            let Some(OutputClause::Output { select_items, .. }) = &update.output else {
                unreachable!()
            };
            let projection = plan.projection(select_items).unwrap();
            assert_eq!(
                rows(
                    &db,
                    &format!("{projection} ORDER BY captured.__msduck_deleted_0")
                ),
                vec![vec![10, 110, 1, 2, 2], vec![20, 120, 3, 4, 5]]
            );
            assert_eq!(
                rows(&db, "SELECT id,a,b FROM t ORDER BY id"),
                vec![vec![110, 2, 2], vec![120, 4, 5]]
            );
            assert_eq!(
                rows(&db, "SELECT CAST(currval('calls') AS INT)"),
                vec![vec![2]]
            );
            db.execute_batch("DROP TABLE images; COMMIT").unwrap();
        }
    }
}
