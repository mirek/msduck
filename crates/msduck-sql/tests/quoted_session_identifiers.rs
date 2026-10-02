use msduck_core::{types::Type, value::Value};
use msduck_sql::{batch, parameter::Parameter, preflight};
use std::collections::HashMap;

#[test]
fn captured_delimited_columns_do_not_require_scalar_bindings() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/quoted-session-identifiers.json"
    ))
    .unwrap();
    for run in fixture["runs"].as_array().unwrap() {
        assert_eq!(run.as_array().unwrap().len(), 30);
        for record in run.as_array().unwrap() {
            let sql = record["sql"].as_str().unwrap();
            let statements = batch::parse(sql).unwrap();
            let original = statements.clone();
            let mut parameters = HashMap::new();
            if let Some(parameter) = record.get("parameter") {
                parameters.insert(
                    "@p".into(),
                    Parameter {
                        value: parameter["value"].as_i64().map_or(Value::Null, |value| {
                            Value::Int(i32::try_from(value).unwrap())
                        }),
                        data_type: Type::Int,
                    },
                );
            }
            let before = parameters.clone();
            for _ in 0..2 {
                let result = preflight::variables(&statements, &parameters);
                if record["name"] == "missing parameter" {
                    assert_eq!(
                        result.unwrap_err().to_string(),
                        "Must declare the scalar variable @missing",
                        "{sql}"
                    );
                } else {
                    let bindings = result.unwrap_or_else(|error| panic!("{sql}: {error}"));
                    let locals = usize::from(record["name"] == "local variable");
                    assert_eq!(bindings.len(), parameters.len() + locals, "{sql}");
                    for (name, input) in &parameters {
                        assert_eq!(bindings[name].data_type, input.data_type, "{sql}");
                        assert_eq!(bindings[name].value, input.value, "{sql}");
                    }
                    if locals == 1 {
                        assert_eq!(bindings["@p"].data_type, Type::Int);
                        assert_eq!(bindings["@p"].value, Value::Null);
                    }
                }
                assert_eq!(statements, original, "{sql}");
                assert_eq!(parameters.len(), before.len());
                for (name, input) in &before {
                    assert_eq!(parameters[name].data_type, input.data_type);
                    assert_eq!(parameters[name].value, input.value);
                }
            }
        }
    }
}

#[test]
fn quoted_names_do_not_displace_the_first_real_variable_error() {
    for sql in [
        "SELECT [@p], @missing, @later FROM dbo.quoted_session_columns",
        "SELECT \"@p\", @missing, @later FROM dbo.quoted_session_columns",
        "SELECT @missing; DECLARE @missing INT",
    ] {
        let statements = batch::parse(sql).unwrap();
        assert_eq!(
            preflight::variables(&statements, &HashMap::new())
                .unwrap_err()
                .to_string(),
            "Must declare the scalar variable @missing",
            "{sql}"
        );
    }
}
