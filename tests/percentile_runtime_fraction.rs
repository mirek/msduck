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

#[test]
fn invalid_character_keeps_error_metadata() {
    let server = Server::open(":memory:").unwrap();
    let mut session = Session::new(server.connection().unwrap()).unwrap();
    let sql = "SELECT PERCENTILE_CONT(@p) WITHIN GROUP(ORDER BY n) OVER() AS p FROM(VALUES(1),(2),(3),(4))s(n)";
    session
        .validate_prepared_sql(
            sql,
            &[(
                "@p".into(),
                msduck_core::types::Type::Character(
                    msduck_core::character::CharacterType::new(
                        msduck_core::character::Family::Nvarchar,
                        msduck_core::character::Length::Bounded(16),
                    )
                    .unwrap(),
                ),
            )],
        )
        .unwrap();
    let parameters = HashMap::from([(
        "@p".into(),
        Parameter {
            value: Value::Text("abc".into()),
            data_type: msduck_core::types::Type::Character(
                msduck_core::character::CharacterType::new(
                    msduck_core::character::Family::Nvarchar,
                    msduck_core::character::Length::Bounded(16),
                )
                .unwrap(),
            ),
        },
    )]);
    let (bytes, ok) = session.batch_response(sql, &parameters, false, None);
    assert!(!ok);
    assert_eq!(bytes.first(), Some(&0x81), "{bytes:?}");
}
