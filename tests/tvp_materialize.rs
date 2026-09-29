use msduck::tvp_binding::{
    BoundTvp, Column, ColumnKind, OwnedCell, Supply, TableType,
    materialize::{Error, Transaction, with_relation},
};
use msduck_tds::tvp::Limits;

fn binding() -> BoundTvp {
    BoundTvp {
        table_type: TableType {
            schema: "dbo".into(),
            name: "TvpProbe".into(),
            user_type_id: 257,
            object_id: 1000,
            columns: vec![
                Column {
                    name: "id".into(),
                    nullable: true,
                    kind: ColumnKind::Int,
                },
                Column {
                    name: "label".into(),
                    nullable: true,
                    kind: ColumnKind::Nvarchar {
                        max_units: 10,
                        collation: "SQL_Latin1_General_CP1_CI_AS".into(),
                    },
                },
                Column {
                    name: "payload".into(),
                    nullable: true,
                    kind: ColumnKind::Varbinary { max_bytes: 10 },
                },
            ],
        },
        supply: Supply::Explicit,
        rows: Vec::new(),
    }
}
fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}
fn count(db: &duckdb::Connection, name: &str) -> i64 {
    db.query_row("SELECT count(*) FROM duckdb_tables() WHERE database_name='temp' AND schema_name='main' AND table_name=?", [name], |r|r.get(0)).unwrap()
}
fn verify(db: &duckdb::Connection, bound: &BoundTvp, name: &str, transaction: Transaction) {
    with_relation(
        db,
        bound,
        name,
        Limits::default(),
        transaction,
        |relation| {
            assert_eq!(relation.binding().supply, bound.supply);
            let mut statement = db.prepare(&format!(
                "SELECT id,label.__msduck_utf16le,payload FROM {} ORDER BY id NULLS FIRST",
                relation.sql_name()
            ))?;
            let mut rows = statement.query([])?;
            for expected in &bound.rows {
                let row = rows.next()?.expect("expected retained row");
                let id: Option<i32> = row.get(0)?;
                let unicode: Option<Vec<u8>> = row.get(1)?;
                let binary: Option<Vec<u8>> = row.get(2)?;
                assert_eq!(
                    id,
                    match &expected[0] {
                        OwnedCell::Null => None,
                        OwnedCell::Int(v) => Some(*v),
                        _ => panic!(),
                    }
                );
                assert_eq!(
                    unicode,
                    match &expected[1] {
                        OwnedCell::Null => None,
                        OwnedCell::Nvarchar(v) =>
                            Some(v.iter().flat_map(|u| u.to_le_bytes()).collect()),
                        _ => panic!(),
                    }
                );
                assert_eq!(
                    binary,
                    match &expected[2] {
                        OwnedCell::Null => None,
                        OwnedCell::Varbinary(v) => Some(v.clone()),
                        _ => panic!(),
                    }
                );
            }
            assert!(rows.next()?.is_none());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(count(db, name), 0);
}

#[test]
fn retained_cells_raw_surrogates_and_empty_supply_survive_materialization() {
    let db = duckdb::Connection::open_in_memory().unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../reference/tvp-wire.json")).unwrap();
    let mut replayed = 0;
    for run in fixture["runs"].as_array().unwrap() {
        for observation in run["observations"].as_array().unwrap() {
            if observation["request"]["tvp"]["columnCount"] == 65535 {
                continue;
            }
            let mut bound = binding();
            for row in observation["request"]["tvp"]["rows"].as_array().unwrap() {
                bound.rows.push(
                    row.as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                        .map(|(i, cell)| {
                            if cell["null"] == true {
                                return OwnedCell::Null;
                            }
                            let bytes = hex(cell["valueHex"].as_str().unwrap());
                            match i {
                                0 => OwnedCell::Int(i32::from_le_bytes(bytes.try_into().unwrap())),
                                1 => OwnedCell::Nvarchar(
                                    bytes
                                        .chunks_exact(2)
                                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                                        .collect(),
                                ),
                                2 => OwnedCell::Varbinary(bytes),
                                _ => panic!(),
                            }
                        })
                        .collect(),
                );
            }
            verify(&db, &bound, "__msduck_tvp_replay", Transaction::Owned);
            replayed += 1;
        }
    }
    assert_eq!(replayed, 12);
    let mut bound = binding();
    bound.rows = vec![
        vec![OwnedCell::Null, OwnedCell::Null, OwnedCell::Null],
        vec![
            OwnedCell::Int(1),
            OwnedCell::Nvarchar(vec![]),
            OwnedCell::Varbinary(vec![]),
        ],
        vec![
            OwnedCell::Int(2),
            OwnedCell::Nvarchar(vec![0xd800, 0, 0xdc01]),
            OwnedCell::Varbinary(vec![0, 255]),
        ],
    ];
    verify(&db, &bound, "__msduck_tvp_raw", Transaction::Owned);
    for supply in [Supply::Omitted, Supply::DefaultBit] {
        bound.rows.clear();
        bound.supply = supply;
        verify(&db, &bound, "__msduck_tvp_empty", Transaction::Owned);
    }
}

#[test]
fn caller_transactions_isolation_collision_and_quoted_names_are_preserved() {
    let db = duckdb::Connection::open_in_memory().unwrap();
    let other = db.try_clone().unwrap();
    db.execute_batch(
        "CREATE TABLE sentinel(n INT); BEGIN TRANSACTION; INSERT INTO sentinel VALUES(7)",
    )
    .unwrap();
    let mut bound = binding();
    bound.rows = vec![vec![
        OwnedCell::Int(1),
        OwnedCell::Null,
        OwnedCell::Varbinary(vec![]),
    ]];
    let name = "__msduck_tvp_\"; DROP TABLE sentinel; --";
    with_relation(
        &db,
        &bound,
        name,
        Limits::default(),
        Transaction::Caller,
        |relation| {
            assert_eq!(count(&db, name), 1);
            assert_eq!(count(&other, name), 0);
            let total: i64 = db.query_row(
                &format!("SELECT count(*) FROM {}", relation.sql_name()),
                [],
                |r| r.get(0),
            )?;
            assert_eq!(total, 1);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(count(&db, name), 0);
    let failed: Result<(), Error> = with_relation(
        &db,
        &bound,
        "__msduck_tvp_failure",
        Limits::default(),
        Transaction::Caller,
        |_| {
            db.execute_batch("INSERT INTO sentinel VALUES(8)")?;
            anyhow::bail!("callback failure")
        },
    );
    assert!(matches!(failed, Err(Error::Operation(_))));
    assert_eq!(count(&db, "__msduck_tvp_failure"), 0);
    db.execute_batch("ROLLBACK").unwrap();
    let total: i64 = db
        .query_row("SELECT count(*) FROM sentinel", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 0);
    db.execute_batch("CREATE TEMP TABLE __msduck_tvp_collision(n INT); INSERT INTO __msduck_tvp_collision VALUES(42)").unwrap();
    assert!(
        with_relation(
            &db,
            &bound,
            "__msduck_tvp_collision",
            Limits::default(),
            Transaction::Owned,
            |_| Ok(())
        )
        .is_err()
    );
    let n: i32 = db
        .query_row("SELECT n FROM temp.main.__msduck_tvp_collision", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 42);
    with_relation(
        &db,
        &bound,
        "__msduck_tvp_commit",
        Limits::default(),
        Transaction::Owned,
        |_| {
            db.execute_batch("INSERT INTO sentinel VALUES(9)")?;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        db.query_row("SELECT sum(n) FROM sentinel", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        9
    );
    let failed: Result<(), Error> = with_relation(
        &db,
        &bound,
        "__msduck_tvp_rollback",
        Limits::default(),
        Transaction::Owned,
        |_| {
            db.execute_batch("INSERT INTO sentinel VALUES(10)")?;
            anyhow::bail!("rollback callback")
        },
    );
    assert!(failed.is_err());
    assert_eq!(
        db.query_row("SELECT sum(n) FROM sentinel", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        9
    );
    bound.table_type.columns[0].name = "id\"; DROP TABLE sentinel; --".into();
    with_relation(
        &db,
        &bound,
        "__msduck_tvp_column",
        Limits::default(),
        Transaction::Owned,
        |r| {
            let n: i64 = db.query_row(
                &format!("SELECT count(*) FROM {}", r.sql_name()),
                [],
                |row| row.get(0),
            )?;
            assert_eq!(n, 1);
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn malformed_values_and_limits_reject_before_writes() {
    let db = duckdb::Connection::open_in_memory().unwrap();
    let good = binding();
    let mut invalids = Vec::new();
    let mut b = binding();
    b.rows = vec![vec![]];
    invalids.push(b);
    let mut b = binding();
    b.rows = vec![vec![
        OwnedCell::Int(1),
        OwnedCell::Nvarchar(vec![0; 11]),
        OwnedCell::Null,
    ]];
    invalids.push(b);
    let mut b = binding();
    b.rows = vec![vec![
        OwnedCell::Varbinary(vec![]),
        OwnedCell::Null,
        OwnedCell::Null,
    ]];
    invalids.push(b);
    let mut b = binding();
    b.table_type.columns[0].nullable = false;
    b.rows = vec![vec![OwnedCell::Null; 3]];
    invalids.push(b);
    let mut b = binding();
    b.table_type.columns[1].name = "ID".into();
    invalids.push(b);
    let mut b = binding();
    b.supply = Supply::Omitted;
    b.rows = vec![vec![OwnedCell::Null; 3]];
    invalids.push(b);
    let mut b = binding();
    b.table_type.columns[1].kind = ColumnKind::Nvarchar {
        max_units: 4001,
        collation: "x".into(),
    };
    invalids.push(b);
    for b in invalids {
        assert!(
            with_relation(
                &db,
                &b,
                "__msduck_tvp_invalid",
                Limits::default(),
                Transaction::Owned,
                |_| -> anyhow::Result<()> { panic!("invalid callback") }
            )
            .is_err()
        );
        assert_eq!(count(&db, "__msduck_tvp_invalid"), 0);
    }
    for limits in [
        Limits {
            max_columns: 2,
            ..Limits::default()
        },
        Limits {
            max_rows: 0,
            ..Limits::default()
        },
        Limits {
            max_cells: 2,
            ..Limits::default()
        },
        Limits {
            max_cell_bytes: 3,
            ..Limits::default()
        },
        Limits {
            max_input_bytes: 3,
            ..Limits::default()
        },
    ] {
        let mut b = binding();
        b.rows = vec![vec![OwnedCell::Int(1), OwnedCell::Null, OwnedCell::Null]];
        assert!(matches!(
            with_relation(
                &db,
                &b,
                "__msduck_tvp_limit",
                limits,
                Transaction::Owned,
                |_| Ok(())
            ),
            Err(Error::Limit(_))
        ));
        assert_eq!(count(&db, "__msduck_tvp_limit"), 0);
    }
    assert!(
        with_relation(
            &db,
            &good,
            "ordinary",
            Limits::default(),
            Transaction::Owned,
            |_| Ok(())
        )
        .is_err()
    );
}
