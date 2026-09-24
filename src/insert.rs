//! Convert INSERT targets after source-column type resolution.
use crate::assignment::storage_kind as target_kind;
use anyhow::{Result, anyhow, ensure};
use duckdb::Connection;
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};

pub fn money_columns(
    statement: &Statement,
    parameters: &std::collections::HashMap<String, crate::parameter::Parameter>,
) -> Vec<bool> {
    let Statement::Insert(insert) = statement else {
        return vec![];
    };
    let Some(source) = &insert.source else {
        return vec![];
    };
    let money = |expr: &Expr| crate::engine::money_expr(expr, parameters);
    match source.body.as_ref() {
        SetExpr::Values(values) => (0..values.rows.first().map_or(0, |row| row.len()))
            .map(|column| {
                let expressions = values
                    .rows
                    .iter()
                    .filter_map(|row| row.get(column))
                    .collect::<Vec<_>>();
                expressions.iter().any(|expr| money(expr)) && expressions.iter().all(|expr| {
                    money(expr)
                        || crate::engine::integral_expr(expr, parameters)
                        || matches!(expr, Expr::Value(value) if matches!(value.value, Value::Null))
                })
            })
            .collect(),
        SetExpr::Select(select) => select
            .projection
            .iter()
            .map(|item| match item {
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    money(expr)
                }
                _ => false,
            })
            .collect(),
        _ => vec![],
    }
}

pub fn lower(db: &Connection, statement: &mut Statement, money_columns: &[bool]) -> Result<()> {
    let Statement::Insert(insert) = statement else {
        return Ok(());
    };
    let TableObject::TableName(name) = &insert.table else {
        return Ok(());
    };
    let parts = name
        .0
        .iter()
        .map(|part| {
            part.as_ident()
                .map(|id| id.value.as_str())
                .ok_or_else(|| anyhow!("unsupported INSERT target"))
        })
        .collect::<Result<Vec<_>>>()?;
    let (schema, table) = match parts.as_slice() {
        [table] => ("dbo", *table),
        [schema, table] => (*schema, *table),
        _ => return Ok(()),
    };
    let mut metadata = db.prepare("SELECT column_name, data_type, column_default FROM information_schema.columns WHERE table_catalog=current_database() AND table_schema=? COLLATE NOCASE AND table_name=? COLLATE NOCASE ORDER BY ordinal_position")?;
    let mut columns = metadata
        .query_map([schema, table], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let utf16 = crate::assignment::utf16_targets(&columns);
    crate::assignment::declared_targets(db, schema, table, &mut columns)?;
    if insert.columns.is_empty()
        && insert.source.is_some()
        && columns
            .iter()
            .any(|c| crate::identity::is_default(c.2.as_deref()))
    {
        insert.columns = columns
            .iter()
            .filter(|c| !crate::identity::is_default(c.2.as_deref()))
            .map(|c| ObjectName::from(vec![Ident::with_quote('"', &c.0)]))
            .collect();
        ensure!(
            !insert.columns.is_empty(),
            "identity-only tables require DEFAULT VALUES"
        );
    }
    for name in &insert.columns {
        if let Some(id) = name.0.last().and_then(|p| p.as_ident()) {
            ensure!(
                !columns.iter().any(|c| c.0.eq_ignore_ascii_case(&id.value)
                    && crate::identity::is_default(c.2.as_deref())),
                "{}",
                crate::identity::EXPLICIT
            );
        }
    }
    let targets = if insert.columns.is_empty() {
        columns.iter().collect::<Vec<_>>()
    } else {
        insert
            .columns
            .iter()
            .map(|name| {
                let id = name
                    .0
                    .first()
                    .and_then(|p| p.as_ident())
                    .ok_or_else(|| anyhow!("unsupported INSERT column"))?;
                columns
                    .iter()
                    .find(|c| c.0.eq_ignore_ascii_case(&id.value))
                    .ok_or_else(|| anyhow!("Invalid column name {}", id.value))
            })
            .collect::<Result<Vec<_>>>()?
    };
    let omitted_defaults = columns
        .iter()
        .filter(|column| {
            !insert.columns.is_empty()
                && target_kind(&column.1).is_some()
                && column.2.is_some()
                && !targets
                    .iter()
                    .any(|target| target.0.eq_ignore_ascii_case(&column.0))
        })
        .collect::<Vec<_>>();
    if !targets.iter().any(|c| target_kind(&c.1).is_some()) && omitted_defaults.is_empty() {
        return Ok(());
    }
    let mut source = if let Some(source) = &insert.source {
        source.clone()
    } else {
        parse_query(&format!(
            "VALUES ({})",
            vec!["DEFAULT"; targets.len()].join(",")
        ))?
    };
    if let SetExpr::Values(values) = source.body.as_mut() {
        for row in &mut values.rows {
            ensure!(
                row.len() == targets.len(),
                "INSERT column count does not match source"
            );
            for (value, target) in row.iter_mut().zip(&targets) {
                if matches!(value, Expr::Identifier(id) if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("DEFAULT"))
                {
                    *value = Parser::new(&GenericDialect {})
                        .try_with_sql(target.2.as_deref().unwrap_or("NULL"))?
                        .parse_expr()?;
                }
            }
        }
    }
    // Bind each explicit source arm to the target's physical Unicode domain
    // before DuckDB tries to unify VALUES/UNION rows containing both VARCHAR
    // literals and raw UTF-16 carriers. Width validation remains target-side.
    fn unicode_sources(body: &mut SetExpr, unicode: &[bool], ansi: &[bool]) {
        fn pack(value: &mut Expr, ansi: bool) {
            *value = crate::engine::unary_function("__msduck_carrier_input", value.clone());
            if ansi {
                *value = crate::engine::binary_function(
                    "__msduck_cast_carrier_varchar",
                    value.clone(),
                    msduck_sql::expr::number(-1),
                );
            }
        }
        match body {
            SetExpr::Values(values) => {
                for row in &mut values.rows {
                    for ((value, unicode), ansi) in row.iter_mut().zip(unicode).zip(ansi) {
                        if *unicode || *ansi {
                            pack(value, *ansi);
                        }
                    }
                }
            }
            SetExpr::Select(select)
                if select.projection.len() == unicode.len()
                    && select.projection.iter().all(|item| {
                        matches!(
                            item,
                            SelectItem::UnnamedExpr(_) | SelectItem::ExprWithAlias { .. }
                        )
                    }) =>
            {
                for ((item, unicode), ansi) in select.projection.iter_mut().zip(unicode).zip(ansi) {
                    if *unicode || *ansi {
                        match item {
                            SelectItem::UnnamedExpr(value)
                            | SelectItem::ExprWithAlias { expr: value, .. } => pack(value, *ansi),
                            _ => unreachable!(),
                        }
                    }
                }
            }
            SetExpr::SetOperation { left, right, .. } => {
                unicode_sources(left, unicode, ansi);
                unicode_sources(right, unicode, ansi);
            }
            SetExpr::Query(query) => unicode_sources(&mut query.body, unicode, ansi),
            _ => {}
        }
    }
    unicode_sources(
        &mut source.body,
        &targets
            .iter()
            .map(|t| utf16.contains(&t.0.to_lowercase()))
            .collect::<Vec<_>>(),
        &targets.iter().enumerate().map(|(i,t)| {
            money_columns.get(i) != Some(&true) && matches!(target_kind(&t.1).and_then(|t| msduck_sql::sql_type::declaration(&t).ok()),
                Some(msduck_core::types::Type::Character(t)) if matches!(t.family(), msduck_core::character::Family::Varchar | msduck_core::character::Family::Char))
        }).collect::<Vec<_>>(),
    );
    // Describe the source without executing its rows. Binding the original
    // INSERT would apply DuckDB's rounding cast before our rewrite, rejecting
    // otherwise valid fractional values at integer limits.
    let mut describe = db.prepare(&format!("DESCRIBE {source}"))?;
    let nulls = vec![duckdb::types::Value::Null; describe.parameter_count()];
    let source_columns = describe
        .query_map(duckdb::params_from_iter(nulls.iter()), |_| Ok(()))?
        .collect::<duckdb::Result<Vec<_>>>()?
        .len();
    ensure!(
        source_columns == targets.len(),
        "INSERT column count does not match source"
    );
    let aliases = (0..targets.len())
        .map(|i| format!("__value{i}"))
        .collect::<Vec<_>>();
    let projection = targets
        .iter()
        .zip(&aliases)
        .enumerate()
        .map(|(index, (target, alias))| {
            if let Some(kind) = target_kind(&target.1) {
                let value = Expr::Identifier(Ident::new(alias));
                crate::assignment::convert_for_storage(
                    value,
                    &kind,
                    money_columns.get(index) == Some(&true),
                    utf16.contains(&target.0.to_lowercase()),
                )
                .to_string()
            } else {
                alias.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    // Only generated identifiers and whitelisted type names enter the skeleton.
    // The user's source is attached as an AST, preserving bindings and CTEs.
    let mut wrapper = parse_query(&format!(
        "SELECT {projection} FROM (SELECT NULL) AS __insert_source({})",
        aliases.join(",")
    ))?;
    let SetExpr::Select(select) = wrapper.body.as_mut() else {
        unreachable!()
    };
    let TableFactor::Derived { subquery, .. } = &mut select.from[0].relation else {
        unreachable!()
    };
    *subquery = source;
    for column in omitted_defaults {
        let kind = target_kind(&column.1).expect("filtered target default");
        let value = Parser::new(&GenericDialect {})
            .try_with_sql(column.2.as_deref().expect("filtered default"))?
            .parse_expr()?;
        select.projection.push(SelectItem::UnnamedExpr(
            crate::assignment::convert_for_storage(
                value,
                &kind,
                false,
                utf16.contains(&column.0.to_lowercase()),
            ),
        ));
        insert
            .columns
            .push(ObjectName::from(vec![Ident::with_quote('"', &column.0)]));
    }
    insert.source = Some(wrapper);
    Ok(())
}

fn parse_query(sql: &str) -> Result<Box<Query>> {
    let mut statements = Parser::parse_sql(&GenericDialect {}, sql)?;
    let Statement::Query(query) = statements.remove(0) else {
        return Err(anyhow!("expected INSERT query"));
    };
    Ok(query)
}
