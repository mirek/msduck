use msduck_core::{
    character::{CharacterType, Family, Length},
    diagnostic::SqlError,
    types::{BinaryType, DecimalType, Scale, Type},
};
use msduck_sql::string_agg::{self, Accumulator, Error, Separator};
use sqlparser::{ast::*, parser::Parser};
use std::{collections::BTreeSet, ops::ControlFlow};

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../reference/string-agg.json")).unwrap()
}
/// Every retained run captured identical results; iterate all of them.
fn cases() -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for container in fixture()["containers"].as_array().unwrap() {
        for run in container["runs"].as_array().unwrap() {
            out.extend(run.as_array().unwrap().iter().cloned());
        }
    }
    assert_eq!(out.len(), 4 * 42);
    out
}
fn error(case: &serde_json::Value) -> Option<SqlError> {
    let e = case["result"]["errors"].as_array().unwrap().first()?;
    let mut error = SqlError::new(
        e["number"].as_i64().unwrap() as i32,
        e["state"].as_u64().unwrap() as u8,
        e["message"].as_str().unwrap(),
    );
    error.severity = e["class"].as_u64().unwrap() as u8;
    Some(error)
}
fn calls(sql: &str) -> Vec<Function> {
    let statements = Parser::parse_sql(&msduck_sql::dialect::ServerDialect, sql).unwrap();
    let mut functions = Vec::new();
    let _ = visit_expressions(&statements, |expr| {
        if let Expr::Function(f) = expr
            && f.name.to_string().eq_ignore_ascii_case("STRING_AGG")
        {
            functions.push(f.clone());
        }
        ControlFlow::<()>::Continue(())
    });
    functions
}
fn chars(family: Family, length: Length) -> CharacterType {
    CharacterType::new(family, length).unwrap()
}
fn varchar(n: u16) -> Type {
    Type::Character(chars(Family::Varchar, Length::Bounded(n)))
}
fn nvarchar(n: u16) -> Type {
    Type::Character(chars(Family::Nvarchar, Length::Bounded(n)))
}
fn max(family: Family) -> Type {
    Type::Character(chars(family, Length::Max))
}
fn described(case: &serde_json::Value) -> Option<CharacterType> {
    let sets = case["result"]["sets"].as_array().unwrap();
    let column = sets.first()?["columns"].as_array()?.last()?;
    let family = match column["type"].as_str()? {
        "VarChar" => Family::Varchar,
        "NVarChar" => Family::Nvarchar,
        other => panic!("unexpected STRING_AGG descriptor {other}"),
    };
    assert_eq!(column["flags"], 1, "nullable result");
    Some(match (family, column["length"].as_u64()?) {
        (_, 65535) => chars(family, Length::Max),
        (Family::Varchar, 8000) => chars(family, Length::Bounded(8000)),
        (Family::Nvarchar, 8000) => chars(family, Length::Bounded(4000)),
        other => panic!("unexpected length {other:?}"),
    })
}

#[test]
fn call_diagnostics_match_every_capture() {
    let syntax = BTreeSet::from([102, 4113, 8733, 8711]);
    let mut seen = BTreeSet::new();
    for case in cases() {
        let sql = case["sql"].as_str().unwrap();
        let functions = calls(sql);
        if functions.is_empty() {
            continue;
        }
        let expected = error(&case).filter(|e| {
            syntax.contains(&e.number) || (e.number == 8116 && e.message.contains("int"))
        });
        let parsed: Result<Vec<_>, _> = functions.iter().map(string_agg::call).collect();
        let result = parsed.and_then(|calls| {
            let calls: Vec<_> = calls.into_iter().map(Option::unwrap).collect();
            let orders: Vec<_> = calls.iter().map(|c| c.order_by).collect();
            string_agg::check_orderings(&orders).map(|()| calls)
        });
        match (&expected, result) {
            (Some(expected), Err(Error::Sql(actual))) => {
                assert_eq!(&actual, expected, "{}", case["name"]);
                seen.insert(actual.number);
            }
            (None, Ok(calls)) => {
                for call in calls {
                    if sql.contains("WITHIN GROUP") {
                        assert!(!call.order_by.is_empty());
                    }
                }
            }
            (expected, actual) => panic!("{}: {expected:?} vs {actual:?}", case["name"]),
        }
    }
    assert_eq!(seen, BTreeSet::from([102, 4113, 8116, 8711, 8733]));
}

#[test]
fn separator_forms() {
    let check = |sql, expected: Separator| {
        let f = calls(sql).remove(0);
        let call = string_agg::call(&f).unwrap().unwrap();
        assert_eq!(call.separator, expected, "{sql}");
    };
    check(
        "SELECT STRING_AGG(v,N'|') FROM t",
        Separator::Literal {
            text: "|",
            unicode: true,
        },
    );
    check(
        "SELECT STRING_AGG(v,' | ') FROM t",
        Separator::Literal {
            text: " | ",
            unicode: false,
        },
    );
    check(
        "SELECT STRING_AGG(v,@sep) FROM t",
        Separator::Variable("@sep"),
    );
    check(
        "SELECT STRING_AGG(v,CAST(NULL AS NVARCHAR(10))) FROM t",
        Separator::TypedNull(chars(Family::Nvarchar, Length::Bounded(10))),
    );
    // Forms without captured behavior stay explicit.
    for sql in [
        "SELECT STRING_AGG(v,CAST('|' AS VARCHAR(20))) FROM t",
        "SELECT STRING_AGG(v,'a'+'b') FROM t",
        "SELECT STRING_AGG(v,NULL) FROM t",
        "SELECT STRING_AGG(ALL v,'|') FROM t",
        "SELECT STRING_AGG(v) FROM t",
    ] {
        let f = calls(sql).remove(0);
        assert!(
            matches!(string_agg::call(&f), Err(Error::Unsupported(_))),
            "{sql}"
        );
    }
}

/// Source declaration and separator (declaration, is-variable) per capture.
/// Declarations come from the captured SQL; binding them is adapter work.
fn declarations(name: &str) -> Option<(Type, Type, bool)> {
    let txt = nvarchar(20);
    let ansi = varchar(20);
    let n = |s: &str| string_agg::literal_type(s, true).unwrap();
    let a = |s: &str| string_agg::literal_type(s, false).unwrap();
    Some(match name {
        "unicode null elimination" | "unicode all null" | "unicode source null" => {
            (nvarchar(10), n("|"), false)
        }
        "ansi null elimination" => (varchar(10), a("|"), false),
        "unicode empty source" => (nvarchar(1), n("|"), false),
        "unicode null separator" => (nvarchar(1), nvarchar(10), false),
        "unicode empty separator" => (nvarchar(1), n(""), false),
        "ansi spaces" => (varchar(3), a(" | "), false),
        "ordered ascending" | "ordered descending" | "ordered null keys" | "ordered ties"
        | "grouped" | "outer empty group" => (txt, n("|"), false),
        "ansi source column" => (ansi, a("|"), false),
        "ansi with unicode separator" => (ansi, n("|"), false),
        "unicode with ansi separator" => (txt, a("|"), false),
        "int expression" => (Type::Int, a("|"), false),
        "decimal expression" => (
            Type::Decimal(DecimalType::new(8, 2).unwrap()),
            a("|"),
            false,
        ),
        "datetime expression" => (Type::DateTime2(Scale::new(0).unwrap()), a("|"), false),
        "binary expression" => (
            Type::Binary(BinaryType::new(false, Length::Bounded(2)).unwrap()),
            a("|"),
            false,
        ),
        "varchar max source" => (max(Family::Varchar), a("|"), false),
        "nvarchar max source" => (max(Family::Nvarchar), n("|"), false),
        "varchar bounded with max separator variable" => (varchar(20), max(Family::Varchar), true),
        "nvarchar bounded with max separator variable" => {
            (nvarchar(20), max(Family::Nvarchar), true)
        }
        "varchar bounded overflow" => (varchar(3000), a(""), false),
        "nvarchar bounded overflow" => (nvarchar(2000), n(""), false),
        "varchar max beyond bounded limit" => (max(Family::Varchar), a(""), false),
        "nvarchar max beyond bounded limit" => (max(Family::Nvarchar), n(""), false),
        "unicode separator parameter"
        | "unicode null separator parameter"
        | "unicode separator replay" => (txt, nvarchar(10), true),
        "ansi separator parameter" => (txt, varchar(10), true),
        "bound source expression" => (nvarchar(20), n("|"), false),
        _ => return None,
    })
}

#[test]
fn output_and_separator_declarations_match_every_capture() {
    let mut covered = 0;
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        let Some((source, separator, variable)) = declarations(name) else {
            continue;
        };
        covered += 1;
        let expected = error(&case);
        let result = string_agg::output(source).and_then(|output| {
            let output = output.expect("captured source family");
            string_agg::check_separator(output, separator, variable)
                .map(|checked| (output, checked.expect("captured separator family")))
        });
        match (expected, result) {
            // 9829 is raised while accumulating, after the descriptor.
            (Some(e), Ok((output, ()))) if e.number == 9829 => {
                assert_eq!(Some(output), described(&case), "{name}");
            }
            (Some(e), Err(actual)) => assert_eq!(actual, e, "{name}"),
            (None, Ok((output, ()))) => assert_eq!(Some(output), described(&case), "{name}"),
            (expected, actual) => panic!("{name}: {expected:?} vs {actual:?}"),
        }
    }
    assert_eq!(covered, 4 * 34);
}

#[test]
fn uncaptured_declarations_stay_unknown() {
    for source in [
        Type::Character(chars(Family::Char, Length::Bounded(3))),
        Type::Character(chars(Family::Nchar, Length::Bounded(3))),
        Type::BigInt,
        Type::Float,
        Type::Binary(BinaryType::new(true, Length::Bounded(2)).unwrap()),
        Type::Xml,
    ] {
        assert_eq!(string_agg::output(source), Ok(None), "{source:?}");
    }
    let output = chars(Family::Nvarchar, Length::Bounded(4000));
    assert_eq!(
        string_agg::check_separator(output, Type::BigInt, false),
        Ok(None)
    );
    assert_eq!(
        string_agg::check_separator(output, max(Family::Nvarchar), false),
        Ok(None)
    );
}

type Group = Vec<Option<&'static str>>;

/// Inputs in evaluation (WITHIN GROUP) order, one group per result row.
fn inputs(name: &str) -> Option<(Vec<Group>, Option<&'static str>)> {
    Some(match name {
        "unicode null elimination" | "ansi null elimination" => {
            (vec![vec![Some("a"), None, Some("b")]], Some("|"))
        }
        "unicode empty source" => (vec![vec![]], Some("|")),
        "unicode all null" => (vec![vec![None, None]], Some("|")),
        "unicode null separator" | "unicode null separator parameter" => {
            (vec![vec![Some("a"), Some("b")]], None)
        }
        "unicode empty separator" => (vec![vec![Some("a"), Some("b")]], Some("")),
        "ansi spaces" => (vec![vec![Some(" a "), Some("b ")]], Some(" | ")),
        "unicode source null" => (vec![vec![None]], Some("|")),
        "ordered ascending" | "ansi source column" => {
            (vec![vec![Some("a"), Some("b"), None]], Some("|"))
        }
        // NULL keys sort first; the NULL-keyed row has a NULL value.
        "ordered null keys" => (vec![vec![None, Some("a"), Some("b")]], Some("|")),
        "ordered descending" => (vec![vec![None, Some("b"), Some("a")]], Some("|")),
        "ordered ties" => (vec![vec![Some("x"), Some("x")]], Some("|")),
        "grouped" => (
            vec![vec![Some("a"), Some("b"), None], vec![Some("x"), Some("x")]],
            Some("|"),
        ),
        "outer empty group" => (
            vec![
                vec![Some("b"), Some("a"), None],
                vec![Some("x"), Some("x")],
                vec![None],
            ],
            Some("|"),
        ),
        "int expression" => (vec![vec![Some("1"), Some("2"), None]], Some("|")),
        "varchar max source" | "nvarchar max source" => {
            (vec![vec![Some("a"), Some("b")]], Some("|"))
        }
        "unicode separator parameter" | "ansi separator parameter" | "unicode separator replay" => {
            (vec![vec![Some("a"), Some("b"), None]], Some(":"))
        }
        "bound source expression" => (vec![vec![Some("bound"), Some("bound")]], Some("|")),
        _ => return None,
    })
}

#[test]
fn accumulation_matches_captured_rows() {
    let mut covered = 0;
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        let Some((groups, separator)) = inputs(name) else {
            continue;
        };
        covered += 1;
        let output = described(&case).unwrap();
        let rows = case["result"]["sets"][0]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), groups.len(), "{name}");
        for (group, row) in groups.into_iter().zip(rows) {
            let mut acc = Accumulator::new(output);
            for value in group {
                acc.push(value, separator).unwrap();
            }
            let expected = row.as_array().unwrap().last().unwrap().as_str();
            assert_eq!(acc.finish().as_deref(), expected, "{name}");
        }
    }
    assert_eq!(covered, 4 * 23);
}

#[test]
fn bounded_overflow_and_max_lengths_match_captures() {
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        let (unit, count) = match name {
            "varchar bounded overflow" | "varchar max beyond bounded limit" => ("x", 3000),
            "nvarchar bounded overflow" | "nvarchar max beyond bounded limit" => ("x", 2000),
            _ => continue,
        };
        let value = unit.repeat(count);
        let mut acc = Accumulator::new(described(&case).unwrap());
        let result: Result<Vec<()>, _> = (0..3).map(|_| acc.push(Some(&value), Some(""))).collect();
        match error(&case) {
            Some(expected) => assert_eq!(result.unwrap_err(), expected, "{name}"),
            None => {
                result.unwrap();
                let row = &case["result"]["sets"][0]["rows"][0][0];
                assert_eq!(acc.finish().as_deref(), row.as_str(), "{name}");
            }
        }
    }
}

#[test]
fn bounded_limit_counts_output_bytes() {
    let mut ansi = Accumulator::new(chars(Family::Varchar, Length::Bounded(8000)));
    ansi.push(Some(&"x".repeat(7999)), Some("|")).unwrap();
    ansi.push(Some("y"), None).unwrap();
    assert!(ansi.push(Some("z"), None).is_err());
    let mut unicode = Accumulator::new(chars(Family::Nvarchar, Length::Bounded(4000)));
    unicode.push(Some(&"x".repeat(3999)), Some("|")).unwrap();
    // A separator counts toward the limit and is added only between values.
    assert_eq!(unicode.push(Some("y"), Some("|")).unwrap_err().number, 9829);
    unicode.push(None, Some("|")).unwrap();
    unicode.push(Some("y"), None).unwrap();
    assert_eq!(unicode.finish().unwrap().encode_utf16().count(), 4000);
}
