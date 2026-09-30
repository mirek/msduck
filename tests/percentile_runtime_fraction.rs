use msduck::{
    engine::{Parameter, Session},
    server::Server,
};
use msduck_core::{types::Type, value::Value};
use std::collections::HashMap;

#[test]
fn declaration_only_preparation_and_runtime_empty_null_distinction() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    for function in ["CONT", "DISC"] {
        for empty in [false, true] {
            let sql = format!(
                "SELECT PERCENTILE_{function}(@p) WITHIN GROUP(ORDER BY n) OVER() AS p FROM(VALUES(CAST(NULL AS INT)))s(n){}",
                if empty { " WHERE 1=0" } else { "" }
            );
            session
                .validate_prepared_sql(&sql, &[("@p".into(), Type::Float)])
                .unwrap();
            let parameters = HashMap::from([(
                "@p".into(),
                Parameter {
                    value: Value::Null,
                    data_type: Type::Float,
                },
            )]);
            let (_, ok) = session.batch_response(&sql, &parameters, false, None);
            assert_eq!(ok, empty, "{sql}");
            let valid = HashMap::from([(
                "@p".into(),
                Parameter {
                    value: Value::Double(0.5),
                    data_type: Type::Float,
                },
            )]);
            assert!(
                session.batch_response(&sql, &valid, false, None).1,
                "recovery {sql}"
            );
        }
    }
}
