//! Checked, nonexecuting integer precision expressions for temporal constructors.

#[cfg(test)]
mod tests {
    #[test]
    fn rejected_precision_never_executes_volatile_values() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE precision_calls START 1")
            .unwrap();
        for sql in [
            "SELECT TIMEFROMPARTS(0,0,0,0,nextval('precision_calls'))",
            "SELECT DATETIME2FROMPARTS(2024,1,1,0,0,0,0,nextval('precision_calls'))",
        ] {
            assert!(
                !session
                    .batch_response(sql, &Default::default(), false, None)
                    .1
            );
        }
        assert_eq!(
            session
                .db
                .query_row("SELECT nextval('precision_calls')", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
