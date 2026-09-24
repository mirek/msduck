use msduck_core::{types::Type, value::Value};
use msduck_sql::{batch, parameter::Parameter, preflight};
use std::collections::HashMap;

#[test]
fn explicit_bindings_and_skipped_declarations_do_not_evaluate_or_mutate_inputs() {
    let parameters = HashMap::from([(
        "@input".into(),
        Parameter {
            value: Value::Int(7),
            data_type: Type::Int,
        },
    )]);
    let statements = batch::parse(
        "IF 1=0 BEGIN DECLARE @x INT = 1/0; END; \
         DECLARE @y INT = @input; WHILE 1=0 BEGIN CONTINUE; END; SELECT @X, @y;",
    )
    .unwrap();
    let original = statements.clone();
    for _ in 0..2 {
        let bindings = preflight::variables(&statements, &parameters).unwrap();
        assert_eq!(bindings.len(), 3);
        assert_eq!(bindings["@input"].value, Value::Int(7));
        for name in ["@x", "@y"] {
            assert_eq!(bindings[name].value, Value::Null);
            assert_eq!(bindings[name].data_type, Type::Int);
        }
    }
    assert_eq!(statements, original);
    assert_eq!(parameters.len(), 1);
    assert_eq!(parameters["@input"].value, Value::Int(7));
}

#[test]
fn traversal_preserves_first_errors_in_unexecuted_branches_and_ddl() {
    for (sql, expected) in [
        (
            "INSERT INTO dbo.t VALUES (1); IF 1=1 DECLARE @x INT; ELSE DECLARE @X INT;",
            "The variable name @x has already been declared",
        ),
        (
            "IF 1=0 SELECT @missing; SELECT @later",
            "Must declare the scalar variable @missing",
        ),
        (
            "SELECT @late; DECLARE @late INT",
            "Must declare the scalar variable @late",
        ),
        (
            "WHILE 1=0 BEGIN BREAK; END; CONTINUE",
            "loop control outside WHILE",
        ),
        (
            "SELECT * FROM OPENJSON(N'{}', @path)",
            "Must declare the scalar variable @path",
        ),
        (
            "CREATE VIEW sys.v AS SELECT @missing",
            "System catalog objects cannot be changed",
        ),
        (
            "CREATE VIEW dbo.v AS SELECT @missing",
            "unsupported variable or session global in view definition",
        ),
        (
            "SELECT 1; CREATE VIEW dbo.v AS SELECT 1 AS x",
            "CREATE VIEW must be the only statement in a batch",
        ),
        (
            "ALTER TABLE dbo.t ADD x INT DEFAULT @missing",
            "unsupported session value in ALTER TABLE default",
        ),
    ] {
        let statements = batch::parse(sql).unwrap();
        let original = statements.clone();
        for _ in 0..2 {
            assert_eq!(
                preflight::variables(&statements, &HashMap::new())
                    .unwrap_err()
                    .to_string(),
                expected,
                "{sql}"
            );
        }
        assert_eq!(statements, original);
    }
}
