use msduck_core::character::{Family, Length};
use msduck_sql::{
    dialect::ServerDialect,
    result_types::{self, ResultType},
};
use sqlparser::parser::Parser;

fn projection(sql: &str) -> Vec<Option<ResultType>> {
    let statement = Parser::parse_sql(&ServerDialect, sql).unwrap().remove(0);
    result_types::projection(&statement)
}

fn text(family: Family, width: u16) -> Option<ResultType> {
    Some(ResultType::Character {
        family,
        length: Length::Bounded(width),
    })
}

#[test]
fn direct_literals_keep_reference_character_widths() {
    // Captured from pinned SQL Server 2025 at main 000f928 (issue #483).
    let expected = vec![
        text(Family::Nvarchar, 2),
        text(Family::Nvarchar, 4),
        text(Family::Varchar, 1),
        text(Family::Nvarchar, 1),
        text(Family::Varchar, 3),
        text(Family::Nvarchar, 2),
    ];
    assert_eq!(
        projection(
            "SELECT N'ok' AS n, N'text' AS t, '' AS e, N'' AS ne, 'abc' AS a, N'😀' AS smile"
        ),
        expected
    );
    assert_eq!(
        projection("SELECT N'abc' AS n WHERE 1=0"),
        vec![text(Family::Nvarchar, 3)]
    );
}

#[test]
fn set_operation_merges_literal_widths() {
    assert_eq!(
        projection("SELECT N'a' AS n UNION ALL SELECT N'long'"),
        vec![text(Family::Nvarchar, 4)]
    );
    assert_eq!(
        projection("SELECT 'a' AS n UNION ALL SELECT 'long'"),
        vec![text(Family::Varchar, 4)]
    );
    assert!(projection("SELECT *, N'a' FROM t").is_empty());
    assert_eq!(projection("SELECT unknown_column"), vec![None]);
}
