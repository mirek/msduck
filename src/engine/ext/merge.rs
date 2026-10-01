//! MERGE statements, including table hints.
//!
//! A MERGE runs in four steps, all inside one backend transaction (the
//! caller's, or one opened for the statement):
//!
//! 1. **Candidates.** One T-SQL query joins the target and the source with the
//!    ON condition (INNER, LEFT, RIGHT or FULL, depending on which WHEN
//!    families exist). It passes through the ordinary query pipeline, so
//!    names, variables, parameters, collations and conversions bind exactly
//!    as in a SELECT. Natively, the query gains the target's physical row id
//!    and a source row marker, every row is classified into the first WHEN
//!    clause whose family and condition match, and the action values are
//!    converted to the target columns' storage types. The result is
//!    materialized once, so no clause or value is evaluated after a write.
//! 2. **Selection.** `TOP` keeps the first n (or n percent) action rows. A
//!    target row that would be updated or deleted twice fails with 8672
//!    before anything is written.
//! 3. **Images.** The new image of every selected row is materialized:
//!    assigned values, the pre-image for unassigned columns of an update,
//!    and defaults (including IDENTITY values) for omitted insert columns.
//! 4. **Writes.** DELETE, then one UPDATE per update clause, then one INSERT
//!    per insert clause, each keyed by the row ids captured in step 1. OUTPUT
//!    rows are projected from the images, so `$action`, `inserted`,
//!    `deleted` and source columns refer to the same row.
//!
//! See docs/gaps-merge.md for behavior and limits.
use super::Feature;
use crate::engine::{Execution, Parameter, Session, Translator, rand};
use anyhow::{Result, anyhow, bail, ensure};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::*;
use std::{
    collections::HashMap,
    ops::ControlFlow,
    sync::atomic::{AtomicU64, Ordering},
};

mod output;
mod target;

use target::Target;

#[derive(Default)]
pub(crate) struct State;

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "merge"
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        let (with, merge) = match statement {
            Statement::Merge(merge) => (None, merge.clone()),
            Statement::Query(query) => match query.body.as_ref() {
                SetExpr::Merge(Statement::Merge(merge)) => (query.with.clone(), merge.clone()),
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        execute(session, with, merge, parameters).map(Some)
    }
}

/// DONE CurCmd of a MERGE statement.
const COMMAND: u16 = 279;

const DUPLICATE: &str = "The MERGE statement attempted to UPDATE or DELETE the same row more than once. This happens when a target row matches more than one source row. A MERGE statement cannot UPDATE/DELETE the same row of the target table multiple times. Refine the ON clause to ensure a target row matches at most one source row, or use the GROUP BY clause to group the source rows.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Matched,
    ByTarget,
    BySource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Insert = 1,
    Update = 2,
    Delete = 3,
}

/// Where an assigned or inserted column's value comes from.
#[derive(Clone, Debug)]
enum Assigned {
    /// The converted candidate value `__msduck_v{n}`.
    Stage(usize),
    /// The column's default (DEFAULT keyword or an omitted insert column).
    Default,
}

struct Arm {
    family: Family,
    /// Index of the `__msduck_p{n}` predicate column.
    predicate: Option<usize>,
    kind: Kind,
    /// Target column index and value, in statement order.
    values: Vec<(usize, Assigned)>,
}

static NEXT: AtomicU64 = AtomicU64::new(1);

fn ident(name: &str) -> Ident {
    Ident::with_quote('"', name)
}

fn quoted(name: &str) -> String {
    ident(name).to_string()
}

/// A statement-owned temporary table name.
fn scratch(role: &str) -> String {
    format!(
        "temp.main.{}",
        quoted(&format!(
            "__msduck_merge_{role}_{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    )
}

fn execute(
    session: &mut Session,
    with: Option<With>,
    merge: Merge,
    parameters: &mut HashMap<String, Parameter>,
) -> Result<Execution> {
    if session.transaction_doomed {
        return Err(crate::query_error::attach_context(
            session.require_committable().unwrap_err(),
            vec![],
            0xc5,
        ));
    }
    let top = msduck_sql::merge::top_clause(&merge)?;
    let plan = Plan::bind(session, with, merge, parameters)?;
    let own = session.transactions == 0;
    if own {
        session.db.execute_batch("BEGIN TRANSACTION")?;
    }
    let result = plan.run(session, top, parameters);
    match &result {
        Ok(_) if own => {
            if let Err(error) = session.db.execute_batch("COMMIT") {
                let _ = session.db.execute_batch("ROLLBACK");
                return Err(error.into());
            }
        }
        Ok(_) => {}
        Err(_) if own => {
            let _ = session.db.execute_batch("ROLLBACK");
        }
        Err(error) => {
            if error
                .downcast_ref::<SqlError>()
                .is_some_and(|error| error.number == 8672)
            {
                // SQL Server rolls the caller's transaction back for 8672,
                // even with XACT_ABORT OFF.
                let tokens = session.rollback_transaction("")?;
                return Err(super::Partial {
                    tokens,
                    error: result.err().unwrap(),
                }
                .into());
            }
            // A failed native statement aborts the backend transaction, which
            // cannot be resumed, and a failure after the first write cannot be
            // undone without the caller's earlier work. Report either as
            // uncommittable (XACT_STATE -1) rather than keep a partial MERGE.
            if plan.written.get() || session.db.execute_batch("SELECT 1").is_err() {
                session.transaction_doomed = true;
            }
        }
    }
    result
}

struct Plan {
    target: Target,
    /// Target and source qualifiers, as written (alias or table name).
    target_qualifier: Ident,
    source_qualifier: Ident,
    arms: Vec<Arm>,
    /// Candidate query in T-SQL, before lowering.
    candidates: Query,
    /// Number of `__msduck_p{n}` and `__msduck_v{n}` columns.
    predicates: usize,
    values: usize,
    /// Whether each staged value is money, which changes its conversion.
    money: Vec<bool>,
    output: Option<output::Output>,
    /// Set once the first write starts.
    written: std::cell::Cell<bool>,
}

impl Plan {
    fn bind(
        session: &mut Session,
        with: Option<With>,
        merge: Merge,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Self> {
        let mut table = merge.table.clone();
        session.qualify_databases(&mut table)?;
        let TableFactor::Table {
            name, alias, args, ..
        } = &table
        else {
            bail!("unsupported MERGE target: {}", merge.table);
        };
        ensure!(args.is_none(), "unsupported MERGE target: {}", merge.table);
        ensure!(
            alias.as_ref().is_none_or(|alias| alias.columns.is_empty()),
            "unsupported MERGE target alias column list"
        );
        let target = Target::load(session, name)?;
        let target_qualifier = alias.as_ref().map_or_else(
            || name.0.last().and_then(|p| p.as_ident()).cloned().unwrap(),
            |alias| alias.name.clone(),
        );
        let source_qualifier = match &merge.source {
            TableFactor::Table {
                alias: Some(alias), ..
            }
            | TableFactor::Derived {
                alias: Some(alias), ..
            }
            | TableFactor::Function {
                alias: Some(alias), ..
            } => alias.name.clone(),
            TableFactor::Table { name, .. } => name
                .0
                .last()
                .and_then(|p| p.as_ident())
                .cloned()
                .ok_or_else(|| anyhow!("unsupported MERGE source {}", merge.source))?,
            TableFactor::Derived { .. } => {
                return Err(SqlError::syntax(102, 1, "Incorrect syntax near 'ON'.").into());
            }
            _ => bail!("unsupported MERGE source {}", merge.source),
        };
        if target_qualifier
            .value
            .eq_ignore_ascii_case(&source_qualifier.value)
        {
            return Err(SqlError::new(
                1011,
                1,
                format!(
                    "The correlation name '{}' is specified multiple times in a FROM clause.",
                    source_qualifier.value
                ),
            )
            .into());
        }

        // Candidate projection: clause predicates, then action values, then
        // the target pre-image, then (for OUTPUT) every source column.
        let mut projection = Vec::new();
        let mut predicates = 0;
        let mut values = 0;
        let mut arms = Vec::new();
        let mut money = Vec::new();
        let mut stage = |expr: Expr, projection: &mut Vec<SelectItem>| {
            money.push(crate::engine::money_expr(&expr, parameters));
            projection.push(SelectItem::ExprWithAlias {
                expr,
                alias: ident(&format!("__msduck_v{values}")),
            });
            values += 1;
            values - 1
        };
        let default = |expr: &Expr| matches!(expr, Expr::Identifier(id) if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("DEFAULT"));
        for clause in &merge.clauses {
            let family = match clause.clause_kind {
                MergeClauseKind::Matched => Family::Matched,
                MergeClauseKind::NotMatched | MergeClauseKind::NotMatchedByTarget => {
                    Family::ByTarget
                }
                MergeClauseKind::NotMatchedBySource => Family::BySource,
            };
            let predicate = clause.predicate.as_ref().map(|predicate| {
                projection.push(SelectItem::ExprWithAlias {
                    expr: Expr::Case {
                        case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                        end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                        operand: None,
                        conditions: vec![CaseWhen {
                            condition: predicate.clone(),
                            result: Expr::value(Value::Number("1".into(), false)),
                        }],
                        else_result: Some(Box::new(Expr::value(Value::Number("0".into(), false)))),
                    },
                    alias: ident(&format!("__msduck_p{predicates}")),
                });
                predicates += 1;
                predicates - 1
            });
            let (kind, assignments) = match &clause.action {
                MergeAction::Delete { .. } => (Kind::Delete, vec![]),
                MergeAction::Update(update) => {
                    ensure!(
                        update.update_predicate.is_none() && update.delete_predicate.is_none(),
                        "unsupported MERGE UPDATE ... WHERE"
                    );
                    let MergeUpdateKind::Set(assignments) = &update.kind else {
                        bail!("unsupported MERGE UPDATE SET *");
                    };
                    let mut columns = Vec::new();
                    for assignment in assignments {
                        let AssignmentTarget::ColumnName(column) = &assignment.target else {
                            bail!("unsupported MERGE assignment target {}", assignment.target);
                        };
                        let index = target.assigned(column, &target_qualifier)?;
                        ensure!(
                            !columns.iter().any(|(other, _)| *other == index),
                            SqlError::new(
                                264,
                                1,
                                format!(
                                    "The column name '{}' is specified more than once in the SET clause or column list of an INSERT. A column cannot be assigned more than one value in the same clause. Modify the clause to make sure that a column is updated only once. If this statement updates or inserts columns into a view, column aliasing can conceal the duplication in your code.",
                                    target.columns[index].name
                                )
                            )
                        );
                        let value = if default(&assignment.value) {
                            Assigned::Default
                        } else {
                            Assigned::Stage(stage(assignment.value.clone(), &mut projection))
                        };
                        columns.push((index, value));
                    }
                    (Kind::Update, columns)
                }
                MergeAction::Insert(insert) => {
                    let MergeInsertKind::Values(rows) = &insert.kind else {
                        bail!("unsupported MERGE INSERT form");
                    };
                    ensure!(
                        insert.insert_predicate.is_none(),
                        "unsupported MERGE INSERT ... WHERE"
                    );
                    let [row] = rows.rows.as_slice() else {
                        // SQL Server's grammar allows one VALUES row here.
                        return Err(SqlError::syntax(102, 1, "Incorrect syntax near ','.").into());
                    };
                    if msduck_sql::merge::is_default_values(&insert.columns, &row.content) {
                        (Kind::Insert, vec![])
                    } else {
                        let columns = target.insert_columns(&insert.columns)?;
                        if row.content.len() != columns.len() {
                            return Err(SqlError {
                            number: if row.content.len() > columns.len() { 110 } else { 109 },
                            state: 1,
                            severity: 15,
                            message_utf16: None,
                            message: if row.content.len() > columns.len() {
                                "There are fewer columns in the INSERT statement than values specified in the VALUES clause. The number of values in the VALUES clause must match the number of columns specified in the INSERT statement."
                            } else {
                                "There are more columns in the INSERT statement than values specified in the VALUES clause. The number of values in the VALUES clause must match the number of columns specified in the INSERT statement."
                            }
                            .into(),
                        }
                        .into());
                        }
                        let mut assigned = Vec::new();
                        for (index, expr) in columns.into_iter().zip(row.content.iter()) {
                            let value = if default(expr) {
                                Assigned::Default
                            } else {
                                Assigned::Stage(stage(expr.clone(), &mut projection))
                            };
                            assigned.push((index, value));
                        }
                        (Kind::Insert, assigned)
                    }
                }
                MergeAction::DoNothing { .. } => bail!("unsupported MERGE DO NOTHING"),
            };
            arms.push(Arm {
                family,
                predicate,
                kind,
                values: assignments,
            });
        }
        ensure!(
            !arms.is_empty(),
            "A MERGE statement must have at least one WHEN clause"
        );
        for (index, column) in target.columns.iter().enumerate() {
            projection.push(SelectItem::ExprWithAlias {
                expr: Expr::CompoundIdentifier(vec![
                    target_qualifier.clone(),
                    Ident::with_quote('[', &column.name),
                ]),
                alias: ident(&format!("__msduck_d{index}")),
            });
        }
        let output = merge
            .output
            .as_ref()
            .map(|clause| {
                output::Output::bind(
                    session,
                    clause,
                    &target,
                    &arms,
                    &merge.table,
                    &merge.source,
                    with.clone(),
                    parameters,
                )
            })
            .transpose()?;
        if output.is_some() {
            projection.push(SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(ObjectName::from(vec![
                    source_qualifier.clone(),
                ])),
                WildcardAdditionalOptions::default(),
            ));
        }

        let has = |family| arms.iter().any(|arm| arm.family == family);
        let on = merge.on.as_ref().clone();
        let join_operator = match (has(Family::ByTarget), has(Family::BySource)) {
            (false, false) => JoinOperator::Inner(JoinConstraint::On(on)),
            (true, false) => JoinOperator::RightOuter(JoinConstraint::On(on)),
            (false, true) => JoinOperator::LeftOuter(JoinConstraint::On(on)),
            (true, true) => JoinOperator::FullOuter(JoinConstraint::On(on)),
        };
        let Statement::Query(mut candidates) = msduck_sql::batch::parse("SELECT 1")?.remove(0)
        else {
            unreachable!("SELECT template")
        };
        candidates.with = with;
        let SetExpr::Select(select) = candidates.body.as_mut() else {
            unreachable!("SELECT template")
        };
        select.projection = projection;
        select.from = vec![TableWithJoins {
            relation: table.clone(),
            joins: vec![Join {
                relation: merge.source.clone(),
                global: false,
                join_operator,
            }],
        }];
        Ok(Self {
            target,
            target_qualifier,
            source_qualifier,
            arms,
            candidates: *candidates,
            predicates,
            values,
            money,
            output,
            written: std::cell::Cell::new(false),
        })
    }

    fn run(
        &self,
        session: &mut Session,
        top: Option<Top>,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Execution> {
        let stage = scratch("stage");
        let images = scratch("image");
        let result = self.run_with(session, top, parameters, &stage, &images);
        // After a native failure the backend transaction is aborted and its
        // rollback removes these tables instead.
        for name in [stage, images] {
            let _ = session
                .db
                .execute_batch(&format!("DROP TABLE IF EXISTS {name}"));
        }
        result
    }

    fn run_with(
        &self,
        session: &mut Session,
        top: Option<Top>,
        parameters: &mut HashMap<String, Parameter>,
        stage: &str,
        images: &str,
    ) -> Result<Execution> {
        let (lowered, values, scopes) = session.lower_query(
            Statement::Query(Box::new(self.candidates.clone())),
            parameters,
        )?;
        let candidates = self.native_candidates(lowered)?;
        let sql = self.stage_sql(stage, &candidates, &session.database().name)?;
        session
            .db
            .execute(&sql, duckdb::params_from_iter(values.iter()))
            .map_err(|error| storage_failure(error.into()))?;
        drop(scopes);

        // TOP keeps the first action rows; their order is unspecified.
        let eligible: i64 =
            session
                .db
                .query_row(&format!("SELECT count(*) FROM {stage}"), [], |r| r.get(0))?;
        let take = match top {
            None => eligible as u64,
            Some(top) => session.top(top, eligible as u64, parameters)?,
        };
        // A target row matched by several source rows fails when one of its
        // actions is an UPDATE; repeated DELETEs remove it once.
        let updates = self
            .arms
            .iter()
            .enumerate()
            .filter(|(_, arm)| arm.kind == Kind::Update)
            .map(|(index, _)| (index + 1).to_string())
            .collect::<Vec<_>>();
        if !updates.is_empty() {
            let duplicate: bool = session.db.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {stage} WHERE __msduck_row <= ? AND __msduck_tid IS NOT NULL GROUP BY __msduck_tid HAVING count(*) > 1 AND bool_or(__msduck_arm IN ({})))",
                    updates.join(", ")
                ),
                [take as i64],
                |r| r.get(0),
            )?;
            if duplicate {
                return Err(SqlError::new(8672, 1, DUPLICATE).into());
            }
        }
        session
            .db
            .execute(&self.image_sql(images, stage)?, [take as i64])
            .map_err(write_failure)?;
        // Report constraint violations before writing, so a failure inside the
        // caller's transaction leaves it usable, as SQL Server does.
        self.target.check(
            session,
            images,
            session.transactions > 0,
            &session.database().name,
        )?;
        let output = self
            .output
            .as_ref()
            .map(|output| {
                Ok::<_, anyhow::Error>((output, output.prepare(session, images, parameters)?))
            })
            .transpose()?;
        self.written.set(true);
        let count = self.write(session, images)?;
        match output {
            None => Ok(Execution::statement(vec![], Some(count), COMMAND)),
            Some((output, prepared)) => output.emit(session, prepared, count, COMMAND),
        }
    }

    /// Add the target row id and the source marker to the lowered candidate
    /// query.
    fn native_candidates(&self, mut query: Statement) -> Result<Query> {
        let Statement::Query(query) = &mut query else {
            bail!("MERGE candidates lowered to a non-query")
        };
        let SetExpr::Select(select) = query.body.as_mut() else {
            bail!("MERGE candidates lowered to a non-SELECT")
        };
        let [from] = select.from.as_mut_slice() else {
            bail!("MERGE candidate source shape changed during lowering")
        };
        let [join] = from.joins.as_mut_slice() else {
            bail!("MERGE candidate source shape changed during lowering")
        };
        let target =
            qualifier(&from.relation).unwrap_or_else(|| ident(&self.target_qualifier.value));
        let source =
            qualifier(&join.relation).unwrap_or_else(|| ident(&self.source_qualifier.value));
        // Mark every source row so an unmatched target row (all source
        // columns NULL) is distinguishable, and give rows a stable order.
        let relation = std::mem::replace(
            &mut join.relation,
            TableFactor::Table {
                name: ObjectName::from(vec![Ident::new("x")]),
                alias: None,
                args: None,
                with_hints: vec![],
                version: None,
                with_ordinality: false,
                partitions: vec![],
                json_path: None,
                sample: None,
                index_hints: vec![],
            },
        );
        let mut wrapper = native_query(&format!(
            "SELECT *, row_number() OVER () AS \"__msduck_src\" FROM x AS {}",
            quoted(&source.value)
        ))?;
        let SetExpr::Select(inner) = wrapper.body.as_mut() else {
            unreachable!("native template")
        };
        inner.from[0].relation = relation;
        join.relation = TableFactor::Derived {
            lateral: false,
            subquery: Box::new(wrapper),
            alias: Some(TableAlias {
                explicit: true,
                name: source.clone(),
                columns: vec![],
                at: None,
            }),
            sample: None,
        };
        select.projection.push(SelectItem::ExprWithAlias {
            expr: Expr::CompoundIdentifier(vec![target, Ident::new("rowid")]),
            alias: ident("__msduck_tid"),
        });
        select.projection.push(SelectItem::ExprWithAlias {
            expr: Expr::CompoundIdentifier(vec![source, ident("__msduck_src")]),
            alias: ident("__msduck_seq"),
        });
        Ok(query.as_ref().clone())
    }

    fn arm_case(&self) -> String {
        let family = |family: Family| {
            let arms = self
                .arms
                .iter()
                .enumerate()
                .filter(|(_, arm)| arm.family == family)
                .map(|(index, arm)| {
                    let condition = arm.predicate.map_or_else(
                        || "TRUE".to_owned(),
                        |p| format!("{} = 1", quoted(&format!("__msduck_p{p}"))),
                    );
                    format!("WHEN {condition} THEN {}", index + 1)
                })
                .collect::<Vec<_>>();
            if arms.is_empty() {
                "NULL".to_owned()
            } else {
                format!("CASE {} END", arms.join(" "))
            }
        };
        format!(
            "CASE WHEN __msduck_tid IS NOT NULL AND __msduck_seq IS NOT NULL THEN {} WHEN __msduck_tid IS NOT NULL THEN {} WHEN __msduck_seq IS NOT NULL THEN {} END",
            family(Family::Matched),
            family(Family::BySource),
            family(Family::ByTarget)
        )
    }

    /// Classify and convert every candidate once.
    fn stage_sql(&self, name: &str, candidates: &Query, database: &str) -> Result<String> {
        let mut columns = vec![
            "row_number() OVER (ORDER BY __msduck_tid NULLS LAST, __msduck_seq) AS __msduck_row"
                .to_owned(),
            "__msduck_tid".to_owned(),
            "__msduck_arm".to_owned(),
        ];
        let mut converted = vec![None; self.values];
        for (index, arm) in self.arms.iter().enumerate() {
            for (column, value) in &arm.values {
                if let Assigned::Stage(slot) = value {
                    converted[*slot] = Some((index + 1, *column));
                }
            }
        }
        for (slot, conversion) in converted.into_iter().enumerate() {
            let (arm, column) = conversion.expect("every staged value belongs to an arm");
            let value = Expr::Identifier(ident(&format!("__msduck_v{slot}")));
            let value = self
                .target
                .convert(column, value, self.money[slot], database)?;
            columns.push(format!(
                "CASE WHEN __msduck_arm = {arm} THEN {value} END AS \"__msduck_v{slot}\""
            ));
        }
        for index in 0..self.target.columns.len() {
            columns.push(format!("\"__msduck_d{index}\""));
        }
        if self.output.is_some() {
            let internal = (0..self.predicates)
                .map(|p| format!("\"__msduck_p{p}\""))
                .chain((0..self.values).map(|v| format!("\"__msduck_v{v}\"")))
                .chain((0..self.target.columns.len()).map(|d| format!("\"__msduck_d{d}\"")))
                .chain(
                    [
                        "__msduck_tid",
                        "__msduck_seq",
                        "__msduck_src",
                        "__msduck_arm",
                    ]
                    .map(quoted),
                )
                .collect::<Vec<_>>()
                .join(",");
            columns.push(format!("__l2.* EXCLUDE ({internal})"));
        }
        Ok(format!(
            "CREATE TEMP TABLE {name} AS SELECT {} FROM (SELECT *, {} AS __msduck_arm FROM ({candidates}) AS __l1) AS __l2 WHERE __msduck_arm IS NOT NULL ORDER BY __msduck_row",
            columns.join(", "),
            self.arm_case()
        ))
    }

    /// The selected rows with their complete new images.
    fn image_sql(&self, name: &str, stage: &str) -> Result<String> {
        let kind = self
            .arms
            .iter()
            .enumerate()
            .map(|(index, arm)| format!("WHEN {} THEN {}", index + 1, arm.kind as u8))
            .collect::<Vec<_>>()
            .join(" ");
        let mut columns = vec![
            "__msduck_row".to_owned(),
            "__msduck_tid".to_owned(),
            "__msduck_arm".to_owned(),
            format!("CASE __msduck_arm {kind} END AS __msduck_kind"),
        ];
        for (index, column) in self.target.columns.iter().enumerate() {
            if column.generated {
                continue;
            }
            let mut branches = Vec::new();
            for (arm_index, arm) in self.arms.iter().enumerate() {
                let value = match arm.kind {
                    Kind::Delete => continue,
                    Kind::Update => match arm.values.iter().find(|(c, _)| *c == index) {
                        Some((_, Assigned::Stage(slot))) => format!("\"__msduck_v{slot}\""),
                        Some((_, Assigned::Default)) => self.target.default(index)?,
                        None => format!("\"__msduck_d{index}\""),
                    },
                    Kind::Insert => match arm.values.iter().find(|(c, _)| *c == index) {
                        Some((_, Assigned::Stage(slot))) => format!("\"__msduck_v{slot}\""),
                        Some((_, Assigned::Default)) | None => self.target.default(index)?,
                    },
                };
                branches.push(format!("WHEN {} THEN {value}", arm_index + 1));
            }
            let value = if branches.is_empty() {
                "NULL".to_owned()
            } else {
                format!("CASE __msduck_arm {} END", branches.join(" "))
            };
            columns.push(format!(
                "CAST({value} AS {}) AS \"__msduck_n{index}\"",
                column.physical
            ));
        }
        columns.push("__s.* EXCLUDE (__msduck_row, __msduck_tid, __msduck_arm)".to_owned());
        // Computed columns follow from the new image of the other columns.
        let bindings = self
            .target
            .columns
            .iter()
            .enumerate()
            .filter(|(_, column)| !column.generated)
            .map(|(index, column)| {
                format!("__in.\"__msduck_n{index}\" AS {}", quoted(&column.name))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut generated = Vec::new();
        for (index, column) in self.target.columns.iter().enumerate() {
            if !column.generated {
                continue;
            }
            generated.push(match column.expression() {
                Some(expression) => format!(
                    "CASE WHEN __in.__msduck_kind = 3 THEN NULL ELSE (SELECT CAST({expression} AS {}) FROM (SELECT {bindings}) AS __g) END AS \"__msduck_n{index}\"",
                    column.physical
                ),
                None => format!("CAST(NULL AS {}) AS \"__msduck_n{index}\"", column.physical),
            });
        }
        let generated = if generated.is_empty() {
            String::new()
        } else {
            format!(", {}", generated.join(", "))
        };
        // The first of several DELETEs of one target row is its action.
        Ok(format!(
            "CREATE TEMP TABLE {name} AS SELECT __in.*{generated} FROM (SELECT {} FROM {stage} AS __s WHERE __msduck_row <= ? QUALIFY __msduck_tid IS NULL OR row_number() OVER (PARTITION BY __msduck_tid ORDER BY __msduck_row) = 1) AS __in ORDER BY __msduck_row",
            columns.join(", ")
        ))
    }

    /// Apply the selected actions: deletes, updates, then inserts.
    fn write(&self, session: &Session, images: &str) -> Result<u64> {
        let db = &session.db;
        let table = self.target.backend_name();
        let alias = "\"__msduck_target\"";
        let mut count = 0u64;
        if self.arms.iter().any(|arm| arm.kind == Kind::Delete) {
            count += db
                .execute(
                    &format!(
                        "DELETE FROM {table} WHERE rowid IN (SELECT __msduck_tid FROM {images} WHERE __msduck_kind = 3)"
                    ),
                    [],
                )
                .map_err(write_failure)? as u64;
        }
        for (index, arm) in self.arms.iter().enumerate() {
            if arm.kind != Kind::Update {
                continue;
            }
            let set = arm
                .values
                .iter()
                .map(|(column, _)| {
                    format!(
                        "{} = __msduck_image.\"__msduck_n{column}\"",
                        quoted(&self.target.columns[*column].name)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            count += db
                .execute(
                    &format!(
                        "UPDATE {table} AS {alias} SET {set} FROM {images} AS __msduck_image WHERE {alias}.rowid = __msduck_image.__msduck_tid AND __msduck_image.__msduck_arm = {}",
                        index + 1
                    ),
                    [],
                )
                .map_err(write_failure)? as u64;
        }
        if self.arms.iter().any(|arm| arm.kind == Kind::Insert) {
            let columns = self
                .target
                .columns
                .iter()
                .enumerate()
                .filter(|(_, column)| !column.generated);
            let names = columns
                .clone()
                .map(|(_, column)| quoted(&column.name))
                .collect::<Vec<_>>()
                .join(", ");
            let values = columns
                .map(|(index, _)| format!("\"__msduck_n{index}\""))
                .collect::<Vec<_>>()
                .join(", ");
            count += db
                .execute(
                    &format!(
                        "INSERT INTO {table} ({names}) SELECT {values} FROM {images} WHERE __msduck_kind = 1 ORDER BY __msduck_row"
                    ),
                    [],
                )
                .map_err(write_failure)? as u64;
        }
        Ok(count)
    }
}

/// The alias (or table name) exposing a lowered table factor's columns.
fn qualifier(factor: &TableFactor) -> Option<Ident> {
    match factor {
        TableFactor::Table { alias: Some(a), .. }
        | TableFactor::Derived { alias: Some(a), .. }
        | TableFactor::Function { alias: Some(a), .. } => Some(a.name.clone()),
        TableFactor::Table { name, .. } => name.0.last().and_then(|p| p.as_ident()).cloned(),
        _ => None,
    }
}

fn native_query(sql: &str) -> Result<Query> {
    let mut statements =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::DuckDbDialect {}, sql)?;
    let Statement::Query(query) = statements.remove(0) else {
        bail!("native template is not a query")
    };
    Ok(*query)
}

fn variable(parameter: Option<&Parameter>) -> Option<msduck_sql::merge::top::BoundValue> {
    use msduck_core::value::Value as V;
    use msduck_sql::merge::top::BoundValue;
    let parameter = parameter?;
    Some(match &parameter.value {
        V::Null => BoundValue::Null,
        V::TinyInt(n) => BoundValue::Int(i32::from(*n)),
        V::SmallInt(n) => BoundValue::Int(i32::from(*n)),
        V::Int(n) => BoundValue::Int(*n),
        V::BigInt(n) => BoundValue::Int(i32::try_from(*n).ok()?),
        V::Decimal(value) => BoundValue::Decimal(value.to_string()),
        _ => return None,
    })
}

fn top_error(error: msduck_sql::merge::top::BindError) -> anyhow::Error {
    match error {
        msduck_sql::merge::top::BindError::Sql {
            number,
            state,
            class,
            message,
        } => SqlError {
            number,
            state,
            severity: class,
            message: message.into(),
            message_utf16: None,
        }
        .into(),
        msduck_sql::merge::top::BindError::Unsupported(message) => {
            anyhow!("unsupported {message}")
        }
    }
}

/// A storage conversion failure (2628/8152) terminates only the statement.
fn storage_failure(error: anyhow::Error) -> anyhow::Error {
    let message = error.to_string();
    if let Some(diagnostic) = crate::storage_diagnostic::diagnostic(&message) {
        return crate::query_error::attach_context(diagnostic.into(), vec![], 0xc5);
    }
    if matches!(crate::engine::error_number(&message), 8152 | 2628) {
        return crate::query_error::attach_context(error, vec![], 0xc5);
    }
    error
}

/// Constraint violations terminate the statement (3621) and the batch
/// continues, as for INSERT, UPDATE and DELETE.
fn write_failure(error: duckdb::Error) -> anyhow::Error {
    let error = storage_failure(error.into());
    if error
        .downcast_ref::<crate::query_error::FailedQuery>()
        .is_some()
    {
        return error;
    }
    let message = error.to_string();
    let (state, severity) = match crate::engine::error_number(&message) {
        547 => (0, 16),
        2627 | 2601 => (1, 14),
        515 => (2, 16),
        _ => return error,
    };
    let number = crate::engine::error_number(&message);
    crate::query_error::attach_context(
        SqlError {
            number,
            state,
            severity,
            message,
            message_utf16: None,
        }
        .into(),
        vec![],
        0xc5,
    )
}

impl Session {
    /// The number of action rows `TOP` keeps out of `eligible`.
    fn top(&self, top: Top, eligible: u64, parameters: &HashMap<String, Parameter>) -> Result<u64> {
        use msduck_sql::merge::top::{BindError, bind};
        let bound = match bind(top.clone(), |name| {
            variable(parameters.get(&name.to_lowercase()))
        }) {
            Err(BindError::Unsupported(_)) => {
                // Evaluate other expressions once, then bind the value.
                let Some(TopQuantity::Expr(expr)) = &top.quantity else {
                    bail!("unsupported MERGE TOP form");
                };
                use duckdb::types::Value as V;
                let literal = match self.evaluate_expression(expr.clone(), parameters, false)? {
                    V::Null => Value::Null,
                    V::TinyInt(n) => Value::Number(n.to_string(), false),
                    V::SmallInt(n) => Value::Number(n.to_string(), false),
                    V::Int(n) => Value::Number(n.to_string(), false),
                    V::BigInt(n) => Value::Number(n.to_string(), false),
                    V::UTinyInt(n) => Value::Number(n.to_string(), false),
                    V::Decimal(n) => Value::Number(n.to_string(), false),
                    other => bail!("unsupported MERGE TOP value {other:?}"),
                };
                let negative = matches!(&literal, Value::Number(n, _) if n.starts_with('-'));
                let mut value = Expr::value(match literal {
                    Value::Number(n, long) if negative => {
                        Value::Number(n.trim_start_matches('-').to_owned(), long)
                    }
                    other => other,
                });
                if negative {
                    value = Expr::UnaryOp {
                        op: UnaryOperator::Minus,
                        expr: Box::new(value),
                    };
                }
                bind(
                    Top {
                        quantity: Some(TopQuantity::Expr(value)),
                        ..top
                    },
                    |_| None,
                )
            }
            other => other,
        }
        .map_err(top_error)?;
        Ok(bound.selection(eligible).map_err(top_error)?.take)
    }

    /// Lower a T-SQL query through the same steps as an ordinary SELECT and
    /// return it with its bound values. Keep the returned RAND scopes alive
    /// until the query has run.
    fn lower_query(
        &mut self,
        mut statement: Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<(Statement, Vec<duckdb::types::Value>, Vec<rand::Scope>)> {
        self.lower_database_functions(&mut statement)?;
        self.qualify_databases(&mut statement)?;
        self.lower_session_functions(&mut statement, parameters)?;
        super::rewrite(self, &mut statement, parameters)?;
        crate::query_catalog::validate_cte_columns(&self.db, &statement)?;
        if let Statement::Query(query) = &mut statement {
            crate::query_catalog::bind_query_with_parameters(&self.db, query, parameters)
                .map_err(crate::query_error::compilation)?;
        }
        crate::query_catalog::lower_recursion(&self.db, &mut statement)?;
        crate::query_catalog::bind_binary_operations(&self.db, &mut statement, parameters)?;
        crate::concat_lower::statement(&mut statement, parameters).map_err(anyhow::Error::msg)?;
        crate::aggregate_columns::annotate(&self.db, &mut statement, parameters)
            .map_err(anyhow::Error::msg)?;
        crate::query_catalog::bind_unicode_operations(&self.db, &mut statement, parameters)?;
        crate::concat_lower::annotated_unicode_casts(&mut statement);
        crate::for_json::lower_nested(&self.db, &mut statement, parameters)?;
        let mut translator = Translator {
            parameters,
            values: vec![],
            parameter_slots: HashMap::new(),
            transactions: self.transactions,
            transaction_doomed: self.transaction_doomed,
            original_login: &self.original_login,
            clock: crate::current_time::now(),
            rowcount: self.rowcount,
            last_error: self.last_error,
            caught_error: self.caught_error.as_ref(),
            spid: self.process.spid(),
        };
        if let ControlFlow::Break(error) = VisitMut::visit(&mut statement, &mut translator) {
            bail!(error);
        }
        let mut values = translator.values;
        let scopes = rand::lower(
            &mut statement,
            &mut values,
            &self.diagnostics.rand,
            &self.rand,
        )?;
        Ok((statement, values, scopes))
    }
}

#[cfg(test)]
mod tests;
