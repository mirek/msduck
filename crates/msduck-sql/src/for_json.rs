//! FOR JSON syntax policy, independent of execution and wire encoding.
use anyhow::{Result, bail, ensure};
use msduck_core::{
    diagnostic::SqlError,
    for_json::{Error, Options, PathPlan},
};
use sqlparser::ast::*;

pub fn diagnostic(error: Error) -> SqlError {
    let number = match error {
        Error::UnnamedColumn => 13605,
        Error::InvalidAlias(_) => 13603,
        Error::ConflictingAlias(_) => 13601,
        Error::RootWithoutArrayWrapper => 13620,
        _ => 13609,
    };
    SqlError::new(number, 1, error.to_string())
}

pub fn projection(body: &SetExpr) -> Result<&Select> {
    match body {
        SetExpr::Select(select) => Ok(select),
        SetExpr::Query(query) => projection(&query.body),
        _ => bail!("FOR JSON over set operations is not yet supported"),
    }
}

pub fn name(item: &SelectItem) -> Result<Option<&str>> {
    Ok(match item {
        SelectItem::ExprWithAlias { alias, .. } => Some(&alias.value),
        SelectItem::UnnamedExpr(Expr::Identifier(id)) if !id.value.starts_with('@') => {
            Some(&id.value)
        }
        SelectItem::UnnamedExpr(Expr::CompoundIdentifier(ids)) => {
            ids.last().map(|id| id.value.as_str())
        }
        SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => None,
        _ => return Err(diagnostic(Error::UnnamedColumn).into()),
    })
}

pub fn fragment(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(expr) => fragment(expr),
        Expr::Subquery(query) => {
            matches!(
                query.for_clause,
                Some(ForClause::Json {
                    without_array_wrapper: false,
                    ..
                })
            ) || lowered_fragment(query)
        }
        Expr::Function(function) => matches!(
            function.name.to_string().to_ascii_lowercase().as_str(),
            "json_query" | "__msduck_json_query" | "__msduck_json_array"
        ),
        _ => false,
    }
}

/// FOR JSON always returns NVARCHAR(MAX), including non-promoted unwrapped text.
/// Recognize only its original clause or exact lowered result functions.
pub fn unicode_result(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => unicode_result(inner),
        Expr::Function(function) => matches!(
            function.name.to_string().as_str(),
            "__msduck_json_array" | "__msduck_json_unwrapped"
        ),
        Expr::Subquery(query) => {
            matches!(query.for_clause, Some(ForClause::Json { .. }))
                || matches!(query.body.as_ref(), SetExpr::Select(select)
                    if matches!(select.projection.as_slice(),
                        [SelectItem::ExprWithAlias { expr, .. }] | [SelectItem::UnnamedExpr(expr)]
                            if unicode_result(expr)))
        }
        _ => false,
    }
}

/// Remove a supported outer clause; nested clauses are lowered separately.
pub fn take(statement: &mut Statement) -> Result<Option<Options>> {
    let options = if let Statement::Query(query) = statement {
        if let Some(ForClause::Json {
            for_json,
            root,
            include_null_values,
            without_array_wrapper,
        }) = &query.for_clause
        {
            ensure!(
                *for_json == ForJson::Path,
                "FOR JSON AUTO is not yet supported"
            );
            let select = projection(&query.body)?;
            ensure!(
                select.into.is_none(),
                "SELECT INTO with FOR JSON is not yet supported"
            );
            let names = select
                .projection
                .iter()
                .map(name)
                .collect::<Result<Vec<_>>>()?;
            // Star expansion is validated after binding, before evaluating rows.
            if names.iter().any(Option::is_some) {
                PathPlan::new(names.into_iter().flatten()).map_err(diagnostic)?;
            }
            if root.is_some() && *without_array_wrapper {
                return Err(diagnostic(Error::RootWithoutArrayWrapper).into());
            }
            let options = Options {
                root: root.clone(),
                include_null_values: *include_null_values,
                without_array_wrapper: *without_array_wrapper,
            };
            query.for_clause = None;
            Some(options)
        } else {
            None
        }
    } else {
        None
    };
    Ok(options)
}

/// Preserve diagnostic identity across the legacy batch preflight string boundary.
pub fn error_number(message: &str) -> Option<i32> {
    if message == Error::UnnamedColumn.to_string() {
        Some(13605)
    } else if message == Error::RootWithoutArrayWrapper.to_string() {
        Some(13620)
    } else if message.starts_with("invalid FOR JSON PATH alias: ") {
        Some(13603)
    } else if message.starts_with("conflicting FOR JSON PATH alias: ") {
        Some(13601)
    } else {
        None
    }
}

/// Internal aggregate wrapper is an explicit fragment marker, not inferred from text.
fn lowered_fragment(query: &Query) -> bool {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return false;
    };
    matches!(select.projection.first(), Some(SelectItem::ExprWithAlias { expr: Expr::Function(f), .. }) if f.name.to_string() == "__msduck_json_array")
}

/// Wrap an already validated source; keep its projection, ordering and row limits intact.
pub fn lower(query: &mut Query, names: Vec<String>, spec: String, options: &Options) -> Result<()> {
    use crate::expr::{binary_function, unary_function};
    use sqlparser::{dialect::GenericDialect, parser::Parser};
    struct Names(std::collections::HashSet<String>);
    impl Visitor for Names {
        type Break = ();
        fn pre_visit_ident(&mut self, id: &Ident) -> std::ops::ControlFlow<()> {
            self.0.insert(id.value.to_lowercase());
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut used = Names(Default::default());
    let _ = Visit::visit(query, &mut used);
    let mut alias = "__msduck_json_source".to_owned();
    while used.0.contains(&alias) {
        alias.push('_');
    }
    PathPlan::new(&names).map_err(diagnostic)?;
    let literal = |s: String| Expr::Value(Value::SingleQuotedString(s).into());
    let mut row = unary_function("row", Expr::Value(Value::Null.into()));
    let Expr::Function(f) = &mut row else {
        unreachable!()
    };
    let FunctionArguments::List(args) = &mut f.args else {
        unreachable!()
    };
    args.args = names
        .into_iter()
        .map(|name| {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::CompoundIdentifier(vec![
                Ident::new(&alias),
                Ident::with_quote('"', name),
            ])))
        })
        .collect();
    let row = binary_function("__msduck_json_row", row, literal(spec));
    // Keep exact UTF-16 row carriers through aggregation. Ordering and limits
    // remain in the preserved source query; serialize each source row once.
    let aggregate = unary_function("list", row);
    let aggregate = binary_function(
        "coalesce",
        aggregate,
        Expr::Array(Array {
            elem: vec![],
            named: false,
        }),
    );
    let function = if options.without_array_wrapper {
        "__msduck_json_unwrapped"
    } else {
        "__msduck_json_array"
    };
    let wrapped = binary_function(
        function,
        aggregate,
        literal(
            options
                .root
                .as_ref()
                .map(|root| format!("1{root}"))
                .unwrap_or_else(|| "0".into()),
        ),
    );
    let mut template = Parser::parse_sql(
        &GenericDialect,
        "SELECT NULL AS x FROM (SELECT 1) AS __msduck_json_source",
    )?
    .remove(0);
    let Statement::Query(outer) = &mut template else {
        unreachable!()
    };
    let SetExpr::Select(select) = outer.body.as_mut() else {
        unreachable!()
    };
    select.projection = vec![SelectItem::ExprWithAlias {
        expr: wrapped,
        alias: Ident::with_quote('"', "JSON_F52E2B61-18A1-11d1-B105-00805F49916B"),
    }];
    let TableFactor::Derived {
        subquery,
        alias: source_alias,
        ..
    } = &mut select.from[0].relation
    else {
        unreachable!()
    };
    source_alias.as_mut().unwrap().name = Ident::new(alias);
    **subquery = query.clone();
    *query = *outer.clone();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::parser::Parser;
    #[test]
    fn star_binding_and_invalid_projection_syntax() {
        for sql in [
            "SELECT * FROM t FOR JSON PATH",
            "SELECT t.* FROM t FOR JSON PATH",
            "SELECT 1 AS a FOR JSON PATH",
        ] {
            let mut statement = Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0);
            assert!(take(&mut statement).unwrap().is_some());
        }
        for (sql, number) in [
            ("SELECT 1 FOR JSON PATH", 13605),
            ("SELECT 1 AS a,2 AS [a.b] FOR JSON PATH", 13601),
            ("SELECT 1 AS [a.] FOR JSON PATH", 13603),
            (
                "SELECT 1 AS a FOR JSON PATH, ROOT('r'), WITHOUT_ARRAY_WRAPPER",
                13620,
            ),
        ] {
            let mut statement = Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0);
            let error = take(&mut statement).unwrap_err();
            assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, number);
            assert_eq!(error_number(&error.to_string()), Some(number));
        }
    }
}
