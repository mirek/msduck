#[cfg(test)]
mod tests {
    use sqlparser::ast::*;
    use std::ops::ControlFlow;
    #[test]
    fn grouping_combines_offset_payloads_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        assert!(session.batch_response("SELECT CAST(CASE WHEN i%4=0 THEN NULL WHEN i%2=0 THEN '2024-01-01T12:00:00.123+05:30' ELSE '2024-01-01T06:30:00.123Z' END AS DATETIMEOFFSET(3)) AS d INTO dbo.offset_source FROM range(6000) r(i)",&Default::default(),false,None).1);
        assert!(session.batch_response("SELECT d,COUNT(*) AS n INTO dbo.offset_groups FROM dbo.offset_source GROUP BY d",&Default::default(),false,None).1);
        let stats = session
            .db
            .query_row(
                "SELECT count(*),count(d),CAST(sum(n) AS BIGINT),min(n),max(n) FROM dbo.offset_groups",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(stats, (2, 1, 6000, 1500, 4500));
        let (ticks,offset):(i64,i16)=session.db.query_row("SELECT d.__msduck_datetimeoffset_3,d.__msduck_offset_minutes FROM dbo.offset_groups WHERE d IS NOT NULL",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(
            ticks,
            crate::datetime2::DateTime2::parse_iso("2024-01-01T06:30:00.123")
                .unwrap()
                .ticks()
        );
        assert!([0, 330].contains(&offset));
    }
    #[test]
    fn grouping_combines_numeric_tags_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        let value = "__msduck_pack_integer_variant(__msduck_identity_variant(CASE WHEN i%3=0 THEN 52 ELSE 127 END,CASE WHEN i%4=0 THEN NULL ELSE i%4 END))";
        let mut statement = sqlparser::parser::Parser::parse_sql(
            &crate::dialect::ServerDialect,
            &format!("SELECT {value} AS v,COUNT(*) AS n FROM range(6000) r(i) GROUP BY {value}"),
        )
        .unwrap()
        .remove(0);
        crate::aggregate_columns::annotate(&db, &mut statement, &Default::default()).unwrap();
        struct Lower;
        impl VisitorMut for Lower {
            type Break = String;
            fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
                crate::variant_pack::lower(expr);
                match crate::aggregate::mark(expr, &Default::default()) {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(error) => ControlFlow::Break(error),
                }
            }
            fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
                crate::variant_pack::canonicalize(expr);
                crate::variant_results::lower(expr);
                crate::variant_compare::lower(expr);
                ControlFlow::Continue(())
            }
        }
        assert!(VisitMut::visit(&mut statement, &mut Lower).is_continue());
        let sql = format!(
            "SELECT count(*),count(v),sum(n),min(n),max(n),sum(v.__msduck_variant_integer) FROM ({statement}) q"
        );
        assert_eq!(
            db.query_row(&sql, [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?
            )))
            .unwrap(),
            (4, 3, 6000, 1500, 1500, 6)
        );
    }
}
