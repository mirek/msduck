//! Pure DML target canonicalization.
use anyhow::Result;
use sqlparser::ast::*;

/// Resolve a target appearing in a flat inner/cross FROM join tree. Moving ON
/// predicates into WHERE is equivalent only for these join kinds.
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
    resolve_from_target(&mut update.table, sources, &mut update.selection, "UPDATE")?;
    if sources.is_empty() {
        update.from = None;
    }
    Ok(())
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
    let matches = |relation: &TableFactor| {
        let TableFactor::Table { name, alias, .. } = relation else {
            return false;
        };
        if let Some(alias) = alias {
            target.0.len() == 1
                && target.0[0]
                    .as_ident()
                    .is_some_and(|id| id.value.eq_ignore_ascii_case(&alias.name.value))
        } else {
            target.0.len() == name.0.len() && target.0.iter().zip(&name.0).all(|(a,b)| {
                matches!((a.as_ident(), b.as_ident()), (Some(a),Some(b)) if a.value.eq_ignore_ascii_case(&b.value))
            })
        }
    };
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
