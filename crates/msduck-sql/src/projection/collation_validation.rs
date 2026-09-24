//! Validate sensitive operations at their own expression boundaries.
use super::*;
use msduck_core::{
    collation::{Conflict, Operation},
    diagnostic::SqlError,
};
use std::ops::ControlFlow;

type Rows = Vec<Option<Vec<Source>>>;

struct SelectBindings {
    outer: Rows,
    // Node identities are used only during this immutable AST traversal. They
    // never escape into a plan or affect name resolution through iteration order.
    on: HashMap<*const Expr, Option<Vec<Source>>>,
    apply: HashMap<*const Query, Option<Vec<Source>>>,
}

fn select_bindings(
    catalog: &CatalogSnapshot,
    select: &Select,
    scope: &Scope,
    outer: Rows,
) -> SelectBindings {
    let mut result = SelectBindings {
        outer,
        on: HashMap::new(),
        apply: HashMap::new(),
    };
    let mut base = scope.clone();
    base.rows = result.outer.clone();
    for table in &select.from {
        let mut prefix = source(catalog, &table.relation, &base).map(|value| vec![value]);
        for join in &table.joins {
            let apply = matches!(
                join.join_operator,
                JoinOperator::CrossApply | JoinOperator::OuterApply
            );
            let mut input = base.clone();
            if apply {
                input.rows.push(prefix.clone());
                if let TableFactor::Derived { subquery, .. } = &join.relation {
                    result.apply.insert(subquery.as_ref(), prefix.clone());
                }
            }
            prefix = prefix
                .zip(source_with_correlation(
                    catalog,
                    &join.relation,
                    &input,
                    apply,
                ))
                .map(|(mut left, right)| {
                    left.push(right);
                    left
                });
            if let JoinOperator::Join(JoinConstraint::On(expr))
            | JoinOperator::Inner(JoinConstraint::On(expr))
            | JoinOperator::Left(JoinConstraint::On(expr))
            | JoinOperator::LeftOuter(JoinConstraint::On(expr))
            | JoinOperator::Right(JoinConstraint::On(expr))
            | JoinOperator::RightOuter(JoinConstraint::On(expr))
            | JoinOperator::FullOuter(JoinConstraint::On(expr)) = &join.join_operator
            {
                result.on.insert(expr, prefix.clone());
            }
        }
    }
    result
}

#[derive(Clone, Copy)]
enum Bin2Input {
    Unicode,
    Windows1252,
}

struct CaseInput {
    data_type: DataType,
    collation: String,
    marker: &'static str,
}
#[derive(Default)]
struct Bindings {
    comparisons: Vec<(usize, Bin2Input)>,
    cases: Vec<(usize, CaseInput)>,
    binary: Vec<(usize, BinaryInput)>,
    extrema: Vec<(usize, super::character_extrema::Input)>,
}
enum BinaryDirection {
    FromCharacter { unicode: bool },
    ToUnicode { style: Expr, trying: bool },
}
struct BinaryInput {
    direction: BinaryDirection,
    target: DataType,
    width: i32,
    fixed: bool,
}
fn binary_input(catalog: &CatalogSnapshot, expr: &Expr, scope: &Scope) -> Option<BinaryInput> {
    let (source, target, style, trying) = match expr {
        Expr::Cast {
            expr,
            data_type,
            format: None,
            kind,
        } => (
            expr.as_ref(),
            data_type,
            crate::expr::number(0),
            matches!(kind, CastKind::TryCast | CastKind::SafeCast),
        ),
        Expr::Convert {
            expr,
            data_type: Some(data_type),
            charset: None,
            styles,
            is_try,
            ..
        } if styles.len() <= 1 => (
            expr.as_ref(),
            data_type,
            styles
                .first()
                .cloned()
                .unwrap_or_else(|| crate::expr::number(0)),
            *is_try,
        ),
        _ => return None,
    };
    let unicode_width = if matches!(target, DataType::Nvarchar(_)) {
        crate::expression_metadata::character::nvarchar_cast_width(target)
            .ok()
            .map(|n| (n.map(i32::from).unwrap_or(-1), false))
    } else {
        crate::expression_metadata::character::nchar_cast_width(target)
            .ok()
            .flatten()
            .map(|n| (i32::from(n), true))
    };
    if let Some((width, fixed)) = unicode_width {
        let info = member_expression(catalog, source, &[], scope)?;
        if !matches!(info.system_type_id, Some(165 | 173)) {
            return None;
        }
        return Some(BinaryInput {
            direction: BinaryDirection::ToUnicode { style, trying },
            target: target.clone(),
            width,
            fixed,
        });
    }
    if !matches!(&style, Expr::Value(v) if matches!(&v.value, Value::Number(n, _) if n == "0")) {
        return None;
    }
    let (width, fixed) = match target {
        DataType::Varbinary(Some(BinaryLength::Max)) => (-1, false),
        DataType::Varbinary(None) => (30, false),
        DataType::Varbinary(Some(BinaryLength::IntegerLength { length }))
            if (1..=8000).contains(length) =>
        {
            (*length as i32, false)
        }
        DataType::Binary(None) => (30, true),
        DataType::Binary(Some(length)) if (1..=8000).contains(length) => (*length as i32, true),
        _ => return None,
    };
    let info = member_expression(catalog, source, &[], scope)?;
    let unicode = match info.system_type_id? {
        231 | 239 => true,
        167 | 175 => {
            let label = expression_collation(catalog, source, &[], scope)?.ok()?;
            if !matches!(
                label.name()?.to_ascii_lowercase().as_str(),
                "sql_latin1_general_cp1_ci_as"
                    | "latin1_general_100_ci_as"
                    | "latin1_general_100_cs_as"
                    | "latin1_general_100_ci_ai"
                    | "latin1_general_100_cs_ai"
                    | "latin1_general_100_bin2"
            ) {
                return None;
            }
            false
        }
        _ => return None,
    };
    Some(BinaryInput {
        direction: BinaryDirection::FromCharacter { unicode },
        target: target.clone(),
        width,
        fixed,
    })
}
fn case_input(catalog: &CatalogSnapshot, expr: &Expr, scope: &Scope) -> Option<CaseInput> {
    let Expr::Function(f) = expr else {
        return None;
    };
    let name = f.name.to_string().to_ascii_uppercase();
    if !matches!(name.as_str(), "LOWER" | "UPPER") {
        return None;
    }
    let source = crate::function_args::unary(f, &name).ok()??;
    if let Expr::Collate { expr, .. } = source
        && let Expr::Cast { expr, .. } = expr.as_ref()
        && let Expr::Function(marker) = expr.as_ref()
        && matches!(
            marker.name.to_string().as_str(),
            "__msduck_case_input_legacy" | "__msduck_case_input_100"
        )
    {
        return None;
    }

    let info = member_expression(catalog, source, &[], scope)?;
    if !matches!(info.system_type_id, Some(231 | 239)) {
        return None;
    }
    let label = expression_collation(catalog, source, &[], scope)?.ok()?;
    let collation = label.name()?.to_owned();
    let family = msduck_core::case_mapping::Family::for_collation(&collation)?;
    let length = match info.max_length? {
        -1 if info.system_type_id == Some(231) => CharacterLength::Max,
        n if n >= 0 && n % 2 == 0 => CharacterLength::IntegerLength {
            length: (n / 2) as u64,
            unit: None,
        },
        _ => return None,
    };
    Some(CaseInput {
        data_type: if info.system_type_id == Some(239) {
            DataType::Custom(
                ObjectName::from(vec![Ident::new("nchar")]),
                vec![(info.max_length.unwrap() / 2).to_string()],
            )
        } else {
            DataType::Nvarchar(Some(length))
        },
        collation,
        marker: match family {
            msduck_core::case_mapping::Family::SqlLatin1 => "__msduck_case_input_legacy",
            msduck_core::case_mapping::Family::Latin1General100 => "__msduck_case_input_100",
        },
    })
}

fn comparison_plan(
    catalog: &CatalogSnapshot,
    query: &Query,
    outer: &Scope,
) -> Result<Bindings, SqlError> {
    struct Check<'a> {
        catalog: &'a CatalogSnapshot,
        outer: &'a Scope,
        queries: Vec<QueryScopes>,
        derived: Option<bool>,
        selects: Vec<SelectBindings>,
        saved_rows: Vec<(*const Expr, Rows)>,
        position: usize,
        bindings: Bindings,
    }
    impl Visitor for Check<'_> {
        type Break = SqlError;

        fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<SqlError> {
            if let TableFactor::Derived { lateral, .. } = factor {
                self.derived = Some(*lateral);
            }
            ControlFlow::Continue(())
        }

        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<SqlError> {
            let mut inherited = self
                .queries
                .last_mut()
                .map(|parent| {
                    parent
                        .definitions
                        .pop_front()
                        .unwrap_or_else(|| parent.body.clone())
                })
                .unwrap_or_else(|| self.outer.clone());
            let derived = self.derived.take();
            if let Some((frame, rows)) = self.selects.last().and_then(|frame| {
                frame
                    .apply
                    .get(&(query as *const Query))
                    .map(|rows| (frame, rows))
            }) {
                inherited.rows = frame.outer.clone();
                inherited.rows.push(rows.clone());
            } else if derived == Some(false) {
                inherited.rows.clear();
            }
            self.queries.push(scopes(self.catalog, query, &inherited));
            ControlFlow::Continue(())
        }

        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<SqlError> {
            self.queries.pop();
            ControlFlow::Continue(())
        }

        fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<SqlError> {
            let query = self.queries.last_mut().unwrap();
            let scope = &mut query.body;
            self.selects.push(select_bindings(
                self.catalog,
                select,
                scope,
                query.inherited.rows.clone(),
            ));
            // Rebind this SELECT against its inherited rows, not the complete
            // local frame already retained for query-level ORDER BY. Otherwise
            // an APPLY input can accidentally find a later alias in that frame.
            let mut declarations = scope.clone();
            declarations.rows = query.inherited.rows.clone();
            scope
                .rows
                .push(sources(self.catalog, select, &declarations));
            ControlFlow::Continue(())
        }

        fn post_visit_select(&mut self, _: &Select) -> ControlFlow<SqlError> {
            self.queries.last_mut().unwrap().body.rows.pop();
            self.selects.pop();
            ControlFlow::Continue(())
        }

        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<SqlError> {
            if let Some(frame) = self.selects.last()
                && let Some(rows) = frame.on.get(&(expr as *const Expr))
            {
                let scope = &mut self.queries.last_mut().unwrap().body;
                let mut local = frame.outer.clone();
                local.push(rows.clone());
                self.saved_rows
                    .push((expr, std::mem::replace(&mut scope.rows, local)));
            }
            ControlFlow::Continue(())
        }

        fn post_visit_expr(&mut self, expr: &Expr) -> ControlFlow<SqlError> {
            let scope = &self.queries.last().unwrap().body;
            let label = match expr {
                Expr::BinaryOp { left, op, right } if comparison_operation(op).is_some() => {
                    let label = sensitive_collation(
                        [left.as_ref(), right.as_ref()].into_iter().map(|value| {
                            expression_collation(self.catalog, value, &[], scope).or_else(|| {
                                conditional::literal_null(value)
                                    .then(|| {
                                        self.catalog.default_collation.clone().map(|name| {
                                            Ok(msduck_core::collation::Label::CoercibleDefault(
                                                name,
                                            ))
                                        })
                                    })
                                    .flatten()
                            })
                        }),
                        comparison_operation(op).unwrap(),
                    );
                    let types = [left.as_ref(), right.as_ref()].map(|value| {
                        member_expression(self.catalog, value, &[], scope)
                            .and_then(|info| info.system_type_id)
                    });
                    let supported = [left.as_ref(), right.as_ref()].into_iter().zip(types).all(
                        |(value, kind)| {
                            conditional::literal_null(value)
                                || matches!(kind, Some(167 | 175 | 231 | 239))
                        },
                    );
                    let mode = if types.iter().any(|kind| matches!(kind, Some(231 | 239))) {
                        Some(Bin2Input::Unicode)
                    } else if types.iter().any(|kind| matches!(kind, Some(167 | 175))) {
                        Some(Bin2Input::Windows1252)
                    } else {
                        None
                    };
                    if supported
                        && label
                            .as_ref()
                            .and_then(|label| label.as_ref().ok())
                            .and_then(|label| label.name())
                            .is_some_and(|name| {
                                name.eq_ignore_ascii_case("Latin1_General_100_BIN2")
                            })
                        && let Some(mode) = mode
                    {
                        self.bindings.comparisons.push((self.position, mode));
                    }
                    label
                }
                _ => expression_collation(self.catalog, expr, &[], scope),
            };
            if let Some(input) = case_input(self.catalog, expr, scope) {
                self.bindings.cases.push((self.position, input));
            }
            if let Some(input) = binary_input(self.catalog, expr, scope) {
                self.bindings.binary.push((self.position, input));
            }
            if let Some(input) = super::character_extrema::input(self.catalog, expr, scope) {
                self.bindings.extrema.push((self.position, input));
            }
            self.position += 1;
            if let Some(Err(Conflict::Operation(error))) = label {
                return ControlFlow::Break(error);
            }
            if self
                .saved_rows
                .last()
                .is_some_and(|(node, _)| std::ptr::eq(*node, expr))
            {
                self.queries.last_mut().unwrap().body.rows = self.saved_rows.pop().unwrap().1;
            }
            ControlFlow::Continue(())
        }
    }
    let mut check = Check {
        catalog,
        outer,
        queries: vec![],
        derived: None,
        selects: vec![],
        saved_rows: vec![],
        position: 0,
        bindings: Bindings::default(),
    };
    match query.visit(&mut check) {
        ControlFlow::Break(error) => Err(error),
        ControlFlow::Continue(()) => Ok(check.bindings),
    }
}

fn comparison_operation(op: &BinaryOperator) -> Option<Operation> {
    Some(match op {
        BinaryOperator::Eq => Operation::Equal,
        BinaryOperator::NotEq => Operation::NotEqual,
        BinaryOperator::Lt => Operation::Less,
        BinaryOperator::Gt => Operation::Greater,
        BinaryOperator::LtEq => Operation::LessEqual,
        BinaryOperator::GtEq => Operation::GreaterEqual,
        _ => return None,
    })
}

pub fn validate_query_operations(
    catalog: &CatalogSnapshot,
    query: &Query,
    outer: &Scope,
) -> Result<(), SqlError> {
    comparison_plan(catalog, query, outer).map(|_| ())
}

/// Validate before changing the AST. Each planned comparison consumes each
/// operand once; postorder positions refer only to the unchanged input tree.
/// Operand declarations select Unicode units or Windows-1252 byte ordering.
pub fn lower_bin2_comparisons(
    catalog: &CatalogSnapshot,
    query: &mut Query,
    outer: &Scope,
) -> Result<(), SqlError> {
    let positions = comparison_plan(catalog, query, outer)?.comparisons;
    struct Lower {
        position: usize,
        pending: std::iter::Peekable<std::vec::IntoIter<(usize, Bin2Input)>>,
    }
    fn operand(value: Expr) -> Expr {
        let value = match value {
            Expr::Nested(value) | Expr::Collate { expr: value, .. } => return operand(*value),
            value => value,
        };
        crate::expr::unary_function("__msduck_carrier_input", value)
    }
    impl VisitorMut for Lower {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if self
                .pending
                .peek()
                .is_some_and(|(position, _)| *position == self.position)
            {
                let (_, mode) = self.pending.next().unwrap();
                let Expr::BinaryOp { left, op, right } =
                    std::mem::replace(expr, crate::expr::number(0))
                else {
                    unreachable!("planned comparison")
                };
                *expr = Expr::BinaryOp {
                    left: Box::new(crate::expr::binary_function(
                        match mode {
                            Bin2Input::Unicode => "__msduck_bin2_compare",
                            Bin2Input::Windows1252 => "__msduck_bin2_ansi_compare",
                        },
                        operand(*left),
                        operand(*right),
                    )),
                    op,
                    right: Box::new(crate::expr::number(0)),
                };
            }
            self.position += 1;
            ControlFlow::Continue(())
        }
    }
    let mut lower = Lower {
        position: 0,
        pending: positions.into_iter().peekable(),
    };
    let _ = VisitMut::visit(query, &mut lower);
    debug_assert!(lower.pending.next().is_none());
    Ok(())
}

/// Retain original public casing functions while carrying scoped declarations
/// to the late backend translator. Markers are never backend function calls.
pub fn annotate_unicode_case_inputs(
    catalog: &CatalogSnapshot,
    query: &mut Query,
    outer: &Scope,
) -> Result<(), SqlError> {
    let cases = comparison_plan(catalog, query, outer)?.cases;
    struct Annotate {
        position: usize,
        pending: std::iter::Peekable<std::vec::IntoIter<(usize, CaseInput)>>,
    }
    impl VisitorMut for Annotate {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if self
                .pending
                .peek()
                .is_some_and(|(position, _)| *position == self.position)
            {
                let (_, input) = self.pending.next().unwrap();
                let Expr::Function(f) = expr else {
                    unreachable!("bound casing function");
                };
                let FunctionArguments::List(args) = &mut f.args else {
                    unreachable!("bound casing arguments");
                };
                let FunctionArg::Unnamed(FunctionArgExpr::Expr(source)) = &mut args.args[0] else {
                    unreachable!("bound casing operand");
                };
                let original = std::mem::replace(source, crate::expr::number(0));
                *source = Expr::Collate {
                    expr: Box::new(Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(crate::expr::unary_function(input.marker, original)),
                        data_type: input.data_type,
                        format: None,
                    }),
                    collation: ObjectName::from(vec![Ident::new(input.collation)]),
                };
            }
            self.position += 1;
            ControlFlow::Continue(())
        }
    }
    let mut annotate = Annotate {
        position: 0,
        pending: cases.into_iter().peekable(),
    };
    let _ = VisitMut::visit(query, &mut annotate);
    debug_assert!(annotate.pending.next().is_none());
    Ok(())
}

/// Lower statically bound conversions between binary and character values.
/// Byte lengths may split a UTF-16 unit; fixed binary pads on the right.
pub fn lower_unicode_binary_conversions(
    catalog: &CatalogSnapshot,
    query: &mut Query,
    outer: &Scope,
) -> Result<(), SqlError> {
    let positions = comparison_plan(catalog, query, outer)?.binary;
    struct Lower {
        position: usize,
        pending: std::iter::Peekable<std::vec::IntoIter<(usize, BinaryInput)>>,
    }
    fn operand(value: Expr) -> Expr {
        match value {
            Expr::Nested(value) | Expr::Collate { expr: value, .. } => operand(*value),
            value => value,
        }
    }
    impl VisitorMut for Lower {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if self
                .pending
                .peek()
                .is_some_and(|(p, _)| *p == self.position)
            {
                let (_, input) = self.pending.next().unwrap();
                let (Expr::Cast { expr: source, .. } | Expr::Convert { expr: source, .. }) = expr
                else {
                    unreachable!("planned binary conversion")
                };
                let value = match input.direction {
                    BinaryDirection::FromCharacter { unicode } => crate::expr::binary_function(
                        match (unicode, input.fixed) {
                            (true, true) => "__msduck_unicode_binary",
                            (true, false) => "__msduck_unicode_varbinary",
                            (false, true) => "__msduck_ansi_binary",
                            (false, false) => "__msduck_ansi_varbinary",
                        },
                        crate::expr::unary_function(
                            "__msduck_carrier_input",
                            operand(*source.clone()),
                        ),
                        crate::expr::number(input.width),
                    ),
                    BinaryDirection::ToUnicode { style, trying } => {
                        let mut call = crate::expr::binary_function(
                            match (input.fixed, trying) {
                                (false, false) => "__msduck_binary_nvarchar",
                                (true, false) => "__msduck_binary_nchar",
                                (false, true) => "__msduck_try_binary_nvarchar",
                                (true, true) => "__msduck_try_binary_nchar",
                            },
                            operand(*source.clone()),
                            crate::expr::number(input.width),
                        );
                        if let Expr::Function(f) = &mut call
                            && let FunctionArguments::List(args) = &mut f.args
                        {
                            args.args
                                .push(FunctionArg::Unnamed(FunctionArgExpr::Expr(style)));
                        }
                        call
                    }
                };
                *expr = Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(value),
                    data_type: input.target,
                    format: None,
                };
            }
            self.position += 1;
            ControlFlow::Continue(())
        }
    }
    let mut lower = Lower {
        position: 0,
        pending: positions.into_iter().peekable(),
    };
    let _ = VisitMut::visit(query, &mut lower);
    debug_assert!(lower.pending.next().is_none());
    Ok(())
}

/// Reuse the same scope frames as comparison/casing validation.
pub(super) fn extrema_plan(
    catalog: &CatalogSnapshot,
    query: &Query,
    outer: &Scope,
) -> Result<Vec<(usize, super::character_extrema::Input)>, SqlError> {
    Ok(comparison_plan(catalog, query, outer)?.extrema)
}
