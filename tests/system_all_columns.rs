use msduck::{engine::Session, server::Server};

fn count(db: &duckdb::Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn run(session: &mut Session, sql: &str) {
    assert!(
        session
            .batch_response(sql, &Default::default(), false, None)
            .1,
        "{sql}"
    );
}

fn empty_column_count(db: &duckdb::Connection, view: &str) -> usize {
    let mut statement = db
        .prepare(&format!("SELECT * FROM sys.{view} WHERE 1=0"))
        .unwrap();
    let mut rows = statement.query([]).unwrap();
    assert!(rows.next().unwrap().is_none());
    drop(rows);
    statement.column_count()
}

#[test]
fn pinned_builtin_columns_preserve_values_and_separate_membership() {
    let server = Server::open(":memory:").unwrap();
    let db = server.connection().unwrap();
    assert_eq!(empty_column_count(&db, "columns"), 43);
    assert_eq!(empty_column_count(&db, "system_columns"), 43);
    assert_eq!(empty_column_count(&db, "all_columns"), 43);
    assert_eq!(count(&db, "SELECT count(*) FROM sys.columns"), 1269);
    assert_eq!(count(&db, "SELECT count(*) FROM sys.system_columns"), 11534);
    assert_eq!(count(&db, "SELECT count(*) FROM sys.all_columns"), 12803);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM sys.columns u JOIN sys.system_columns s USING(object_id,column_id)"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM (SELECT * FROM sys.all_columns EXCEPT SELECT * EXCLUDE(in_system_columns) FROM main.__msduck_builtin_columns)"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM (SELECT * EXCLUDE(in_system_columns) FROM main.__msduck_builtin_columns EXCEPT SELECT * FROM sys.all_columns)"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM sys.all_columns WHERE object_id NOT IN (SELECT object_id FROM sys.all_objects)"
        ),
        2306
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(DISTINCT object_id) FROM sys.all_columns WHERE object_id NOT IN (SELECT object_id FROM sys.all_objects)"
        ),
        166
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM sys.all_objects WHERE object_id=-1069989784"
        ),
        0
    );
    assert_eq!(
        db.query_row("SELECT __msduck_object_name(-1069989784)", [], |row| row
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "dm_pdw_nodes_os_tasks"
    );
    assert_eq!(
        db.query_row(
            "SELECT __msduck_object_schema_name(-1069989784)",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "sys"
    );
    assert_eq!(
        db.query_row("SELECT __msduck_col_name(-103,1)", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "object_id"
    );
}

#[test]
fn user_columns_join_all_columns_transactionally_and_survive_reopen() {
    let path = std::env::temp_dir().join(format!(
        "msduck-system-columns-{}-{}.duckdb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let (id, name_id, default_id) = {
        let server = Server::open(path.to_str().unwrap()).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        run(
            &mut session,
            "BEGIN TRAN; CREATE TABLE dbo.transient_column(id INT CONSTRAINT df_transient_column DEFAULT 1)",
        );
        assert_eq!(
            count(
                &session.db,
                "SELECT count(*) FROM sys.all_columns WHERE name='id' AND object_id=__msduck_object_id('transient_column',NULL)"
            ),
            1
        );
        assert_eq!(
            count(
                &session.db,
                "SELECT count(*) FROM sys.objects WHERE name='df_transient_column' AND type='D '"
            ),
            1
        );
        run(&mut session, "ROLLBACK");
        assert_eq!(
            count(
                &session.db,
                "SELECT count(*) FROM sys.all_columns WHERE name='id' AND object_id=__msduck_object_id('transient_column',NULL)"
            ),
            0
        );
        assert_eq!(
            count(
                &session.db,
                "SELECT count(*) FROM sys.objects WHERE name='df_transient_column'"
            ),
            0
        );
        run(
            &mut session,
            "CREATE TABLE dbo.persisted_columns(id INT IDENTITY(1,1),label NVARCHAR(20) COLLATE Latin1_General_100_CI_AS CONSTRAINT df_persisted_label DEFAULT N'x')",
        );
        assert!(!session.batch_response("CREATE TABLE dbo.conflicting_default(label NVARCHAR(20) CONSTRAINT df_persisted_label DEFAULT N'y')", &Default::default(), false, None).1);
        assert_eq!(
            count(
                &session.db,
                "SELECT count(*) FROM sys.objects WHERE name='conflicting_default'"
            ),
            0
        );
        run(
            &mut session,
            "CREATE VIEW dbo.persisted_view AS SELECT id,label FROM dbo.persisted_columns",
        );
        let id: i32 = session
            .db
            .query_row(
                "SELECT __msduck_object_id('persisted_columns',NULL)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let name_id: i32 = session
            .db
            .query_row(
                "SELECT column_id FROM sys.columns WHERE object_id=? AND name='label'",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        let default_id: i32 = session
            .db
            .query_row(
                "SELECT default_object_id FROM sys.columns WHERE object_id=? AND name='label'",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT name,type,parent_object_id FROM sys.objects WHERE object_id=?",
                    [default_id],
                    |row| Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i32>(2)?
                    )),
                )
                .unwrap(),
            ("df_persisted_label".to_owned(), "D ".to_owned(), id)
        );
        assert_eq!(
            count(
                &session.db,
                "SELECT count(*) FROM sys.columns WHERE object_id=__msduck_object_id('persisted_columns',NULL)"
            ),
            2
        );
        assert_eq!(
            count(
                &session.db,
                "SELECT count(*) FROM sys.all_columns WHERE object_id IN (__msduck_object_id('persisted_columns',NULL),__msduck_object_id('persisted_view',NULL))"
            ),
            4
        );
        assert_eq!(
            count(
                &session.db,
                "SELECT count(*) FROM sys.system_columns WHERE object_id IN (__msduck_object_id('persisted_columns',NULL),__msduck_object_id('persisted_view',NULL))"
            ),
            0
        );
        assert_eq!(
            count(
                &session.db,
                "SELECT count(*) FROM sys.columns WHERE object_id=__msduck_object_id('persisted_columns',NULL) AND name='id' AND is_identity"
            ),
            1
        );
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT collation_name FROM sys.columns WHERE object_id=? AND name='label'",
                    [id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "Latin1_General_100_CI_AS"
        );
        (id, name_id, default_id)
    };
    let server = Server::open(path.to_str().unwrap()).unwrap();
    let db = server.connection().unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM sys.all_columns"), 12807);
    assert_eq!(count(&db, "SELECT count(*) FROM sys.system_columns"), 11534);
    assert_eq!(
        db.query_row(
            "SELECT column_id FROM sys.all_columns WHERE object_id=? AND name='label'",
            [id],
            |row| row.get::<_, i32>(0)
        )
        .unwrap(),
        name_id
    );
    assert_eq!(
        db.query_row(
            "SELECT default_object_id FROM sys.all_columns WHERE object_id=? AND name='label'",
            [id],
            |row| row.get::<_, i32>(0),
        )
        .unwrap(),
        default_id
    );
    drop(db);
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(
        &mut session,
        "DROP VIEW dbo.persisted_view; DROP TABLE dbo.persisted_columns",
    );
    assert_eq!(
        count(
            &session.db,
            "SELECT count(*) FROM sys.objects WHERE name='df_persisted_label'"
        ),
        0
    );
    assert_eq!(
        session
            .db
            .query_row(
                "SELECT count(*) FROM sys.all_columns WHERE object_id=?",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    drop(session);
    drop(server);
    std::fs::remove_file(path).unwrap();
}
