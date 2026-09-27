//! Give each set-operation branch its own query scope for TOP lowering.
use sqlparser::ast::*;

pub fn wrap_set_branches(body: &mut SetExpr) {
    if let SetExpr::SetOperation { left, right, .. } = body {
        wrap_branch(left);
        wrap_branch(right);
    }
}

fn wrap_branch(body: &mut SetExpr) {
    if matches!(body, SetExpr::Select(select) if select.top.is_some()) {
        *body = SetExpr::Query(Box::new(Query {
            with: None,
            body: Box::new(body.clone()),
            order_by: None,
            limit_clause: None,
            fetch: None,
            locks: vec![],
            for_clause: None,
            settings: None,
            format_clause: None,
            pipe_operators: vec![],
        }));
    } else {
        // Existing query scopes are visited separately by Translator.
        wrap_set_branches(body);
    }
}

pub fn paging(query: &mut Query) -> Result<(), String> {
    let offset = match &mut query.limit_clause {
        Some(LimitClause::LimitOffset { offset, .. }) => offset.as_mut(),
        _ => None,
    };
    if offset.is_none() && query.fetch.is_none() {
        return Ok(());
    }
    if query.order_by.is_none() {
        return Err("OFFSET/FETCH requires ORDER BY".into());
    }
    if query.fetch.is_some() && offset.is_none() {
        return Err("FETCH requires OFFSET".into());
    }
    if let Some(offset) = offset {
        offset.value = checked_count(offset.value.clone(), "__msduck_offset_count");
    }
    if let Some(fetch) = &mut query.fetch {
        if fetch.percent || fetch.with_ties {
            return Err("unsupported FETCH PERCENT/WITH TIES".into());
        }
        let value = fetch.quantity.take().ok_or("FETCH requires a count")?;
        fetch.quantity = Some(checked_count(value, "__msduck_fetch_count"));
    }
    Ok(())
}

fn checked_count(value: Expr, function: &str) -> Expr {
    use crate::expr::{binary_function, unary_function};
    let invalid = unary_function(
        "error",
        Expr::Value(
            Value::SingleQuotedString("A TOP or FETCH clause contains an invalid value.".into())
                .into(),
        ),
    );
    unary_function(function, binary_function("coalesce", value, invalid))
}

/// sqlparser's FETCH parser only accepts Value nodes. Parse the count expression
/// ourselves, then restore its AST after the surrounding statement is parsed.
pub fn fetch_tokens(
    tokens: Vec<sqlparser::tokenizer::TokenWithSpan>,
) -> Result<(Vec<sqlparser::tokenizer::TokenWithSpan>, Vec<Expr>), sqlparser::parser::ParserError> {
    use sqlparser::{
        keywords::Keyword,
        parser::Parser,
        tokenizer::{Token, TokenWithSpan},
    };
    let mut tokens = tokens;
    let mut expressions = Vec::new();
    // Work inside out: parsing an outer count must see normalized FETCH
    // clauses inside its scalar subqueries. Replacements never move earlier
    // token positions, so this reverse walk keeps its indexes valid.
    for index in (0..tokens.len()).rev() {
        if matches!(&tokens[index].token, Token::Word(w) if w.keyword == Keyword::FETCH) {
            let mut parser = Parser::new(&crate::dialect::ServerDialect)
                .with_tokens_with_locations(tokens[index + 1..].to_vec());
            parser.expect_one_of_keywords(&[Keyword::FIRST, Keyword::NEXT])?;
            let expression = parser.parse_expr()?;
            parser.expect_one_of_keywords(&[Keyword::ROW, Keyword::ROWS])?;
            parser.expect_keyword(Keyword::ONLY)?;
            let consumed = parser.index();
            let normalized = "FETCH NEXT ? ROWS ONLY";
            let mut replacement =
                sqlparser::tokenizer::Tokenizer::new(&crate::dialect::ServerDialect, normalized)
                    .tokenize_with_location()?;
            for token in &mut replacement {
                if matches!(token.token, Token::Placeholder(_)) {
                    token.token = Token::Placeholder(format!("msduck:fetch:{}", expressions.len()));
                }
            }
            expressions.push(expression);
            let span = tokens[index].span;
            let replacement = replacement.into_iter().map(|token| TokenWithSpan {
                token: token.token,
                span,
            });
            tokens.splice(index..index + consumed + 1, replacement);
        }
    }
    Ok((tokens, expressions))
}

pub fn restore_fetch(statement: &mut Statement, expressions: &[Expr]) {
    struct Restore<'a>(&'a [Expr]);
    impl VisitorMut for Restore<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> std::ops::ControlFlow<()> {
            if let Some(fetch) = &mut query.fetch
                && let Some(Expr::Value(value)) = &fetch.quantity
                && let Value::Placeholder(marker) = &value.value
                && let Some(index) = marker
                    .strip_prefix("msduck:fetch:")
                    .and_then(|n| n.parse::<usize>().ok())
                && let Some(expression) = self.0.get(index)
            {
                fetch.quantity = Some(expression.clone());
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let _ = VisitMut::visit(statement, &mut Restore(expressions));
}

pub const TIES_WITHOUT_ORDER: &str =
    "The TOP N WITH TIES clause is not allowed without a corresponding ORDER BY clause.";
pub const PERCENT_RANGE: &str = "Percent values must be between 0 and 100.";
pub const INVALID_VALUE: &str = "A TOP or FETCH clause contains an invalid value.";
pub const NEGATIVE_COUNT: &str = "A TOP N or FETCH rowcount value may not be negative.";
pub const NONINTEGER_COUNT: &str = "The number of rows provided for a TOP or FETCH clauses row count parameter must be an integer.";
pub const DISTINCT_ORDER: &str =
    "ORDER BY items must appear in the select list if SELECT DISTINCT is specified.";
const POSITION_PREFIX: &str = "The ORDER BY position number ";
const WILDCARD_ORDINAL: &str = "unsupported TOP PERCENT/WITH TIES ordinal ordering over a wildcard";
const VOLATILE_KEY: &str =
    "unsupported TOP PERCENT/WITH TIES ordering by a volatile select-list expression";
/// Functions that return a different value on each evaluation. Ranking must
/// not evaluate them separately from the projected value.
const VOLATILE: [&str; 9] = [
    "newid",
    "newsequentialid",
    "rand",
    "crypt_gen_random",
    "random",
    "gen_random_uuid",
    "uuid",
    "nextval",
    "setseed",
];
const WINDOW_KEY: &str = "unsupported TOP PERCENT/WITH TIES ordering by a window function";
const AMBIGUOUS_PREFIX: &str = "Ambiguous column name '";

/// Diagnostics captured in reference/select-top-percent.json use class 15.
pub fn diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    let message = message
        .strip_prefix("Invalid Input Error: ")
        .unwrap_or(message);
    if message.starts_with(POSITION_PREFIX) {
        return Some(msduck_core::diagnostic::SqlError::new(108, 1, message));
    }
    if message.starts_with(AMBIGUOUS_PREFIX) {
        return Some(msduck_core::diagnostic::SqlError::new(209, 1, message));
    }
    let number = match message {
        TIES_WITHOUT_ORDER => 1062,
        PERCENT_RANGE => 1031,
        INVALID_VALUE => 1014,
        NEGATIVE_COUNT => 127,
        NONINTEGER_COUNT => 1060,
        DISTINCT_ORDER => 145,
        _ => return None,
    };
    let mut error = msduck_core::diagnostic::SqlError::new(number, 1, message);
    error.severity = 15;
    Some(error)
}

enum Constant {
    Null,
    Number(f64),
    Other,
}

fn constant(expr: &Expr) -> Constant {
    match expr {
        Expr::Nested(inner) => constant(inner),
        Expr::Value(value) => match &value.value {
            Value::Null => Constant::Null,
            Value::Number(n, _) => n.parse().map_or(Constant::Other, Constant::Number),
            _ => Constant::Other,
        },
        Expr::Cast { expr, .. } if matches!(constant(expr), Constant::Null) => Constant::Null,
        Expr::Function(_) if crate::variant_cast::source(expr).is_some() => {
            match crate::variant_cast::source(expr).map(constant) {
                Some(Constant::Null) => Constant::Null,
                _ => Constant::Other,
            }
        }
        Expr::UnaryOp { op, expr } => match (op, constant(expr)) {
            (UnaryOperator::Minus, Constant::Number(n)) => Constant::Number(-n),
            (UnaryOperator::Plus, value @ Constant::Number(_)) => value,
            (UnaryOperator::Minus | UnaryOperator::Plus, Constant::Null) => Constant::Null,
            _ => Constant::Other,
        },
        _ => Constant::Other,
    }
}

fn parse_expr(sql: &str) -> Expr {
    sqlparser::parser::Parser::new(&sqlparser::dialect::GenericDialect {})
        .try_with_sql(sql)
        .and_then(|mut parser| parser.parse_expr())
        .expect("static TOP lowering syntax")
}

fn string(text: &str) -> Expr {
    Expr::Value(Value::SingleQuotedString(text.into()).into())
}

/// The percentage is converted and validated in an uncorrelated scalar
/// subquery, so its expression is evaluated once for the statement.
fn percent(quantity: Expr) -> Result<Expr, String> {
    match constant(&quantity) {
        Constant::Null => return Err(INVALID_VALUE.into()),
        Constant::Number(n) if !(0.0..=100.0).contains(&n) => return Err(PERCENT_RANGE.into()),
        _ => {}
    }
    let mut limit = parse_expr(
        "(SELECT CASE WHEN __msduck_top_percent IS NULL THEN error(NULL) \
         WHEN __msduck_top_percent < 0 OR __msduck_top_percent > 100 THEN error(NULL) \
         ELSE __msduck_top_percent END \
         FROM (SELECT CAST(NULL AS DOUBLE) AS __msduck_top_percent) AS __msduck_top_percent_value)",
    );
    let Expr::Subquery(query) = &mut limit else {
        unreachable!()
    };
    let SetExpr::Select(select) = query.body.as_mut() else {
        unreachable!()
    };
    if let SelectItem::UnnamedExpr(Expr::Case { conditions, .. }) = &mut select.projection[0] {
        for (condition, message) in conditions.iter_mut().zip([INVALID_VALUE, PERCENT_RANGE]) {
            condition.result = crate::expr::unary_function("error", string(message));
        }
    }
    if let Some(TableFactor::Derived { subquery, .. }) =
        select.from.first_mut().map(|from| &mut from.relation)
        && let SetExpr::Select(inner) = subquery.body.as_mut()
        && let SelectItem::ExprWithAlias {
            expr: Expr::Cast { expr, .. },
            ..
        } = &mut inner.projection[0]
    {
        **expr = quantity;
    }
    Ok(limit)
}

fn count(quantity: Expr) -> Result<Expr, String> {
    match constant(&quantity) {
        Constant::Null => return Err(NONINTEGER_COUNT.into()),
        Constant::Number(n) if n < 0.0 => return Err(NEGATIVE_COUNT.into()),
        _ => {}
    }
    use crate::expr::{binary_function, number, unary_function};
    let mut limit = parse_expr("(SELECT NULL)");
    if let Expr::Subquery(query) = &mut limit
        && let SetExpr::Select(select) = query.body.as_mut()
    {
        // The existing count validator reports runtime NULL and negative values.
        select.projection[0] = SelectItem::UnnamedExpr(unary_function(
            "__msduck_top_count",
            binary_function("coalesce", quantity, number(-1)),
        ));
    }
    Ok(limit)
}

fn window(function: &str, keys: Vec<OrderByExpr>) -> Expr {
    let mut expr = parse_expr(&format!("{function}() OVER ()"));
    if let Expr::Function(Function {
        over: Some(WindowType::WindowSpec(spec)),
        ..
    }) = &mut expr
    {
        spec.order_by = keys;
    }
    expr
}

fn projected(item: &SelectItem) -> Result<&Expr, String> {
    match item {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => Ok(expr),
        _ => Err("unsupported TOP PERCENT/WITH TIES ordering over a wildcard".into()),
    }
}

/// An integer ORDER BY constant is a select-list position. Positions count
/// output columns, so they cannot be resolved against an unexpanded wildcard.
fn ordinal(expr: &Expr, projection: &[SelectItem]) -> Result<Option<usize>, String> {
    let Expr::Value(value) = expr else {
        return Ok(None);
    };
    let Value::Number(n, _) = &value.value else {
        return Ok(None);
    };
    let Ok(n) = n.parse::<usize>() else {
        return Ok(None);
    };
    if projection.iter().any(|item| {
        matches!(
            item,
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..)
        )
    }) {
        return Err(WILDCARD_ORDINAL.into());
    }
    if !(1..=projection.len()).contains(&n) {
        return Err(format!(
            "{POSITION_PREFIX}{n} is out of range of the number of items in the select list."
        ));
    }
    Ok(Some(n - 1))
}

fn volatile(expr: &Expr) -> bool {
    let mut found = false;
    let _ = visit_expressions(expr, |expr| {
        if let Expr::Function(function) = expr
            && VOLATILE
                .iter()
                .any(|name| function.name.to_string().eq_ignore_ascii_case(name))
        {
            found = true;
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    });
    found
}

/// Whether an ORDER BY key names a projected column. An unqualified name
/// matches a qualified column with that name; qualified names must agree.
fn same_column(projected: &Expr, key: &Expr) -> bool {
    let parts = |expr: &Expr| -> Option<Vec<String>> {
        match expr {
            Expr::Identifier(id) => Some(vec![id.value.to_lowercase()]),
            Expr::CompoundIdentifier(ids) => {
                Some(ids.iter().map(|id| id.value.to_lowercase()).collect())
            }
            _ => None,
        }
    };
    if projected == key {
        return true;
    }
    match (parts(projected), parts(key)) {
        (Some(a), Some(b)) if a.len() == 1 || b.len() == 1 => a.last() == b.last(),
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// A select-list alias named by an ORDER BY key. SQL Server rejects a name
/// that more than one select-list alias defines.
fn alias(expr: &Expr, projection: &[SelectItem]) -> Result<Option<usize>, String> {
    let Expr::Identifier(id) = expr else {
        return Ok(None);
    };
    let mut matches = projection.iter().enumerate().filter(|(_, item)| {
        matches!(item, SelectItem::ExprWithAlias { alias, .. } if alias.value.eq_ignore_ascii_case(&id.value))
    });
    let first = matches.next().map(|(index, _)| index);
    if matches.next().is_some() {
        return Err(format!("{AMBIGUOUS_PREFIX}{}'.", id.value));
    }
    Ok(first)
}

/// A ranking window cannot order by another window function.
fn windowed(expr: &Expr) -> bool {
    let mut found = false;
    let _ = visit_expressions(expr, |expr| {
        if matches!(expr, Expr::Function(Function { over: Some(_), .. })) {
            found = true;
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    });
    found
}

/// Rank ORDER BY keys as SQL Server resolves them: select-list aliases and
/// ordinals refer to projected expressions; other keys see source columns.
fn source_keys(
    keys: &[OrderByExpr],
    projection: &[SelectItem],
) -> Result<Vec<OrderByExpr>, String> {
    keys.iter()
        .map(|key| {
            let mut key = key.clone();
            let index = match ordinal(&key.expr, projection)? {
                Some(index) => Some(index),
                None => alias(&key.expr, projection)?,
            };
            if let Some(index) = index {
                let expr = projected(&projection[index])?;
                // The ranking copy would be evaluated apart from the projection.
                if volatile(expr) {
                    return Err(VOLATILE_KEY.into());
                }
                key.expr = expr.clone();
            }
            if windowed(&key.expr) {
                return Err(WINDOW_KEY.into());
            }
            Ok(key)
        })
        .collect()
}

fn output_name(item: &SelectItem) -> Option<&Ident> {
    match item {
        SelectItem::ExprWithAlias { alias, .. } => Some(alias),
        SelectItem::UnnamedExpr(Expr::Identifier(id)) => Some(id),
        SelectItem::UnnamedExpr(Expr::CompoundIdentifier(parts)) => parts.last(),
        _ => None,
    }
}

/// DISTINCT keys must name select-list items; they are then ordered by the
/// derived output columns after duplicates are removed.
fn distinct_keys(
    keys: &[OrderByExpr],
    projection: &[SelectItem],
) -> Result<Vec<OrderByExpr>, String> {
    for item in projection {
        projected(item)?;
    }
    keys.iter()
        .map(|key| {
            let index = match ordinal(&key.expr, projection)? {
                Some(index) => Some(index),
                None => alias(&key.expr, projection)?,
            }
            .or_else(|| {
                projection.iter().position(|item| {
                    same_column(projected(item).expect("checked projection"), &key.expr)
                })
            })
            .ok_or(DISTINCT_ORDER)?;
            let name = output_name(&projection[index])
                .ok_or("unsupported TOP DISTINCT ordering by an unnamed select-list item")?;
            let duplicates = projection
                .iter()
                .filter_map(output_name)
                .filter(|other| other.value.eq_ignore_ascii_case(&name.value))
                .count();
            if duplicates != 1 {
                return Err("unsupported TOP DISTINCT ordering by a repeated output name".into());
            }
            let mut key = key.clone();
            key.expr = Expr::Identifier(name.clone());
            Ok(key)
        })
        .collect()
}

/// Lower SELECT TOP PERCENT and TOP WITH TIES to a QUALIFY filter over the
/// rows that TOP sees: after grouping, HAVING and DISTINCT. PERCENT counts are
/// the ceiling of percentage times rows; WITH TIES keeps every row whose rank
/// among the ORDER BY keys is within the count. Plain TOP is left unchanged.
pub fn ranked(query: &mut Query) -> Result<(), String> {
    let SetExpr::Select(select) = query.body.as_mut() else {
        return Ok(());
    };
    if !select
        .top
        .as_ref()
        .is_some_and(|top| top.percent || top.with_ties)
    {
        return Ok(());
    }
    let top = select.top.take().expect("checked TOP");
    if query.limit_clause.is_some() || query.fetch.is_some() {
        return Err("TOP cannot be combined with OFFSET/FETCH in the same query".into());
    }
    let keys = match &query.order_by {
        None => vec![],
        Some(OrderBy {
            kind: OrderByKind::Expressions(keys),
            ..
        }) => keys.clone(),
        Some(_) => return Err("unsupported TOP PERCENT/WITH TIES ORDER BY form".into()),
    };
    if top.with_ties && keys.is_empty() {
        return Err(TIES_WITHOUT_ORDER.into());
    }
    let quantity = match top.quantity {
        Some(TopQuantity::Constant(n)) => crate::expr::number(n),
        Some(TopQuantity::Expr(expr)) => expr,
        None => return Err("TOP requires a count".into()),
    };
    if select.qualify.is_some() {
        return Err("unsupported TOP PERCENT/WITH TIES with QUALIFY".into());
    }
    let distinct = match &select.distinct {
        None | Some(Distinct::All) => false,
        Some(Distinct::Distinct) => true,
        Some(_) => return Err("unsupported TOP PERCENT/WITH TIES DISTINCT form".into()),
    };
    let keys = if distinct {
        distinct_keys(&keys, &select.projection)?
    } else {
        source_keys(&keys, &select.projection)?
    };
    // Without ORDER BY, TOP may return any rows. A constant key satisfies the
    // ranking function's ORDER BY requirement without imposing an order.
    let rank_keys = if keys.is_empty() {
        vec![OrderByExpr {
            expr: Expr::Value(Value::Null.into()),
            options: OrderByOptions {
                sort: None,
                nulls_first: None,
            },
            with_fill: None,
        }]
    } else {
        keys.clone()
    };
    let rank = window(if top.with_ties { "rank" } else { "row_number" }, rank_keys);
    let limit = if top.percent {
        let rows = parse_expr("count(*) OVER ()");
        let scaled = Expr::BinaryOp {
            left: Box::new(Expr::BinaryOp {
                left: Box::new(percent(quantity)?),
                op: BinaryOperator::Multiply,
                right: Box::new(rows),
            }),
            op: BinaryOperator::Divide,
            right: Box::new(crate::expr::number(100)),
        };
        crate::expr::unary_function("ceil", scaled)
    } else {
        count(quantity)?
    };
    let filter = Expr::BinaryOp {
        left: Box::new(rank),
        op: BinaryOperator::LtEq,
        right: Box::new(limit),
    };
    if distinct {
        let into = select.into.take();
        let inner = std::mem::replace(
            query.body.as_mut(),
            SetExpr::Values(Values {
                explicit_row: false,
                value_keyword: false,
                rows: vec![],
            }),
        );
        let mut outer = sqlparser::parser::Parser::new(&sqlparser::dialect::GenericDialect {})
            .try_with_sql("SELECT * FROM (SELECT 1) AS __msduck_top_distinct")
            .and_then(|mut parser| parser.parse_query())
            .expect("static TOP lowering syntax");
        let SetExpr::Select(outer_select) = outer.body.as_mut() else {
            unreachable!()
        };
        if let TableFactor::Derived { subquery, .. } = &mut outer_select.from[0].relation {
            *subquery.body = inner;
        }
        outer_select.into = into;
        outer_select.qualify = Some(filter);
        query.body = outer.body;
        if let Some(OrderBy {
            kind: OrderByKind::Expressions(order),
            ..
        }) = &mut query.order_by
        {
            *order = keys;
        }
    } else {
        select.qualify = Some(filter);
    }
    Ok(())
}
