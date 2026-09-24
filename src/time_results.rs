//! Infer TIME scales for exact conditional result conversion.

pub use msduck_sql::expression_metadata::temporal::time_scale as scale;

#[cfg(test)]
mod tests {
    #[test]
    fn set_source_window_defaults_round_once_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE time_set_defaults START 1")
            .unwrap();
        let sql = "WITH q AS (SELECT i,CAST(NULL AS TIME(2)) t FROM range(3000) r(i) UNION ALL SELECT i+3000,CAST(NULL AS TIME(4)) FROM range(3000) r(i)) SELECT LAG(t,6001,printf('01:02:%02d.1234567',nextval('time_set_defaults')%60)) OVER(ORDER BY i) t INTO dbo.time_set_chunks FROM q";
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1
        );
        let counts: (i64, i64) = session.db.query_row(
            "SELECT count(*),count(*) FILTER (WHERE epoch_ns(t)%1000000000=123500000) FROM dbo.time_set_chunks",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(counts, (6000, 6000));
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('time_set_defaults')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn time_window_defaults_convert_before_storage_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE time_window_defaults START 1")
            .unwrap();
        let sql = "SELECT LAG(CAST('12:00:00' AS TIME(2)),6001,printf('01:02:%02d.1249',nextval('time_window_defaults')%60)) OVER(ORDER BY i) t INTO dbo.time_window_chunks FROM range(6000) r(i)";
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1
        );
        let wrong:i64=session.db.query_row("SELECT count(*) FROM dbo.time_window_chunks WHERE epoch_ns(t)%1000000000<>120000000",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        assert_eq!(
            session
                .db
                .query_row("SELECT currval('time_window_defaults')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn case_converts_only_selected_time_branches_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE case_time; CREATE SEQUENCE case_text")
            .unwrap();
        let sql = "SELECT CASE WHEN i%3=0 THEN NULL WHEN i%3=1 THEN CAST(printf('12:00:%02d.1249',nextval('case_time')%60) AS TIME(2)) ELSE printf('12:00:%02d.1249',nextval('case_text')%60) END AS t INTO dbo.time_case_chunks FROM range(6000) r(i)";
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1
        );
        assert_eq!(session.db.query_row("SELECT count(*),count(t),count(*) FILTER (WHERE epoch_ns(t)%1000000000=120000000) FROM dbo.time_case_chunks",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).unwrap(),(6000,4000,4000));
        for name in ["case_time", "case_text"] {
            assert_eq!(
                session
                    .db
                    .query_row("SELECT currval(?)", [name], |r| r.get::<_, i64>(0))
                    .unwrap(),
                2000
            );
        }
    }

    #[test]
    fn isnull_evaluates_needed_time_values_once_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE time_first; CREATE SEQUENCE time_replacement")
            .unwrap();
        let sql = "SELECT ISNULL(CAST(CASE WHEN i%2=0 THEN printf('12:00:%02d.1249',nextval('time_first')%60) ELSE NULL END AS TIME(2)),CAST(printf('12:00:%02d.1249',nextval('time_replacement')%60) AS TIME(7))) AS t INTO dbo.time_conditional FROM range(6000) r(i)";
        assert!(
            session
                .batch_response(sql, &Default::default(), false, None)
                .1
        );
        assert_eq!(session.db.query_row("SELECT count(*) FROM dbo.time_conditional WHERE epoch_ns(t)%1000000000=120000000",[],|r|r.get::<_,i64>(0)).unwrap(),6000);
        for name in ["time_first", "time_replacement"] {
            assert_eq!(
                session
                    .db
                    .query_row("SELECT currval(?)", [name], |r| r.get::<_, i64>(0))
                    .unwrap(),
                3000
            );
        }
    }
}
