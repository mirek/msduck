//! Catalog identities and definitions through transactions and restarts
//! (docs/gaps-catalog.md). tests/compat/catalog.test.mjs compares every
//! captured SQL Server response through tedious; these tests cover what a
//! single connection cannot show there: persistence across a restart,
//! rollback of renames and definitions, and the keys lifecycle.
use msduck::engine::Session;
use msduck::server::Server;

fn run(session: &mut Session, sql: &str) {
    let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok && !response.contains(&0xAA), "{sql}: {response:?}");
}

/// Whether a batch reports an error (an ERROR token).
fn fails(session: &mut Session, sql: &str) -> bool {
    let (response, _) = session.batch_response(sql, &Default::default(), false, None);
    response.contains(&0xAA)
}

/// Rows of a backend query, each as `|`-separated text.
fn rows(session: &Session, sql: &str) -> Vec<String> {
    use duckdb::types::Value;
    let mut statement = session.db.prepare(sql).unwrap();
    let mut rows = statement.query([]).unwrap();
    let mut result = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let columns = row.as_ref().column_count();
        let values = (0..columns).map(|i| match row.get::<_, Value>(i).unwrap() {
            Value::Null => "NULL".to_owned(),
            Value::Boolean(b) => (b as u8).to_string(),
            Value::TinyInt(v) => v.to_string(),
            Value::SmallInt(v) => v.to_string(),
            Value::Int(v) => v.to_string(),
            Value::BigInt(v) => v.to_string(),
            Value::UTinyInt(v) => v.to_string(),
            Value::Text(v) => v,
            other => panic!("unexpected value {other:?}"),
        });
        result.push(values.collect::<Vec<_>>().join("|"));
    }
    result
}

fn temporary_path(name: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "msduck-gaps-catalog-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    directory.join("catalog.duckdb")
}

const OBJECTS: &str = "SELECT name,rtrim(type),object_id FROM sys.objects
    WHERE parent_object_id=__msduck_object_id('dbo.parent','U') OR object_id=__msduck_object_id('dbo.parent','U')
    ORDER BY object_id";

#[test]
fn definitions_identities_and_layouts_survive_restart() {
    let path = temporary_path("restart");
    let path = path.to_str().unwrap();
    let (objects, definitions, layout);
    {
        let server = Server::open(path).unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        run(
            &mut session,
            "CREATE TABLE dbo.parent(id INT NOT NULL CONSTRAINT pk_parent PRIMARY KEY NONCLUSTERED,
               code INT NOT NULL CONSTRAINT uq_parent UNIQUE CLUSTERED, qty INT DEFAULT 0,
               label VARCHAR(20) CONSTRAINT df_label DEFAULT (CAST(1 AS VARCHAR)),
               doubled AS qty * 2, CONSTRAINT ck_qty CHECK (qty IN (0, 1)))",
        );
        run(
            &mut session,
            "CREATE VIEW dbo.parent_view AS SELECT id, code FROM dbo.parent",
        );
        run(
            &mut session,
            "CREATE PROCEDURE dbo.parent_proc @id INT AS SELECT @id",
        );
        objects = rows(&session, OBJECTS);
        definitions = rows(
            &session,
            "SELECT name,definition FROM sys.default_constraints UNION ALL
             SELECT name,definition FROM sys.check_constraints UNION ALL
             SELECT name,definition FROM sys.computed_columns UNION ALL
             SELECT o.name,m.definition FROM sys.sql_modules m JOIN sys.objects o USING(object_id)
             ORDER BY 1",
        );
        layout = rows(
            &session,
            "SELECT name,index_id,type_desc FROM sys.indexes WHERE object_id=__msduck_object_id('dbo.parent','U') ORDER BY index_id",
        );
    }
    // The generated DEFAULT name ends with its object ID.
    let generated = objects
        .iter()
        .find(|row| row.starts_with("DF__parent__qty__"))
        .unwrap();
    let id: i32 = generated.rsplit('|').next().unwrap().parse().unwrap();
    assert!(generated.starts_with(&format!("DF__parent__qty__{:08X}|D|", id as u32)));
    assert_eq!(
        definitions,
        [
            format!("{}|((0))", generated.split('|').next().unwrap()),
            "ck_qty|([qty]=(1) OR [qty]=(0))".into(),
            "df_label|(CONVERT([varchar],(1)))".into(),
            "doubled|([qty]*(2))".into(),
            "parent_proc|CREATE PROCEDURE dbo.parent_proc @id INT AS SELECT @id".into(),
            "parent_view|CREATE VIEW dbo.parent_view AS SELECT id, code FROM dbo.parent".into(),
        ]
    );
    assert_eq!(
        layout,
        ["uq_parent|1|CLUSTERED", "pk_parent|2|NONCLUSTERED"]
    );
    for _ in 0..2 {
        let server = Server::open(path).unwrap();
        let session = Session::new(server.connection().unwrap()).unwrap();
        assert_eq!(rows(&session, OBJECTS), objects);
        assert_eq!(
            rows(
                &session,
                "SELECT name,definition FROM sys.default_constraints UNION ALL
                 SELECT name,definition FROM sys.check_constraints UNION ALL
                 SELECT name,definition FROM sys.computed_columns UNION ALL
                 SELECT o.name,m.definition FROM sys.sql_modules m JOIN sys.objects o USING(object_id)
                 ORDER BY 1",
            ),
            definitions
        );
        assert_eq!(
            rows(
                &session,
                "SELECT name,index_id,type_desc FROM sys.indexes WHERE object_id=__msduck_object_id('dbo.parent','U') ORDER BY index_id",
            ),
            layout
        );
    }
}

#[test]
fn key_constraint_identities_follow_the_keys_lifecycle() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let keys = |session: &Session| {
        rows(
            session,
            "SELECT name,object_id,unique_index_id FROM sys.key_constraints ORDER BY name",
        )
    };
    run(
        &mut session,
        "CREATE TABLE dbo.k(id NVARCHAR(10) NOT NULL CONSTRAINT pk_k PRIMARY KEY, code INT NULL CONSTRAINT uq_k UNIQUE)",
    );
    let created = keys(&session);
    assert_eq!(created.len(), 2);
    // Columns added around the rebuilt indexes keep the identities.
    run(&mut session, "ALTER TABLE dbo.k ADD extra INT NULL");
    run(
        &mut session,
        "ALTER TABLE dbo.k ALTER COLUMN extra BIGINT NULL",
    );
    assert_eq!(keys(&session), created);
    // A rolled back drop keeps them; dropping and creating again does not.
    run(&mut session, "BEGIN TRAN; DROP TABLE dbo.k; ROLLBACK");
    assert_eq!(keys(&session), created);
    run(
        &mut session,
        "DROP TABLE dbo.k; CREATE TABLE dbo.k(id NVARCHAR(10) NOT NULL CONSTRAINT pk_k PRIMARY KEY, code INT NULL CONSTRAINT uq_k UNIQUE)",
    );
    let recreated = keys(&session);
    assert_eq!(recreated.len(), 2);
    for (old, new) in created.iter().zip(&recreated) {
        assert_ne!(old, new);
    }
    // sp_rename keeps the identity, and the name follows in every view.
    run(&mut session, "EXEC sp_rename 'dbo.pk_k', 'pk_renamed'");
    let renamed = keys(&session);
    assert_eq!(
        renamed[0].split('|').skip(1).collect::<Vec<_>>(),
        recreated[0].split('|').skip(1).collect::<Vec<_>>()
    );
    assert!(renamed[0].starts_with("pk_renamed|"));
    assert_eq!(
        rows(
            &session,
            "SELECT name,is_primary_key FROM sys.indexes WHERE object_id=__msduck_object_id('dbo.k','U') AND index_id=1"
        ),
        ["pk_renamed|1"]
    );
    // The keys-managed index still enforces the renamed constraint.
    run(&mut session, "INSERT dbo.k(id, code) VALUES (N'a', 1)");
    assert!(fails(
        &mut session,
        "INSERT dbo.k(id, code) VALUES (N'a', 2)"
    ));
}

#[test]
fn renames_roll_back_with_their_transaction() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(
        &mut session,
        "CREATE TABLE dbo.r(id INT NOT NULL CONSTRAINT pk_r PRIMARY KEY, v INT CONSTRAINT df_r DEFAULT 1, w INT);
         CREATE INDEX ix_r_w ON dbo.r(w);
         INSERT dbo.r(id, v, w) VALUES (1, 2, 3)",
    );
    let snapshot = |session: &Session| {
        (
            rows(
                session,
                "SELECT name,object_id FROM sys.objects WHERE name IN ('r','r2') OR parent_object_id IN (SELECT object_id FROM sys.objects WHERE name IN ('r','r2')) ORDER BY object_id",
            ),
            rows(
                session,
                "SELECT c.name,c.column_id FROM sys.columns c JOIN sys.objects o USING(object_id) WHERE o.name IN ('r','r2') ORDER BY column_id",
            ),
            rows(
                session,
                "SELECT i.name,i.index_id FROM sys.indexes i JOIN sys.objects o USING(object_id) WHERE o.name IN ('r','r2') ORDER BY index_id",
            ),
        )
    };
    let before = snapshot(&session);
    run(
        &mut session,
        "BEGIN TRAN;
         EXEC sp_rename 'dbo.r.w', 'w2', 'COLUMN';
         EXEC sp_rename 'dbo.r.ix_r_w', 'ix_r_w2', 'INDEX';
         EXEC sp_rename 'df_r', 'df_r2';
         EXEC sp_rename 'dbo.r', 'r2'",
    );
    let during = snapshot(&session);
    assert_ne!(during, before);
    assert!(during.0.iter().any(|row| row.starts_with("r2|")));
    assert!(during.1.contains(&"w2|3".to_owned()));
    assert!(during.2.contains(&"ix_r_w2|2".to_owned()));
    run(&mut session, "ROLLBACK");
    assert_eq!(snapshot(&session), before);
    // The data and the index survive in place.
    run(&mut session, "UPDATE dbo.r SET w = 4 WHERE w = 3");
    // A committed rename keeps every identity.
    run(
        &mut session,
        "EXEC sp_rename 'dbo.r.w', 'w2', 'COLUMN'; EXEC sp_rename 'dbo.r', 'r2'",
    );
    let after = snapshot(&session);
    assert_eq!(
        after
            .0
            .iter()
            .map(|row| row.split('|').nth(1).unwrap().to_owned())
            .collect::<Vec<_>>(),
        before
            .0
            .iter()
            .map(|row| row.split('|').nth(1).unwrap().to_owned())
            .collect::<Vec<_>>()
    );
    run(&mut session, "INSERT dbo.r2(id, v, w2) VALUES (2, 3, 4)");
    assert!(fails(
        &mut session,
        "INSERT dbo.r2(id, v, w2) VALUES (2, 3, 4)"
    ));
}

#[test]
fn view_and_default_text_follows_only_committed_statements() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(&mut session, "CREATE TABLE dbo.t(id INT, v INT)");
    run(&mut session, "BEGIN TRAN");
    run(&mut session, "CREATE VIEW dbo.v AS SELECT id FROM dbo.t");
    run(
        &mut session,
        "ALTER TABLE dbo.t ADD CONSTRAINT df_v DEFAULT (7) FOR v",
    );
    assert_eq!(
        rows(
            &session,
            "SELECT definition FROM sys.default_constraints WHERE name='df_v'"
        ),
        ["((7))"]
    );
    run(&mut session, "ROLLBACK");
    assert!(rows(&session, "SELECT * FROM main.__msduck_view_sources").is_empty());
    assert!(rows(&session, "SELECT * FROM main.__msduck_default_sources").is_empty());
    // A failed ALTER VIEW keeps the old text.
    run(&mut session, "CREATE VIEW dbo.v AS SELECT id FROM dbo.t");
    assert!(fails(
        &mut session,
        "ALTER VIEW dbo.v AS SELECT missing FROM dbo.t"
    ));
    assert_eq!(
        rows(
            &session,
            "SELECT main.__msduck_object_definition(__msduck_object_id('dbo.v','V'))"
        ),
        ["CREATE VIEW dbo.v AS SELECT id FROM dbo.t"]
    );
    run(&mut session, "ALTER VIEW dbo.v AS SELECT v FROM dbo.t");
    assert_eq!(
        rows(
            &session,
            "SELECT main.__msduck_object_definition(__msduck_object_id('dbo.v','V'))"
        ),
        ["CREATE VIEW dbo.v AS SELECT v FROM dbo.t"]
    );
}

#[test]
fn bound_defaults_and_checks_follow_sql_server_alter_column_rules() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    run(
        &mut session,
        "CREATE TABLE dbo.a(id INT, t TIME(3) DEFAULT '12:00:00.1249', v INT DEFAULT 1,
           s VARCHAR(10) CONSTRAINT ck_s CHECK (s <> ''), n INT CONSTRAINT ck_n CHECK (n > 0))",
    );
    // A DEFAULT allows another precision of the same type, not another type.
    run(&mut session, "ALTER TABLE dbo.a ALTER COLUMN t TIME(2)");
    assert!(fails(
        &mut session,
        "ALTER TABLE dbo.a ALTER COLUMN v BIGINT"
    ));
    // A CHECK allows another length of a variable-length type only.
    run(&mut session, "ALTER TABLE dbo.a ALTER COLUMN s VARCHAR(20)");
    assert!(fails(
        &mut session,
        "ALTER TABLE dbo.a ALTER COLUMN n BIGINT"
    ));
    // Dropping a column with a DEFAULT fails until the DEFAULT is dropped by
    // its generated name.
    assert!(fails(&mut session, "ALTER TABLE dbo.a DROP COLUMN v"));
    let name = rows(
        &session,
        "SELECT d.name FROM sys.default_constraints d JOIN sys.columns c ON c.object_id=d.parent_object_id AND c.column_id=d.parent_column_id WHERE c.name='v'",
    )
    .remove(0);
    assert!(name.starts_with("DF__a__v__"));
    run(
        &mut session,
        &format!("ALTER TABLE dbo.a DROP CONSTRAINT {name}; ALTER TABLE dbo.a DROP COLUMN v"),
    );
    // ALTER TABLE ... ADD gives unnamed DEFAULTs objects too.
    run(&mut session, "ALTER TABLE dbo.a ADD w INT DEFAULT 5");
    assert_eq!(
        rows(
            &session,
            "SELECT left(name,10),definition,is_system_named FROM sys.default_constraints WHERE parent_column_id=(SELECT column_id FROM sys.columns WHERE object_id=__msduck_object_id('dbo.a','U') AND name='w')"
        ),
        ["DF__a__w__|((5))|1"]
    );
}
