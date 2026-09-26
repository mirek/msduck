#[path = "../src/string_split.rs"]
mod string_split;

use serde_json::Value;
use sqlparser::{
    ast::{SetExpr, Statement, TableFactor},
    dialect::GenericDialect,
    parser::Parser,
};
use string_split::{Binding, DeclaredType, Error};

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../reference/string-split.json")).unwrap()
}

fn case<'a>(fixture: &'a Value, name: &str) -> &'a Value {
    fixture["containers"][0]["runs"][0]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap()
}

fn factor(sql: &str) -> TableFactor {
    let statements = Parser::parse_sql(&GenericDialect {}, sql).unwrap();
    let Statement::Query(query) = statements.last().unwrap() else {
        panic!("query")
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        panic!("select")
    };
    select.from[0].relation.clone()
}

fn chars(unicode: bool, max_bytes: u16, nullable: bool) -> DeclaredType {
    DeclaredType::Character {
        unicode,
        max_bytes,
        nullable,
        collation: None,
    }
}

fn bound(entry: &Value, source: DeclaredType, separator: DeclaredType) -> Binding {
    string_split::bind(&factor(entry["sql"].as_str().unwrap()), &source, &separator)
        .unwrap()
        .unwrap()
}

#[test]
fn captured_declarations_do_not_depend_on_input_values() {
    let fixture = fixture();
    let baseline = &fixture["containers"][0]["runs"][0];
    for container in fixture["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            assert_eq!(
                run, baseline,
                "all four retained SQL Server captures agree exactly"
            );
        }
    }
    for (name, source, separator, ordinal) in [
        (
            "ansi basic",
            chars(false, 10, false),
            chars(false, 1, false),
            false,
        ),
        (
            "unicode basic",
            chars(true, 20, false),
            chars(true, 2, false),
            false,
        ),
        (
            "empty input",
            chars(false, 12, true),
            chars(false, 1, false),
            false,
        ),
        (
            "null ansi input",
            chars(false, 12, true),
            chars(false, 1, false),
            false,
        ),
        (
            "varchar max input",
            chars(false, u16::MAX, true),
            chars(false, 1, false),
            false,
        ),
        (
            "nvarchar max input",
            chars(true, u16::MAX, true),
            chars(true, 2, false),
            false,
        ),
        (
            "varchar with unicode separator",
            chars(false, 12, true),
            chars(true, 2, false),
            false,
        ),
        (
            "ordinal one ordered",
            chars(false, 5, false),
            chars(false, 1, false),
            true,
        ),
        (
            "ordinal empty input",
            chars(false, 12, true),
            chars(false, 1, false),
            true,
        ),
        (
            "ordinal null input",
            chars(true, 24, true),
            chars(true, 2, false),
            true,
        ),
        (
            "ordinal null value only",
            chars(false, 3, false),
            chars(false, 1, false),
            false,
        ),
        (
            "ordinal cast constant",
            chars(false, 3, false),
            chars(false, 1, false),
            true,
        ),
    ] {
        let entry = case(&fixture, name);
        let binding = bound(entry, source, separator);
        let value = binding.value.unwrap();
        let columns = entry["result"]["sets"][0]["columns"].as_array().unwrap();
        assert_eq!(value.unicode, columns[0]["type"] == "NVarChar", "{name}");
        assert_eq!(
            value.max_bytes as u64,
            columns[0]["length"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(
            value.nullable,
            columns[0]["flags"].as_u64().unwrap() & 1 != 0,
            "{name}"
        );
        assert_eq!(binding.ordinal, ordinal, "{name}");
        assert_eq!(columns.len(), if ordinal { 2 } else { 1 }, "{name}");
        if ordinal {
            assert_eq!(columns[1]["type"], "BigInt");
            assert_eq!(columns[1]["flags"], 0);
        }
    }
    let unknown = bound(
        case(&fixture, "ansi basic"),
        DeclaredType::Unknown,
        chars(false, 1, false),
    );
    assert_eq!(
        unknown.value, None,
        "unknown declarations must not fabricate width"
    );
    let explicit = bound(
        case(&fixture, "explicit binary collation"),
        DeclaredType::Character {
            unicode: true,
            max_bytes: 6,
            nullable: false,
            collation: Some("Latin1_General_100_BIN2".into()),
        },
        chars(true, 2, false),
    );
    assert_eq!(
        explicit.value.unwrap().collation.as_deref(),
        Some("Latin1_General_100_BIN2")
    );
}

#[test]
fn captured_utf16_tokens_include_empty_edges_and_null_distinction() {
    let fixture = fixture();
    for (name, input, separator, ordinal) in [
        ("ansi basic", Some("alpha,beta"), Some(","), false),
        ("empty input", Some(""), Some(","), false),
        ("repeated separator", Some("a,,b"), Some(","), false),
        ("edge separators", Some(",a,"), Some(","), false),
        ("ordinal one ordered", Some("b,a,b"), Some(","), true),
        ("ordinal empty input", Some(""), Some(","), true),
        ("null ansi input", None, Some(","), false),
    ] {
        let input = input.map(|value| value.encode_utf16().collect::<Vec<_>>());
        let separator = separator.map(|value| value.encode_utf16().collect::<Vec<_>>());
        let tokens =
            string_split::split_units(input.as_deref(), separator.as_deref(), ordinal).unwrap();
        let mut actual = tokens
            .into_iter()
            .map(|token| {
                let value = String::from_utf16(&token.units).unwrap();
                if ordinal {
                    vec![
                        Value::String(value),
                        Value::String(token.ordinal.unwrap().to_string()),
                    ]
                } else {
                    vec![Value::String(value)]
                }
            })
            .collect::<Vec<_>>();
        if !ordinal {
            actual.sort_by_key(|row| row[0].as_str().unwrap().to_owned());
        }
        assert_eq!(
            Value::from(actual),
            case(&fixture, name)["result"]["sets"][0]["rows"],
            "{name}"
        );
    }
    let lone = [0xd800, ',' as u16, 0xdc00];
    let tokens = string_split::split_units(Some(&lone), Some(&[',' as u16]), false).unwrap();
    assert_eq!(tokens[0].units, [0xd800]);
    assert_eq!(tokens[1].units, [0xdc00]);
}

#[test]
fn captured_primary_errors_retain_phase_specific_shape() {
    let fixture = fixture();
    for (name, source, separator) in [
        (
            "integer input",
            DeclaredType::Other("int".into()),
            chars(false, 1, false),
        ),
        (
            "binary input",
            DeclaredType::Other("varbinary".into()),
            chars(false, 1, false),
        ),
        (
            "integer separator",
            chars(false, 3, false),
            DeclaredType::Other("int".into()),
        ),
        (
            "ordinal two",
            chars(false, 3, false),
            chars(false, 1, false),
        ),
        (
            "ordinal negative",
            chars(false, 3, false),
            chars(false, 1, false),
        ),
        (
            "ordinal variable",
            chars(false, 3, false),
            chars(false, 1, false),
        ),
        (
            "ordinal decimal",
            chars(false, 3, false),
            chars(false, 1, false),
        ),
        (
            "missing separator",
            chars(false, 3, false),
            chars(false, 1, false),
        ),
        (
            "too many arguments",
            chars(false, 3, false),
            chars(false, 1, false),
        ),
    ] {
        let entry = case(&fixture, name);
        let sql = if name == "ordinal variable" {
            "SELECT value,ordinal FROM STRING_SPLIT('a,b',',',@ordinal) ORDER BY ordinal"
        } else {
            entry["sql"].as_str().unwrap()
        };
        let error = string_split::bind(&factor(sql), &source, &separator).unwrap_err();
        let Error::Sql(error) = error else {
            panic!("SQL diagnostic for {name}")
        };
        let expected = &entry["result"]["errors"][0];
        assert_eq!(
            error.number as u64,
            expected["number"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(
            error.state as u64,
            expected["state"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(
            error.class as u64,
            expected["class"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(
            error.message,
            expected["message"].as_str().unwrap(),
            "{name}"
        );
        assert!(
            entry["result"]["sets"].as_array().unwrap().is_empty(),
            "compile error precedes metadata"
        );
    }
    for name in [
        "empty ansi separator",
        "null ansi separator",
        "two character separator",
        "supplementary separator",
    ] {
        let entry = case(&fixture, name);
        let separator = match name {
            "empty ansi separator" => Some(Vec::new()),
            "null ansi separator" => None,
            "two character separator" => Some("::".encode_utf16().collect()),
            _ => Some("😀".encode_utf16().collect()),
        };
        let error =
            string_split::split_units(Some(&[97, 98]), separator.as_deref(), false).unwrap_err();
        let Error::Sql(error) = error else {
            panic!("SQL separator error")
        };
        assert_eq!(
            error.number as u64,
            entry["result"]["errors"][0]["number"].as_u64().unwrap()
        );
        assert_eq!(
            error.state as u64,
            entry["result"]["errors"][0]["state"].as_u64().unwrap()
        );
        assert_eq!(
            error.message,
            entry["result"]["errors"][0]["message"].as_str().unwrap()
        );
        assert!(
            entry["result"]["sets"][0]["columns"].is_array(),
            "error follows value metadata"
        );
    }
}

#[test]
fn unrelated_table_function_is_untouched() {
    let ordinary = factor("SELECT value FROM GENERATE_SERIES(1, 3)");
    assert!(!string_split::is_string_split(&ordinary));
    assert_eq!(
        string_split::bind(&ordinary, &DeclaredType::Unknown, &DeclaredType::Unknown).unwrap(),
        None
    );
    let oversized = vec![1u16; string_split::MAX_CAPTURED_UNITS + 1];
    assert!(matches!(
        string_split::split_units(Some(&oversized), Some(&[2]), false),
        Err(Error::Unsupported(_))
    ));
}
