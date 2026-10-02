//! Logical computed-column properties over explicit source declarations.
use crate::binding_scope::{Field, Source};
use msduck_core::result::{Origin, Properties};
use sqlparser::ast::Expr;

/// Reuse the expression metadata rules; neither stored values nor backend
/// expression evaluation participate in computed-column NULL inference.
pub fn nullable(expression: &Expr, columns: &[(String, bool)]) -> Option<bool> {
    let source = Source {
        qualifiers: vec![],
        fields: columns
            .iter()
            .map(|(name, nullable)| Field {
                name: name.clone(),
                info: None,
                collation: None,
                json_fragment: false,
                properties: Properties {
                    nullable: Some(*nullable),
                    origin: Origin::Stored,
                },
            })
            .collect(),
    };
    crate::result_properties::expression(expression, &[source], &[]).nullable
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::{SelectItem, SetExpr, Statement};

    #[test]
    fn retained_computed_profile_nullability_uses_declared_inputs() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../../reference/gaps-catalog.json"
        ))
        .unwrap();
        let rows = &fixture["definitionProfile"]["runs"][0];
        let records = rows.as_array().unwrap();
        let setup = records
            .iter()
            .find(|r| r["name"] == "setup computed definitions")
            .unwrap()["sql"]
            .as_str()
            .unwrap();
        let Statement::CreateTable(table) = crate::batch::parse(setup).unwrap().remove(0) else {
            panic!()
        };
        let expected = records
            .iter()
            .find(|r| r["name"] == "computed definitions")
            .unwrap()["result"]["sets"][0]["rows"]
            .as_array()
            .unwrap();
        let sources = vec![
            ("a".into(), false),
            ("b".into(), true),
            ("label".into(), true),
        ];
        for (column, row) in table
            .columns
            .iter()
            .filter(|c| crate::dialect::computed_column::computed(c).is_some())
            .zip(expected)
        {
            let (expression, _) = crate::dialect::computed_column::computed(column).unwrap();
            assert_eq!(
                nullable(expression, &sources),
                row[7].as_bool(),
                "{}",
                column.name
            );
        }
        let Statement::Query(query) = crate::batch::parse("SELECT unsupported(a)")
            .unwrap()
            .remove(0)
        else {
            panic!()
        };
        let SetExpr::Select(select) = query.body.as_ref() else {
            panic!()
        };
        let SelectItem::UnnamedExpr(expression) = &select.projection[0] else {
            panic!()
        };
        assert_eq!(nullable(expression, &sources), None);
    }
}
