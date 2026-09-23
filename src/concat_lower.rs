//! Bind character concatenation before physical AST rewriting.
use msduck_core::{
    character::{Family, Length},
    collation::Label,
    types::Type,
};
use msduck_sql::{
    concat::{Declaration, Node, Plan},
    parameter::Parameter,
};
use sqlparser::ast::*;
use std::collections::HashMap;

fn declaration(expr: &Expr, parameters: &HashMap<String, Parameter>) -> Option<Declaration> {
    if let Expr::Function(f) = expr
        && let Some(source) = msduck_sql::function_args::unary(f, "__msduck_carrier_input")
            .ok()
            .flatten()
    {
        return declaration(source, parameters);
    }
    let trim_source = match expr {
        Expr::Trim { expr, .. } => Some(expr.as_ref()),
        Expr::Function(f)
            if matches!(
                f.name.to_string().to_ascii_uppercase().as_str(),
                "LTRIM" | "RTRIM"
            ) =>
        {
            match &f.args {
                FunctionArguments::List(args) => match args.args.first() {
                    Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(source))) => Some(source),
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    };
    if let Some(source) = trim_source {
        let mut d = plan(source, parameters).ok()??.declaration;
        d.family = match d.family {
            Family::Nchar => Family::Nvarchar,
            Family::Char => Family::Varchar,
            family => family,
        };
        return Some(d);
    }
    if let Expr::Collate {
        expr: inner,
        collation,
    } = expr
    {
        let mut d = plan(inner, parameters).ok()??.declaration;
        // Validate the name against the currently supported wire/code-page table.
        crate::tds::collation::Collation::for_name(&collation.to_string())?;
        d.collation = Label::Explicit(collation.to_string());
        return Some(d);
    }
    let shape = msduck_sql::expression_metadata::storage::kind(expr, parameters, &|_| None)
        .and_then(|kind| msduck_sql::sql_type::declaration(&kind).ok())
        .and_then(|kind| {
            if let Type::Character(t) = kind {
                Some((t.family(), t.length()))
            } else {
                None
            }
        })
        .or_else(|| match msduck_sql::result_types::expression_type(expr) {
            Some(msduck_sql::result_types::ResultType::Character { family, length }) => {
                Some((family, length))
            }
            _ => None,
        })?;
    Some(Declaration {
        family: shape.0,
        length: shape.1,
        collation: Label::CoercibleDefault("SQL_Latin1_General_CP1_CI_AS".into()),
    })
}
fn plan(expr: &Expr, parameters: &HashMap<String, Parameter>) -> Result<Option<Plan>, String> {
    msduck_sql::concat::bind(expr, &|e| declaration(e, parameters))
        .map_err(|e| format!("character concatenation binding failed: {e:?}"))
}
fn unary(name: &str, value: Expr) -> Expr {
    msduck_sql::expr::unary_function(name, value)
}
fn emit(plan: Plan) -> Expr {
    match plan.node {
        Node::Leaf(expr) => *expr,
        Node::Collate { child } => emit(*child),
        Node::Concat { left, right } => {
            let unicode = matches!(plan.declaration.family, Family::Nchar | Family::Nvarchar);
            let operand = |p: Plan| {
                let value = emit(p);
                if unicode {
                    unary("__msduck_carrier_input", value)
                } else {
                    value
                }
            };
            let mut call = msduck_sql::expr::binary_function(
                if unicode {
                    "__msduck_concat_unicode"
                } else {
                    "__msduck_concat_cp1252"
                },
                operand(*left),
                operand(*right),
            );
            if let Expr::Function(f) = &mut call
                && let FunctionArguments::List(args) = &mut f.args
            {
                args.args.push(FunctionArg::Unnamed(FunctionArgExpr::Expr(
                    msduck_sql::expr::number(match plan.declaration.length {
                        Length::Max => -1,
                        Length::Bounded(n) => i32::from(n),
                    }),
                )));
            }
            call
        }
    }
}
fn concatenation(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(e) | Expr::Collate { expr: e, .. } => concatenation(e),
        Expr::BinaryOp {
            op: BinaryOperator::Plus | BinaryOperator::StringConcat,
            ..
        } => true,
        _ => false,
    }
}
pub fn trim_input(expr: Expr, parameters: &HashMap<String, Parameter>) -> Expr {
    if declaration(&expr, parameters)
        .is_some_and(|d| matches!(d.family, Family::Nchar | Family::Nvarchar))
    {
        unary("__msduck_carrier_input", expr)
    } else {
        expr
    }
}
pub fn lower(expr: &mut Expr, parameters: &HashMap<String, Parameter>) -> Result<(), String> {
    if let Expr::Cast {
        expr: source,
        data_type,
        kind: CastKind::Cast,
        format: None,
    } = expr
        && (concatenation(source)
            || matches!(source.as_ref(), Expr::Trim { .. })
            || matches!(source.as_ref(), Expr::Function(f) if matches!(f.name.to_string().to_ascii_uppercase().as_str(), "LTRIM" | "RTRIM")))
        && let Some(bound) = plan(source, parameters)?
        && matches!(bound.declaration.family, Family::Nchar | Family::Nvarchar)
        && let Ok(Type::Character(target)) = msduck_sql::sql_type::declaration(data_type)
    {
        let name = match target.family() {
            Family::Nvarchar => "__msduck_cast_carrier_nvarchar",
            Family::Nchar => "__msduck_cast_carrier_nchar",
            Family::Varchar => "__msduck_cast_carrier_varchar",
            Family::Char => "__msduck_cast_carrier_char",
        };
        *expr = msduck_sql::expr::binary_function(
            name,
            unary("__msduck_carrier_input", emit(bound)),
            msduck_sql::expr::number(match target.length() {
                Length::Max => -1,
                Length::Bounded(n) => i32::from(n),
            }),
        );
        return Ok(());
    }
    if let Expr::Function(f) = expr {
        let name = f.name.to_string().to_ascii_uppercase();
        if matches!(name.as_str(), "LEFT" | "RIGHT")
            && let FunctionArguments::List(args) = &f.args
            && matches!(f.parameters, FunctionArguments::None)
            && f.over.is_none()
            && f.filter.is_none()
            && f.null_treatment.is_none()
            && f.within_group.is_empty()
            && args.duplicate_treatment.is_none()
            && args.clauses.is_empty()
            && let [
                FunctionArg::Unnamed(FunctionArgExpr::Expr(source)),
                FunctionArg::Unnamed(FunctionArgExpr::Expr(count)),
            ] = args.args.as_slice()
            && concatenation(source)
            && let Some(bound) = plan(source, parameters)?
            && matches!(bound.declaration.family, Family::Nchar | Family::Nvarchar)
        {
            let count = Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(count.clone()),
                data_type: DataType::Int(None),
                format: None,
            };
            *expr = msduck_sql::expr::binary_function(
                if name == "LEFT" {
                    "__msduck_left_unicode"
                } else {
                    "__msduck_right_unicode"
                },
                emit(bound),
                count,
            );
            return Ok(());
        }
    }
    if concatenation(expr) {
        if let Some(bound) = plan(expr, parameters)? {
            *expr = emit(bound);
        }
        return Ok(());
    }
    // Length consumers need the logical type before the child becomes a carrier.
    if let Expr::Function(f) = expr {
        let name = f.name.to_string().to_ascii_uppercase();
        if matches!(name.as_str(), "LEN" | "DATALENGTH")
            && let Some(source) = msduck_sql::function_args::unary(f, &name)?
            && concatenation(source)
            && let Some(bound) = plan(source, parameters)?
        {
            let unicode = matches!(bound.declaration.family, Family::Nchar | Family::Nvarchar);
            let large = bound.declaration.length == Length::Max;
            let native = match (unicode, name.as_str()) {
                (true, "LEN") => "__msduck_carrier_len",
                (true, _) => "__msduck_carrier_datalength",
                (false, "LEN") => "__msduck_len",
                (false, _) => "__msduck_datalength_ansi",
            };
            *expr = Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(unary(native, emit(bound))),
                data_type: if large {
                    DataType::BigInt(None)
                } else {
                    DataType::Int(None)
                },
                format: None,
            };
        }
    }
    Ok(())
}

pub fn statement<T: VisitMut>(
    node: &mut T,
    parameters: &HashMap<String, Parameter>,
) -> Result<(), String> {
    struct Lower<'a>(&'a HashMap<String, Parameter>);
    impl VisitorMut for Lower<'_> {
        type Break = String;
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<String> {
            // Preserve character operators until parent coercions have bound.
            // Length consumers alone need an earlier rewrite to avoid rejecting
            // the original concatenation as an unknown DATALENGTH operand.
            if !matches!(expr, Expr::Function(_)) {
                return std::ops::ControlFlow::Continue(());
            }
            match lower(expr, self.0) {
                Ok(()) => std::ops::ControlFlow::Continue(()),
                Err(e) => std::ops::ControlFlow::Break(e),
            }
        }
    }
    match node.visit(&mut Lower(parameters)) {
        std::ops::ControlFlow::Continue(()) => Ok(()),
        std::ops::ControlFlow::Break(e) => Err(e),
    }
}

/// Recursive set members must agree on physical Unicode representation. Run
/// after logical operand annotation: the wrappers must not erase CTE types while
/// the binder is still resolving recursive references. Keep the original CAST
/// inside the wrapper so its conversion/width checks remain in effect.
pub fn recursive_carriers<T: VisitMut>(node: &mut T) {
    struct Normalize(Vec<bool>);
    impl VisitorMut for Normalize {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> std::ops::ControlFlow<()> {
            self.0.push(
                self.0.last().copied().unwrap_or(false)
                    || query.with.as_ref().is_some_and(|w| w.recursive),
            );
            std::ops::ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> std::ops::ControlFlow<()> {
            self.0.pop();
            std::ops::ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<()> {
            if self.0.last() == Some(&true)
                && let Expr::Cast {
                    expr: source,
                    data_type,
                    kind: CastKind::Cast,
                    format: None,
                } = expr
                && let Ok(Type::Character(target)) = msduck_sql::sql_type::declaration(data_type)
                && matches!(target.family(), Family::Nchar | Family::Nvarchar)
            {
                // typeof is bind-time only. Each selected branch evaluates its
                // source once; ordinary numeric conversions retain the original
                // CAST, while an existing carrier never passes through VARCHAR.
                let sql = format!(
                    "CASE WHEN typeof(__msduck_recursive_source)='STRUCT(__msduck_utf16le BLOB)' THEN CAST(__msduck_recursive_source AS STRUCT(__msduck_utf16le BLOB)) ELSE __msduck_carrier_input(CAST(__msduck_recursive_source AS {data_type})) END"
                );
                let mut dispatch =
                    sqlparser::parser::Parser::new(&sqlparser::dialect::DuckDbDialect {})
                        .try_with_sql(&sql)
                        .expect("generated cast syntax")
                        .parse_expr()
                        .expect("generated cast expression");
                struct Substitute(Expr);
                impl VisitorMut for Substitute {
                    type Break = ();
                    fn post_visit_expr(&mut self, e: &mut Expr) -> std::ops::ControlFlow<()> {
                        if matches!(e,Expr::Identifier(id) if id.value=="__msduck_recursive_source")
                        {
                            *e = self.0.clone();
                        }
                        std::ops::ControlFlow::Continue(())
                    }
                }
                let _ = VisitMut::visit(&mut dispatch, &mut Substitute(*source.clone()));
                *expr = msduck_sql::expr::binary_function(
                    if target.family() == Family::Nchar {
                        "__msduck_cast_carrier_nchar"
                    } else {
                        "__msduck_cast_carrier_nvarchar"
                    },
                    dispatch,
                    msduck_sql::expr::number(match target.length() {
                        Length::Max => -1,
                        Length::Bounded(n) => i32::from(n),
                    }),
                );
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let _ = node.visit(&mut Normalize(Vec::new()));
}
