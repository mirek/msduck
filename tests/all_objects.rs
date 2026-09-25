use msduck::{engine::Session, server::Server};

fn count(db: &duckdb::Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn built_in_membership_is_a_disjoint_catalog_union() {
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM sys.objects"), 118);
    assert_eq!(count(&db, "SELECT count(*) FROM sys.system_objects"), 2624);
    assert_eq!(count(&db, "SELECT count(*) FROM sys.all_objects"), 2742);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM sys.objects o JOIN sys.system_objects s USING(object_id)"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM ((SELECT object_id FROM sys.all_objects EXCEPT SELECT object_id FROM sys.objects EXCEPT SELECT object_id FROM sys.system_objects))"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM sys.all_objects WHERE parent_object_id <> 0 AND parent_object_id NOT IN (SELECT object_id FROM sys.all_objects)"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM sys.system_objects WHERE object_id < 0 AND is_ms_shipped"
        ),
        2624
    );
    let help_id: i32 = db
        .query_row("SELECT __msduck_object_id('sys.sp_help','P')", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(help_id, -784136858);
    let help_name: String = db
        .query_row("SELECT __msduck_object_name(?)", [help_id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(help_name, "sp_help");
}

#[test]
fn user_objects_join_the_union_transactionally() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let run = |session: &mut Session, sql| {
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1,
            "{sql}"
        );
    };
    run(
        &mut session,
        "BEGIN TRAN; CREATE TABLE dbo.catalog_test(id INT)",
    );
    let id: i32 = session
        .db
        .query_row(
            "SELECT object_id FROM sys.all_objects WHERE name='catalog_test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count(
            &session.db,
            "SELECT count(*) FROM sys.objects WHERE name='catalog_test'"
        ),
        1
    );
    assert_eq!(
        count(
            &session.db,
            "SELECT count(*) FROM sys.system_objects WHERE name='catalog_test'"
        ),
        0
    );
    run(&mut session, "ROLLBACK");
    assert_eq!(
        count(
            &session.db,
            "SELECT count(*) FROM sys.all_objects WHERE name='catalog_test'"
        ),
        0
    );
    run(&mut session, "CREATE TABLE dbo.catalog_test(id INT)");
    let new_id: i32 = session
        .db
        .query_row(
            "SELECT object_id FROM sys.all_objects WHERE name='catalog_test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(id, new_id);
    assert_eq!(
        count(&session.db, "SELECT count(*) FROM sys.all_objects"),
        2743
    );
}

#[test]
fn built_in_and_user_catalog_ids_survive_reopen() {
    let path = std::env::temp_dir().join(format!(
        "msduck-all-objects-{}-{}.duckdb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let (user_id, builtin_clock) = {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(
            session
                .batch_response(
                    "CREATE TABLE dbo.catalog_persist(id INT)",
                    &Default::default(),
                    false,
                    None,
                )
                .1
        );
        let id = session
            .db
            .query_row(
                "SELECT object_id FROM sys.all_objects WHERE name='catalog_persist'",
                [],
                |row| row.get::<_, i32>(0),
            )
            .unwrap();
        let clock = session
            .db
            .query_row(
                "SELECT CAST(create_date AS VARCHAR) FROM sys.objects WHERE name='wpr_bucket_table'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        (id, clock)
    };
    let server = Server::open(path.to_str().unwrap()).unwrap();
    let db = server.connection().unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM sys.all_objects"), 2743);
    assert_eq!(
        db.query_row(
            "SELECT object_id FROM sys.all_objects WHERE name='catalog_persist'",
            [],
            |row| row.get::<_, i32>(0),
        )
        .unwrap(),
        user_id
    );
    assert_eq!(
        db.query_row(
            "SELECT CAST(create_date AS VARCHAR) FROM sys.objects WHERE name='wpr_bucket_table'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        builtin_clock
    );
    drop(db);
    drop(server);
    std::fs::remove_file(path).unwrap();
}

/// Run through scripts/bench-object-catalog-startup.mjs so both trees use the
/// same CPU affinity. Timing starts after the test executable has launched.
#[test]
#[ignore = "manual two-core startup benchmark"]
fn benchmark_builtin_catalog_startup() {
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    let samples = std::env::var("MSDUCK_CATALOG_BENCH_SAMPLES")
        .unwrap_or_else(|_| "20".to_owned())
        .parse::<usize>()
        .unwrap();
    assert!((5..=200).contains(&samples));
    let path = std::env::temp_dir().join(format!(
        "msduck-catalog-bench-{}-{}.duckdb",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let db = server.connection().unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM sys.all_objects"), 2742);
    }
    let mut memory_open = Vec::with_capacity(samples);
    let mut memory_query = Vec::with_capacity(samples);
    let mut reopen = Vec::with_capacity(samples);
    let mut reopen_query = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let server = Server::open(":memory:").unwrap();
        memory_open.push(start.elapsed().as_secs_f64() * 1_000.0);
        let db = server.connection().unwrap();
        let start = Instant::now();
        assert_eq!(count(&db, "SELECT count(*) FROM sys.all_objects"), 2742);
        memory_query.push(start.elapsed().as_secs_f64() * 1_000.0);
        drop(db);
        drop(server);

        let start = Instant::now();
        let server = Server::open(path.to_str().unwrap()).unwrap();
        reopen.push(start.elapsed().as_secs_f64() * 1_000.0);
        let db = server.connection().unwrap();
        let start = Instant::now();
        assert_eq!(count(&db, "SELECT count(*) FROM sys.all_objects"), 2742);
        reopen_query.push(start.elapsed().as_secs_f64() * 1_000.0);
    }
    std::fs::remove_file(path).unwrap();
    let summary = |mut values: Vec<f64>| {
        values.sort_by(f64::total_cmp);
        serde_json::json!({
            "p50_ms": values[values.len() / 2],
            "p95_ms": values[(values.len() * 95).div_ceil(100) - 1],
        })
    };
    println!(
        "catalog_startup_benchmark={}",
        serde_json::json!({
            "samples": samples,
            "memory_open": summary(memory_open),
            "memory_first_query": summary(memory_query),
            "persistent_reopen": summary(reopen),
            "persistent_first_query": summary(reopen_query),
        })
    );
}
