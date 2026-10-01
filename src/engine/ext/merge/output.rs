//! MERGE OUTPUT: `$action`, `inserted`, `deleted` and source columns.
//!
//! Metadata binds a T-SQL query in which `inserted` and `deleted` are the
//! target table and `$action` is an NVARCHAR(10) column; outer joins make
//! an image nullable when some clause lacks it (an INSERT has no deleted
//! row, a DELETE no inserted row). Execution lowers that same query and
//! replaces its FROM clause with the materialized images, one row per
//! action.
use super::{Arm, Family, Kind, Target, native_query, quoted};
use crate::engine::{Execution, Parameter, Session};
use anyhow::{Result, bail};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::{collections::HashMap, ops::ControlFlow};

const ACTION: &str = "__msduck_merge_action";

pub(super) struct Output {
    query: Query,
    fields: Vec<crate::query_catalog::Field>,
    types: Vec<Option<crate::tds::Type>>,
    sink: Option<crate::output_sink::Bound>,
    columns: Vec<String>,
    source: Ident,
}

/// Replace `$action` with the action column, and reject column references
/// that name neither an image nor the source, as SQL Server does (4104).
struct Items<'a> {
    source: &'a str,
    depth: usize,
}
impl VisitorMut for Items<'_> {
    type Break = anyhow::Error;
    fn pre_visit_query(&mut self, _: &mut Query) -> ControlFlow<anyhow::Error> {
        self.depth += 1;
        ControlFlow::Continue(())
    }
    fn post_visit_query(&mut self, _: &mut Query) -> ControlFlow<anyhow::Error> {
        self.depth -= 1;
        ControlFlow::Continue(())
    }
    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<anyhow::Error> {
        match expr {
            Expr::Value(ValueWithSpan {
                value: Value::Placeholder(name),
                ..
            }) if name.eq_ignore_ascii_case("$action") => {
                *expr = Expr::CompoundIdentifier(vec![
                    Ident::with_quote('[', ACTION),
                    Ident::with_quote('[', "action"),
                ]);
            }
            Expr::CompoundIdentifier(parts)
                if self.depth == 0
                    && parts.len() == 2
                    && ![ACTION, "inserted", "deleted", self.source]
                        .iter()
                        .any(|name| parts[0].value.eq_ignore_ascii_case(name)) =>
            {
                let name = parts
                    .iter()
                    .map(|part| part.value.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                return ControlFlow::Break(
                    SqlError::new(
                        4104,
                        1,
                        format!("The multi-part identifier \"{name}\" could not be bound."),
                    )
                    .into(),
                );
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

impl Output {
    #[allow(clippy::too_many_arguments)]
    pub fn bind(
        session: &mut Session,
        clause: &OutputClause,
        target: &Target,
        arms: &[Arm],
        table: &TableFactor,
        source: &TableFactor,
        with: Option<With>,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Self> {
        let OutputClause::Output {
            select_items,
            into_table,
            ..
        } = clause
        else {
            bail!("unsupported MERGE RETURNING clause");
        };
        let mut items = select_items.clone();
        // Result positions of bare `$action` items, which SQL Server
        // declares NOT NULL. Wildcards over the images expand to the target's
        // columns; one over the source is sized from the bound fields below.
        let mut actions = Vec::new();
        let mut position = 0usize;
        let mut source_wildcard = None;
        for item in &mut items {
            let action = |expr: &Expr| matches!(expr, Expr::Value(ValueWithSpan { value: Value::Placeholder(name), .. }) if name.eq_ignore_ascii_case("$action"));
            match item {
                SelectItem::UnnamedExpr(expr) if action(expr) => {
                    *item = SelectItem::ExprWithAlias {
                        expr: expr.clone(),
                        alias: Ident::with_quote('[', "$action"),
                    };
                    actions.push((position, source_wildcard.is_some()));
                    position += 1;
                }
                SelectItem::ExprWithAlias { expr, .. } if action(expr) => {
                    actions.push((position, source_wildcard.is_some()));
                    position += 1;
                }
                SelectItem::UnnamedExpr(_) | SelectItem::ExprWithAlias { .. } => position += 1,
                SelectItem::QualifiedWildcard(
                    SelectItemQualifiedWildcardKind::ObjectName(name),
                    _,
                ) if ["inserted", "deleted"]
                    .iter()
                    .any(|image| name.to_string().eq_ignore_ascii_case(image)) =>
                {
                    position += target.columns.len();
                }
                _ => source_wildcard = Some(position),
            }
        }
        let qualifier = super::qualifier(source).unwrap_or_else(|| Ident::new("source"));
        if let ControlFlow::Break(error) = VisitMut::visit(
            &mut items,
            &mut Items {
                source: &qualifier.value,
                depth: 0,
            },
        ) {
            return Err(error);
        }
        let has_kind = |kind| arms.iter().any(|arm| arm.kind == kind);
        let join = |nullable: bool| {
            let on = JoinConstraint::On(Expr::BinaryOp {
                left: Box::new(Expr::value(Value::Number("1".into(), false))),
                op: BinaryOperator::Eq,
                right: Box::new(Expr::value(Value::Number("1".into(), false))),
            });
            if nullable {
                JoinOperator::LeftOuter(on)
            } else {
                JoinOperator::Inner(on)
            }
        };
        let image = |alias: &str| {
            let mut factor = table.clone();
            if let TableFactor::Table {
                alias: a,
                with_hints,
                ..
            } = &mut factor
            {
                *a = Some(TableAlias {
                    explicit: true,
                    name: Ident::with_quote('[', alias),
                    columns: vec![],
                    at: None,
                });
                with_hints.clear();
            }
            factor
        };
        let Statement::Query(mut query) = msduck_sql::batch::parse(&format!(
            "SELECT 1 FROM (SELECT CAST(N'INSERT' AS NVARCHAR(10)) AS [i], CAST(N'UPDATE' AS NVARCHAR(10)) AS [u], CAST(N'DELETE' AS NVARCHAR(10)) AS [d], CAST(N'INSERT' AS NVARCHAR(10)) AS [action]) AS [{ACTION}]"
        ))?
        .remove(0) else {
            unreachable!("OUTPUT template")
        };
        query.with = with;
        let SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!("OUTPUT template")
        };
        select.projection = items;
        select.from[0].joins = vec![
            Join {
                relation: image("inserted"),
                global: false,
                join_operator: join(has_kind(Kind::Delete)),
            },
            Join {
                relation: image("deleted"),
                global: false,
                join_operator: join(has_kind(Kind::Insert)),
            },
            Join {
                relation: source.clone(),
                global: false,
                join_operator: join(arms.iter().any(|arm| arm.family == Family::BySource)),
            },
        ];
        let mut bound = query.clone();
        session.lower_database_functions(&mut bound)?;
        session.qualify_databases(&mut bound)?;
        let mut fields =
            crate::query_catalog::bind_query_with_parameters(&session.db, &mut bound, parameters)
                .map_err(crate::query_error::compilation)?
                .unwrap_or_default();
        let expanded = fields.len().saturating_sub(position);
        for (position, after_wildcard) in actions {
            let index = position + if after_wildcard { expanded } else { 0 };
            if let Some(field) = fields.get_mut(index) {
                field.properties.nullable = Some(false);
            }
        }
        let types = crate::result_types::bound_projection(
            &session.db,
            &Statement::Query(bound),
            parameters,
        )?;
        let sink = into_table
            .as_ref()
            .map(|into| {
                let sink = msduck_sql::output::Sink::parse(into)?;
                let sink = crate::output_sink::bind(&session.db, &sink, &fields)?;
                session.validate_prepared_statements(
                    vec![sink.statement.clone()],
                    &sink.declarations,
                )?;
                Ok::<_, anyhow::Error>(sink)
            })
            .transpose()?;
        let source = super::qualifier(source).unwrap_or_else(|| Ident::new("source"));
        Ok(Self {
            query: *query,
            fields,
            types,
            sink,
            columns: target.columns.iter().map(|c| c.name.clone()).collect(),
            source,
        })
    }

    /// Lower the OUTPUT projection over the images and prepare it, before
    /// any write, so binding errors cannot follow a write.
    pub fn prepare(
        &self,
        session: &mut Session,
        images: &str,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Prepared> {
        let (mut lowered, values, scopes) =
            session.lower_query(Statement::Query(Box::new(self.query.clone())), parameters)?;
        let Statement::Query(query) = &mut lowered else {
            bail!("MERGE OUTPUT lowered to a non-query")
        };
        let SetExpr::Select(select) = query.body.as_mut() else {
            bail!("MERGE OUTPUT lowered to a non-SELECT")
        };
        let actions = match &select.from.first().map(|from| &from.relation) {
            Some(TableFactor::Derived { subquery, .. }) => match subquery.body.as_ref() {
                SetExpr::Select(actions) => actions
                    .projection
                    .iter()
                    .take(3)
                    .map(|item| item_expr(item).to_string())
                    .collect::<Vec<_>>(),
                _ => bail!("MERGE OUTPUT action shape changed during lowering"),
            },
            _ => bail!("MERGE OUTPUT action shape changed during lowering"),
        };
        let [insert, update, delete] = actions.as_slice() else {
            bail!("MERGE OUTPUT action shape changed during lowering")
        };
        let (internal, source_columns): (Vec<_>, Vec<_>) = {
            let mut statement = session
                .db
                .prepare(&format!("SELECT * FROM {images} LIMIT 0"))?;
            statement.execute([])?;
            statement
                .column_names()
                .into_iter()
                .partition(|name| name.starts_with("__msduck_"))
        };
        // Source columns get internal names in `__o`, so an unqualified
        // source column in OUTPUT resolves only through the source alias.
        let images = format!(
            "(SELECT {} FROM {images})",
            internal
                .iter()
                .map(|name| quoted(name))
                .chain(
                    source_columns
                        .iter()
                        .enumerate()
                        .map(|(index, name)| format!("{} AS \"__msduck_s{index}\"", quoted(name)))
                )
                .collect::<Vec<_>>()
                .join(", ")
        );
        let image = |prefix: &str| {
            self.columns
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    format!("__o.\"__msduck_{prefix}{index}\" AS {}", quoted(name))
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let source = if source_columns.is_empty() {
            "NULL AS \"__msduck_none\"".to_owned()
        } else {
            source_columns
                .iter()
                .enumerate()
                .map(|(index, name)| format!("__o.\"__msduck_s{index}\" AS {}", quoted(name)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let native = native_query(&format!(
            "SELECT 1 FROM {images} AS __o, \
             LATERAL (SELECT CASE __o.__msduck_kind WHEN 1 THEN {insert} WHEN 2 THEN {update} ELSE {delete} END AS \"action\") AS {}, \
             LATERAL (SELECT {}) AS \"inserted\", \
             LATERAL (SELECT {}) AS \"deleted\", \
             LATERAL (SELECT {source}) AS {} \
             ORDER BY __o.__msduck_row",
            quoted(ACTION),
            image("n"),
            image("d"),
            quoted(&self.source.value),
        ))?;
        let SetExpr::Select(from) = native.body.as_ref() else {
            unreachable!("native template")
        };
        select.from = from.from.clone();
        query.order_by = native.order_by.clone();
        let values = renumber(&mut lowered, &values)?;
        let sql = lowered.to_string();
        session.db.prepare(&sql)?;
        Ok(Prepared {
            sql,
            values,
            _scopes: scopes,
        })
    }

    /// Project the OUTPUT rows from the images and return them, or store
    /// them in the OUTPUT INTO table.
    pub fn emit(
        &self,
        session: &mut Session,
        prepared: Prepared,
        count: u64,
        command: u16,
    ) -> Result<Execution> {
        let Prepared { sql, values, .. } = &prepared;
        if let Some(sink) = &self.sink {
            let rows = {
                let mut statement = session.db.prepare(sql)?;
                let batches = statement
                    .query_arrow(duckdb::params_from_iter(values.iter()))
                    .map_err(|error| {
                        crate::output_sink::failed(
                            error.into(),
                            msduck_sql::output::Operation::Update,
                        )
                    })?;
                Session::collect_output_rows(batches, sink.declarations.len())?
            };
            drop(prepared);
            for row in rows {
                let mut bindings = sink
                    .declarations
                    .iter()
                    .zip(row)
                    .map(|((name, kind), value)| {
                        (
                            name.clone(),
                            Parameter {
                                data_type: *kind,
                                value,
                            },
                        )
                    })
                    .collect();
                session
                    .execute_inner(sink.statement.clone(), &mut bindings, false)
                    .map_err(|error| {
                        crate::output_sink::failed(error, msduck_sql::output::Operation::Update)
                    })?;
            }
            return Ok(Execution::statement(vec![], Some(count), command));
        }
        let mut statement = session.db.prepare(sql)?;
        let batches = statement.query_arrow(duckdb::params_from_iter(values.iter()))?;
        let schema = batches.get_schema();
        let (tokens, _) = Session::encode_batches(batches, &schema, &self.fields, &self.types)?;
        drop(statement);
        drop(prepared);
        Ok(Execution::result_set(tokens, Some(count), command))
    }
}

/// A lowered OUTPUT projection. Its RAND scopes live until it has run.
pub(super) struct Prepared {
    sql: String,
    values: Vec<duckdb::types::Value>,
    _scopes: Vec<crate::engine::rand::Scope>,
}

/// Keep only the bound values the query still references (replacing the
/// FROM clause drops the source's), renumbered in order of appearance.
fn renumber(
    statement: &mut Statement,
    values: &[duckdb::types::Value],
) -> Result<Vec<duckdb::types::Value>> {
    struct Renumber<'a> {
        values: &'a [duckdb::types::Value],
        map: HashMap<usize, usize>,
        kept: Vec<duckdb::types::Value>,
    }
    impl VisitorMut for Renumber<'_> {
        type Break = anyhow::Error;
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<anyhow::Error> {
            if let Expr::Value(ValueWithSpan {
                value: Value::Placeholder(slot),
                ..
            }) = expr
                && let Some(index) = slot.strip_prefix('$').and_then(|n| n.parse::<usize>().ok())
            {
                let Some(value) = index.checked_sub(1).and_then(|i| self.values.get(i)) else {
                    return ControlFlow::Break(anyhow::anyhow!(
                        "unbound MERGE OUTPUT value {slot}"
                    ));
                };
                let next = self.kept.len() + 1;
                let position = *self.map.entry(index).or_insert_with(|| {
                    self.kept.push(value.clone());
                    next
                });
                *slot = format!("${position}");
            }
            ControlFlow::Continue(())
        }
    }
    let mut renumber = Renumber {
        values,
        map: HashMap::new(),
        kept: Vec::new(),
    };
    if let ControlFlow::Break(error) = VisitMut::visit(statement, &mut renumber) {
        return Err(error);
    }
    Ok(renumber.kept)
}

fn item_expr(item: &SelectItem) -> &Expr {
    match item {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => expr,
        _ => unreachable!("action items are expressions"),
    }
}
