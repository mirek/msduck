use msduck_core::character::{Family, Length};
use msduck_sql::{
    dialect::ServerDialect,
    result_types::{self, ResultType},
};
use serde_json::Value;
use sqlparser::parser::Parser;

fn expected(column: &Value) -> Option<ResultType> {
    let family = match column["type"].as_str().unwrap() {
        "NVarChar" => Family::Nvarchar,
        "VarChar" => Family::Varchar,
        _ => return None,
    };
    let bytes = column["length"].as_u64().unwrap();
    let length = if bytes == u16::MAX as u64 {
        Length::Max
    } else {
        Length::Bounded((bytes / if family == Family::Nvarchar { 2 } else { 1 }) as u16)
    };
    Some(ResultType::Character { family, length })
}

#[test]
fn captured_conditional_literal_declarations() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../reference/conditional-literal-folding.json"
    ))
    .unwrap();
    for case in fixture["results"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let query = case["query"].as_str().unwrap();
        let reference = &case["reference"];
        assert!(reference["errors"].as_array().unwrap().is_empty(), "{name}");
        assert_eq!(reference["sets"].as_array().unwrap().len(), 1, "{name}");
        let statement = Parser::parse_sql(&ServerDialect, query).unwrap().remove(0);
        let expected: Vec<_> = reference["sets"][0]["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(expected)
            .collect();
        assert_eq!(result_types::projection(&statement), expected, "{name}");
    }
}

#[test]
fn unknown_predicates_and_values_cannot_be_folded_from_parameter_values() {
    let queries = [
        (
            "SELECT CASE WHEN @flag=1 THEN N'a' ELSE N'longer' END",
            Length::Bounded(6),
        ),
        ("SELECT IIF(@flag=1,N'a',N'longer')", Length::Bounded(6)),
        (
            "SELECT CASE WHEN unknown_column=1 THEN N'a' ELSE N'longer' END",
            Length::Bounded(6),
        ),
    ];
    for (query, length) in queries {
        let statement = Parser::parse_sql(&ServerDialect, query).unwrap().remove(0);
        assert_eq!(
            result_types::projection(&statement),
            vec![Some(ResultType::Character {
                family: Family::Nvarchar,
                length,
            })],
            "{query}"
        );
    }
    for query in [
        "SELECT CASE WHEN 1=1 THEN N'a' ELSE unknown_column END",
        "SELECT IIF(1=1,N'a',unknown_column)",
        "SELECT COALESCE(N'a',unknown_column)",
    ] {
        let statement = Parser::parse_sql(&ServerDialect, query).unwrap().remove(0);
        assert_eq!(result_types::projection(&statement), vec![None], "{query}");
    }
}
