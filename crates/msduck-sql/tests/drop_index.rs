#[path = "../src/drop_index.rs"]
mod drop_index;
use drop_index::*;
use sqlparser::ast::{Ident, ObjectName, ObjectNamePart};
fn name(value: &str) -> ObjectName {
    ObjectName(vec![ObjectNamePart::Identifier(Ident::with_quote(
        '"', value,
    ))])
}
fn catalog() -> (Vec<Table>, Vec<Index>) {
    let tables = [
        (1, "dbo", "a"),
        (2, "dbo", "b"),
        (3, "alt", "a"),
        (4, "alt", "odd.table"),
    ]
    .into_iter()
    .map(|(id, s, n)| Table {
        id,
        schema: s.into(),
        name: n.into(),
    })
    .collect();
    let indexes = [
        (1, 1, "ix"),
        (1, 2, "only_a"),
        (2, 1, "ix"),
        (3, 1, "ix"),
        (4, 1, "odd.index"),
    ]
    .into_iter()
    .map(|(table_id, id, n)| Index {
        table_id,
        id,
        name: n.into(),
        clustered: false,
        constraint_backed: false,
        backend_name: name(&format!("private_{table_id}_{id}")),
    })
    .collect();
    (tables, indexes)
}
#[test]
fn captured_diagnostics_and_remaining_indexes_match() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../reference/drop-index.json")).unwrap();
    for case in fixture["results"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let (tables, indexes) = catalog();
        let result = parse(case["sql"].as_str().unwrap())
            .and_then(|r| bind(&r, &tables, &indexes, &["dbo"], str::eq_ignore_ascii_case));
        let (drops, error) = match result {
            Ok(plan) => (plan.drops, plan.terminal),
            Err(Error::Sql(d)) => (vec![], Some(d)),
            Err(e) => panic!("{id}: {e:?}"),
        };
        let expected = case["result"]["errors"].as_array().unwrap();
        assert_eq!(usize::from(error.is_some()), expected.len(), "{id}");
        if let Some(e) = error {
            let actual = serde_json::json!({"number":e.number,"state":e.state,"class":e.class,"lineNumber":1,"message":e.message});
            assert_eq!(actual, expected[0], "{id}");
        }
        let mut remaining = vec![];
        for i in &indexes {
            if drops
                .iter()
                .any(|d| d.table_id == i.table_id && d.index_id == i.id)
            {
                continue;
            }
            let t = tables.iter().find(|t| t.id == i.table_id).unwrap();
            remaining.push(vec![t.schema.clone(), t.name.clone(), i.name.clone()]);
        }
        remaining.sort();
        assert_eq!(
            serde_json::json!(remaining),
            case["remaining"]["sets"][0]["rows"],
            "{id}"
        );
        for d in drops {
            assert_eq!(
                d.statement.to_string(),
                format!("DROP INDEX \"private_{}_{}\"", d.table_id, d.index_id)
            );
        }
    }
}
#[test]
fn catalog_comparison_and_unknown_inputs_are_explicit() {
    let (tables, indexes) = catalog();
    let r = parse("DROP INDEX IX ON DBO.A").unwrap();
    assert_eq!(
        bind(&r, &tables, &indexes, &["dbo"], str::eq_ignore_ascii_case)
            .unwrap()
            .drops
            .len(),
        1
    );
    assert_eq!(
        bind(&r, &tables, &indexes, &["dbo"], str::eq)
            .unwrap()
            .terminal
            .unwrap()
            .state,
        6
    );
    assert!(matches!(
        parse("DROP INDEX ix ON db.dbo.a"),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        parse("DROP INDEX ix ON dbo.a WITH (ONLINE=ON)"),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        parse("DROP INDEX ix ON dbo.a; SELECT 1"),
        Err(Error::Unsupported(_))
    ));
    let mut duplicate = indexes.clone();
    duplicate.push(indexes[0].clone());
    assert!(matches!(
        bind(&r, &tables, &duplicate, &["dbo"], str::eq_ignore_ascii_case),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn schema_precedence_and_backend_identity_are_caller_owned() {
    let (tables, mut indexes) = catalog();
    indexes[3].backend_name = name("x\"; DROP TABLE a;--");
    let r = parse("DROP INDEX ix ON a").unwrap();
    let plan = bind(
        &r,
        &tables,
        &indexes,
        &["alt", "dbo"],
        str::eq_ignore_ascii_case,
    )
    .unwrap();
    assert_eq!(plan.drops[0].table_id, 3);
    assert_eq!(
        plan.drops[0].statement.to_string(),
        "DROP INDEX \"x\"\"; DROP TABLE a;--\""
    );
    indexes[3].constraint_backed = true;
    assert!(matches!(
        bind(
            &r,
            &tables,
            &indexes,
            &["alt", "dbo"],
            str::eq_ignore_ascii_case
        ),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn inconsistent_catalog_identities_cannot_drop_another_tables_index() {
    let (mut tables, mut indexes) = catalog();
    let r = parse("DROP INDEX only_a ON dbo.b").unwrap();
    tables[1].id = tables[0].id;
    assert!(matches!(
        bind(&r, &tables, &indexes, &["dbo"], str::eq),
        Err(Error::Unsupported(_))
    ));
    let (tables, _) = catalog();
    indexes[2].backend_name = indexes[0].backend_name.clone();
    assert!(matches!(
        bind(&r, &tables, &indexes, &["dbo"], str::eq),
        Err(Error::Unsupported(_))
    ));
    let (_, mut indexes) = catalog();
    indexes[2].table_id = 900;
    assert!(matches!(
        bind(&r, &tables, &indexes, &["dbo"], str::eq),
        Err(Error::Unsupported(_))
    ));
}
