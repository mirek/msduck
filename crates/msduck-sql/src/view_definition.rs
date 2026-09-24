//! Syntax checks for persisted views, without catalog access.
use anyhow::{Result, bail, ensure};
use sqlparser::ast::*;
use std::ops::ControlFlow;
pub fn validate(view: &CreateView) -> Result<()> {
    ensure!(
        !view.or_replace
            && !view.materialized
            && !view.secure
            && !view.if_not_exists
            && !view.temporary
            && !view.copy_grants
            && !view.with_no_schema_binding
            && view.to.is_none()
            && view.params.is_none()
            && view.cluster_by.is_empty()
            && view.comment.is_none()
            && matches!(view.options, CreateTableOptions::None),
        "unsupported CREATE VIEW options"
    );
    ensure!(
        view.name.0.len() <= 2,
        "unsupported cross-database view name"
    );
    ensure!(view.columns.len() <= 1024, "view exceeds 1024 columns");
    ensure!(
        view.columns
            .iter()
            .all(|c| c.data_type.is_none() && c.options.is_none()),
        "unsupported view column options"
    );
    struct Definition;
    impl Visitor for Definition {
        type Break = String;
        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<String> {
            if let Some(name) = crate::openjson_path::path_variable(factor) {
                return self.pre_visit_expr(&Expr::Identifier(Ident::new(name)));
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
            if matches!(expr, Expr::Identifier(id) if id.value.starts_with('@')) {
                return ControlFlow::Break(
                    "unsupported variable or session global in view definition".into(),
                );
            }
            if let Expr::Function(f) = expr {
                let name = f.name.to_string().to_uppercase();
                if name.starts_with("ERROR_") {
                    return ControlFlow::Break(
                        "unsupported session error function in view definition".into(),
                    );
                }
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_relation(&mut self, name: &ObjectName) -> ControlFlow<String> {
            if name.0.iter().any(|part| {
                part.as_ident()
                    .is_some_and(|id| id.value.starts_with('#') || id.value.starts_with('@'))
            }) {
                return ControlFlow::Break(
                    "temporary objects are not allowed in view definitions".into(),
                );
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<String> {
            let top = matches!(query.body.as_ref(), SetExpr::Select(s) if s.top.is_some());
            if query.order_by.is_some() && !top && query.limit_clause.is_none() {
                return ControlFlow::Break("ORDER BY in a view requires TOP or OFFSET".into());
            }
            ControlFlow::Continue(())
        }
        fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<String> {
            if select.into.is_some() {
                return ControlFlow::Break("INTO is not allowed in a view definition".into());
            }
            ControlFlow::Continue(())
        }
    }
    if let ControlFlow::Break(message) = view.visit(&mut Definition) {
        bail!(message);
    }
    Ok(())
}

/// Normalize ALTER to the shared definition shape without losing its
/// existing-object requirement (the executor retains the original name).
pub fn alter_definition(statement: &Statement) -> Result<Option<CreateView>> {
    let Statement::AlterView {
        name,
        columns,
        query,
        with_options,
    } = statement
    else {
        return Ok(None);
    };
    ensure!(with_options.is_empty(), "unsupported ALTER VIEW options");
    let mut template = sqlparser::parser::Parser::parse_sql(
        &crate::dialect::ServerDialect,
        "CREATE VIEW placeholder AS SELECT 1",
    )?;
    let Statement::CreateView(mut view) = template.remove(0) else {
        unreachable!()
    };
    view.name = name.clone();
    view.columns = columns
        .iter()
        .map(|name| ViewColumnDef {
            name: name.clone(),
            data_type: None,
            options: None,
        })
        .collect();
    view.query = query.clone();
    view.or_alter = true;
    validate(&view)?;
    Ok(Some(view))
}
