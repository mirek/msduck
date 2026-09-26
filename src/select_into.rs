//! SELECT INTO creates the destination before executing its row-producing query.
use anyhow::{Result, bail, ensure};
use duckdb::{Connection, types::Value};
use sqlparser::ast::*;
use std::{collections::HashSet, ops::ControlFlow};

fn first_select(body: &mut SetExpr) -> Option<&mut Select> {
    match body {
        SetExpr::Select(select) => Some(select),
        SetExpr::SetOperation { left, .. } => first_select(left),
        SetExpr::Query(query) => first_select(&mut query.body),
        _ => None,
    }
}

pub fn take(statement: &mut Statement) -> Result<Option<String>> {
    let mut target = None;
    if let Statement::Query(query) = statement
        && let Some(select) = first_select(&mut query.body)
        && let Some(into) = select.into.take()
    {
        ensure!(
            !into.temporary && !into.unlogged && !into.table,
            "unsupported SELECT INTO modifiers"
        );
        let parts = match into.targets.as_slice() {
            [Expr::Identifier(id)] => vec![id.clone()],
            [Expr::CompoundIdentifier(ids)] => ids.clone(),
            _ => bail!("unsupported SELECT INTO target"),
        };
        ensure!(
            (1..=2).contains(&parts.len())
                && parts.iter().all(|id| !id.value.starts_with(['@', '#'])),
            "unsupported SELECT INTO target"
        );
        for item in &select.projection {
            if let SelectItem::ExprWithAlias { alias, .. } = item {
                ensure!(
                    !alias.value.is_empty(),
                    "SELECT INTO expressions require column names"
                );
            }
            ensure!(
                !matches!(item, SelectItem::UnnamedExpr(expr) if !matches!(expr, Expr::Identifier(_) | Expr::CompoundIdentifier(_))),
                "SELECT INTO expressions require column names"
            );
        }
        target = Some(
            parts
                .iter()
                .map(|id| Ident::with_quote('"', &id.value).to_string())
                .collect::<Vec<_>>()
                .join("."),
        );
    }
    struct Nested;
    impl Visitor for Nested {
        type Break = ();
        fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<()> {
            if select.into.is_some() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        }
    }
    ensure!(
        !Visit::visit(statement, &mut Nested).is_break(),
        "SELECT INTO is only supported on the first SELECT of the outer query"
    );
    Ok(target)
}

pub fn definition(db: &Connection, target: &str, source: &str, values: &[Value]) -> Result<String> {
    definition_with_fields(db, target, source, values, None)
}

fn definition_with_fields(
    db: &Connection,
    target: &str,
    source: &str,
    values: &[Value],
    fields: Option<&[crate::query_catalog::Field]>,
) -> Result<String> {
    let mut describe = db.prepare(&format!("DESCRIBE {source}"))?;
    let columns = describe
        .query_map(duckdb::params_from_iter(values.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    // The logical binder sees declarations before backend lowering. Its fields
    // are safe to apply only when they align with the physical DESCRIBE result;
    // an unresolved property then falls back to backend nullability.
    let aligned = fields.filter(|fields| {
        fields.len() == columns.len()
            && fields
                .iter()
                .zip(&columns)
                .all(|(field, (name, _, _))| field.name.eq_ignore_ascii_case(name))
    });
    let mut seen = HashSet::new();
    let mut definitions = Vec::new();
    for (index, (name, kind, nullable)) in columns.into_iter().enumerate() {
        ensure!(
            !name.is_empty(),
            "SELECT INTO expressions require column names"
        );
        ensure!(
            seen.insert(name.to_lowercase()),
            "SELECT INTO column names must be unique"
        );
        let not_null = aligned
            .and_then(|fields| fields[index].properties.nullable)
            .map_or(nullable == "NO", |nullable| !nullable);
        definitions.push(format!(
            "{} {kind}{}",
            Ident::with_quote('"', name),
            if not_null { " NOT NULL" } else { "" }
        ));
    }
    Ok(format!("CREATE TABLE {target} ({})", definitions.join(",")))
}

pub fn execute(
    db: &Connection,
    target: &str,
    source: &str,
    values: &[Value],
    autocommit: bool,
    columns: Option<&[crate::query_catalog::Field]>,
) -> Result<u64> {
    let ddl = definition_with_fields(db, target, source, values, columns)?;
    if autocommit {
        db.execute_batch("BEGIN TRANSACTION")?;
    }
    let result = (|| -> Result<()> {
        db.execute_batch(&ddl)?;
        crate::object_catalog::sync(db)?;
        if let Some(columns) = columns {
            crate::query_catalog::record(db, target, columns)?;
        }
        if autocommit {
            db.execute_batch("COMMIT")?;
        }
        Ok(())
    })();
    if result.is_err() && autocommit {
        let _ = db.execute_batch("ROLLBACK");
    }
    result?;
    // Separate statements deliberately retain the empty table if insertion fails.
    Ok(db.execute(
        &format!("INSERT INTO {target} {source}"),
        duckdb::params_from_iter(values.iter()),
    )? as u64)
}

#[cfg(test)]
mod nullability_tests {
    use super::*;
    use msduck_core::result::Properties;

    fn field(name: &str, nullable: Option<bool>) -> crate::query_catalog::Field {
        crate::query_catalog::Field {
            name: name.into(),
            info: None,
            collation: None,
            json_fragment: false,
            properties: Properties {
                nullable,
                ..Properties::default()
            },
        }
    }

    #[test]
    fn unresolved_or_misaligned_fields_leave_backend_nullability_unchanged() {
        let db = Connection::open_in_memory().unwrap();
        let source = "SELECT 1 AS id,CAST(NULL AS INT) AS n";
        let backend = definition(&db, "dest", source, &[]).unwrap();
        let known = definition_with_fields(
            &db,
            "dest",
            source,
            &[],
            Some(&[field("ID", Some(false)), field("n", Some(true))]),
        )
        .unwrap();
        assert!(known.contains("\"id\" INTEGER NOT NULL"));
        assert!(known.contains("\"n\" INTEGER"));
        assert!(!known.contains("\"n\" INTEGER NOT NULL"));
        for fields in [
            vec![field("id", Some(false))],
            vec![field("other", Some(false)), field("n", Some(false))],
            vec![field("id", None), field("n", None)],
        ] {
            assert_eq!(
                definition_with_fields(&db, "dest", source, &[], Some(&fields)).unwrap(),
                backend
            );
        }
    }
}
