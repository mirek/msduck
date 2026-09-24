//! Native acquisition experiment for joined OUTPUT; not server feature coverage.
//! Choose one source per target before evaluating assignments, retaining that
//! source alongside the old target so output cannot select a different match.
use duckdb::Connection;

fn count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn joined_candidates_are_deduplicated_before_assignment_evaluation() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY,n INTEGER); INSERT INTO t VALUES(10,1),(20,2); CREATE TABLE s(id INTEGER,choice INTEGER,extra INTEGER); INSERT INTO s VALUES(10,1,7),(10,2,9),(20,1,11),(20,2,13); CREATE SEQUENCE evaluations START 1; BEGIN TRANSACTION").unwrap();
    // Explicit ordering makes this experiment reproducible. SQL Server does
    // not promise which conflicting match an UPDATE FROM will select.
    db.execute_batch("CREATE TEMP TABLE candidates AS SELECT t.rowid AS rid,t AS target,s AS source FROM t JOIN s ON t.id=s.id QUALIFY ROW_NUMBER() OVER(PARTITION BY t.rowid ORDER BY s.choice)=1").unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM candidates"), 2);
    db.execute_batch("CREATE TEMP TABLE images AS SELECT rid,target,source,target.id+100 AS new_id,CAST(target.n+source.extra+nextval('evaluations')+CASE WHEN source.choice=2 THEN error('discarded source evaluated') ELSE 0 END AS INTEGER) AS new_n FROM candidates").unwrap();
    assert_eq!(count(&db, "SELECT currval('evaluations')"), 2);
    db.execute_batch(
        "UPDATE t SET id=images.new_id,n=images.new_n FROM images WHERE t.rowid=images.rid",
    )
    .unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM t JOIN images ON t.id=images.new_id AND t.n=images.new_n WHERE images.new_id=images.target.id+100 AND images.source.choice=1"
        ),
        2
    );
    assert_eq!(
        count(
            &db,
            "SELECT CAST(SUM(new_n-target.n-source.extra) AS BIGINT) FROM images"
        ),
        3
    );
    // The images retain the selected source after its underlying table changes.
    db.execute_batch("UPDATE s SET extra=999").unwrap();
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM images WHERE source.extra<>999"),
        2
    );
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM t WHERE (id=10 AND n=1) OR (id=20 AND n=2)"
        ),
        2
    );
    assert!(db.prepare("SELECT * FROM candidates").is_err());
    assert!(db.prepare("SELECT * FROM images").is_err());
}

#[test]
fn generated_candidate_plan_retains_null_sources_and_excludes_null_targets() {
    use sqlparser::ast::{Ident, Statement, TableFactor};
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY,n INTEGER); INSERT INTO t VALUES(10,1),(20,2); CREATE TABLE s(id INTEGER,extra INTEGER); INSERT INTO s VALUES(10,7),(10,9),(30,11); CREATE SEQUENCE evaluations START 1; BEGIN TRANSACTION").unwrap();
    let Statement::Update(update) = sqlparser::parser::Parser::parse_sql(
        &msduck_sql::dialect::ServerDialect,
        "UPDATE t SET id=p.id+100,n=CAST(p.n+COALESCE(s.extra,0)+nextval('evaluations') AS INTEGER) FROM t p FULL JOIN s ON p.id=s.id",
    )
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    let resolved = msduck_sql::output_target::resolve(
        &update,
        &std::collections::HashMap::from([("t".into(), 42), ("s".into(), 99)]),
    )
    .unwrap();
    let TableFactor::Table {
        name,
        alias: Some(alias),
        ..
    } = &resolved.relation
    else {
        unreachable!()
    };
    assert_eq!(name.to_string(), "t");
    assert_eq!(alias.name.value, "p");
    let reference = |alias: &str, name: &str| vec![Ident::new(alias), Ident::new(name)];
    let plan = msduck_sql::output_join::plan(
        None,
        resolved.sources,
        update.selection.clone(),
        reference("p", "rowid"),
        vec![
            reference("p", "id"),
            reference("p", "n"),
            reference("s", "id"),
            reference("s", "extra"),
        ],
    )
    .unwrap();
    db.execute_batch(&format!("CREATE TEMP TABLE candidates AS {}", plan.capture))
        .unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM candidates"), 2);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM candidates WHERE __msduck_source_2 IS NULL"
        ),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM candidates WHERE __msduck_source_2=30"
        ),
        0
    );
    let mut physical = update.clone();
    physical.table.relation = resolved.relation;
    let images = msduck_sql::output_update::from_candidates(
        &physical,
        &[Ident::new("id"), Ident::new("n")],
        &plan,
        sqlparser::ast::ObjectName::from(vec![Ident::new("candidates")]),
        sqlparser::ast::ObjectName::from(vec![Ident::new("images")]),
        Ident::new("selected"),
    )
    .unwrap();
    db.execute_batch(&format!("CREATE TEMP TABLE images AS {}", images.capture))
        .unwrap();
    assert_eq!(count(&db, "SELECT currval('evaluations')"), 2);
    db.execute_batch(&images.write.to_string()).unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM t JOIN images ON t.id=images.__msduck_inserted_0 AND t.n=images.__msduck_inserted_1"
        ),
        2
    );
    assert_eq!(
        count(
            &db,
            "SELECT CAST(SUM(__msduck_inserted_1-__msduck_deleted_1-COALESCE(__msduck_source_3,0)) AS BIGINT) FROM images"
        ),
        3
    );
    db.execute_batch("UPDATE s SET extra=999").unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM images WHERE __msduck_source_3=999"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM images WHERE __msduck_inserted_0=__msduck_deleted_0+100"
        ),
        2
    );
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM t WHERE id IN (10,20)"), 2);
    assert!(db.prepare("SELECT * FROM candidates").is_err());
}
