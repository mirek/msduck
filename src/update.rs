//! Target conversions for UPDATE, preserving backend row atomicity.
use anyhow::{Result, anyhow, ensure};
use duckdb::Connection;
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};
use std::collections::HashMap;

pub fn money_assignments(
    statement: &Statement,
    parameters: &HashMap<String, crate::parameter::Parameter>,
) -> Vec<bool> {
    match statement {
        Statement::Update(update) => update
            .assignments
            .iter()
            .map(|a| crate::engine::money_expr(&a.value, parameters))
            .collect(),
        Statement::Query(query) => match query.body.as_ref() {
            SetExpr::Update(statement) => money_assignments(statement, parameters),
            _ => vec![],
        },
        _ => vec![],
    }
}

pub fn is_query(statement: &Statement) -> bool {
    matches!(statement, Statement::Query(query) if !matches!(query.body.as_ref(), SetExpr::Update(_) | SetExpr::Delete(_)))
}

pub fn lower(db: &Connection, statement: &mut Statement, money: &[bool]) -> Result<()> {
    if let Statement::Query(query) = statement
        && let SetExpr::Update(statement) = query.body.as_mut()
    {
        return lower(db, statement, money);
    }
    let Statement::Update(update) = statement else {
        return Ok(());
    };
    let TableFactor::Table { name, .. } = &update.table.relation else {
        return Ok(());
    };
    let parts = name
        .0
        .iter()
        .map(|p| {
            p.as_ident()
                .map(|id| id.value.as_str())
                .ok_or_else(|| anyhow!("unsupported UPDATE target"))
        })
        .collect::<Result<Vec<_>>>()?;
    let (schema, table) = match parts.as_slice() {
        [table] => ("dbo", *table),
        [schema, table] => (*schema, *table),
        _ => return Ok(()),
    };
    let mut metadata = db.prepare("SELECT column_name,data_type,column_default FROM information_schema.columns WHERE table_catalog=current_database() AND table_schema=? COLLATE NOCASE AND table_name=? COLLATE NOCASE")?;
    let mut columns = metadata
        .query_map([schema, table], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let utf16 = crate::assignment::utf16_targets(&columns);
    crate::assignment::declared_targets(db, schema, table, &mut columns)?;
    for (index, assignment) in update.assignments.iter_mut().enumerate() {
        let AssignmentTarget::ColumnName(name) = &assignment.target else {
            continue;
        };
        let Some(name) = name.0.last().and_then(|p| p.as_ident()) else {
            continue;
        };
        let Some(column) = columns
            .iter()
            .find(|c| c.0.eq_ignore_ascii_case(&name.value))
        else {
            continue;
        };
        ensure!(
            !crate::identity::is_default(column.2.as_deref()),
            "{}",
            crate::identity::UPDATE
        );
        let mut value = assignment.value.clone();
        if matches!(&value, Expr::Identifier(id) if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("DEFAULT"))
        {
            value = Parser::new(&GenericDialect {})
                .try_with_sql(column.2.as_deref().unwrap_or("NULL"))?
                .parse_expr()?;
        }
        assignment.value = match crate::assignment::storage_kind(&column.1) {
            Some(kind) => crate::assignment::convert_for_storage(
                value,
                &kind,
                money.get(index) == Some(&true),
                utf16.contains(&column.0.to_lowercase()),
            ),
            None => value,
        };
    }
    Ok(())
}

pub use msduck_sql::update::canonicalize;

/// Image capture must apply the physical target conversion before OUTPUT reads
/// inserted values. A later UPDATE coercion cannot fix an already captured image.
pub fn lower_captured(db: &Connection, update: Update, money: &[bool]) -> Result<Update> {
    let mut statement = Statement::Update(update);
    lower(db, &mut statement, money)?;
    let Statement::Update(mut update) = statement else {
        unreachable!()
    };
    let TableFactor::Table { name, .. } = &update.table.relation else {
        anyhow::bail!("captured assignments require a base table")
    };
    let parts = name
        .0
        .iter()
        .map(|p| p.as_ident().map(|id| id.value.as_str()))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| anyhow!("invalid captured target"))?;
    let (schema, table) = match parts.as_slice() {
        [table] => ("dbo", *table),
        [schema, table] => (*schema, *table),
        _ => anyhow::bail!("unsupported captured target qualification"),
    };
    let mut query = db.prepare("SELECT column_name,data_type FROM information_schema.columns WHERE table_catalog=current_database() AND table_schema=? COLLATE NOCASE AND table_name=? COLLATE NOCASE")?;
    let columns = query
        .query_map([schema, table], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    for assignment in &mut update.assignments {
        let AssignmentTarget::ColumnName(name) = &assignment.target else {
            anyhow::bail!("unsupported captured assignment")
        };
        let column = name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .ok_or_else(|| anyhow!("missing captured column"))?;
        let (_, storage) = columns
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&column.value))
            .ok_or_else(|| anyhow!("unknown captured assignment column"))?;
        let kind = Parser::new(&sqlparser::dialect::DuckDbDialect {})
            .try_with_sql(storage)?
            .parse_data_type()?;
        assignment.value = Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(assignment.value.clone()),
            data_type: kind,
            format: None,
        };
    }
    Ok(update)
}

/// Expand parser markers with the target's type before expression translation,
/// so integer division and string concatenation can use their normal rules.
pub fn expand_compound(db: &Connection, statement: &mut Statement) -> Result<()> {
    if let Statement::Query(query) = statement
        && let SetExpr::Update(statement) = query.body.as_mut()
    {
        return expand_compound(db, statement);
    }
    let Statement::Update(update) = statement else {
        return Ok(());
    };
    if !update.assignments.iter().any(|a| matches!(&a.value, Expr::Function(f) if f.name.to_string().starts_with("__msduck_compound_"))) { return Ok(()) }
    let TableFactor::Table { name, alias, .. } = &update.table.relation else {
        anyhow::bail!("unsupported compound UPDATE target")
    };
    let parts = name
        .0
        .iter()
        .map(|p| {
            p.as_ident()
                .map(|id| id.value.as_str())
                .ok_or_else(|| anyhow!("unsupported UPDATE target"))
        })
        .collect::<Result<Vec<_>>>()?;
    let (schema, table) = match parts.as_slice() {
        [table] => ("dbo", *table),
        [schema, table] => (*schema, *table),
        _ => anyhow::bail!("unsupported compound UPDATE target"),
    };
    let qualifier = alias
        .as_ref()
        .map(|a| a.name.clone())
        .unwrap_or_else(|| Ident::with_quote('"', table));
    let mut metadata = db.prepare("SELECT column_name,data_type FROM information_schema.columns WHERE table_catalog=current_database() AND table_schema=? COLLATE NOCASE AND table_name=? COLLATE NOCASE")?;
    let columns = metadata
        .query_map([schema, table], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    for assignment in &mut update.assignments {
        let Expr::Function(function) = &assignment.value else {
            continue;
        };
        let name = function.name.to_string();
        let Some(operator) = name.strip_prefix("__msduck_compound_") else {
            continue;
        };
        let op = match operator {
            "add" => BinaryOperator::Plus,
            "sub" => BinaryOperator::Minus,
            "mul" => BinaryOperator::Multiply,
            "div" => BinaryOperator::Divide,
            "mod" => BinaryOperator::Modulo,
            "and" => BinaryOperator::BitwiseAnd,
            "or" => BinaryOperator::BitwiseOr,
            "xor" => BinaryOperator::BitwiseXor,
            _ => anyhow::bail!("unsupported compound UPDATE operator"),
        };
        let AssignmentTarget::ColumnName(target) = &assignment.target else {
            anyhow::bail!("unsupported compound UPDATE target")
        };
        let column = target
            .0
            .last()
            .and_then(|p| p.as_ident())
            .ok_or_else(|| anyhow!("unsupported UPDATE column"))?;
        let kind = columns
            .iter()
            .find(|c| c.0.eq_ignore_ascii_case(&column.value))
            .ok_or_else(|| anyhow!("Invalid column name {}", column.value))?;
        let kind = if kind.1.eq_ignore_ascii_case("FLOAT") {
            // DuckDB FLOAT is single precision; T-SQL FLOAT defaults to double.
            DataType::Real
        } else {
            Parser::new(&GenericDialect {})
                .try_with_sql(&kind.1)?
                .parse_data_type()?
        };
        let FunctionArguments::List(args) = &function.args else {
            anyhow::bail!("invalid compound UPDATE expression")
        };
        let [
            FunctionArg::Unnamed(FunctionArgExpr::Expr(left)),
            FunctionArg::Unnamed(FunctionArgExpr::Expr(right)),
        ] = args.args.as_slice()
        else {
            anyhow::bail!("invalid compound UPDATE expression")
        };
        assignment.value = Expr::BinaryOp {
            left: Box::new(Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(match left {
                    Expr::Identifier(column) => {
                        Expr::CompoundIdentifier(vec![qualifier.clone(), column.clone()])
                    }
                    _ => left.clone(),
                }),
                data_type: kind,
                format: None,
            }),
            op,
            right: Box::new(Expr::Nested(Box::new(right.clone()))),
        };
    }
    Ok(())
}
