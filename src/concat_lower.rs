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
                "LTRIM" | "RTRIM" | "LOWER" | "UPPER"
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
            || matches!(source.as_ref(), Expr::Function(f) if matches!(f.name.to_string().to_ascii_uppercase().as_str(), "LTRIM" | "RTRIM" | "LOWER" | "UPPER")))
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

/// Preserve Unicode carriers through migrated consumer annotations and recursive
/// set members. Other consumers migrate with their native adapter; forcing every
/// legacy Unicode producer to STRUCT here would break still-text-only functions.
/// Run after binding so wrappers cannot erase declarations during inference.
pub fn annotated_unicode_casts<T: VisitMut>(node: &mut T) {
    #[derive(Default)]
    struct Normalize {
        recursive: Vec<bool>,
        consumer: Vec<bool>,
    }
    impl VisitorMut for Normalize {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> std::ops::ControlFlow<()> {
            self.recursive.push(
                self.recursive.last().copied().unwrap_or(false)
                    || query.with.as_ref().is_some_and(|w| w.recursive),
            );
            std::ops::ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &mut Query) -> std::ops::ControlFlow<()> {
            self.recursive.pop();
            std::ops::ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<()> {
            let inherited = self.consumer.last().copied().unwrap_or(false);
            let unicode_cast = matches!(expr,
                Expr::Cast { data_type, kind: CastKind::Cast, format: None, .. }
                    if matches!(msduck_sql::sql_type::declaration(data_type),
                        Ok(Type::Character(target)) if matches!(target.family(), Family::Nchar | Family::Nvarchar)));
            if unicode_cast && (inherited || self.recursive.last() == Some(&true)) {
                // Operand annotation can revisit a leaf through several enclosing
                // expressions. Reapplying an identical character cast is idempotent;
                // keep the first conversion, including its truncation and errors.
                if let Expr::Cast {
                    expr: source,
                    data_type,
                    ..
                } = expr
                {
                    while let Expr::Cast {
                        expr: inner,
                        data_type: inner_type,
                        kind: CastKind::Cast,
                        format: None,
                    } = source.as_ref()
                    {
                        if inner_type != data_type {
                            break;
                        }
                        *source = inner.clone();
                    }
                }
            }
            let currency_cast = matches!(expr,
                Expr::Cast { data_type, .. } | Expr::Convert { data_type: Some(data_type), .. }
                    if msduck_sql::money_cast::money_type(data_type).is_some());
            let json_consumer = matches!(expr, Expr::Function(function)
                if matches!(function.name.to_string().to_ascii_uppercase().as_str(),
                    "ISJSON" | "JSON_VALUE" | "JSON_QUERY" | "JSON_PATH_EXISTS" | "STRING_ESCAPE"
                    | "__MSDUCK_MIN_BIN2_UNICODE" | "__MSDUCK_MAX_BIN2_UNICODE"
                    | "__MSDUCK_MIN_BIN2_ANSI" | "__MSDUCK_MAX_BIN2_ANSI"));
            // Only the direct migrated operand (through parentheses/casts) changes
            // representation. Descendant text producers own their own adapters.
            self.consumer.push(
                currency_cast
                    || json_consumer
                    || (unicode_cast || matches!(expr, Expr::Nested(_))) && inherited,
            );
            std::ops::ControlFlow::Continue(())
        }
        fn post_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<()> {
            self.consumer.pop();
            let consumer = self.consumer.last() == Some(&true);
            let json_result = matches!(expr, Expr::Cast { expr: source, .. }
                if msduck_sql::for_json::unicode_result(source));
            let extrema_result = matches!(expr, Expr::Cast { expr: source, .. }
                if msduck_sql::projection::character_extrema::bound_result(source));
            let known_carrier = matches!(expr, Expr::Cast { expr: source, .. }
                if matches!(source.as_ref(), Expr::Function(f)
                    if matches!(f.name.to_string().as_str(),
                        "__msduck_cast_carrier_nvarchar" | "__msduck_cast_carrier_nchar"
                        | "__msduck_binary_nvarchar" | "__msduck_binary_nchar"
                        | "__msduck_try_binary_nvarchar" | "__msduck_try_binary_nchar")));
            if (consumer
                || json_result
                || extrema_result
                || known_carrier
                || self.recursive.last() == Some(&true))
                && let Expr::Cast {
                    expr: source,
                    data_type,
                    kind: CastKind::Cast,
                    format: None,
                } = expr
                && let Ok(Type::Character(target)) = msduck_sql::sql_type::declaration(data_type)
                && (extrema_result
                    || known_carrier
                    || matches!(target.family(), Family::Nchar | Family::Nvarchar))
            {
                // typeof is bind-time only. Each selected branch evaluates its
                // source once; ordinary numeric conversions retain the original
                // CAST, while an existing carrier never passes through VARCHAR.
                let sql = format!(
                    "CASE WHEN typeof(__msduck_cast_source)='STRUCT(__msduck_utf16le BLOB)' THEN CAST(__msduck_cast_source AS STRUCT(__msduck_utf16le BLOB)) ELSE __msduck_carrier_input(CAST(__msduck_cast_source AS {data_type})) END"
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
                        if matches!(e,Expr::Identifier(id) if id.value=="__msduck_cast_source") {
                            *e = self.0.clone();
                        }
                        std::ops::ControlFlow::Continue(())
                    }
                }
                if known_carrier || json_result || extrema_result {
                    // A nested normalized cast already has the exact physical type.
                    // Repeating a typeof dispatch would multiply the source AST.
                    dispatch = *source.clone();
                    if extrema_result {
                        dispatch =
                            msduck_sql::expr::unary_function("__msduck_carrier_input", dispatch);
                    }
                } else {
                    let _ = VisitMut::visit(&mut dispatch, &mut Substitute(*source.clone()));
                }
                let cast_width = match target.family() {
                    Family::Nvarchar => {
                        msduck_sql::expression_metadata::character::nvarchar_cast_width(data_type)
                            .ok()
                            .map(|n| n.map(i32::from).unwrap_or(-1))
                    }
                    Family::Nchar => {
                        msduck_sql::expression_metadata::character::nchar_cast_width(data_type)
                            .ok()
                            .flatten()
                            .map(i32::from)
                    }
                    Family::Varchar | Family::Char => {
                        msduck_sql::expression_metadata::character::varchar_cast_width(data_type)
                            .ok()
                            .map(|n| if n == u16::MAX { -1 } else { i32::from(n) })
                    }
                };
                *expr = msduck_sql::expr::binary_function(
                    match target.family() {
                        Family::Nchar => "__msduck_cast_carrier_nchar",
                        Family::Nvarchar => "__msduck_cast_carrier_nvarchar",
                        Family::Char => "__msduck_cast_carrier_char",
                        Family::Varchar => "__msduck_cast_carrier_varchar",
                    },
                    dispatch,
                    msduck_sql::expr::number(cast_width.unwrap_or_else(|| match target.length() {
                        Length::Max => -1,
                        Length::Bounded(n) => i32::from(n),
                    })),
                );
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let _ = node.visit(&mut Normalize::default());
}
