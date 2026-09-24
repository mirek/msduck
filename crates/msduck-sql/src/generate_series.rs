//! GENERATE_SERIES relational lowering over explicit arguments, without I/O.
use crate::{expression_metadata::storage, parameter::Parameter};
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};
use std::{
    collections::{HashMap, HashSet},
    ops::ControlFlow,
};

pub const ZERO_STEP: &str =
    "Argument value 0 is invalid for argument 3 of generate_series function.";

pub fn is_series(factor: &TableFactor) -> bool {
    matches!(factor, TableFactor::Table { name, args: Some(_), .. }
        if name.0.len() == 1 && name.0[0].as_ident().is_some_and(|id| id.value.eq_ignore_ascii_case("generate_series")))
}

pub fn result_type(
    factor: &TableFactor,
    parameters: &HashMap<String, Parameter>,
) -> Option<DataType> {
    if !is_series(factor) {
        return None;
    }
    let TableFactor::Table {
        args: Some(args), ..
    } = factor
    else {
        return None;
    };
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = args.args.first()? else {
        return None;
    };
    let kind = storage::kind(value, parameters, &|_| None)?;
    shape(kind.clone())?;
    Some(kind)
}

fn shape(kind: DataType) -> Option<DataType> {
    Some(match kind {
        DataType::TinyInt(_) => DataType::TinyInt(None),
        DataType::SmallInt(_) => DataType::SmallInt(None),
        DataType::Int(_) | DataType::Integer(_) => DataType::Int(None),
        DataType::BigInt(_) => DataType::BigInt(None),
        DataType::Decimal(info) | DataType::Numeric(info) | DataType::Dec(info) => {
            DataType::Decimal(match info {
                ExactNumberInfo::None => ExactNumberInfo::PrecisionAndScale(18, 0),
                ExactNumberInfo::Precision(p) => ExactNumberInfo::PrecisionAndScale(p, 0),
                info => info,
            })
        }
        _ => return None,
    })
}

pub fn lower(
    factor: &mut TableFactor,
    parameters: &HashMap<String, Parameter>,
) -> Result<(), String> {
    if !is_series(factor) {
        return Ok(());
    }
    let TableFactor::Table {
        args: Some(args),
        alias,
        with_hints,
        ..
    } = factor
    else {
        return Ok(());
    };
    if !(2..=3).contains(&args.args.len()) || args.settings.is_some() || !with_hints.is_empty() {
        return Err("GENERATE_SERIES requires two or three scalar arguments without hints".into());
    }
    let values = args
        .args
        .iter()
        .map(|arg| match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) => Ok(value.clone()),
            _ => Err("GENERATE_SERIES requires positional scalar arguments".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut known = None;
    for value in &values {
        if let Some(kind) = storage::kind(value, parameters, &|_| None) {
            let kind =
                shape(kind).ok_or("GENERATE_SERIES requires integer or decimal arguments")?;
            if known.as_ref().is_some_and(|first| first != &kind) {
                return Err("GENERATE_SERIES arguments must have matching numeric types".into());
            }
            known = Some(kind);
        }
    }
    let output_alias = alias.clone();
    struct Names(HashSet<String>);
    impl Visitor for Names {
        type Break = ();
        fn pre_visit_ident(&mut self, id: &Ident) -> ControlFlow<()> {
            self.0.insert(id.value.to_ascii_lowercase());
            ControlFlow::Continue(())
        }
    }
    let mut names = Names(HashSet::new());
    let _ = Visit::visit(factor, &mut names);
    let mut prefix = "__msduck_series".to_string();
    while names.0.iter().any(|name| name.starts_with(&prefix)) {
        prefix.push('_');
    }
    // All generated identifiers are renamed before inserting caller expressions.
    let sql = "WITH RECURSIVE __g_input AS (SELECT NULL AS __g_start, NULL AS __g_stop, NULL AS __g_arg),
        __g_checked AS (SELECT __g_start, __g_stop, CASE WHEN __g_arg=0 THEN error('Argument value 0 is invalid for argument 3 of generate_series function.') ELSE __g_arg END AS __g_step FROM __g_input WHERE CASE WHEN typeof(__g_start)<>typeof(__g_stop) THEN error('GENERATE_SERIES arguments must have matching numeric types') ELSE true END),
        __g_rows(__g_value,__g_stop,__g_step) AS (
            SELECT __g_start,__g_stop,__g_step FROM __g_checked
            WHERE (__g_step>0 AND __g_start<=__g_stop) OR (__g_step<0 AND __g_start>=__g_stop)
            UNION ALL
            SELECT __g_next,__g_stop,__g_step FROM __g_rows
            CROSS JOIN LATERAL (SELECT TRY(cast_to_type(__g_value+__g_step,__g_value)) AS __g_next) AS __g_calc
            WHERE __g_next IS NOT NULL AND __g_next<>__g_value AND ((__g_step>0 AND __g_next<=__g_stop) OR (__g_step<0 AND __g_next>=__g_stop))
        ) SELECT __g_value AS value FROM __g_rows";
    let Statement::Query(mut query) = Parser::parse_sql(&GenericDialect {}, sql)
        .map_err(|e| e.to_string())?
        .remove(0)
    else {
        unreachable!()
    };
    if matches!(
        known,
        Some(DataType::TinyInt(_) | DataType::SmallInt(_) | DataType::Int(_) | DataType::BigInt(_))
    ) {
        let Statement::Query(native) = Parser::parse_sql(&GenericDialect {},
            "SELECT cast_to_type(__g_native.value,__g_checked.__g_start) AS value FROM __g_checked CROSS JOIN main.generate_series(__g_checked.__g_start,__g_checked.__g_stop,__g_checked.__g_step) AS __g_native(value)")
            .map_err(|e| e.to_string())?.remove(0) else { unreachable!() };
        query.body = native.body;
        let with = query.with.as_mut().unwrap();
        with.cte_tables.truncate(2);
        with.recursive = false;
    }
    struct Rename<'a>(&'a str);
    impl VisitorMut for Rename<'_> {
        type Break = ();
        fn pre_visit_ident(&mut self, id: &mut Ident) -> ControlFlow<()> {
            if let Some(suffix) = id.value.strip_prefix("__g") {
                id.value = format!("{}{suffix}", self.0);
            }
            ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(&mut query, &mut Rename(&prefix));
    let with = query.with.as_mut().unwrap();
    with.cte_tables[0].materialized = Some(CteAsMaterialized::Materialized);
    let SetExpr::Select(input) = with.cte_tables[0].query.body.as_mut() else {
        unreachable!()
    };
    for (item, value) in input.projection.iter_mut().zip(values.iter()) {
        let SelectItem::ExprWithAlias { expr, .. } = item else {
            unreachable!()
        };
        *expr = value.clone();
    }
    if values.len() == 2 {
        let SetExpr::Select(checked) = with.cte_tables[1].query.body.as_mut() else {
            unreachable!()
        };
        let SelectItem::ExprWithAlias { expr, .. } = &mut checked.projection[2] else {
            unreachable!()
        };
        *expr = Expr::Case {
            case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
            end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
            operand: None,
            conditions: vec![CaseWhen {
                condition: Expr::BinaryOp {
                    left: Box::new(Expr::Identifier(Ident::new(format!("{prefix}_start")))),
                    op: BinaryOperator::LtEq,
                    right: Box::new(Expr::Identifier(Ident::new(format!("{prefix}_stop")))),
                },
                result: Expr::Value(Value::Number("1".into(), false).into()),
            }],
            else_result: Some(Box::new(Expr::UnaryOp {
                op: UnaryOperator::Minus,
                expr: Box::new(Expr::Value(Value::Number("1".into(), false).into())),
            })),
        };
    }
    let alias = output_alias.unwrap_or_else(|| TableAlias {
        name: Ident::new("generate_series"),
        columns: vec![],
        explicit: false,
        at: None,
    });
    *factor = TableFactor::Derived {
        lateral: true,
        subquery: query,
        alias: Some(alias),
        sample: None,
    };
    Ok(())
}
