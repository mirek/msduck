//! Native integration coverage for SQL_VARIANT plans.

#[cfg(test)]
mod tests {
    use sqlparser::{dialect::GenericDialect, parser::Parser};
    #[test]
    fn distinct_uses_values_once_and_merges_nulls_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE SEQUENCE distinct_calls").unwrap();
        let mut statement = Parser::parse_sql(&GenericDialect {}, "SELECT DISTINCT __msduck_pack_integer_variant(CASE WHEN nextval('distinct_calls')%3=0 THEN NULL ELSE __msduck_identity_variant(CASE WHEN i%2=0 THEN 48 ELSE 127 END,1) END) AS v FROM range(6000) r(i)").unwrap().remove(0);
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
        assert_eq!(
            db.query_row("SELECT currval('distinct_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
    #[test]
    fn ordering_uses_each_projected_volatile_value_once() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE SEQUENCE order_calls").unwrap();
        let mut statement = Parser::parse_sql(&GenericDialect {}, "SELECT __msduck_pack_integer_variant(nextval('order_calls')) AS x FROM range(6000) ORDER BY x DESC").unwrap().remove(0);
        crate::aggregate_columns::annotate(&db, &mut statement, &Default::default()).unwrap();
        let sql = format!("SELECT x.__msduck_variant_integer FROM ({statement}) AS ordered");
        let mut prepared = db.prepare(&sql).unwrap();
        let values = prepared
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(values, (1..=6000).rev().collect::<Vec<_>>());
        assert_eq!(
            db.query_row("SELECT currval('order_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
}
