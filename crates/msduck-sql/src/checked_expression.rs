//! Bounded single-row plans carrying integer faults as data between AST nodes.
use sqlparser::{ast::*, dialect::DuckDbDialect, parser::Parser};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Int,
    BigInt,
    Boolean,
}
impl Kind {
    fn integer(self) -> bool {
        matches!(self, Self::Int | Self::BigInt)
    }
    fn sql(self) -> &'static str {
        match self {
            Self::Int => "INTEGER",
            Self::BigInt => "BIGINT",
            Self::Boolean => "BOOLEAN",
        }
    }
}

pub struct Plan {
    pub query: Box<Query>,
    pub kind: Kind,
}
struct Builder<'a> {
    parameters: &'a HashMap<String, Kind>,
    nodes: Vec<String>,
    leaves: Vec<(usize, Expr)>,
    checked: bool,
}
fn literal_null(expr: &Expr) -> bool {
    match expr {
        Expr::Value(value) => matches!(value.value, Value::Null),
        Expr::Nested(inner) | Expr::Cast { expr: inner, .. } => literal_null(inner),
        _ => false,
    }
}
const NULL_ERRORS: &str = "CAST(NULL AS INTEGER) AS error_number,CAST(NULL AS UTINYINT) AS error_state,CAST(NULL AS UTINYINT) AS error_severity,CAST(NULL AS VARCHAR) AS error_message";
const FIELDS: [&str; 4] = [
    "error_number",
    "error_state",
    "error_severity",
    "error_message",
];
fn errors(sources: &[&str]) -> String {
    FIELDS
        .iter()
        .map(|field| {
            let values = sources
                .iter()
                .map(|source| format!("{source}.{field}"))
                .collect::<Vec<_>>();
            let value = if values.len() == 1 {
                values[0].clone()
            } else {
                format!("COALESCE({})", values.join(","))
            };
            format!("{value} AS {field}")
        })
        .collect::<Vec<_>>()
        .join(",")
}
impl Builder<'_> {
    fn push(&mut self, sql: String, kind: Kind) -> (usize, Kind) {
        let index = self.nodes.len();
        self.nodes.push(sql);
        (index, kind)
    }
    fn leaf(&mut self, expr: &Expr, kind: Kind) -> (usize, Kind) {
        let index = self.nodes.len();
        self.leaves.push((index, expr.clone()));
        self.push(format!("SELECT CAST(0 AS {}) AS value,CAST(NULL AS INTEGER) AS error_number,CAST(NULL AS UTINYINT) AS error_state,CAST(NULL AS UTINYINT) AS error_severity,CAST(NULL AS VARCHAR) AS error_message",kind.sql()),kind)
    }
    fn unary(&mut self, child: usize, value: String, kind: Kind) -> (usize, Kind) {
        self.push(
            format!(
                "SELECT {value} AS value,{} FROM __msduck_checked_e{child} l",
                errors(&["l"])
            ),
            kind,
        )
    }
    fn binary(
        &mut self,
        left: (usize, Kind),
        right: (usize, Kind),
        op: &BinaryOperator,
        null_comparison: bool,
    ) -> Option<(usize, Kind)> {
        if !left.1.integer() || !right.1.integer() {
            return None;
        }
        let from = format!(
            "FROM __msduck_checked_e{} l CROSS JOIN __msduck_checked_e{} r",
            left.0, right.0
        );
        let arithmetic = match op {
            BinaryOperator::Plus => Some("add"),
            BinaryOperator::Minus => Some("subtract"),
            BinaryOperator::Multiply => Some("multiply"),
            BinaryOperator::Divide => Some("divide"),
            BinaryOperator::Modulo => Some("modulo"),
            _ => None,
        };
        if let Some(name) = arithmetic {
            self.checked = true;
            let kind = if left.1 == Kind::BigInt || right.1 == Kind::BigInt {
                Kind::BigInt
            } else {
                Kind::Int
            };
            return Some(self.push(format!("SELECT c.v.value AS value,{} {from} CROSS JOIN LATERAL (SELECT __msduck_checked_{name}(l.value,r.value) AS v) c",errors(&["l","r","c.v"])),kind));
        }
        if matches!(
            op,
            BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Lt
                | BinaryOperator::LtEq
                | BinaryOperator::Gt
                | BinaryOperator::GtEq
        ) {
            // The pinned reference folds a literal-NULL comparison to UNKNOWN,
            // but still reports the child fault when the NULL is a parameter.
            // Do not use runtime values to make this syntax-level decision.
            return Some(self.push(
                format!(
                    "SELECT l.value {op} r.value AS value,{} {from}",
                    if null_comparison {
                        NULL_ERRORS.into()
                    } else {
                        errors(&["l", "r"])
                    }
                ),
                Kind::Boolean,
            ));
        }
        None
    }
    fn expression(&mut self, expr: &Expr, depth: usize) -> Option<(usize, Kind)> {
        if depth > 64 || self.nodes.len() > 256 {
            return None;
        }
        match expr {
            Expr::Nested(inner) => self.expression(inner, depth + 1),
            Expr::Value(value) => match &value.value {
                Value::Number(n, _) if n.parse::<i32>().is_ok() => Some(self.leaf(expr, Kind::Int)),
                Value::Null => Some(self.leaf(expr, Kind::Int)),
                _ => None,
            },
            Expr::Identifier(id) => self
                .parameters
                .get(&id.value.to_lowercase())
                .copied()
                .filter(|kind| kind.integer())
                .map(|kind| self.leaf(expr, kind)),
            Expr::BinaryOp { left, op, right } => {
                let null_comparison = literal_null(left) || literal_null(right);
                let left = self.expression(left, depth + 1)?;
                let right = self.expression(right, depth + 1)?;
                self.binary(left, right, op, null_comparison)
            }
            Expr::UnaryOp { op, expr } => {
                let child = self.expression(expr, depth + 1)?;
                match op {
                    UnaryOperator::Plus if child.1.integer() => Some(child),
                    UnaryOperator::Minus if child.1.integer() => {
                        let zero = self.leaf(&crate::expr::number(0), Kind::Int);
                        self.binary(zero, child, &BinaryOperator::Minus, false)
                    }
                    UnaryOperator::Not if child.1 == Kind::Boolean => {
                        Some(self.unary(child.0, "NOT l.value".into(), Kind::Boolean))
                    }
                    _ => None,
                }
            }
            Expr::IsNull(inner) | Expr::IsNotNull(inner) => {
                let child = self.expression(inner, depth + 1)?;
                let not = if matches!(expr, Expr::IsNotNull(_)) {
                    "NOT "
                } else {
                    ""
                };
                Some(self.unary(child.0, format!("l.value IS {not}NULL"), Kind::Boolean))
            }
            Expr::Cast {
                kind: CastKind::Cast,
                expr,
                data_type,
                format: None,
            } => {
                let target = match data_type {
                    DataType::Int(_) | DataType::Integer(_) => Kind::Int,
                    DataType::BigInt(_) => Kind::BigInt,
                    _ => return None,
                };
                let child = self.expression(expr, depth + 1)?;
                if child.1 == target || child.1 == Kind::Int && target == Kind::BigInt {
                    Some(self.unary(
                        child.0,
                        format!("CAST(l.value AS {})", target.sql()),
                        target,
                    ))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Plans only the complete supported expression. Unsupported types, leaves,
/// CASE/Boolean conjunctions, narrowing conversions and subqueries keep the
/// caller's existing execution path; no partial rewrite changes their semantics.
/// Materialized nodes prevent repeated evaluation when value and error fields
/// are read separately. Original leaf ASTs are inserted without text reparsing.
pub fn plan(expression: &Expr, parameters: &HashMap<String, Kind>) -> Option<Plan> {
    let mut builder = Builder {
        parameters,
        nodes: vec![],
        leaves: vec![],
        checked: false,
    };
    let (last, kind) = builder.expression(expression, 0)?;
    if !builder.checked || builder.nodes.len() > 256 {
        return None;
    }
    let ctes = builder
        .nodes
        .iter()
        .enumerate()
        .map(|(i, sql)| format!("__msduck_checked_e{i} AS ({sql})"))
        .collect::<Vec<_>>()
        .join(",");
    let mut statements=Parser::parse_sql(&DuckDbDialect{},&format!("WITH {ctes} SELECT value,error_number,error_state,error_severity,error_message FROM __msduck_checked_e{last}")).expect("generated checked expression query");
    let Statement::Query(mut query) = statements.remove(0) else {
        return None;
    };
    let tables = &mut query.with.as_mut()?.cte_tables;
    for table in tables.iter_mut() {
        table.materialized = Some(CteAsMaterialized::Materialized);
    }
    for (index, expr) in builder.leaves {
        let SetExpr::Select(select) = tables[index].query.body.as_mut() else {
            return None;
        };
        let SelectItem::ExprWithAlias {
            expr: Expr::Cast { expr: inner, .. },
            ..
        } = &mut select.projection[0]
        else {
            return None;
        };
        **inner = expr;
    }
    Some(Plan { query, kind })
}
