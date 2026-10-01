//! Pure DML target canonicalization.
use anyhow::Result;
use sqlparser::ast::*;

/// Alias of the target row in a canonical outer-join UPDATE or DELETE. The
/// FROM tree keeps the user's own alias for its copy of the target, so ON,
/// APPLY, SET and WHERE expressions bind exactly as written.
pub const OUTER_TARGET: &str = "__msduck_outer_target";

/// Resolve a target appearing in a FROM join tree. In a flat inner/cross tree
/// the ON predicates move into WHERE, which is equivalent only for those join
/// kinds. A target joined through outer joins, APPLY or a nested join keeps
/// the tree intact and is matched by row identity instead (see
/// [`outer_target`]).
pub fn canonicalize(statement: &mut Statement) -> Result<()> {
    struct Resolve;
    impl VisitorMut for Resolve {
        type Break = String;
        fn pre_visit_statement(
            &mut self,
            statement: &mut Statement,
        ) -> std::ops::ControlFlow<String> {
            if let Statement::Update(update) = statement
                && let Err(error) = resolve_alias(update)
            {
                return std::ops::ControlFlow::Break(error.to_string());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    if let std::ops::ControlFlow::Break(error) = statement.visit(&mut Resolve) {
        anyhow::bail!(error);
    }
    Ok(())
}

fn resolve_alias(update: &mut Update) -> Result<()> {
    let Some(UpdateTableFromKind::AfterSet(sources)) = &mut update.from else {
        return Ok(());
    };
    if let Some(found) = outer_target(&update.table, sources, "UPDATE")? {
        // UPDATE <copy alias> ... FROM <tree> becomes
        // UPDATE <table> AS __msduck_outer_target ... FROM <tree>
        // WHERE __msduck_outer_target.rowid = <copy>.rowid AND (<where>).
        // Every SET and WHERE reference still names the copy in the tree,
        // whose values are the target row's values before the write.
        let identity = identity_match(&found)?;
        update.table = TableWithJoins {
            relation: renamed(found, Ident::new(OUTER_TARGET)),
            joins: vec![],
        };
        update.selection = Some(conjoin(identity, update.selection.take()));
        return Ok(());
    }
    resolve_from_target(&mut update.table, sources, &mut update.selection, "UPDATE")?;
    if sources.is_empty() {
        update.from = None;
    }
    Ok(())
}

/// Whether an UPDATE names its target inside a FROM tree that needs the
/// row-identity form: outer joins, APPLY or a nested join around the target.
/// Such an UPDATE must choose one joined row per target row, so execution
/// routes it through the joined image stages.
pub fn has_outer_target(update: &Update) -> bool {
    let Some(UpdateTableFromKind::AfterSet(sources)) = &update.from else {
        return false;
    };
    matches!(outer_target(&update.table, sources, "UPDATE"), Ok(Some(_)))
}

/// Drop the qualifier of SET columns that name the UPDATE target
/// (`SET t.value = ...` with target `t`), which the backend cannot parse.
/// Qualifiers naming anything else stay, so they still fail to bind.
pub fn unqualify_assignments(update: &mut Update) {
    let TableFactor::Table { name: target, .. } = &update.table.relation else {
        return;
    };
    let target = target
        .0
        .iter()
        .map(|part| part.as_ident().map(|id| id.value.to_lowercase()))
        .collect::<Option<Vec<_>>>();
    let Some(target) = target else {
        return;
    };
    for assignment in &mut update.assignments {
        let AssignmentTarget::ColumnName(name) = &mut assignment.target else {
            continue;
        };
        let Some(parts) = name
            .0
            .iter()
            .map(|part| part.as_ident().map(|id| id.value.to_lowercase()))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let Some((_, qualifier)) = parts.split_last() else {
            continue;
        };
        if !qualifier.is_empty() && target.ends_with(qualifier) {
            name.0.drain(..qualifier.len());
        }
    }
}

/// The target's relation when it appears exactly once in a FROM tree that
/// joins it through outer joins, APPLY or a nested join; `None` when the
/// target is absent from FROM or the tree is a flat inner/cross join.
pub(crate) fn outer_target(
    table: &TableWithJoins,
    sources: &[TableWithJoins],
    operation: &str,
) -> Result<Option<TableFactor>> {
    let TableFactor::Table {
        name: target,
        alias: None,
        ..
    } = &table.relation
    else {
        return Ok(None);
    };
    let mut found = vec![];
    let mut outer = false;
    for source in sources {
        let flat = source.joins.iter().all(|join| {
            matches!(
                &join.join_operator,
                JoinOperator::Join(_) | JoinOperator::Inner(_) | JoinOperator::CrossJoin(_)
            )
        });
        let before = found.len();
        search(target, &source.relation, false, &mut found);
        for join in &source.joins {
            search(target, &join.relation, false, &mut found);
        }
        if found.len() > before && (!flat || found[before..].iter().any(|(nested, _)| *nested)) {
            outer = true;
        }
    }
    if !outer {
        return Ok(None);
    }
    anyhow::ensure!(found.len() == 1, "ambiguous {operation} target in FROM");
    anyhow::ensure!(
        table.joins.is_empty(),
        "unsupported joined {operation} target"
    );
    Ok(found.pop().map(|(_, relation)| relation))
}

fn search(
    target: &ObjectName,
    relation: &TableFactor,
    nested: bool,
    found: &mut Vec<(bool, TableFactor)>,
) {
    match relation {
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            search(target, &table_with_joins.relation, true, found);
            for join in &table_with_joins.joins {
                search(target, &join.relation, true, found);
            }
        }
        relation if names_target(target, relation) => found.push((nested, relation.clone())),
        _ => {}
    }
}

/// Whether a FROM relation is the statement's target: its alias equals a
/// one-part target name, or, without an alias, its name equals the target.
fn names_target(target: &ObjectName, relation: &TableFactor) -> bool {
    let TableFactor::Table { name, alias, .. } = relation else {
        return false;
    };
    if let Some(alias) = alias {
        target.0.len() == 1
            && target.0[0]
                .as_ident()
                .is_some_and(|id| id.value.eq_ignore_ascii_case(&alias.name.value))
    } else {
        target.0.len() == name.0.len()
            && target.0.iter().zip(&name.0).all(|(a, b)| {
                matches!((a.as_ident(), b.as_ident()), (Some(a), Some(b)) if a.value.eq_ignore_ascii_case(&b.value))
            })
    }
}

/// The base table of a FROM relation under a new alias, without hints.
pub(crate) fn renamed(mut relation: TableFactor, name: Ident) -> TableFactor {
    if let TableFactor::Table {
        alias, with_hints, ..
    } = &mut relation
    {
        *alias = Some(TableAlias {
            explicit: true,
            name,
            columns: vec![],
            at: None,
        });
        with_hints.clear();
    }
    relation
}

/// The qualifier that names a FROM relation's columns: its alias, or the last
/// part of its table name.
pub(crate) fn qualifier(relation: &TableFactor) -> Result<Ident> {
    let TableFactor::Table { name, alias, .. } = relation else {
        anyhow::bail!("outer DML target must be a table")
    };
    match alias {
        Some(alias) => Ok(alias.name.clone()),
        None => name
            .0
            .last()
            .and_then(|part| part.as_ident())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("invalid outer DML target name")),
    }
}

/// `__msduck_outer_target.rowid = <copy>.rowid`.
fn identity_match(copy: &TableFactor) -> Result<Expr> {
    Ok(Expr::BinaryOp {
        left: Box::new(Expr::CompoundIdentifier(vec![
            Ident::new(OUTER_TARGET),
            Ident::new("rowid"),
        ])),
        op: BinaryOperator::Eq,
        right: Box::new(Expr::CompoundIdentifier(vec![
            qualifier(copy)?,
            Ident::new("rowid"),
        ])),
    })
}

fn conjoin(first: Expr, rest: Option<Expr>) -> Expr {
    match rest {
        Some(rest) => Expr::BinaryOp {
            left: Box::new(first),
            op: BinaryOperator::And,
            right: Box::new(Expr::Nested(Box::new(rest))),
        },
        None => first,
    }
}

pub(crate) fn resolve_from_target(
    table: &mut TableWithJoins,
    sources: &mut Vec<TableWithJoins>,
    selection: &mut Option<Expr>,
    operation: &str,
) -> Result<()> {
    let TableFactor::Table {
        name: target,
        alias: None,
        ..
    } = &table.relation
    else {
        return Ok(());
    };
    let matches = |relation: &TableFactor| names_target(target, relation);
    let found = sources
        .iter()
        .enumerate()
        .flat_map(|(index, source)| {
            std::iter::once(&source.relation)
                .chain(source.joins.iter().map(|join| &join.relation))
                .filter(|relation| matches(relation))
                .map(move |relation| (index, relation.clone()))
        })
        .collect::<Vec<_>>();
    if found.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(found.len() == 1, "ambiguous {operation} target in FROM");
    anyhow::ensure!(
        table.joins.is_empty(),
        "unsupported joined {operation} target"
    );
    let (index, relation) = &found[0];
    let mut predicates = Vec::new();
    for join in &sources[*index].joins {
        let constraint = match &join.join_operator {
            JoinOperator::Join(c) | JoinOperator::Inner(c) | JoinOperator::CrossJoin(c) => c,
            _ => anyhow::bail!("unsupported outer/lateral join in {operation} target tree"),
        };
        match constraint {
            JoinConstraint::On(expr) => predicates.push(expr.clone()),
            JoinConstraint::None => {}
            _ => anyhow::bail!("unsupported {operation} join constraint"),
        }
    }
    let entry = sources.remove(*index);
    let remaining = std::iter::once(entry.relation)
        .chain(entry.joins.into_iter().map(|j| j.relation))
        .filter(|candidate| !matches(candidate))
        .map(|relation| TableWithJoins {
            relation,
            joins: vec![],
        })
        .collect::<Vec<_>>();
    sources.splice(*index..*index, remaining);
    table.relation = relation.clone();
    for predicate in predicates {
        *selection = Some(match selection.take() {
            Some(previous) => Expr::BinaryOp {
                left: Box::new(Expr::Nested(Box::new(previous))),
                op: BinaryOperator::And,
                right: Box::new(Expr::Nested(Box::new(predicate))),
            },
            None => predicate,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(sql: &str) -> Result<String> {
        let mut statement = crate::batch::parse(sql)?.remove(0);
        canonicalize(&mut statement)?;
        crate::delete::canonicalize(&mut statement)?;
        Ok(statement.to_string())
    }

    #[test]
    fn outer_trees_keep_their_joins_and_match_the_target_by_row_identity() {
        assert_eq!(
            canonical("UPDATE target SET value = COALESCE(source.value, 0) FROM items target LEFT JOIN foo source ON source.id = target.id").unwrap(),
            "UPDATE items AS __msduck_outer_target SET value = COALESCE(source.value, 0) FROM items target LEFT JOIN foo source ON source.id = target.id WHERE __msduck_outer_target.rowid = target.rowid"
        );
        assert_eq!(
            canonical("UPDATE items SET value = 1 FROM foo s RIGHT JOIN items ON items.id = s.id WHERE s.value > 2").unwrap(),
            "UPDATE items AS __msduck_outer_target SET value = 1 FROM foo s RIGHT JOIN items ON items.id = s.id WHERE __msduck_outer_target.rowid = items.rowid AND (s.value > 2)"
        );
        assert_eq!(
            canonical("UPDATE t SET value = x.v FROM items t OUTER APPLY (SELECT TOP (1) s.value AS v FROM foo s WHERE s.id = t.id) x").unwrap(),
            "UPDATE items AS __msduck_outer_target SET value = x.v FROM items t OUTER APPLY (SELECT TOP (1) s.value AS v FROM foo s WHERE s.id = t.id) x WHERE __msduck_outer_target.rowid = t.rowid"
        );
        // A target nested inside a parenthesized join is matched the same way.
        assert_eq!(
            canonical("UPDATE t SET value = 1 FROM foo s JOIN (items t JOIN bar b ON b.id = t.id) ON s.id = t.id").unwrap(),
            "UPDATE items AS __msduck_outer_target SET value = 1 FROM foo s JOIN (items t JOIN bar b ON b.id = t.id) ON s.id = t.id WHERE __msduck_outer_target.rowid = t.rowid"
        );
        // Canonicalization is idempotent: the canonical target is aliased.
        let once = canonical("UPDATE t SET value = 1 FROM items t FULL JOIN foo s ON s.id = t.id")
            .unwrap();
        assert_eq!(canonical(&once).unwrap(), once);
    }

    #[test]
    fn flat_inner_trees_and_unrelated_sources_keep_their_forms() {
        assert_eq!(
            canonical("UPDATE t SET value = s.value FROM items t JOIN foo s ON s.id = t.id")
                .unwrap(),
            "UPDATE items t SET value = s.value FROM foo s WHERE s.id = t.id"
        );
        // The target is not part of this tree, so its outer join is a source.
        assert_eq!(
            canonical("UPDATE items SET value = 1 FROM foo s LEFT JOIN bar b ON b.id = s.id")
                .unwrap(),
            "UPDATE items SET value = 1 FROM foo s LEFT JOIN bar b ON b.id = s.id"
        );
        let update = |sql: &str| match crate::batch::parse(sql).unwrap().remove(0) {
            Statement::Update(update) => update,
            _ => unreachable!(),
        };
        assert!(has_outer_target(&update(
            "UPDATE t SET value = 1 FROM items t LEFT JOIN foo s ON s.id = t.id"
        )));
        assert!(!has_outer_target(&update(
            "UPDATE t SET value = 1 FROM items t JOIN foo s ON s.id = t.id"
        )));
        assert!(!has_outer_target(&update(
            "UPDATE items SET value = 1 FROM foo s LEFT JOIN bar b ON b.id = s.id"
        )));
    }

    #[test]
    fn set_columns_lose_only_target_qualifiers() {
        let Statement::Update(mut update) = crate::batch::parse(
            "UPDATE t SET t.value = 1, dbo.t.name = 'x', s.other = 2, value = 3 FROM items t LEFT JOIN foo s ON s.id = t.id",
        )
        .unwrap()
        .remove(0) else {
            unreachable!()
        };
        unqualify_assignments(&mut update);
        let targets = update
            .assignments
            .iter()
            .map(|a| a.target.to_string())
            .collect::<Vec<_>>();
        assert_eq!(targets, ["value", "dbo.t.name", "s.other", "value"]);
    }

    #[test]
    fn ambiguous_outer_targets_fail() {
        let error = canonical(
            "UPDATE items SET value = 1 FROM items LEFT JOIN foo s ON s.id = items.id CROSS JOIN items",
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "ambiguous UPDATE target in FROM");
    }
}
