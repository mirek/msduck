//! sys.indexes and sys.index_columns list PRIMARY KEY and UNIQUE constraint
//! indexes and keys-managed indexes (src/index_catalog.rs).
//!
//! The expected rows were captured from SQL Server 2022 (16.0.4236.2,
//! `mcr.microsoft.com/mssql/server:2022-latest`) running the same statements
//! in fresh databases. Known differences are asserted separately, with the
//! SQL Server rows in comments.
use msduck::engine::Session;
use msduck::server::Server;

fn run(session: &mut Session, sql: &str) {
    let (response, ok) = session.batch_response(sql, &Default::default(), false, None);
    assert!(ok && !response.contains(&0xAA), "{sql}: {response:?}");
}

/// Rows of a backend query, each as `|`-separated text with NULL as `NULL`
/// and booleans as 0/1, like sqlcmd prints them.
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

fn session(server: &Server) -> Session {
    Session::new(server.connection().unwrap()).unwrap()
}

const INDEXES: &str = "SELECT o.name AS table_name,i.name,i.index_id,i.type,i.type_desc,i.is_unique,
    i.data_space_id,i.ignore_dup_key,i.is_primary_key,i.is_unique_constraint,i.fill_factor,i.is_padded,
    i.is_disabled,i.is_hypothetical,i.is_ignored_in_optimization,i.allow_row_locks,i.allow_page_locks,
    i.has_filter,i.filter_definition,i.compression_delay,i.suppress_dup_key_messages,i.auto_created,
    i.optimize_for_sequential_key
  FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id WHERE rtrim(o.type)='U'
  ORDER BY o.name,i.index_id";

const INDEX_COLUMNS: &str = "SELECT o.name AS table_name,i.name AS index_name,ic.index_column_id,
    c.name AS column_name,ic.column_id,ic.key_ordinal,ic.partition_ordinal,ic.is_descending_key,
    ic.is_included_column,ic.column_store_order_ordinal
  FROM sys.index_columns ic JOIN sys.objects o ON o.object_id=ic.object_id
  JOIN sys.indexes i ON i.object_id=ic.object_id AND i.index_id=ic.index_id
  JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id
  WHERE rtrim(o.type)='U' ORDER BY o.name,ic.index_id,ic.index_column_id";

const SCHEMA: &[&str] = &[
    "CREATE TABLE items (id int NOT NULL CONSTRAINT pk_items PRIMARY KEY, name nvarchar(100) NULL)",
    "CREATE TABLE orders (id int NOT NULL CONSTRAINT pk_orders PRIMARY KEY NONCLUSTERED, placed int NOT NULL, customer int NULL)",
    "CREATE CLUSTERED INDEX cx_orders ON orders(placed)",
    "CREATE INDEX ix_orders_customer ON orders(customer)",
    "CREATE TABLE lines (order_id int NOT NULL, line int NOT NULL, sku varchar(20) NOT NULL, CONSTRAINT pk_lines PRIMARY KEY (order_id, line))",
    "CREATE TABLE people (id int NOT NULL CONSTRAINT pk_people PRIMARY KEY, email nvarchar(200) NULL CONSTRAINT uq_people_email UNIQUE, first nvarchar(50) NOT NULL, last nvarchar(50) NOT NULL, CONSTRAINT uq_people_name UNIQUE (last, first))",
    "CREATE INDEX ix_people_last ON people(last)",
    "CREATE TABLE events (id int NOT NULL, kind int NULL, seen datetime2 NULL, payload nvarchar(400) NULL)",
    "CREATE UNIQUE INDEX ux_events_kind ON events(kind DESC, id) INCLUDE (payload)",
    "CREATE INDEX fx_events_seen ON events(seen) WHERE seen IS NOT NULL AND kind > 5",
    "CREATE INDEX ix_events_kind ON events(kind) INCLUDE (seen, payload) WHERE kind IN (1, 2)",
    "CREATE TABLE tags (code nvarchar(40) NOT NULL CONSTRAINT pk_tags PRIMARY KEY, label nvarchar(100) NOT NULL)",
];

#[test]
fn constraint_and_managed_index_rows_match_sql_server() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    for sql in SCHEMA {
        run(&mut s, sql);
    }
    assert_eq!(
        rows(&s, INDEXES),
        [
            "events|NULL|0|0|HEAP|0|1|0|0|0|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
            "events|ux_events_kind|2|2|NONCLUSTERED|1|1|0|0|0|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
            "events|fx_events_seen|3|2|NONCLUSTERED|0|1|0|0|0|0|0|0|0|0|1|1|1|([seen] IS NOT NULL AND [kind]>(5))|NULL|0|0|0",
            "events|ix_events_kind|4|2|NONCLUSTERED|0|1|0|0|0|0|0|0|0|0|1|1|1|([kind] IN ((1), (2)))|NULL|0|0|0",
            "items|pk_items|1|1|CLUSTERED|1|1|0|1|0|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
            "lines|pk_lines|1|1|CLUSTERED|1|1|0|1|0|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
            "orders|cx_orders|1|1|CLUSTERED|0|1|0|0|0|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
            "orders|pk_orders|2|2|NONCLUSTERED|1|1|0|1|0|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
            "orders|ix_orders_customer|3|2|NONCLUSTERED|0|1|0|0|0|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
            "people|pk_people|1|1|CLUSTERED|1|1|0|1|0|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
            "people|uq_people_name|2|2|NONCLUSTERED|1|1|0|0|1|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
            "people|uq_people_email|3|2|NONCLUSTERED|1|1|0|0|1|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
            "people|ix_people_last|4|2|NONCLUSTERED|0|1|0|0|0|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
            "tags|pk_tags|1|1|CLUSTERED|1|1|0|1|0|0|0|0|0|0|1|1|0|NULL|NULL|0|0|0",
        ]
    );
    assert_eq!(
        rows(&s, INDEX_COLUMNS),
        [
            "events|ux_events_kind|1|kind|2|1|0|1|0|0",
            "events|ux_events_kind|2|id|1|2|0|0|0|0",
            "events|ux_events_kind|3|payload|4|0|0|0|1|0",
            "events|fx_events_seen|1|seen|3|1|0|0|0|0",
            "events|ix_events_kind|1|kind|2|1|0|0|0|0",
            "events|ix_events_kind|2|seen|3|0|0|0|1|0",
            "events|ix_events_kind|3|payload|4|0|0|0|1|0",
            "items|pk_items|1|id|1|1|0|0|0|0",
            "lines|pk_lines|1|order_id|1|1|0|0|0|0",
            "lines|pk_lines|2|line|2|2|0|0|0|0",
            "orders|cx_orders|1|placed|2|1|0|0|0|0",
            "orders|pk_orders|1|id|1|1|0|0|0|0",
            "orders|ix_orders_customer|1|customer|3|1|0|0|0|0",
            "people|pk_people|1|id|1|1|0|0|0|0",
            "people|uq_people_name|1|last|4|1|0|0|0|0",
            "people|uq_people_name|2|first|3|2|0|0|0|0",
            "people|uq_people_email|1|email|2|1|0|0|0|0",
            "people|ix_people_last|1|last|4|1|0|0|0|0",
            "tags|pk_tags|1|code|1|1|0|0|0|0",
        ]
    );
}

/// The owner's repro: both queries failed with 40515 for a table with an
/// integer primary key.
#[test]
fn object_id_filters_work_through_the_batch_path() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, SCHEMA[0]);
    for sql in [
        "SELECT * FROM sys.indexes WHERE object_id=OBJECT_ID(N'items')",
        "SELECT * FROM sys.index_columns WHERE object_id=OBJECT_ID(N'items')",
    ] {
        let (response, ok) = s.batch_response(sql, &Default::default(), false, None);
        assert!(ok && !response.contains(&0xAA), "{sql}: {response:?}");
    }
    let (response, _) = s.batch_response(
        "SELECT name FROM sys.indexes WHERE object_id=OBJECT_ID(N'items')",
        &Default::default(),
        false,
        None,
    );
    let name: Vec<u8> = "pk_items"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    assert!(response.windows(name.len()).any(|w| w == name));
    assert_eq!(
        rows(
            &s,
            "SELECT name,index_id,type_desc,is_primary_key FROM sys.indexes WHERE object_id=(SELECT object_id FROM sys.objects WHERE name='items')"
        ),
        ["pk_items|1|CLUSTERED|1"]
    );
    assert_eq!(
        rows(
            &s,
            "SELECT index_id,index_column_id,column_id,key_ordinal FROM sys.index_columns WHERE object_id=(SELECT object_id FROM sys.objects WHERE name='items')"
        ),
        ["1|1|1|1"]
    );
}

#[test]
fn constraints_of_one_statement_are_numbered_in_reverse_declaration_order() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    for sql in [
        "CREATE TABLE t1 (a int CONSTRAINT u1 UNIQUE, b int CONSTRAINT u2 UNIQUE, c int CONSTRAINT u3 UNIQUE)",
        "CREATE TABLE t2 (a int, b int, c int, CONSTRAINT v1 UNIQUE(a), CONSTRAINT v2 UNIQUE(b), CONSTRAINT v3 UNIQUE(c))",
        "CREATE TABLE t3 (a int CONSTRAINT w1 UNIQUE, b int, c int CONSTRAINT w2 UNIQUE, CONSTRAINT w3 UNIQUE(b), CONSTRAINT w4 UNIQUE(a,b))",
        "CREATE TABLE t4 (a int NOT NULL, b int, CONSTRAINT x1 UNIQUE(b), CONSTRAINT x2 PRIMARY KEY (a))",
        "CREATE TABLE t9 (a int NOT NULL, b int NOT NULL, c int, CONSTRAINT pk_t9 PRIMARY KEY (b, a))",
        "CREATE TABLE t10 (a int NOT NULL, b int NOT NULL, c int)",
        "CREATE CLUSTERED INDEX cx_t10 ON t10(c, a)",
        "CREATE UNIQUE INDEX ux_t10 ON t10(b DESC, a)",
    ] {
        run(&mut s, sql);
    }
    assert_eq!(
        rows(
            &s,
            "SELECT o.name,i.name,i.index_id FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id
             WHERE rtrim(o.type)='U' AND o.name IN ('t1','t2','t3','t4') ORDER BY o.name,i.index_id"
        ),
        [
            "t1|NULL|0",
            "t1|u3|2",
            "t1|u2|3",
            "t1|u1|4",
            "t2|NULL|0",
            "t2|v3|2",
            "t2|v2|3",
            "t2|v1|4",
            "t3|NULL|0",
            "t3|w4|2",
            "t3|w3|3",
            "t3|w2|4",
            "t3|w1|5",
            "t4|x2|1",
            "t4|x1|2",
        ]
    );
    // A clustered index lists its columns in column order.
    assert_eq!(
        rows(
            &s,
            "SELECT o.name,ic.index_id,ic.index_column_id,ic.column_id,ic.key_ordinal,ic.is_descending_key
             FROM sys.index_columns ic JOIN sys.objects o ON o.object_id=ic.object_id
             WHERE o.name IN ('t9','t10') ORDER BY o.name,ic.index_id,ic.index_column_id"
        ),
        [
            "t10|1|1|1|2|0",
            "t10|1|2|3|1|0",
            "t10|2|1|2|1|1",
            "t10|2|2|1|2|0",
            "t9|1|1|1|2|0",
            "t9|1|2|2|1|0",
        ]
    );
}

/// Index IDs follow the stored catalog order: a dropped index's ID is reused,
/// and a clustered index created after nonclustered ones does not shift them.
#[test]
fn index_ids_reuse_gaps_around_constraints_and_clustered_indexes() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    for sql in [
        "CREATE TABLE f (id int NOT NULL CONSTRAINT pk_f PRIMARY KEY, v int, w int, x int)",
        "CREATE INDEX i1 ON f(v)",
        "CREATE INDEX i2 ON f(w)",
        "CREATE INDEX i3 ON f(x)",
        "DROP INDEX i2 ON f",
        "CREATE INDEX i4 ON f(w)",
        "CREATE TABLE g (id int NOT NULL CONSTRAINT pk_g PRIMARY KEY NONCLUSTERED, v int, w int)",
        "CREATE INDEX j1 ON g(v)",
        "CREATE CLUSTERED INDEX cx_g ON g(w)",
        "CREATE INDEX j2 ON g(w, v)",
    ] {
        run(&mut s, sql);
    }
    // SQL Server: the same rows.
    assert_eq!(
        rows(
            &s,
            "SELECT o.name,i.name,i.index_id,i.type_desc FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id
             WHERE rtrim(o.type)='U' ORDER BY o.name,i.index_id"
        ),
        [
            "f|pk_f|1|CLUSTERED",
            "f|i1|2|NONCLUSTERED",
            "f|i4|3|NONCLUSTERED",
            "f|i3|4|NONCLUSTERED",
            "g|cx_g|1|CLUSTERED",
            "g|pk_g|2|NONCLUSTERED",
            "g|j1|3|NONCLUSTERED",
            "g|j2|4|NONCLUSTERED",
        ]
    );
}

#[test]
fn same_named_tables_in_two_databases_keep_their_own_rows() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, SCHEMA[0]);
    run(&mut s, "CREATE INDEX ix_items_name ON items(name)");
    run(&mut s, "CREATE DATABASE other");
    run(&mut s, "USE other");
    run(&mut s, SCHEMA[0]);
    run(&mut s, "CREATE INDEX ix_items_name ON items(name)");
    run(&mut s, "CREATE INDEX ix_items_both ON items(name, id)");
    let query = "SELECT i.name,i.index_id,i.type_desc FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id
        WHERE o.name='items' ORDER BY i.index_id";
    assert_eq!(
        rows(&s, query),
        [
            "pk_items|1|CLUSTERED",
            "ix_items_name|2|NONCLUSTERED",
            "ix_items_both|3|NONCLUSTERED"
        ]
    );
    assert_eq!(msduck::index_catalog::acquire(&s.db).unwrap().len(), 2);
    let columns = "SELECT count(*) FROM sys.index_columns ic JOIN sys.objects o ON o.object_id=ic.object_id WHERE o.name='items'";
    assert_eq!(rows(&s, columns), ["4"]);
    run(&mut s, "DROP INDEX ix_items_both ON items");
    run(&mut s, "USE master");
    assert_eq!(
        rows(&s, query),
        ["pk_items|1|CLUSTERED", "ix_items_name|2|NONCLUSTERED"]
    );
    assert_eq!(msduck::index_catalog::acquire(&s.db).unwrap().len(), 1);
    assert_eq!(rows(&s, columns), ["2"]);
    // Dropping the other database's table leaves this one's index alone.
    run(&mut s, "USE other");
    run(&mut s, "DROP TABLE items");
    run(&mut s, "USE master");
    assert_eq!(msduck::index_catalog::acquire(&s.db).unwrap().len(), 1);
}

/// Differences from SQL Server that remain: the CLUSTERED and NONCLUSTERED
/// keywords of PRIMARY KEY and UNIQUE constraints, and the key order of a
/// constraint, are not recorded, so SQL Server's defaults apply.
#[test]
fn unrecorded_constraint_options_use_sql_server_defaults() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    for sql in [
        "CREATE TABLE a (k nvarchar(450) NOT NULL CONSTRAINT pk_a PRIMARY KEY NONCLUSTERED, v int)",
        "CREATE TABLE b (id int NOT NULL, code int NULL, CONSTRAINT uq_b_code UNIQUE CLUSTERED (code))",
        "CREATE TABLE c (x int NOT NULL, y int NOT NULL, CONSTRAINT pk_c PRIMARY KEY (y, x DESC))",
    ] {
        run(&mut s, sql);
    }
    // SQL Server:
    //   a|NULL|0|HEAP, a|pk_a|2|NONCLUSTERED
    //   b|uq_b_code|1|CLUSTERED
    //   c|pk_c|1|CLUSTERED
    assert_eq!(
        rows(
            &s,
            "SELECT o.name,i.name,i.index_id,i.type_desc FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id
             WHERE rtrim(o.type)='U' ORDER BY o.name,i.index_id"
        ),
        [
            "a|pk_a|1|CLUSTERED",
            "b|NULL|0|HEAP",
            "b|uq_b_code|2|NONCLUSTERED",
            "c|pk_c|1|CLUSTERED",
        ]
    );
    // SQL Server: x (column 1) is key 2 and descending.
    assert_eq!(
        rows(
            &s,
            "SELECT ic.index_column_id,ic.column_id,ic.key_ordinal,ic.is_descending_key FROM sys.index_columns ic
             JOIN sys.objects o ON o.object_id=ic.object_id WHERE o.name='c' ORDER BY ic.index_column_id"
        ),
        ["1|1|2|0", "2|2|1|0"]
    );
}

#[test]
fn filter_definitions_use_sql_server_normal_form() {
    use msduck::index_catalog::filter_definition;
    for (recorded, expected) in [
        ("W > 5", "([W]>(5))"),
        ("w IN (1, 2, 3)", "([w] IN ((1), (2), (3)))"),
        ("s = N'abc' AND v <> 'x''y'", "([s]=N'abc' AND [v]<>'x''y')"),
        (
            "w >= -5 AND d < 1.50 AND [w] <> 7",
            "([w]>=(-5) AND [d]<(1.50) AND [w]<>(7))",
        ),
        ("w IS NULL", "([w] IS NULL)"),
        (
            "w <= 0 AND w IS NOT NULL AND s IS NOT NULL",
            "([w]<=(0) AND [w] IS NOT NULL AND [s] IS NOT NULL)",
        ),
        ("(w > 1) AND (w < 9)", "([w]>(1) AND [w]<(9))"),
        (
            "s IN (N'a', N'b') AND w <> -2 AND \"w\" > 1",
            "(([s] IN (N'a', N'b')) AND [w]<>(-2) AND [w]>(1))",
        ),
        ("s = 'plain' AND w = +3", "([s]='plain' AND [w]=(3))"),
    ] {
        assert_eq!(filter_definition(recorded), expected, "{recorded}");
    }
}

#[test]
fn rows_survive_alter_table_and_restart() {
    let directory =
        std::env::temp_dir().join(format!("msduck-gaps-index-catalog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("catalog.duckdb");
    let path = path.to_str().unwrap();
    let before = {
        let server = Server::open(path).unwrap();
        let mut s = session(&server);
        for sql in SCHEMA {
            run(&mut s, sql);
        }
        run(&mut s, "CREATE TABLE t7 (a int NOT NULL, b int, c int)");
        run(
            &mut s,
            "ALTER TABLE t7 ADD CONSTRAINT q1 UNIQUE(b), CONSTRAINT q2 UNIQUE(c), CONSTRAINT q3 PRIMARY KEY (a)",
        );
        // SQL Server: q3 1, q2 2, q1 3.
        assert_eq!(
            rows(
                &s,
                "SELECT i.name,i.index_id,i.type_desc FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id
                 WHERE o.name='t7' ORDER BY i.index_id"
            ),
            ["q3|1|CLUSTERED", "q2|2|NONCLUSTERED", "q1|3|NONCLUSTERED"]
        );
        let before = (rows(&s, INDEXES), rows(&s, INDEX_COLUMNS));
        // The keys feature rebuilds the table's indexes around the change.
        run(&mut s, "ALTER TABLE events ADD extra int NULL");
        assert_eq!((rows(&s, INDEXES), rows(&s, INDEX_COLUMNS)), before);
        before
    };
    let server = Server::open(path).unwrap();
    let s = session(&server);
    assert_eq!((rows(&s, INDEXES), rows(&s, INDEX_COLUMNS)), before);
    drop(s);
    drop(server);
    let _ = std::fs::remove_dir_all(&directory);
}

/// IDs are kept once assigned: constraints added or dropped later do not
/// renumber other indexes, and a primary key that turns out nonclustered
/// takes the ID SQL Server gave it.
#[test]
fn index_ids_are_kept_across_later_constraint_changes() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    for sql in [
        "CREATE TABLE t (a int CONSTRAINT u1 UNIQUE, b int)",
        "ALTER TABLE t ADD CONSTRAINT u2 UNIQUE(b)",
        "CREATE TABLE t2 (id int NOT NULL, v int)",
        "CREATE INDEX ix ON t2(v)",
        "ALTER TABLE t2 ADD CONSTRAINT uq2 UNIQUE(id)",
        "CREATE TABLE t3 (a int CONSTRAINT u31 UNIQUE, b int)",
        "CREATE INDEX ix3 ON t3(b)",
        "ALTER TABLE t3 DROP CONSTRAINT u31",
        "CREATE TABLE t4 (id int NOT NULL CONSTRAINT pk_t4 PRIMARY KEY NONCLUSTERED, a int CONSTRAINT u41 UNIQUE, b int)",
        "CREATE INDEX ix4 ON t4(b)",
        "CREATE CLUSTERED INDEX cx4 ON t4(a)",
    ] {
        run(&mut s, sql);
    }
    // SQL Server: the same rows.
    assert_eq!(
        rows(
            &s,
            "SELECT o.name,i.name,i.index_id,i.type_desc FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id
             WHERE rtrim(o.type)='U' ORDER BY o.name,i.index_id"
        ),
        [
            "t|NULL|0|HEAP",
            "t|u1|2|NONCLUSTERED",
            "t|u2|3|NONCLUSTERED",
            "t2|NULL|0|HEAP",
            "t2|ix|2|NONCLUSTERED",
            "t2|uq2|3|NONCLUSTERED",
            "t3|NULL|0|HEAP",
            "t3|ix3|3|NONCLUSTERED",
            "t4|cx4|1|CLUSTERED",
            "t4|u41|2|NONCLUSTERED",
            "t4|pk_t4|3|NONCLUSTERED",
            "t4|ix4|4|NONCLUSTERED",
        ]
    );
}

/// A new database's first transaction reads the constraint record; reading
/// the views must not abort it.
#[test]
fn first_transaction_of_a_new_database_reads_constraints() {
    let server = Server::open(":memory:").unwrap();
    let mut s = session(&server);
    run(&mut s, "CREATE DATABASE fresh");
    for database in ["master", "fresh"] {
        run(&mut s, &format!("USE {database}"));
        run(
            &mut s,
            "BEGIN TRANSACTION;
             CREATE TABLE p (id int NOT NULL CONSTRAINT pk_p PRIMARY KEY, v int, w int);
             CREATE INDEX ixd ON p(v DESC, w) INCLUDE (id);
             ALTER TABLE p ADD extra int NULL;
             SELECT * FROM sys.indexes;
             COMMIT",
        );
        assert_eq!(
            rows(
                &s,
                "SELECT i.name,i.index_id,i.type_desc FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id
                 WHERE o.name='p' ORDER BY i.index_id"
            ),
            ["pk_p|1|CLUSTERED", "ixd|2|NONCLUSTERED"],
            "{database}"
        );
        assert_eq!(
            rows(
                &s,
                "SELECT ic.index_id,ic.index_column_id,ic.column_id,ic.key_ordinal,ic.is_descending_key,ic.is_included_column
                 FROM sys.index_columns ic JOIN sys.objects o ON o.object_id=ic.object_id
                 WHERE o.name='p' ORDER BY ic.index_id,ic.index_column_id"
            ),
            ["1|1|1|1|0|0", "2|1|2|1|1|0", "2|2|3|2|0|0", "2|3|1|0|0|1"],
            "{database}"
        );
    }
}
