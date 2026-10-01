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
    struct Resolve(Scopes);
    impl VisitorMut for Resolve {
        // Keep the error value, so SQL Server numbers such as 8154 survive.
        type Break = anyhow::Error;
        fn pre_visit_query(&mut self, query: &mut Query) -> std::ops::ControlFlow<anyhow::Error> {
            self.0.enter(query);
            std::ops::ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> std::ops::ControlFlow<anyhow::Error> {
            self.0.leave();
            std::ops::ControlFlow::Continue(())
        }
        fn pre_visit_statement(
            &mut self,
            statement: &mut Statement,
        ) -> std::ops::ControlFlow<anyhow::Error> {
            if let Statement::Update(update) = statement
                && let Err(error) = resolve_alias(update, &self.0.names())
            {
                return std::ops::ControlFlow::Break(error);
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    if let std::ops::ControlFlow::Break(error) = statement.visit(&mut Resolve(Scopes::default())) {
        return Err(error);
    }
    Ok(())
}

/// Common table expression names visible to the statements a visitor
/// reaches, innermost last.
#[derive(Default)]
pub(crate) struct Scopes(Vec<Vec<String>>);

impl Scopes {
    pub(crate) fn enter(&mut self, query: &Query) {
        self.0.push(cte_names(query.with.as_ref()));
    }
    pub(crate) fn leave(&mut self) {
        self.0.pop();
    }
    pub(crate) fn names(&self) -> Vec<String> {
        self.0.iter().flatten().cloned().collect()
    }
}

/// The lower-case names a WITH clause defines. A FROM relation with one of
/// these names is the CTE, never a table that shares its name.
pub fn cte_names(with: Option<&With>) -> Vec<String> {
    with.map(|with| {
        with.cte_tables
            .iter()
            .map(|cte| cte.alias.name.value.to_lowercase())
            .collect()
    })
    .unwrap_or_default()
}

fn resolve_alias(update: &mut Update, ctes: &[String]) -> Result<()> {
    let Some(UpdateTableFromKind::AfterSet(sources)) = &mut update.from else {
        return Ok(());
    };
    if let Some(found) = outer_target(&update.table, sources, ctes, "UPDATE")? {
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
    resolve_from_target(
        &mut update.table,
        sources,
        ctes,
        &mut update.selection,
        "UPDATE",
    )?;
    if sources.is_empty() {
        update.from = None;
    }
    Ok(())
}

/// Whether an UPDATE names its target inside a FROM tree that needs the
/// row-identity form: outer joins, APPLY or a nested join around the target.
/// Such an UPDATE must choose one joined row per target row, so execution
/// routes it through the joined image stages. `ctes` are the names of the
/// enclosing WITH ([`cte_names`]).
pub fn has_outer_target(update: &Update, ctes: &[String]) -> bool {
    let Some(UpdateTableFromKind::AfterSet(sources)) = &update.from else {
        return false;
    };
    matches!(
        outer_target(&update.table, sources, ctes, "UPDATE"),
        Ok(Some(_))
    )
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
    ctes: &[String],
    operation: &str,
) -> Result<Option<TableFactor>> {
    let Some(found) = locate(table, sources, ctes, operation)? else {
        return Ok(None);
    };
    let flat = sources[found.tree].joins.iter().all(|join| {
        matches!(
            &join.join_operator,
            JoinOperator::Join(_) | JoinOperator::Inner(_) | JoinOperator::CrossJoin(_)
        )
    });
    if flat && !found.nested {
        return Ok(None);
    }
    anyhow::ensure!(
        table.joins.is_empty(),
        "unsupported joined {operation} target"
    );
    Ok(Some(found.relation))
}

/// Where the statement's target appears in its FROM trees.
#[derive(Clone)]
struct Located {
    /// Index of the FROM tree that contains the target.
    tree: usize,
    /// Whether the target sits inside a parenthesized join.
    nested: bool,
    relation: TableFactor,
}

/// Find the FROM relation that is the statement's target, as SQL Server
/// binds it: a relation whose alias is the one-part target name, or an
/// unaliased relation spelled like the target. Otherwise the single reference
/// to the same table under any alias or qualification; among several, the
/// one unaliased reference, or error 8154. Without a catalog, two names
/// denote the same table when their last parts match and any schema parts
/// both give agree, taking `dbo` for a missing schema. A one-part name that
/// names a CTE refers to the CTE, not to a table.
fn locate(
    table: &TableWithJoins,
    sources: &[TableWithJoins],
    ctes: &[String],
    operation: &str,
) -> Result<Option<Located>> {
    let TableFactor::Table {
        name: target,
        alias: None,
        ..
    } = &table.relation
    else {
        return Ok(None);
    };
    let mut relations = vec![];
    for (index, source) in sources.iter().enumerate() {
        collect(index, &source.relation, false, &mut relations);
        for join in &source.joins {
            collect(index, &join.relation, false, &mut relations);
        }
    }
    let named = relations
        .iter()
        .filter(|found| names_target(target, &found.relation))
        .collect::<Vec<_>>();
    if !named.is_empty() {
        anyhow::ensure!(named.len() == 1, "ambiguous {operation} target in FROM");
        return Ok(named.into_iter().next().cloned());
    }
    let same = relations
        .iter()
        .filter(|found| !names_cte(&found.relation, ctes) && same_table(target, &found.relation))
        .collect::<Vec<_>>();
    match same.as_slice() {
        [] => Ok(None),
        [found] => Ok(Some((*found).clone())),
        several => {
            let unaliased = several
                .iter()
                .filter(|found| matches!(found.relation, TableFactor::Table { alias: None, .. }))
                .collect::<Vec<_>>();
            match unaliased.as_slice() {
                [found] => Ok(Some((**found).clone())),
                _ => Err(msduck_core::diagnostic::SqlError::new(
                    8154,
                    1,
                    format!("The table '{target}' is ambiguous."),
                )
                .into()),
            }
        }
    }
}

fn collect(tree: usize, relation: &TableFactor, nested: bool, found: &mut Vec<Located>) {
    match relation {
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            collect(tree, &table_with_joins.relation, true, found);
            for join in &table_with_joins.joins {
                collect(tree, &join.relation, true, found);
            }
        }
        TableFactor::Table { args: None, .. } => found.push(Located {
            tree,
            nested,
            relation: relation.clone(),
        }),
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

fn names_cte(relation: &TableFactor, ctes: &[String]) -> bool {
    matches!(relation, TableFactor::Table { name, .. }
        if name.0.len() == 1
            && name.0[0].as_ident().is_some_and(|id| ctes.contains(&id.value.to_lowercase())))
}

/// Whether two table names denote the same table (see [`locate`]).
fn same_table(target: &ObjectName, relation: &TableFactor) -> bool {
    let TableFactor::Table { name, .. } = relation else {
        return false;
    };
    let parts = |name: &ObjectName| {
        name.0
            .iter()
            .map(|part| part.as_ident().map(|id| id.value.to_lowercase()))
            .collect::<Option<Vec<_>>>()
    };
    let (Some(a), Some(b)) = (parts(target), parts(name)) else {
        return false;
    };
    let schema = |parts: &[String]| match parts {
        [_] => Some("dbo".to_string()),
        [.., schema, _] => Some(schema.clone()),
        [] => None,
    };
    let database = |parts: &[String]| (parts.len() == 3).then(|| parts[0].clone());
    a.last() == b.last()
        && schema(&a) == schema(&b)
        && match (database(&a), database(&b)) {
            (Some(a), Some(b)) => a == b,
            _ => true,
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
    ctes: &[String],
    selection: &mut Option<Expr>,
    operation: &str,
) -> Result<()> {
    let Some(Located {
        tree: index,
        nested,
        relation,
    }) = locate(table, sources, ctes, operation)?
    else {
        return Ok(());
    };
    anyhow::ensure!(!nested, "unsupported nested {operation} target");
    anyhow::ensure!(
        table.joins.is_empty(),
        "unsupported joined {operation} target"
    );
    let matches = |candidate: &TableFactor| *candidate == relation;
    let mut predicates = Vec::new();
    for join in &sources[index].joins {
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
    let entry = sources.remove(index);
    let remaining = std::iter::once(entry.relation)
        .chain(entry.joins.into_iter().map(|j| j.relation))
        .filter(|candidate| !matches(candidate))
        .map(|relation| TableWithJoins {
            relation,
            joins: vec![],
        })
        .collect::<Vec<_>>();
    sources.splice(index..index, remaining);
    table.relation = relation;
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
        let update_of = |sql: &str| match crate::batch::parse(sql).unwrap().remove(0) {
            Statement::Update(update) => update,
            _ => unreachable!(),
        };
        assert!(has_outer_target(
            &update_of("UPDATE t SET value = 1 FROM items t LEFT JOIN foo s ON s.id = t.id"),
            &[]
        ));
        assert!(!has_outer_target(
            &update_of("UPDATE t SET value = 1 FROM items t JOIN foo s ON s.id = t.id"),
            &[]
        ));
        assert!(!has_outer_target(
            &update_of("UPDATE items SET value = 1 FROM foo s LEFT JOIN bar b ON b.id = s.id"),
            &[]
        ));
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
    fn table_named_targets_bind_to_their_single_reference() {
        // An aliased or differently qualified reference to the target table.
        assert_eq!(
            canonical("UPDATE items SET value = 0 FROM items t LEFT JOIN foo s ON s.id = t.id WHERE s.id IS NULL").unwrap(),
            "UPDATE items AS __msduck_outer_target SET value = 0 FROM items t LEFT JOIN foo s ON s.id = t.id WHERE __msduck_outer_target.rowid = t.rowid AND (s.id IS NULL)"
        );
        assert_eq!(
            canonical(
                "UPDATE dbo.items SET value = 0 FROM items LEFT JOIN foo s ON s.id = items.id"
            )
            .unwrap(),
            "UPDATE items AS __msduck_outer_target SET value = 0 FROM items LEFT JOIN foo s ON s.id = items.id WHERE __msduck_outer_target.rowid = items.rowid"
        );
        // The same rule applies to flat inner trees.
        assert_eq!(
            canonical("UPDATE items SET value = 0 FROM items t JOIN foo s ON s.id = t.id").unwrap(),
            "UPDATE items t SET value = 0 FROM foo s WHERE s.id = t.id"
        );
        // Several references: the unaliased one, else 8154.
        assert_eq!(
            canonical(
                "UPDATE items SET value = 0 FROM items LEFT JOIN items p ON p.id = items.id - 1"
            )
            .unwrap(),
            "UPDATE items AS __msduck_outer_target SET value = 0 FROM items LEFT JOIN items p ON p.id = items.id - 1 WHERE __msduck_outer_target.rowid = items.rowid"
        );
        let error = canonical(
            "UPDATE items SET value = 0 FROM items t LEFT JOIN items u ON u.id = t.id + 1",
        )
        .unwrap_err();
        let error = error
            .downcast_ref::<msduck_core::diagnostic::SqlError>()
            .unwrap();
        assert_eq!(
            (error.number, error.message.as_str()),
            (8154, "The table 'items' is ambiguous.")
        );
        // A CTE named like the target table is not the target.
        assert_eq!(
            canonical("WITH items AS (SELECT * FROM dbo.items WHERE id > 2) UPDATE dbo.items SET value = 0 FROM items c JOIN foo s ON s.id = c.id").unwrap(),
            "WITH items AS (SELECT * FROM dbo.items WHERE id > 2) UPDATE dbo.items SET value = 0 FROM items c JOIN foo s ON s.id = c.id"
        );
        // Other schemas are other tables.
        assert_eq!(
            canonical(
                "UPDATE items SET value = 0 FROM sales.items t LEFT JOIN foo s ON s.id = t.id"
            )
            .unwrap(),
            "UPDATE items SET value = 0 FROM sales.items t LEFT JOIN foo s ON s.id = t.id"
        );
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
