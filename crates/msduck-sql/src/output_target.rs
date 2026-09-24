//! Resolve UPDATE's target over explicit catalog identities without changing joins.
use anyhow::{Result, ensure};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::collections::HashMap;

pub struct Resolved {
    /// The selected FROM relation, or the independent UPDATE target.
    /// Derived targets still require writable-projection resolution by callers.
    pub relation: TableFactor,
    /// Original FROM tree, with an independent target prepended when necessary.
    pub sources: Vec<TableWithJoins>,
}

fn alias(factor: &TableFactor) -> Result<Option<&TableAlias>> {
    Ok(match factor {
        TableFactor::Table { alias, .. }
        | TableFactor::Derived { alias, .. }
        | TableFactor::NestedJoin { alias, .. }
        | TableFactor::TableFunction { alias, .. }
        | TableFactor::Function { alias, .. }
        | TableFactor::UNNEST { alias, .. }
        | TableFactor::JsonTable { alias, .. }
        | TableFactor::OpenJsonTable { alias, .. }
        | TableFactor::Pivot { alias, .. }
        | TableFactor::Unpivot { alias, .. }
        | TableFactor::MatchRecognize { alias, .. } => alias.as_ref(),
        _ => anyhow::bail!("unsupported joined OUTPUT source scope"),
    })
}

/// Canonical column prefix, preserving identifier boundaries and quoting.
/// A quoted alias containing a dot is one identifier, not a schema path.
pub fn qualifier(factor: &TableFactor) -> Result<Vec<Ident>> {
    if let Some(alias) = alias(factor)? {
        return Ok(vec![alias.name.clone()]);
    }
    if let TableFactor::Table { name, .. } = factor {
        return name
            .0
            .iter()
            .map(|part| {
                part.as_ident()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("unsupported source name component"))
            })
            .collect();
    }
    anyhow::bail!("joined OUTPUT source requires a column qualifier")
}

/// Visible qualifier spellings, preserving components rather than joining dots.
pub fn qualifiers(factor: &TableFactor) -> Result<Vec<Vec<Ident>>> {
    let canonical = qualifier(factor)?;
    let mut result = vec![canonical.clone()];
    if alias(factor)?.is_none() && canonical.len() > 1 {
        result.push(vec![canonical.last().unwrap().clone()]);
    }
    Ok(result)
}

fn factors<'a>(tree: &'a TableWithJoins, result: &mut Vec<&'a TableFactor>) {
    for factor in
        std::iter::once(&tree.relation).chain(tree.joins.iter().map(|join| &join.relation))
    {
        if let TableFactor::NestedJoin {
            table_with_joins,
            alias: None,
        } = factor
        {
            factors(table_with_joins, result);
        } else {
            result.push(factor);
        }
    }
}

/// Keys are original AST name spellings (`ObjectName::to_string`), acquired by
/// the adapter. Equal IDs mean the same catalog object even when one reference
/// is schema-qualified and another is not. Aliases take priority over objects.
/// This selects a relation, not its writability or a physical row identity.
pub fn resolve(update: &Update, object_ids: &HashMap<String, i64>) -> Result<Resolved> {
    ensure!(
        update.table.joins.is_empty(),
        "unsupported joined UPDATE target declaration"
    );
    let TableFactor::Table { name: target, .. } = &update.table.relation else {
        anyhow::bail!("UPDATE target must be a relation name")
    };
    let mut sources = match &update.from {
        Some(UpdateTableFromKind::BeforeSet(sources) | UpdateTableFromKind::AfterSet(sources)) => {
            sources.clone()
        }
        None => vec![],
    };
    let mut visible = vec![];
    for source in &sources {
        factors(source, &mut visible);
    }
    let target_alias = match target.0.as_slice() {
        [part] => part.as_ident(),
        _ => None,
    };
    let mut aliases = vec![];
    for factor in &visible {
        if let (Some(target), Some(alias)) = (target_alias, alias(factor)?)
            && target.value.eq_ignore_ascii_case(&alias.name.value)
        {
            aliases.push(*factor);
        }
    }
    ensure!(aliases.len() <= 1, "ambiguous UPDATE target alias");
    if let Some(relation) = aliases.first() {
        return Ok(Resolved {
            relation: (*relation).clone(),
            sources,
        });
    }
    let id = object_ids
        .get(&target.to_string())
        .ok_or_else(|| SqlError::new(208, 1, format!("Invalid object name '{target}'.")))?;
    let matching = visible.iter().filter(|factor| {
        matches!(factor, TableFactor::Table { name, args: None, .. } if object_ids.get(&name.to_string()) == Some(id))
    }).copied().collect::<Vec<_>>();
    let selected = if matching.len() > 1 {
        let unaliased = matching
            .iter()
            .filter(|factor| matches!(factor, TableFactor::Table { alias: None, .. }))
            .copied()
            .collect::<Vec<_>>();
        ensure!(
            unaliased.len() == 1,
            SqlError::new(8154, 1, format!("The table '{target}' is ambiguous."))
        );
        Some(unaliased[0].clone())
    } else {
        matching.first().map(|factor| (*factor).clone())
    };
    let relation = selected.unwrap_or_else(|| {
        sources.insert(0, update.table.clone());
        update.table.relation.clone()
    });
    Ok(Resolved { relation, sources })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn update(sql: &str) -> Update {
        let Statement::Update(update) =
            sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        update
    }
    fn catalog() -> HashMap<String, i64> {
        HashMap::from([
            ("output_ref".into(), 42),
            ("dbo.output_ref".into(), 42),
            ("other".into(), 99),
            ("t".into(), 77),
        ])
    }
    #[test]
    fn aliases_and_catalog_identity_preserve_outer_and_self_join_trees() {
        for (sql, expected) in [
            (
                "UPDATE t SET n=1 FROM output_ref t LEFT JOIN other s ON t.id=s.id",
                "output_ref t",
            ),
            (
                "UPDATE output_ref SET n=1 FROM output_ref t FULL JOIN other s ON t.id=s.id",
                "output_ref t",
            ),
            (
                "UPDATE dbo.output_ref SET n=1 FROM output_ref t RIGHT JOIN other s ON t.id=s.id",
                "output_ref t",
            ),
            (
                "UPDATE output_ref SET n=1 FROM output_ref JOIN output_ref u ON output_ref.id=u.id",
                "output_ref",
            ),
            (
                "UPDATE t SET n=1 FROM (SELECT id,n FROM output_ref) t",
                "(SELECT id, n FROM output_ref) t",
            ),
        ] {
            let original = update(sql);
            let before = original.clone();
            let resolved = resolve(&original, &catalog()).unwrap();
            assert_eq!(resolved.relation.to_string(), expected);
            let Some(UpdateTableFromKind::AfterSet(sources)) = &original.from else {
                unreachable!()
            };
            assert_eq!(&resolved.sources, sources);
            assert_eq!(original, before);
        }
    }
    #[test]
    fn independent_targets_are_added_once_and_ambiguous_objects_keep_reference_error() {
        let original = update("UPDATE output_ref SET n=s.n FROM other s WHERE output_ref.id=s.id");
        let resolved = resolve(&original, &catalog()).unwrap();
        assert_eq!(resolved.sources.len(), 2);
        assert_eq!(resolved.sources[0], original.table);
        for (sql, number) in [
            (
                "UPDATE output_ref SET n=1 FROM output_ref t JOIN output_ref u ON t.id=u.id",
                8154,
            ),
            ("UPDATE missing SET n=1 FROM output_ref t", 208),
        ] {
            let error = resolve(&update(sql), &catalog()).err().unwrap();
            assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, number);
        }
    }
}
