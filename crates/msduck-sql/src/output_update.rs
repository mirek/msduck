//! Paired UPDATE images over already-bound, storage-converted assignments.
//! The adapter must materialize `capture` before executing `write`, inside the
//! same transaction. No post-write row identity is used to recover old values.
use anyhow::{Result, ensure};
use sqlparser::ast::*;
use std::ops::ControlFlow;

pub struct ColumnImage {
    pub column: Ident,
    pub deleted: Ident,
    pub inserted: Ident,
}

pub struct Plan {
    pub capture: Box<Query>,
    pub write: Update,
    pub columns: Vec<ColumnImage>,
    source: TableWithJoins,
    alias: Ident,
    source_columns: Vec<crate::output_join::Column>,
}

fn column(alias: &Ident, name: &Ident) -> Expr {
    Expr::CompoundIdentifier(vec![alias.clone(), name.clone()])
}

fn same_ids(left: &[Ident], right: &[Ident]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| a.value.eq_ignore_ascii_case(&b.value))
}

impl Plan {
    /// Rewrite already-bound OUTPUT items over the materialized image relation.
    /// Keep logical result fields separately: private slots are storage, not
    /// declarations or provenance. Parameters remain expressions to bind later.
    pub fn projection(&self, items: &[SelectItem]) -> Result<Box<Query>> {
        let mut projection = Vec::new();
        for item in items {
            match item {
                SelectItem::QualifiedWildcard(
                    SelectItemQualifiedWildcardKind::ObjectName(name),
                    options,
                ) => {
                    ensure!(
                        *options == Default::default(),
                        "unsupported OUTPUT wildcard modifiers"
                    );
                    let prefix = name
                        .0
                        .iter()
                        .map(|part| part.as_ident().cloned())
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| anyhow::anyhow!("invalid OUTPUT image wildcard"))?;
                    if let [image] = prefix.as_slice()
                        && matches!(
                            image.value.to_ascii_lowercase().as_str(),
                            "deleted" | "inserted"
                        )
                    {
                        for field in &self.columns {
                            let slot = self.slot(&image.value, &field.column.value)?;
                            projection.push(SelectItem::ExprWithAlias {
                                expr: column(&self.alias, slot),
                                alias: field.column.clone(),
                            });
                        }
                    } else {
                        let mut found = false;
                        for source in &self.source_columns {
                            if same_ids(&source.reference[..source.reference.len() - 1], &prefix) {
                                found = true;
                                projection.push(SelectItem::ExprWithAlias {
                                    expr: column(&self.alias, &source.slot),
                                    alias: source.reference.last().unwrap().clone(),
                                });
                            }
                        }
                        ensure!(
                            found,
                            msduck_core::diagnostic::SqlError::syntax(
                                107,
                                1,
                                format!(
                                    "The column prefix '{}' does not match with a table name or alias name used in the query.",
                                    prefix
                                        .iter()
                                        .map(|id| id.value.as_str())
                                        .collect::<Vec<_>>()
                                        .join(".")
                                )
                            )
                        );
                    }
                }
                SelectItem::UnnamedExpr(expr) => {
                    let label = crate::projection::expression_name(expr);
                    let mut expr = expr.clone();
                    self.rewrite(&mut expr)?;
                    projection.push(match label {
                        Some(label) => SelectItem::ExprWithAlias {
                            expr,
                            alias: Ident::with_quote('"', label),
                        },
                        None => SelectItem::UnnamedExpr(expr),
                    });
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    let mut expr = expr.clone();
                    self.rewrite(&mut expr)?;
                    projection.push(SelectItem::ExprWithAlias {
                        expr,
                        alias: alias.clone(),
                    });
                }
                _ => anyhow::bail!("OUTPUT requires qualified image wildcards"),
            }
        }
        let Statement::Query(mut query) = crate::batch::parse("SELECT 1 FROM __image")?.remove(0)
        else {
            unreachable!()
        };
        let SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!()
        };
        select.from = vec![self.source.clone()];
        select.projection = projection;
        Ok(query)
    }

    fn slot(&self, image: &str, name: &str) -> Result<&Ident> {
        use msduck_core::diagnostic::SqlError;
        let deleted = if image.eq_ignore_ascii_case("deleted") {
            true
        } else if image.eq_ignore_ascii_case("inserted") {
            false
        } else {
            return Err(SqlError::new(
                4104,
                1,
                format!("The multi-part identifier \"{image}.{name}\" could not be bound."),
            )
            .into());
        };
        let field = self
            .columns
            .iter()
            .find(|field| field.column.value.eq_ignore_ascii_case(name))
            .ok_or_else(|| SqlError::new(207, 1, format!("Invalid column name '{name}'.")))?;
        Ok(if deleted {
            &field.deleted
        } else {
            &field.inserted
        })
    }

    fn rewrite(&self, expr: &mut Expr) -> Result<()> {
        struct Images<'a>(&'a Plan);
        impl VisitorMut for Images<'_> {
            type Break = anyhow::Error;
            fn pre_visit_query(&mut self, _: &mut Query) -> ControlFlow<Self::Break> {
                ControlFlow::Break(anyhow::anyhow!(
                    "OUTPUT image projection requires scalar expressions without subqueries"
                ))
            }
            fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
                if let Expr::CompoundIdentifier(parts) = expr {
                    if let [image, name] = parts.as_slice()
                        && matches!(
                            image.value.to_ascii_lowercase().as_str(),
                            "deleted" | "inserted"
                        )
                    {
                        return match self.0.slot(&image.value, &name.value) {
                            Ok(slot) => {
                                *expr = column(&self.0.alias, slot);
                                ControlFlow::Continue(())
                            }
                            Err(error) => ControlFlow::Break(error),
                        };
                    }
                    if let Some(source) = self
                        .0
                        .source_columns
                        .iter()
                        .find(|source| same_ids(&source.reference, parts))
                    {
                        *expr = column(&self.0.alias, &source.slot);
                        return ControlFlow::Continue(());
                    }
                    let known_source = !parts.is_empty()
                        && self.0.source_columns.iter().any(|source| {
                            same_ids(
                                &source.reference[..source.reference.len() - 1],
                                &parts[..parts.len() - 1],
                            )
                        });
                    if known_source {
                        return ControlFlow::Break(
                            msduck_core::diagnostic::SqlError::new(
                                207,
                                1,
                                format!("Invalid column name '{}'.", parts.last().unwrap().value),
                            )
                            .into(),
                        );
                    }
                    let [image, name] = parts.as_slice() else {
                        return ControlFlow::Break(
                            msduck_core::diagnostic::SqlError::new(
                                4104,
                                1,
                                format!(
                                    "The multi-part identifier \"{}\" could not be bound.",
                                    parts
                                        .iter()
                                        .map(|id| id.value.as_str())
                                        .collect::<Vec<_>>()
                                        .join(".")
                                ),
                            )
                            .into(),
                        );
                    };
                    match self.0.slot(&image.value, &name.value) {
                        Ok(slot) => *expr = column(&self.0.alias, slot),
                        Err(error) => return ControlFlow::Break(error),
                    }
                }
                ControlFlow::Continue(())
            }
        }
        match VisitMut::visit(expr, &mut Images(self)) {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(error) => Err(error),
        }
    }
}

/// Describe both logical images against the original target declarations.
/// This query is only a binding/metadata input; it must never acquire rows.
pub fn logical_projection(target: ObjectName, items: Vec<SelectItem>) -> Result<Box<Query>> {
    let Statement::Query(mut query) =
        crate::batch::parse("SELECT 1 FROM __target AS inserted, __target AS deleted WHERE 1=0")?
            .remove(0)
    else {
        unreachable!()
    };
    let SetExpr::Select(select) = query.body.as_mut() else {
        unreachable!()
    };
    select.projection = items;
    for source in &mut select.from {
        let TableFactor::Table { name, .. } = &mut source.relation else {
            unreachable!()
        };
        *name = target.clone();
    }
    Ok(query)
}

/// `columns` must describe every stored target column in declaration order.
/// Assignments must already include DEFAULT expansion and storage conversions.
/// Generated columns and joined targets require a richer acquisition plan and
/// must be resolved by the caller before using this single-table primitive.
/// The caller owns the private relation and supplies its collision-free alias.
pub fn plan(
    update: &Update,
    columns: &[Ident],
    relation: ObjectName,
    image_alias: Ident,
) -> Result<Plan> {
    ensure!(
        update.from.is_none() && update.table.joins.is_empty(),
        "paired UPDATE images require an unjoined target"
    );
    ensure!(
        update.limit.is_none() && update.order_by.is_empty() && update.or.is_none(),
        "paired UPDATE image selection must be lowered before acquisition"
    );
    let TableFactor::Table { name, alias, .. } = &update.table.relation else {
        anyhow::bail!("paired UPDATE images require a base table");
    };
    let target_alias = alias
        .as_ref()
        .map(|a| &a.name)
        .or_else(|| name.0.last().and_then(|part| part.as_ident()))
        .ok_or_else(|| anyhow::anyhow!("missing UPDATE target name"))?;
    ensure!(
        !target_alias.value.eq_ignore_ascii_case(&image_alias.value),
        "image alias collides with UPDATE target"
    );
    ensure!(!columns.is_empty(), "missing target column snapshot");
    let mut seen = std::collections::HashSet::new();
    for name in columns {
        ensure!(
            seen.insert(name.value.to_lowercase()),
            "duplicate target column"
        );
        ensure!(
            !name.value.eq_ignore_ascii_case("rowid"),
            "target column shadows physical row identity"
        );
    }
    let mut assignments = std::collections::HashMap::new();
    for assignment in &update.assignments {
        let AssignmentTarget::ColumnName(name) = &assignment.target else {
            anyhow::bail!("paired UPDATE requires scalar column assignments");
        };
        let Some(id) = name.0.last().and_then(|part| part.as_ident()) else {
            anyhow::bail!("missing assignment column");
        };
        ensure!(
            seen.contains(&id.value.to_lowercase()),
            "unknown assignment column"
        );
        ensure!(
            assignments
                .insert(id.value.to_lowercase(), assignment.value.clone())
                .is_none(),
            "duplicate assignment column"
        );
    }
    let Statement::Query(mut capture) =
        crate::batch::parse("SELECT 1 FROM __image_target")?.remove(0)
    else {
        unreachable!()
    };
    let SetExpr::Select(select) = capture.body.as_mut() else {
        unreachable!()
    };
    select.from = vec![update.table.clone()];
    select.selection = update.selection.clone();
    let rowid = Ident::new("rowid");
    let identity = Ident::with_quote('"', "__msduck_identity");
    select.projection = vec![SelectItem::ExprWithAlias {
        expr: column(target_alias, &rowid),
        alias: identity.clone(),
    }];
    let images = columns
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let deleted = Ident::with_quote('"', format!("__msduck_deleted_{index}"));
            let inserted = Ident::with_quote('"', format!("__msduck_inserted_{index}"));
            let old = column(target_alias, name);
            select.projection.push(SelectItem::ExprWithAlias {
                expr: old.clone(),
                alias: deleted.clone(),
            });
            select.projection.push(SelectItem::ExprWithAlias {
                expr: assignments
                    .get(&name.value.to_lowercase())
                    .cloned()
                    .unwrap_or(old),
                alias: inserted.clone(),
            });
            ColumnImage {
                column: name.clone(),
                deleted,
                inserted,
            }
        })
        .collect::<Vec<_>>();
    let Statement::Query(source) =
        crate::batch::parse("SELECT 1 FROM __image_source AS __image_alias")?.remove(0)
    else {
        unreachable!()
    };
    let SetExpr::Select(mut source) = *source.body else {
        unreachable!()
    };
    let TableFactor::Table {
        name,
        alias: Some(alias),
        ..
    } = &mut source.from[0].relation
    else {
        unreachable!()
    };
    *name = relation;
    alias.name = image_alias.clone();
    let mut write = update.clone();
    write.output = None;
    write.returning = None;
    write.from = Some(UpdateTableFromKind::AfterSet(source.from.clone()));
    write.selection = Some(Expr::BinaryOp {
        left: Box::new(column(target_alias, &rowid)),
        op: BinaryOperator::Eq,
        right: Box::new(column(&image_alias, &identity)),
    });
    for assignment in &mut write.assignments {
        let AssignmentTarget::ColumnName(name) = &assignment.target else {
            unreachable!()
        };
        let name = name.0.last().unwrap().as_ident().unwrap();
        let image = images
            .iter()
            .find(|image| image.column.value.eq_ignore_ascii_case(&name.value))
            .unwrap();
        assignment.value = column(&image_alias, &image.inserted);
    }
    Ok(Plan {
        capture,
        write,
        columns: images,
        source: source.from.remove(0),
        alias: image_alias,
        source_columns: vec![],
    })
}

/// Build paired images from a previously materialized joined candidate relation.
/// The UPDATE target must already be resolved to its physical relation and RHS
/// columns must use the candidate plan's canonical references. DEFAULT expansion
/// and storage conversions must finish before capture executes. Adapters binding
/// parameters per stage may finish conversion on assigned inserted slots after
/// translating this plan; private storage never supplies logical declarations.
///
/// Candidate selection (including its WITH/predicate) is deliberately absent
/// here: re-running it could select a different source or repeat volatile work.
/// Preserve source slots alongside old/new target images for subsequent OUTPUT
/// binding. The adapter materializes this capture before executing the write.
pub fn from_candidates(
    update: &Update,
    columns: &[Ident],
    candidates: &crate::output_join::Plan,
    candidate_relation: ObjectName,
    image_relation: ObjectName,
    image_alias: Ident,
) -> Result<Plan> {
    from_candidates_using(
        update,
        columns,
        candidates,
        candidate_relation,
        image_relation,
        image_alias.clone(),
        |expression| candidates.rebind(expression, image_alias.clone()),
    )
}

/// Bind unqualified and correlated assignment references over explicit snapshots.
pub fn from_scoped_candidates(
    update: &Update,
    columns: &[Ident],
    context: crate::output_bind::Context<'_>,
    candidate_relation: ObjectName,
    image_relation: ObjectName,
    image_alias: Ident,
) -> Result<Plan> {
    from_candidates_using(
        update,
        columns,
        context.candidates,
        candidate_relation,
        image_relation,
        image_alias.clone(),
        |expression| {
            crate::output_bind::rebind(
                context.candidates,
                expression,
                context.catalog,
                context.scope,
                image_alias.clone(),
            )
        },
    )
}

fn from_candidates_using(
    update: &Update,
    columns: &[Ident],
    candidates: &crate::output_join::Plan,
    candidate_relation: ObjectName,
    image_relation: ObjectName,
    image_alias: Ident,
    rebind: impl Fn(&Expr) -> Result<Expr>,
) -> Result<Plan> {
    let target = crate::output_target::qualifier(&update.table.relation)?;
    let mut unjoined = update.clone();
    unjoined.from = None;
    unjoined.selection = None;
    let mut result = plan(&unjoined, columns, image_relation, image_alias.clone())?;
    let SetExpr::Select(select) = result.capture.body.as_mut() else {
        unreachable!()
    };
    select.from = vec![result.source.clone()];
    let TableFactor::Table { name, .. } = &mut select.from[0].relation else {
        unreachable!()
    };
    *name = candidate_relation;
    select.projection = vec![SelectItem::ExprWithAlias {
        expr: column(&image_alias, &candidates.identity),
        alias: Ident::with_quote('"', "__msduck_identity"),
    }];
    for image in &result.columns {
        let mut reference = target.clone();
        reference.push(image.column.clone());
        let slot = candidates.slot(&reference).ok_or_else(|| {
            anyhow::anyhow!(
                "target column '{}' missing from joined candidates",
                image.column
            )
        })?;
        let old = column(&image_alias, slot);
        let assignment = update.assignments.iter().find(|assignment| {
            let AssignmentTarget::ColumnName(name) = &assignment.target else {
                return false;
            };
            name.0
                .last()
                .and_then(|part| part.as_ident())
                .is_some_and(|id| id.value.eq_ignore_ascii_case(&image.column.value))
        });
        let new = match assignment {
            Some(assignment) => rebind(&assignment.value)?,
            None => old.clone(),
        };
        select.projection.push(SelectItem::ExprWithAlias {
            expr: old,
            alias: image.deleted.clone(),
        });
        select.projection.push(SelectItem::ExprWithAlias {
            expr: new,
            alias: image.inserted.clone(),
        });
    }
    select.projection.extend(
        candidates
            .columns
            .iter()
            .map(|source| SelectItem::ExprWithAlias {
                expr: column(&image_alias, &source.slot),
                alias: source.slot.clone(),
            }),
    );
    result.source_columns = candidates
        .columns
        .iter()
        .filter(|source| !same_ids(&source.reference[..source.reference.len() - 1], &target))
        .map(|source| crate::output_join::Column {
            reference: source.reference.clone(),
            slot: source.slot.clone(),
        })
        .collect();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn update(sql: &str) -> Update {
        let Statement::Update(update) = crate::batch::parse(sql).unwrap().remove(0) else {
            panic!()
        };
        update
    }
    fn images(update: &Update, names: &[&str]) -> Result<Plan> {
        plan(
            update,
            &names
                .iter()
                .map(|name| Ident::with_quote('"', *name))
                .collect::<Vec<_>>(),
            ObjectName::from(vec![Ident::with_quote('"', "private images")]),
            Ident::new("images"),
        )
    }
    #[test]
    fn joined_images_consume_captured_rows_without_repeating_selection() {
        let Statement::Update(update) = sqlparser::parser::Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "UPDATE t SET n=s.extra+@p FROM s WHERE s.id=t.id AND nextval('selection')>0",
        )
        .unwrap()
        .remove(0) else {
            unreachable!()
        };
        let original = update.clone();
        let resolved = crate::output_target::resolve(
            &update,
            &std::collections::HashMap::from([("t".into(), 1), ("s".into(), 2)]),
        )
        .unwrap();
        let reference = |table, name| vec![Ident::new(table), Ident::new(name)];
        let candidates = crate::output_join::plan(
            None,
            resolved.sources,
            update.selection.clone(),
            reference("t", "rowid"),
            vec![
                reference("t", "id"),
                reference("t", "n"),
                reference("s", "id"),
                reference("s", "extra"),
            ],
        )
        .unwrap();
        let build = |update, columns: &[Ident]| {
            from_candidates(
                update,
                columns,
                &candidates,
                ObjectName::from(vec![Ident::new("candidates")]),
                ObjectName::from(vec![Ident::new("images")]),
                Ident::new("captured"),
            )
        };
        let paired = build(&update, &[Ident::new("id"), Ident::new("n")]).unwrap();
        let capture = paired.capture.to_string();
        assert!(!capture.contains("nextval") && !capture.contains(" WHERE "));
        assert!(capture.contains("captured.\"__msduck_source_3\" + @p"));
        assert!(capture.contains("captured.\"__msduck_source_0\" AS \"__msduck_inserted_0\""));
        assert!(!paired.write.to_string().contains("@p"));
        assert!(candidates.capture.to_string().contains("nextval"));
        assert_eq!(update, original);
        assert!(
            build(
                &update,
                &[Ident::new("id"), Ident::new("n"), Ident::new("missing")]
            )
            .is_err()
        );
        let mut unbound = update.clone();
        unbound.assignments[0].value = Expr::Identifier(Ident::new("extra"));
        let before = unbound.clone();
        assert!(build(&unbound, &[Ident::new("id"), Ident::new("n")]).is_err());
        assert_eq!(unbound, before);
    }
    #[test]
    fn joined_projection_preserves_qualified_sources_labels_and_image_precedence() {
        let update = update("UPDATE t SET n=1");
        let mut paired = images(&update, &["id", "n"]).unwrap();
        // The stored map is the same one retained by from_candidates. Keep
        // multi-part names distinct from single quoted identifiers containing dots.
        paired.source_columns = vec![
            crate::output_join::Column {
                reference: vec![Ident::new("dbo"), Ident::new("s"), Ident::new("n")],
                slot: Ident::new("source0"),
            },
            crate::output_join::Column {
                reference: vec![
                    Ident::with_quote('[', "s.dot"),
                    Ident::with_quote('[', "n.dot"),
                ],
                slot: Ident::new("source1"),
            },
            crate::output_join::Column {
                reference: vec![Ident::new("inserted"), Ident::new("n")],
                slot: Ident::new("shadow"),
            },
        ];
        let items = |sql: &str| {
            let Statement::Query(query) = crate::batch::parse(sql).unwrap().remove(0) else {
                unreachable!()
            };
            let SetExpr::Select(select) = *query.body else {
                unreachable!()
            };
            select.projection
        };
        let input = items(
            "SELECT deleted.n,inserted.n,dbo.s.*,[s.dot].*,[s.dot].[n.dot] AS renamed,inserted.n+dbo.s.n+@p AS mixed",
        );
        let before = input.clone();
        let query = paired.projection(&input).unwrap();
        let sql = query.to_string();
        assert!(sql.contains("images.source0 AS n"));
        assert!(sql.contains("images.source1 AS [n.dot]"));
        assert!(sql.contains("images.source1 AS renamed"));
        assert!(sql.contains("images.\"__msduck_inserted_1\" + images.source0 + @p AS mixed"));
        assert!(!sql.contains("shadow"));
        assert_eq!(input, before);
        for (sql, number) in [
            ("SELECT deleted.n,dbo.s.missing", 207),
            ("SELECT absent.n", 4104),
            ("SELECT absent.*", 107),
            ("SELECT t.id", 4104),
            ("SELECT t.*", 107),
        ] {
            let input = items(sql);
            let before = input.clone();
            let error = paired.projection(&input).err().unwrap();
            assert_eq!(
                error
                    .downcast_ref::<msduck_core::diagnostic::SqlError>()
                    .unwrap()
                    .number,
                number
            );
            assert_eq!(input, before);
        }
        assert!(
            paired
                .projection(&items("SELECT (SELECT dbo.s.n FROM other s)"))
                .is_err()
        );
    }
    #[test]
    fn captures_simultaneous_assignments_and_uses_identity_only_during_write() {
        let original = update(
            "UPDATE t SET id=id+100,n=nextval('counter'),a=b,b=a OUTPUT deleted.id,inserted.id WHERE n>0",
        );
        let before = original.clone();
        let plan = images(&original, &["id", "n", "a", "b", "unchanged"]).unwrap();
        let capture = plan.capture.to_string();
        let write = plan.write.to_string();
        assert_eq!(capture.matches("nextval").count(), 1);
        assert!(!write.contains("nextval"));
        assert!(!write.contains("n > 0"));
        assert!(capture.contains("WHERE n > 0"));
        assert!(capture.contains("b AS \"__msduck_inserted_2\""));
        assert!(capture.contains("a AS \"__msduck_inserted_3\""));
        assert!(capture.contains("t.\"unchanged\" AS \"__msduck_inserted_4\""));
        assert!(write.contains("t.rowid = images.\"__msduck_identity\""));
        assert!(plan.write.output.is_none() && plan.write.returning.is_none());
        assert_eq!(plan.columns[0].deleted.value, "__msduck_deleted_0");
        assert_eq!(original, before);
    }
    #[test]
    fn rejects_ambiguous_identity_and_unresolved_row_selection() {
        for (sql, columns) in [
            ("UPDATE t SET id=1", vec!["id", "rowid"]),
            ("UPDATE t SET id=1,id=2", vec!["id"]),
            ("UPDATE t SET missing=1", vec!["id"]),
            ("UPDATE t SET id=s.id FROM s", vec!["id"]),
        ] {
            let original = update(sql);
            let before = original.clone();
            assert!(images(&original, &columns).is_err(), "{sql}");
            assert_eq!(original, before);
        }
    }

    #[test]
    fn projection_expands_both_images_preserving_labels_parameters_and_inputs() {
        let update = update(
            "UPDATE t SET id=id+1 OUTPUT deleted.*,inserted.*,deleted.id,inserted.[odd name] AS renamed,inserted.id-deleted.id+@p AS delta",
        );
        let plan = images(&update, &["id", "odd name"]).unwrap();
        let Some(OutputClause::Output { select_items, .. }) = &update.output else {
            unreachable!()
        };
        let original = select_items.clone();
        let query = plan.projection(select_items).unwrap();
        let SetExpr::Select(select) = query.body.as_ref() else {
            unreachable!()
        };
        assert_eq!(select.projection.len(), 7);
        let names = select
            .projection
            .iter()
            .map(|item| match item {
                SelectItem::ExprWithAlias { alias, .. } => alias.value.as_str(),
                _ => panic!("expected retained result label"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["id", "odd name", "id", "odd name", "id", "renamed", "delta"]
        );
        let sql = query.to_string();
        assert!(sql.contains("@p"));
        assert!(sql.contains("images.\"__msduck_inserted_0\" - images.\"__msduck_deleted_0\""));
        assert_eq!(*select_items, original);
        let logical =
            logical_projection(ObjectName::from(vec![Ident::new("t")]), original).unwrap();
        assert!(
            logical
                .to_string()
                .contains("t AS inserted, t AS deleted WHERE 1 = 0")
        );
        for (item, number) in [("deleted.missing", 207), ("joined.id", 4104)] {
            let Statement::Query(query) = crate::batch::parse(&format!("SELECT {item}"))
                .unwrap()
                .remove(0)
            else {
                unreachable!()
            };
            let SetExpr::Select(select) = query.body.as_ref() else {
                unreachable!()
            };
            let error = plan.projection(&select.projection).err().unwrap();
            assert_eq!(
                error
                    .downcast_ref::<msduck_core::diagnostic::SqlError>()
                    .unwrap()
                    .number,
                number
            );
        }
    }
}
