//! Scoped character MIN/MAX annotations. Public aggregate names survive until
//! backend lowering so ordinary validation and result inference remain valid.
use super::*;
use msduck_core::{
    character::{CharacterType, Family, Length},
    diagnostic::SqlError,
    types::Type,
};
use std::ops::ControlFlow;

const BIN2: &str = "Latin1_General_100_BIN2";
const UNICODE: &str = "__msduck_extrema_input_unicode";
const ANSI: &str = "__msduck_extrema_input_ansi";

pub(super) struct Input {
    data_type: DataType,
    marker: &'static str,
}

fn argument(f: &Function) -> Option<&Expr> {
    if !matches!(
        f.name.to_string().to_ascii_uppercase().as_str(),
        "MIN" | "MAX"
    ) || !matches!(f.parameters, FunctionArguments::None)
        || f.filter.is_some()
        || f.null_treatment.is_some()
        || !f.within_group.is_empty()
    {
        return None;
    }
    let FunctionArguments::List(args) = &f.args else {
        return None;
    };
    if !args.clauses.is_empty()
        || (f.over.is_some() && args.duplicate_treatment == Some(DuplicateTreatment::Distinct))
    {
        return None;
    }
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = args.args.as_slice() else {
        return None;
    };
    Some(value)
}

fn annotation(source: &Expr) -> Option<(&str, &Expr)> {
    let Expr::Collate { expr, collation } = source else {
        return None;
    };
    if !collation.to_string().eq_ignore_ascii_case(BIN2) {
        return None;
    }
    let Expr::Cast {
        expr,
        data_type,
        kind: CastKind::Cast,
        format: None,
    } = expr.as_ref()
    else {
        return None;
    };
    let Type::Character(character) = crate::sql_type::declaration(data_type).ok()? else {
        return None;
    };
    let Expr::Function(f) = expr.as_ref() else {
        return None;
    };
    let marker = match f.name.to_string().as_str() {
        UNICODE => UNICODE,
        ANSI => ANSI,
        _ => return None,
    };
    let unicode = matches!(character.family(), Family::Nchar | Family::Nvarchar);
    if unicode != (marker == UNICODE) {
        return None;
    }
    Some((marker, crate::function_args::unary(f, marker).ok()??))
}

pub(super) fn logical_source(source: &Expr) -> &Expr {
    annotation(source).map_or(source, |(_, original)| original)
}

pub(super) fn input(catalog: &CatalogSnapshot, expr: &Expr, scope: &Scope) -> Option<Input> {
    let Expr::Function(f) = expr else { return None };
    let source = argument(f)?;
    if annotation(source).is_some() {
        return None;
    }
    let info = member_expression(catalog, source, &[], scope)?;
    let family = match info.system_type_id? {
        167 => Family::Varchar,
        175 => Family::Char,
        231 => Family::Nvarchar,
        239 => Family::Nchar,
        _ => return None,
    };
    let label = expression_collation(catalog, source, &[], scope)?.ok()?;
    if !label.name()?.eq_ignore_ascii_case(BIN2) {
        return None;
    }
    let unicode = matches!(family, Family::Nchar | Family::Nvarchar);
    let length = match info.max_length? {
        -1 => Length::Max,
        n if n > 0 && (!unicode || n % 2 == 0) => {
            Length::Bounded(u16::try_from(n / if unicode { 2 } else { 1 }).ok()?)
        }
        _ => return None,
    };
    let character = CharacterType::new(family, length).ok()?;
    Some(Input {
        data_type: crate::sql_type::ast(Type::Character(character)),
        marker: if unicode { UNICODE } else { ANSI },
    })
}

/// Annotate only operands whose declaration and BIN2 collation are proven.
pub fn annotate(
    catalog: &CatalogSnapshot,
    query: &mut Query,
    outer: &Scope,
) -> Result<(), SqlError> {
    let plan = super::collation_validation::extrema_plan(catalog, query, outer)?;
    struct Annotate {
        position: usize,
        pending: std::iter::Peekable<std::vec::IntoIter<(usize, Input)>>,
    }
    impl VisitorMut for Annotate {
        type Break = ();
        fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if self
                .pending
                .peek()
                .is_some_and(|(p, _)| *p == self.position)
            {
                let (_, input) = self.pending.next().unwrap();
                let Expr::Function(f) = expr else {
                    unreachable!("planned aggregate")
                };
                let FunctionArguments::List(args) = &mut f.args else {
                    unreachable!()
                };
                let FunctionArg::Unnamed(FunctionArgExpr::Expr(source)) = &mut args.args[0] else {
                    unreachable!()
                };
                let original = std::mem::replace(source, crate::expr::number(0));
                *source = Expr::Collate {
                    expr: Box::new(Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(crate::expr::unary_function(input.marker, original)),
                        data_type: input.data_type,
                        format: None,
                    }),
                    collation: ObjectName::from(vec![Ident::new(BIN2)]),
                };
            }
            self.position += 1;
            ControlFlow::Continue(())
        }
    }
    let mut visitor = Annotate {
        position: 0,
        pending: plan.into_iter().peekable(),
    };
    let _ = VisitMut::visit(query, &mut visitor);
    debug_assert!(visitor.pending.next().is_none());
    Ok(())
}

/// True only for a character aggregate carrying the scoped binding annotation.
pub fn bound_result(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(expr) | Expr::Collate { expr, .. } => bound_result(expr),
        Expr::Function(f) => argument(f).and_then(annotation).is_some(),
        Expr::Subquery(query) => match query.body.as_ref() {
            SetExpr::Select(select) => match select.projection.as_slice() {
                [
                    SelectItem::UnnamedExpr(value) | SelectItem::ExprWithAlias { expr: value, .. },
                ] => bound_result(value),
                _ => false,
            },
            _ => false,
        },
        _ => false,
    }
}

/// Consume the annotation after public aggregate validation. The backend adapter
/// normalizes the retained operand and supplies its exact carrier conversion.
pub fn lower(expr: &mut Expr) -> bool {
    let Expr::Function(f) = expr else {
        return false;
    };
    let Some((marker, original)) = argument(f).and_then(annotation) else {
        return false;
    };
    let minimum = f.name.to_string().eq_ignore_ascii_case("MIN");
    let name = match (minimum, marker == UNICODE) {
        (true, true) => "__msduck_min_bin2_unicode",
        (false, true) => "__msduck_max_bin2_unicode",
        (true, false) => "__msduck_min_bin2_ansi",
        (false, false) => "__msduck_max_bin2_ansi",
    };
    let original = original.clone();
    f.name = ObjectName::from(vec![Ident::new(name)]);
    let FunctionArguments::List(args) = &mut f.args else {
        unreachable!()
    };
    // DISTINCT cannot change an extremum. Avoid a backend hash-distinct step
    // that reorders SQL-equal padded spellings before selecting the payload.
    // Invalid DISTINCT OVER calls never receive a binding annotation.
    args.duplicate_treatment = None;
    args.args[0] = FunctionArg::Unnamed(FunctionArgExpr::Expr(original));
    true
}
