#[path = "../src/merge_top.rs"]
mod merge_top;

use merge_top::{BindError, BoundValue, TopAmount};
use sqlparser::parser::Parser;

fn parse(sql: &str) -> sqlparser::ast::Top {
    let start = sql.find("MERGE TOP ").expect("captured MERGE TOP");
    let parser_sql = &sql[start + "MERGE ".len()..];
    let mut parser = Parser::new(&msduck_sql::dialect::ServerDialect)
        .try_with_sql(parser_sql)
        .unwrap();
    merge_top::parse_after_merge(&mut parser).unwrap().unwrap()
}

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../reference/merge-top.json")).unwrap()
}

#[test]
fn captured_top_values_diagnostics_and_count_invariants() {
    let cases = fixture();
    for run in cases["runs"].as_array().unwrap() {
        for case in run.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            if !name.ends_with(" merge") || !case["sql"].as_str().unwrap().contains("MERGE TOP ") {
                continue;
            }
            let top = parse(case["sql"].as_str().unwrap());
            let value = merge_top::bind(top.clone(), |name| {
                assert_eq!(name, "@n");
                Some(BoundValue::Int(2))
            });
            let error = case["result"]["errors"].as_array().unwrap();
            if !error.is_empty()
                && matches!(
                    name,
                    "negative top merge" | "null top merge" | "fractional top merge"
                )
            {
                let BindError::Sql {
                    number,
                    state,
                    class,
                    message,
                } = value.unwrap_err()
                else {
                    panic!("expected SQL diagnostic for {name}")
                };
                assert_eq!(
                    number,
                    error[0]["number"].as_i64().unwrap() as i32,
                    "{name}"
                );
                assert_eq!(state, error[0]["state"].as_u64().unwrap() as u8, "{name}");
                assert_eq!(class, error[0]["class"].as_u64().unwrap() as u8, "{name}");
                assert_eq!(message, error[0]["message"].as_str().unwrap(), "{name}");
                continue;
            }
            let value = value.unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(value.source, top);
            assert!(matches!(value.bound_value, BoundValue::Int(_)));
            if name == "percentage top merge" {
                assert_eq!(value.amount, TopAmount::PercentThousandths(50_000));
            }
            let eligible = match name {
                n if n.starts_with("mixed ") => 4,
                "nonqualifying only top one merge" => 0,
                n if n.starts_with("nonqualifying") => 1,
                n if n.starts_with("duplicate") => 2,
                _ => 3,
            };
            let selection = value.selection(eligible).unwrap();
            assert!(selection.unordered);
            assert!(selection.take <= selection.eligible);
            // Direct OUTPUT can expose a row before 8672 rolls back the write.
            // Successful DONE rows are bounded by selected candidates, but an
            // error's direct OUTPUT count is not a committed affected count.
            if error.is_empty() {
                let affected =
                    case["result"]["done"].as_array().unwrap().last().unwrap()["rowCount"]
                        .as_u64()
                        .unwrap();
                assert!(affected <= selection.take, "{name}");
                if name == "nonqualifying only top one merge" {
                    assert_eq!(affected, 0);
                }
            } else if name == "duplicate top two merge" {
                assert_eq!(error[0]["number"], 8672);
                assert_eq!(
                    case["result"]["sets"][0]["rows"].as_array().unwrap().len(),
                    1
                );
                assert!(
                    case["result"]["done"].as_array().unwrap().last().unwrap()["rowCount"]
                        .is_null()
                );
            }
            match name {
                "mixed top zero merge" => assert_eq!(selection.take, 0),
                "mixed top one merge"
                | "mixed reverse top one merge"
                | "duplicate top one merge" => assert_eq!(selection.take, 1),
                "mixed top two merge"
                | "expression top merge"
                | "variable top merge"
                | "percentage top merge"
                | "mixed reverse top two merge"
                | "duplicate top two merge" => assert_eq!(selection.take, 2),
                "mixed top ten merge" => assert_eq!(selection.take, 4),
                "nonqualifying top one merge" | "nonqualifying top two merge" => {
                    assert_eq!(selection.take, 1)
                }
                "nonqualifying only top one merge" => assert_eq!(selection.take, 0),
                _ => {}
            }
        }
    }
}

#[test]
fn variable_is_read_once_and_unknown_forms_fail_closed() {
    let mut reads = 0;
    let top = parse("MERGE TOP (@n) dbo.t USING dbo.s ON t.id=s.id WHEN MATCHED THEN DELETE;");
    let bound = merge_top::bind(top, |_| {
        reads += 1;
        Some(BoundValue::Int(2))
    })
    .unwrap();
    assert_eq!(reads, 1);
    assert_eq!(bound.selection(3).unwrap().take, 2);
    let top =
        parse("MERGE TOP (50) PERCENT dbo.t USING dbo.s ON t.id=s.id WHEN MATCHED THEN DELETE;");
    assert_eq!(
        merge_top::bind(top, |_| None)
            .unwrap()
            .selection(3)
            .unwrap()
            .take,
        2
    );
    let top =
        parse("MERGE TOP (25) PERCENT dbo.t USING dbo.s ON t.id=s.id WHEN MATCHED THEN DELETE;");
    assert_eq!(
        merge_top::bind(top, |_| None)
            .unwrap()
            .selection(3)
            .unwrap()
            .take,
        1
    );
    let top =
        parse("MERGE TOP (@missing) dbo.t USING dbo.s ON t.id=s.id WHEN MATCHED THEN DELETE;");
    assert!(matches!(
        merge_top::bind(top, |_| None),
        Err(BindError::Unsupported(_))
    ));
}

#[test]
fn percent_boundaries_match_both_captured_databases() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../reference/merge-top-percent.json")).unwrap();
    for run in fixture["runs"].as_array().unwrap() {
        let run = run.as_array().unwrap();
        for case in run {
            let name = case["name"].as_str().unwrap();
            let Some(label) = name.strip_suffix(" merge") else {
                continue;
            };
            let sql = case["sql"].as_str().unwrap();
            let eligible: u64 = sql
                .split("WHERE id<=")
                .nth(1)
                .unwrap()
                .split(')')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            let top = parse(sql);
            let mut reads = 0;
            let bound = merge_top::bind(top.clone(), |variable| {
                assert_eq!(variable, "@p");
                reads += 1;
                Some(if label == "decimal variable" {
                    BoundValue::Decimal("33.333".into())
                } else {
                    BoundValue::Int(25)
                })
            });
            assert_eq!(reads, u32::from(label.ends_with(" variable")), "{label}");
            let errors = case["result"]["errors"].as_array().unwrap();
            if let Some(error) = errors.first() {
                let BindError::Sql {
                    number,
                    state,
                    class,
                    message,
                } = bound.unwrap_err()
                else {
                    panic!("expected captured SQL error for {label}")
                };
                assert_eq!(
                    (number, state, class, message),
                    (
                        error["number"].as_i64().unwrap() as i32,
                        error["state"].as_u64().unwrap() as u8,
                        error["class"].as_u64().unwrap() as u8,
                        error["message"].as_str().unwrap()
                    ),
                    "{label}"
                );
                continue;
            }
            let bound = bound.unwrap_or_else(|error| panic!("{label}: {error:?}"));
            assert_eq!(bound.source, top, "{label}");
            assert!(
                matches!(bound.amount, TopAmount::PercentThousandths(_)),
                "{label}"
            );
            let take = bound.selection(eligible).unwrap();
            assert_eq!(take.eligible, eligible);
            assert!(take.unordered);
            let recorded = case["result"]["done"].as_array().unwrap().last().unwrap()["rowCount"]
                .as_u64()
                .unwrap();
            assert_eq!(take.take, recorded, "{label}");
            let rows = run
                .iter()
                .find(|entry| entry["name"] == format!("{label} rows"))
                .unwrap();
            assert_eq!(
                rows["result"]["sets"][0]["rows"].as_array().unwrap().len() as u64,
                take.take
            );
        }
    }
}

#[test]
fn uncaptured_percentage_precision_stays_unsupported_and_large_counts_are_checked() {
    let top = parse(
        "MERGE TOP (33.3333) PERCENT dbo.t USING dbo.s ON t.id=s.id WHEN MATCHED THEN DELETE;",
    );
    assert!(matches!(
        merge_top::bind(top, |_| None),
        Err(BindError::Unsupported(_))
    ));
    let top =
        parse("MERGE TOP (100) PERCENT dbo.t USING dbo.s ON t.id=s.id WHEN MATCHED THEN DELETE;");
    let bound = merge_top::bind(top, |_| None).unwrap();
    assert_eq!(bound.selection(u64::MAX).unwrap().take, u64::MAX);
    let top =
        parse("MERGE TOP (50) PERCENT dbo.t USING dbo.s ON t.id=s.id WHEN MATCHED THEN DELETE;");
    let bound = merge_top::bind(top, |_| None).unwrap();
    assert_eq!(bound.selection(u64::MAX).unwrap().take, u64::MAX / 2 + 1);
}
