//! Native integration coverage for SQL_VARIANT plans.

#[cfg(test)]
mod tests {
    use sqlparser::{dialect::GenericDialect, parser::Parser};
    #[test]
    fn offset_distinct_preserves_payload_and_evaluates_once() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE SEQUENCE offset_distinct").unwrap();
        let mut statement = Parser::parse_sql(&GenericDialect {}, "SELECT DISTINCT __msduck_datetimeoffset_cast_7(CASE WHEN nextval('offset_distinct')%3=0 THEN NULL WHEN i%2=0 THEN '2024-01-01T12:00:00.1234567+05:30' ELSE '2024-01-01T01:30:00.1234567-05:00' END) AS d FROM range(6000) r(i)").unwrap().remove(0);
        crate::aggregate_columns::annotate(&db, &mut statement, &Default::default()).unwrap();
        db.execute_batch(&format!("CREATE TABLE distinct_offsets AS {statement}"))
            .unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*),count(d) FROM distinct_offsets",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            )
            .unwrap(),
            (2, 1)
        );
        let (ticks,offset) = db.query_row("SELECT d.__msduck_datetimeoffset_7,d.__msduck_offset_minutes FROM distinct_offsets WHERE d IS NOT NULL", [], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i16>(1)?))).unwrap();
        assert_eq!(
            ticks,
            crate::datetime2::DateTime2::parse_iso("2024-01-01T06:30:00.1234567")
                .unwrap()
                .ticks()
        );
        assert!([330, -300].contains(&offset));
        assert_eq!(
            db.query_row("SELECT currval('offset_distinct')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn offset_sets_compare_utc_once_across_chunks() {
        for op in ["UNION", "UNION ALL", "INTERSECT", "EXCEPT"] {
            let server = crate::server::Server::open(":memory:").unwrap();
            let db = server.connection().unwrap();
            db.execute_batch("CREATE SEQUENCE offset_left; CREATE SEQUENCE offset_right")
                .unwrap();
            let sql = format!(
                "SELECT __msduck_datetimeoffset_cast_3(CASE WHEN nextval('offset_left')%3=0 THEN NULL ELSE '2024-01-01T12:00:00.123+05:30' END) AS d FROM range(6000) {op} SELECT __msduck_datetimeoffset_cast_7(CASE WHEN nextval('offset_right')%3=0 THEN NULL ELSE '2024-01-01T01:30:00.123-05:00' END) FROM range(6000)"
            );
            let mut statement = Parser::parse_sql(&GenericDialect {}, &sql)
                .unwrap()
                .remove(0);
            crate::aggregate_columns::annotate(&db, &mut statement, &Default::default()).unwrap();
            let sql = format!("SELECT count(*),count(d) FROM ({statement}) s");
            assert_eq!(
                db.query_row(&sql, [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
                    .unwrap(),
                match op {
                    "UNION ALL" => (12000, 8000),
                    "EXCEPT" => (0, 0),
                    _ => (2, 1),
                },
                "{op}"
            );
            for sequence in ["offset_left", "offset_right"] {
                assert_eq!(
                    db.query_row("SELECT currval(?)", [sequence], |r| r.get::<_, i64>(0))
                        .unwrap(),
                    6000,
                    "{op}"
                );
            }
        }
    }

    #[test]
    fn membership_materializes_each_branch_once_and_matches_nulls() {
        for op in ["INTERSECT", "EXCEPT"] {
            let server = crate::server::Server::open(":memory:").unwrap();
            let db = server.connection().unwrap();
            db.execute_batch("CREATE SEQUENCE member_left; CREATE SEQUENCE member_right")
                .unwrap();
            let sql = format!(
                "SELECT __msduck_pack_integer_variant(CAST(CASE WHEN nextval('member_left')%3=0 THEN NULL ELSE CAST(i%2+1 AS INT) END AS SMALLINT)) AS v FROM range(6000) r(i) {op} SELECT CAST(CASE WHEN nextval('member_right')%2=0 THEN NULL ELSE 1 END AS BIGINT) FROM range(6000)"
            );
            let mut statement = Parser::parse_sql(&GenericDialect {}, &sql)
                .unwrap()
                .remove(0);
            crate::aggregate_columns::annotate(&db, &mut statement, &Default::default()).unwrap();
            let sql = format!(
                "SELECT count(*),count(v),sum(v.__msduck_variant_integer) FROM ({statement}) q"
            );
            assert_eq!(
                db.query_row(&sql, [], |r| Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?
                )))
                .unwrap(),
                if op == "INTERSECT" {
                    (2, 1, 1)
                } else {
                    (1, 1, 2)
                }
            );
            for name in ["member_left", "member_right"] {
                assert_eq!(
                    db.query_row("SELECT currval(?)", [name], |r| r.get::<_, i64>(0))
                        .unwrap(),
                    6000
                );
            }
        }
    }
    #[test]
    fn union_deduplicates_values_with_single_evaluation_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE SEQUENCE union_left; CREATE SEQUENCE union_right")
            .unwrap();
        let mut statement = Parser::parse_sql(&GenericDialect {}, "SELECT __msduck_pack_integer_variant(CAST(CASE WHEN nextval('union_left')%3=0 THEN NULL ELSE 1 END AS SMALLINT)) AS v FROM range(6000) UNION SELECT CAST(CASE WHEN nextval('union_right')%3=0 THEN NULL ELSE 1 END AS BIGINT) FROM range(6000)").unwrap().remove(0);
        crate::aggregate_columns::annotate(&db, &mut statement, &Default::default()).unwrap();
        let sql = format!(
            "SELECT count(*),count(v),sum(v.__msduck_variant_integer) FROM ({statement}) q"
        );
        assert_eq!(
            db.query_row(&sql, [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?
            )))
            .unwrap(),
            (2, 1, 1)
        );
        for name in ["union_left", "union_right"] {
            assert_eq!(
                db.query_row("SELECT currval(?)", [name], |r| r.get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }
    #[test]
    fn union_all_keeps_nulls_duplicates_and_single_evaluation() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE SEQUENCE variant_left; CREATE SEQUENCE variant_right")
            .unwrap();
        let mut statement = Parser::parse_sql(&GenericDialect {}, "SELECT __msduck_pack_integer_variant(CASE WHEN nextval('variant_left')%2=0 THEN NULL ELSE 42 END) AS v FROM range(6000) UNION ALL SELECT nextval('variant_right') FROM range(6000)").unwrap().remove(0);
        crate::aggregate_columns::annotate(&db, &mut statement, &Default::default()).unwrap();
        let sql = format!(
            "SELECT count(*),count(v),sum(v.__msduck_variant_integer) FROM ({statement}) q"
        );
        assert_eq!(
            db.query_row(&sql, [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?
            )))
            .unwrap(),
            (12000, 9000, 18129000)
        );
        for name in ["variant_left", "variant_right"] {
            assert_eq!(
                db.query_row("SELECT currval(?)", [name], |r| r.get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }
}
