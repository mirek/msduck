use msduck::{engine::Session, server::Server};
use msduck_core::types::Type;

#[test]
fn prepared_initializers_and_predicates_bind_session_rand_without_execution() {
    let server = Server::open(":memory:").unwrap();
    let session = Session::new(server.connection().unwrap()).unwrap();
    for sql in [
        "DECLARE @p FLOAT=RAND(@seed); SELECT @p AS r",
        "DECLARE @p FLOAT; SET @p=RAND(@seed); SELECT @p AS r",
        "IF RAND(@seed)>0 SELECT 1 AS r",
        "WHILE RAND(@seed)<0 SELECT 1 AS r",
    ] {
        session
            .validate_prepared_sql(sql, &[("@seed".into(), Type::Int)])
            .unwrap();
    }
}
