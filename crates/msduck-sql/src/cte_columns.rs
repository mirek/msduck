//! CTE declaration, output-name and column-list cardinality checks over syntax and explicit catalog snapshots.
use crate::{
    binding_scope::{QueryScopes, Scope},
    catalog_snapshot::CatalogSnapshot,
    projection,
};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::ops::ControlFlow;

pub fn required<T: Visit>(node: &T) -> bool {
    struct Find;
    impl Visitor for Find {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            if query.with.is_some() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    node.visit(&mut Find).is_break()
}

pub fn validate<T: Visit>(node: &T, catalog: &CatalogSnapshot) -> Result<(), SqlError> {
    if !required(node) {
        return Ok(());
    }
    struct Check<'a> {
        catalog: &'a CatalogSnapshot,
        scopes: Vec<QueryScopes>,
    }
    impl Visitor for Check<'_> {
        type Break = SqlError;
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<SqlError> {
            // Validate before building declaration snapshots: a repeated key would
            // otherwise overwrite an earlier CTE in the scope map.
            if let Some(with) = &query.with {
                let mut names = std::collections::HashSet::new();
                for cte in &with.cte_tables {
                    if !names.insert(cte.alias.name.value.to_lowercase()) {
                        return ControlFlow::Break(SqlError::new(
                            239,
                            1,
                            format!(
                                "Duplicate common table expression name '{}' was specified.",
                                cte.alias.name.value
                            ),
                        ));
                    }
                }
            }
            if let Some(with) = &query.with {
                for cte in &with.cte_tables {
                    if let Err(error) = crate::cte_recursion::validate(cte) {
                        return ControlFlow::Break(error);
                    }
                }
            }
            let inherited = self
                .scopes
                .last_mut()
                .map(|parent| {
                    parent
                        .definitions
                        .pop_front()
                        .unwrap_or_else(|| parent.body.clone())
                })
                .unwrap_or_default();
            let scopes = projection::scopes(self.catalog, query, &inherited);
            if let Some(with) = &query.with {
                for (cte, scope) in with.cte_tables.iter().zip(&scopes.definitions) {
                    if !cte.alias.columns.is_empty()
                        && let Some(width) = known_width(self.catalog, &cte.query, scope)
                        && width != cte.alias.columns.len()
                    {
                        let (number, comparison) = if width > cte.alias.columns.len() {
                            (8158, "more")
                        } else {
                            (8159, "fewer")
                        };
                        return ControlFlow::Break(SqlError::new(
                            number,
                            1,
                            format!(
                                "'{}' has {comparison} columns than were specified in the column list.",
                                cte.alias.name.value
                            ),
                        ));
                    }
                    let names = if cte.alias.columns.is_empty() {
                        output_names(self.catalog, &cte.query, scope)
                    } else {
                        Some(
                            cte.alias
                                .columns
                                .iter()
                                .map(|column| Some(column.name.value.clone()))
                                .collect(),
                        )
                    };
                    if let Some(names) = names {
                        let mut seen = std::collections::HashSet::new();
                        for (index, name) in names.into_iter().enumerate() {
                            let Some(name) = name else {
                                return ControlFlow::Break(SqlError::new(
                                    8155,
                                    1,
                                    format!(
                                        "No column name was specified for column {} of '{}'.",
                                        index + 1,
                                        cte.alias.name.value
                                    ),
                                ));
                            };
                            if !seen.insert(name.to_lowercase()) {
                                return ControlFlow::Break(SqlError::new(
                                    8156,
                                    1,
                                    format!(
                                        "The column '{name}' was specified multiple times for '{}'.",
                                        cte.alias.name.value
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
            self.scopes.push(scopes);
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<SqlError> {
            self.scopes.pop();
            ControlFlow::Continue(())
        }
    }
    match node.visit(&mut Check {
        catalog,
        scopes: Vec::new(),
    }) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

// Keep expression display text separate from SQL result-column names. Backend
// inference may use expression text as a label, but a CTE cannot adopt that label.
fn output_names(
    catalog: &CatalogSnapshot,
    query: &Query,
    scope: &Scope,
) -> Option<Vec<Option<String>>> {
    if matches!(
        query.for_clause,
        Some(ForClause::Json { .. } | ForClause::Xml { .. })
    ) {
        return Some(vec![None]);
    }
    fn body(
        catalog: &CatalogSnapshot,
        query: &Query,
        set: &SetExpr,
        scope: &Scope,
    ) -> Option<Vec<Option<String>>> {
        match set {
            SetExpr::Query(query) => output_names(catalog, query, scope),
            SetExpr::SetOperation { left, .. } => body(catalog, query, left, scope),
            SetExpr::Select(select) => {
                let mut names = Vec::new();
                for item in &select.projection {
                    match item {
                        SelectItem::ExprWithAlias { alias, .. } => {
                            names.push(Some(alias.value.clone()))
                        }
                        SelectItem::UnnamedExpr(expr) => {
                            names.push(projection::expression_name(expr))
                        }
                        _ => {
                            // Resolve each star using the existing binder and its WITH scope.
                            let mut one = query.clone();
                            let mut select = select.clone();
                            select.projection = vec![item.clone()];
                            *one.body = SetExpr::Select(select);
                            names.extend(
                                projection::query_fields(catalog, &one, scope)?
                                    .into_iter()
                                    .map(|field| Some(field.name)),
                            );
                        }
                    }
                }
                Some(names)
            }
            _ => None,
        }
    }
    body(catalog, query, &query.body, scope)
}

fn known_width(catalog: &CatalogSnapshot, query: &Query, scope: &Scope) -> Option<usize> {
    if matches!(
        query.for_clause,
        Some(ForClause::Json { .. } | ForClause::Xml { .. })
    ) {
        return Some(1);
    }
    syntax_width(&query.body)
        .or_else(|| projection::query_fields(catalog, query, scope).map(|fields| fields.len()))
}
fn syntax_width(body: &SetExpr) -> Option<usize> {
    match body {
        SetExpr::Select(select)
            if select.projection.iter().all(|item| {
                matches!(
                    item,
                    SelectItem::UnnamedExpr(_) | SelectItem::ExprWithAlias { .. }
                )
            }) =>
        {
            Some(select.projection.len())
        }
        SetExpr::Query(query) => {
            if matches!(
                query.for_clause,
                Some(ForClause::Json { .. } | ForClause::Xml { .. })
            ) {
                Some(1)
            } else {
                syntax_width(&query.body)
            }
        }
        SetExpr::SetOperation { left, right, .. } => {
            let left = syntax_width(left)?;
            (syntax_width(right)? == left).then_some(left)
        }
        SetExpr::Values(values) => {
            let width = values.rows.first()?.len();
            values
                .rows
                .iter()
                .all(|row| row.len() == width)
                .then_some(width)
        }
        _ => None,
    }
}

/// Legacy diagnostic adapter; match only this validator's complete suffixes.
pub fn error_number(message: &str) -> Option<i32> {
    if let Some(number) = crate::cte_recursion::error_number(message) {
        return Some(number);
    }
    if message.starts_with("Duplicate common table expression name '")
        && message.ends_with("' was specified.")
    {
        return Some(239);
    }
    if message.starts_with("No column name was specified for column ") && message.ends_with("'.") {
        return Some(8155);
    }
    if message.starts_with("The column '")
        && message.contains("' was specified multiple times for '")
        && message.ends_with("'.")
    {
        return Some(8156);
    }
    if !message.starts_with('\'') {
        return None;
    }
    if message.ends_with("' has more columns than were specified in the column list.") {
        Some(8158)
    } else if message.ends_with("' has fewer columns than were specified in the column list.") {
        Some(8159)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding_scope::Field;
    fn statement(sql: &str) -> Statement {
        sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
            .unwrap()
            .remove(0)
    }
    #[test]
    fn duplicate_declarations_are_rejected_before_scope_overwrite() {
        let catalog = CatalogSnapshot::default();
        let invalid = statement("WITH q AS (SELECT 1 AS n),[Q] AS (SELECT 2 AS n) SELECT * FROM q");
        let before = invalid.to_string();
        for _ in 0..3 {
            let error = validate(&invalid, &catalog).unwrap_err();
            assert_eq!(error.number, 239);
            assert_eq!(
                error.message,
                "Duplicate common table expression name 'Q' was specified."
            );
            assert_eq!(error_number(&error.message), Some(239));
            assert_eq!(invalid.to_string(), before);
        }
        let separate = sqlparser::parser::Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "WITH q AS (SELECT 1 AS n) SELECT * FROM q; WITH q AS (SELECT 2 AS n) SELECT * FROM q",
        )
        .unwrap();
        assert!(validate(&separate, &catalog).is_ok());
        assert!(
            validate(
                &statement("WITH q AS (SELECT 1 AS n),r AS (SELECT n FROM q) SELECT * FROM r"),
                &catalog
            )
            .is_ok()
        );
    }

    #[test]
    fn names_follow_left_set_branch_and_explicit_lists_override_expressions() {
        let catalog = CatalogSnapshot::default();
        for (sql, number) in [
            ("WITH q(x,X) AS (SELECT 1,2) SELECT * FROM q", 8156),
            (
                "WITH q AS (SELECT 1 AS x,nextval('calls')) SELECT * FROM q",
                8155,
            ),
            ("WITH q AS (SELECT @n) SELECT * FROM q", 8155),
            (
                "WITH q AS (SELECT 1 AS x,2 AS x UNION ALL SELECT 3,4) SELECT * FROM q",
                8156,
            ),
            (
                "WITH q AS (SELECT 1 AS x FOR JSON PATH) SELECT * FROM q",
                8155,
            ),
            (
                "WITH a(x,y) AS (SELECT 1,2),q AS (SELECT *,x FROM a) SELECT * FROM q",
                8156,
            ),
        ] {
            let error = validate(&statement(sql), &catalog).unwrap_err();
            assert_eq!(error.number, number, "{sql}");
            assert_eq!(error_number(&error.message), Some(number));
        }
        for sql in [
            "WITH q(x,y) AS (SELECT 1 AS same,2 AS same) SELECT * FROM q",
            "WITH q AS (SELECT 1 AS x,2 AS y UNION ALL SELECT 3,4) SELECT * FROM q",
            "WITH q(payload) AS (SELECT 1 AS x FOR JSON PATH) SELECT * FROM q",
            "WITH q AS (SELECT * FROM unknown_source) SELECT * FROM q",
        ] {
            assert!(validate(&statement(sql), &catalog).is_ok(), "{sql}");
        }
        let error = validate(
            &statement("WITH a(x,y) AS (SELECT 1,2),q AS (SELECT *,3 FROM a) SELECT * FROM q"),
            &catalog,
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "No column name was specified for column 3 of 'q'."
        );
    }

    #[test]
    fn counts_are_inferred_without_evaluation_and_unknown_stars_are_deferred() {
        let empty = CatalogSnapshot::default();
        for (sql, number) in [
            (
                "WITH c(x) AS (SELECT nextval('calls'),1/0) SELECT * FROM c",
                8158,
            ),
            ("WITH c(x,y) AS (SELECT 1) SELECT * FROM c", 8159),
            (
                "WITH a(x,y) AS (SELECT 1,2),b(z) AS (SELECT * FROM a) SELECT * FROM b",
                8158,
            ),
            (
                "WITH c(x,y) AS (SELECT 1 AS n FOR JSON PATH) SELECT * FROM c",
                8159,
            ),
            (
                "WITH c(x) AS (SELECT 1,2 UNION ALL SELECT 3,4) SELECT * FROM c",
                8158,
            ),
        ] {
            let error = validate(&statement(sql), &empty).unwrap_err();
            assert_eq!(error.number, number, "{sql}");
            assert_eq!(error_number(&error.message), Some(number));
        }
        let star = statement("WITH c(x) AS (SELECT * FROM dbo.source) SELECT * FROM c");
        assert!(validate(&star, &empty).is_ok());
        let mut known = empty;
        known.tables.insert(
            "dbo.source".into(),
            ["a", "b"]
                .into_iter()
                .map(|name| Field {
                    collation: None,
                    name: name.into(),
                    info: None,
                    properties: Default::default(),
                    json_fragment: false,
                })
                .collect(),
        );
        assert_eq!(validate(&star, &known).unwrap_err().number, 8158);
        assert!(
            validate(
                &statement("WITH c(x,y) AS (SELECT * FROM dbo.source) SELECT * FROM c"),
                &known
            )
            .is_ok()
        );
        assert!(
            validate(
                &statement("WITH c AS (SELECT * FROM dbo.source) SELECT * FROM c"),
                &known
            )
            .is_ok()
        );
    }
}
