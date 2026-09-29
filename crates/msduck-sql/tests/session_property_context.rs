//! Deterministic SESSIONPROPERTY, SESSION_CONTEXT and sp_set_session_context
//! rules, checked against reference/session-property-context.json facts.
use msduck_core::{
    character::{CharacterType, Family, Length},
    diagnostic::SqlError,
    types::Type,
    value::Value,
};
use msduck_sql::{
    parameter::Parameter,
    session_function::{
        Argument, ContextValue, NameArgument, SessionContext, SessionOptions, bind, context_key,
        keys_match, name_argument, positional_arguments, set_call, validate_set_calls,
    },
};
use sqlparser::ast::{Expr, SelectItem, SetExpr, Statement};
use std::collections::HashMap;

fn parse(sql: &str) -> Vec<Statement> {
    let mut statements = msduck_sql::batch::parse(sql).unwrap();
    positional_arguments(&mut statements);
    statements
}
fn call(sql: &str) -> Vec<Argument> {
    set_call(&parse(sql)[0]).unwrap().unwrap()
}
fn sql_error(error: anyhow::Error) -> (i32, u8, u8, String) {
    let error = error
        .downcast::<SqlError>()
        .expect("a SQL Server diagnostic");
    (error.number, error.state, error.severity, error.message)
}
fn nvarchar(length: u16) -> Type {
    Type::Character(CharacterType::new(Family::Nvarchar, Length::Bounded(length)).unwrap())
}

#[test]
fn session_options_follow_captured_names_and_login_state() {
    let login = SessionOptions::LOGIN;
    for (name, value) in [
        ("ANSI_NULLS", Some(true)),
        ("ansi_nulls", Some(true)),
        ("ANSI_NULLS ", Some(true)),
        (" ANSI_NULLS", None),
        ("QUOTED_IDENTIFIER", Some(true)),
        ("NUMERIC_ROUNDABORT", Some(false)),
        ("ANSI_NULL_DFLT_ON", None),
        ("NOPE", None),
    ] {
        assert_eq!(login.property(name), value, "{name}");
    }
    let warnings_off = SessionOptions {
        ansi_warnings: false,
        ..login
    };
    assert_eq!(warnings_off.property("ANSI_WARNINGS"), Some(false));
}

#[test]
fn owner_call_binds_by_position_and_ignores_names() {
    assert_eq!(
        call("exec sys.sp_set_session_context @key = N'email', @value = null"),
        [Argument::NationalString("email".into()), Argument::Null]
    );
    // Captured: names are ignored, so this stores key 'x'.
    assert_eq!(
        call("EXEC sys.sp_set_session_context @value = N'x', @key = N'named'"),
        [
            Argument::NationalString("x".into()),
            Argument::NationalString("named".into())
        ]
    );
    assert_eq!(
        call("EXEC master.sys.sp_set_session_context N'k', -5, 1"),
        [
            Argument::NationalString("k".into()),
            Argument::Integer("-5".into()),
            Argument::Integer("1".into())
        ]
    );
    // Normalization leaves real variables for preflight to check.
    let statements =
        parse("DECLARE @v INT = 1; EXEC sp_set_session_context @key = N'k', @value = @v");
    msduck_sql::preflight::variables(&statements, &HashMap::new()).unwrap();
    let statements = parse("EXEC sp_set_session_context @key = N'k', @value = @missing");
    assert!(msduck_sql::preflight::variables(&statements, &HashMap::new()).is_err());
    // Other procedures are not session context calls.
    assert!(set_call(&parse("EXEC dbo.sp_set_session_context N'k', 1")[0]).is_none());
    assert!(set_call(&parse("EXEC sys.sp_other N'k', 1")[0]).is_none());
}

#[test]
fn expression_arguments_are_compile_time_syntax_errors() {
    let statements = parse("EXEC sys.sp_set_session_context N'expr', 1+1");
    let error = validate_set_calls(&statements).unwrap_err();
    assert_eq!(
        sql_error(error),
        (102, 1, 15, "Incorrect syntax near '+'.".into())
    );
}

#[test]
fn bind_reports_captured_argument_errors() {
    let none = HashMap::new();
    for (sql, expected) in [
        (
            "EXEC sys.sp_set_session_context N'only'",
            (
                16903,
                "The \"sp_set_connection_context\" procedure was called with an incorrect number of parameters.",
            ),
        ),
        (
            "EXEC sys.sp_set_session_context N'extra', 1, 0, 1",
            (
                16914,
                "The \"sp_set_connection_context\" procedure was called with too many parameters.",
            ),
        ),
        (
            "EXEC sys.sp_set_session_context NULL, 1",
            (
                225,
                "The parameters supplied for the procedure \"sp_set_session_context\" are not valid.",
            ),
        ),
        (
            "EXEC sys.sp_set_session_context N'ro_null', 1, NULL",
            (
                15600,
                "An invalid parameter or option was specified for procedure 'sp_set_connection_context'.",
            ),
        ),
    ] {
        let error = bind(&call(sql), &none).unwrap_err();
        assert_eq!(
            sql_error(error),
            (expected.0, 1, 16, expected.1.into()),
            "{sql}"
        );
    }
    let max = HashMap::from([(
        "@v".to_string(),
        Parameter {
            value: Value::Text("x".into()),
            data_type: Type::Character(CharacterType::new(Family::Nvarchar, Length::Max).unwrap()),
        },
    )]);
    let error = bind(&call("EXEC sys.sp_set_session_context N'maxed', @v"), &max).unwrap_err();
    assert_eq!(sql_error(error).0, 15600);
    // Uncaptured families are refused explicitly, not guessed.
    let error = bind(
        &call("EXEC sys.sp_set_session_context N'ansi', 'abc'"),
        &none,
    )
    .unwrap_err();
    assert!(error.downcast_ref::<SqlError>().is_none());
}

#[test]
fn bind_types_values_like_sql_server() {
    let variables = HashMap::from([
        (
            "@v".to_string(),
            Parameter {
                value: Value::BigInt(9_000_000_000),
                data_type: Type::BigInt,
            },
        ),
        (
            "@p".to_string(),
            Parameter {
                value: Value::Text("from parameter".into()),
                data_type: nvarchar(14),
            },
        ),
    ]);
    let bound = bind(
        &call("EXEC sys.sp_set_session_context N'big', @v"),
        &variables,
    )
    .unwrap();
    assert_eq!(bound.value, ContextValue::BigInt(9_000_000_000));
    assert!(!bound.read_only);
    // Captured MaxLength 28 for the 14-character parameter, 34 for a literal.
    let bound = bind(
        &call("EXEC sys.sp_set_session_context N'param', @p, 2"),
        &variables,
    )
    .unwrap();
    assert_eq!(
        bound.value,
        ContextValue::NVarChar {
            text: "from parameter".into(),
            max_bytes: 28
        }
    );
    assert!(bound.read_only);
    let bound = bind(
        &call("EXEC sys.sp_set_session_context N'email', N'owner@example.com'"),
        &variables,
    )
    .unwrap();
    assert_eq!(
        bound.value,
        ContextValue::NVarChar {
            text: "owner@example.com".into(),
            max_bytes: 34
        }
    );
}

#[test]
fn store_applies_captured_key_matching_read_only_and_size_rules() {
    for (stored, probe, matches) in [
        ("email", "Email", true),
        ("email", "email ", true),
        ("email", "EMAIL", false),
        ("abc", "aBc", true),
        ("abc", "abC", false),
        ("abc", " abc", false),
        ("abc", "ábc", false),
        ("locked", "LOCKED", false),
    ] {
        assert_eq!(keys_match(stored, probe), matches, "{stored} {probe}");
    }
    let mut context = SessionContext::default();
    context.set("email", ContextValue::Null, false).unwrap();
    assert_eq!(context.get("Email"), Some(&ContextValue::Null));
    assert_eq!(context.get("missing"), None);
    context.set("locked", ContextValue::Int(1), true).unwrap();
    let error = context
        .set("Locked", ContextValue::Int(2), false)
        .unwrap_err();
    assert_eq!(
        (error.number, error.message.as_str()),
        (
            15664,
            "Cannot set key 'locked' in the session context. The key has been set as read_only for this session."
        )
    );
    assert_eq!(context.get("locked"), Some(&ContextValue::Int(1)));
    // 'LOCKED' differs in its final character, so it is a separate key.
    context.set("LOCKED", ContextValue::Int(3), false).unwrap();
    assert_eq!(context.get("locked"), Some(&ContextValue::Int(1)));
    for key in [String::new(), "k".repeat(129)] {
        let error = context.set(&key, ContextValue::Int(1), false).unwrap_err();
        assert_eq!(error.number, 15666);
        assert_eq!(
            error.message,
            format!(
                "Cannot set key '{key}' in the session context. The size of the key cannot exceed 256 bytes."
            )
        );
    }
    context
        .set(&"k".repeat(128), ContextValue::Int(1), false)
        .unwrap();
    // The byte bound refuses growth without changing the store.
    let large = |n: usize| ContextValue::NVarChar {
        text: "x".repeat(4000),
        max_bytes: u16::try_from(n).unwrap(),
    };
    let mut full = SessionContext::default();
    let mut stored = 0;
    let error = loop {
        match full.set(&format!("fill{stored}"), large(8000), false) {
            Ok(()) => stored += 1,
            Err(error) => break error,
        }
    };
    assert_eq!(error.number, 15665);
    assert!(stored >= 125, "{stored}");
    assert_eq!(full.get(&format!("fill{stored}")), None);
}

#[test]
fn session_context_requires_a_unicode_argument() {
    let variables = HashMap::from([(
        "@k".to_string(),
        Parameter {
            value: Value::Null,
            data_type: nvarchar(10),
        },
    )]);
    let argument = |sql: &str| {
        let statements = parse(sql);
        let Statement::Query(query) = &statements[0] else {
            unreachable!()
        };
        let SetExpr::Select(select) = query.body.as_ref() else {
            unreachable!()
        };
        let SelectItem::UnnamedExpr(Expr::Function(f)) = &select.projection[0] else {
            unreachable!()
        };
        let sqlparser::ast::FunctionArguments::List(list) = &f.args else {
            unreachable!()
        };
        let sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(expr)) =
            &list.args[0]
        else {
            unreachable!()
        };
        name_argument(expr, &variables).unwrap()
    };
    assert_eq!(
        context_key(&argument("SELECT SESSION_CONTEXT(N'email')")).unwrap(),
        Some("email")
    );
    // A NULL nvarchar variable keeps its type and reads NULL.
    assert_eq!(
        context_key(&argument("SELECT SESSION_CONTEXT(@k)")).unwrap(),
        None
    );
    for (sql, kind) in [
        ("SELECT SESSION_CONTEXT('email')", "varchar"),
        ("SELECT SESSION_CONTEXT(NULL)", "NULL"),
    ] {
        let error = context_key(&argument(sql)).unwrap_err();
        assert_eq!(
            (error.number, error.state, error.severity, error.message),
            (
                8116,
                1,
                16,
                format!(
                    "Argument data type {kind} is invalid for argument 1 of session_context function."
                )
            )
        );
    }
    assert_eq!(
        argument("SELECT SESSIONPROPERTY(1)"),
        NameArgument::Other("int")
    );
}

#[test]
fn signatures_and_declarations_match_the_capture() {
    for (sql, name) in [
        ("SELECT SESSIONPROPERTY()", "sessionproperty"),
        (
            "SELECT SESSIONPROPERTY('ANSI_NULLS', 'ANSI_PADDING')",
            "sessionproperty",
        ),
        ("SELECT SESSION_CONTEXT()", "session_context"),
    ] {
        let statements = parse(&format!("INSERT INTO t VALUES(1); IF 1=0 {sql}"));
        let error = msduck_sql::preflight::variables(&statements, &HashMap::new()).unwrap_err();
        assert_eq!(
            sql_error(error),
            (
                174,
                1,
                15,
                format!("The {name} function requires 1 argument(s).")
            )
        );
    }
    let statements = parse("SELECT SESSIONPROPERTY('ANSI_NULLS'), SESSION_CONTEXT(N'k')");
    msduck_sql::preflight::variables(&statements, &HashMap::new()).unwrap();
    let Statement::Query(query) = &statements[0] else {
        unreachable!()
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        unreachable!()
    };
    for item in &select.projection {
        let SelectItem::UnnamedExpr(Expr::Function(f)) = item else {
            unreachable!()
        };
        assert_eq!(
            msduck_sql::session_function::result_type(f)
                .unwrap()
                .to_string(),
            "SQL_VARIANT"
        );
    }
}
