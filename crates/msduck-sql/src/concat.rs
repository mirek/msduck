//! Character concatenation binding over explicit logical operand declarations.
//! The caller resolves names, aliases and code pages before lowering this plan.
use msduck_core::{
    character::{Family, Length},
    collation::{Conflict, Label},
};
use sqlparser::ast::{BinaryOperator, Expr, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub family: Family,
    pub length: Length,
    pub collation: Label,
}
impl Declaration {
    fn valid(&self) -> bool {
        match self.length {
            Length::Max => matches!(self.family, Family::Varchar | Family::Nvarchar),
            Length::Bounded(n) => {
                n <= if matches!(self.family, Family::Nchar | Family::Nvarchar) {
                    4000
                } else {
                    8000
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidDeclaration,
    Collation(Conflict),
    NoCollation { left: String, right: String },
}

/// Leaves retain one source expression; binary nodes retain each truncation point.
/// NULL leaves receive a contextual character declaration, not an observed value.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub declaration: Declaration,
    pub node: Node,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Leaf(Box<Expr>),
    Concat { left: Box<Plan>, right: Box<Plan> },
    Collate { child: Box<Plan> },
}

/// `resolve` supplies declarations from the caller's immutable binding scope.
/// Return None for unresolved, numeric, binary and unresolved alias operands.
/// This function does not rewrite input or inspect any runtime parameter value.
/// COLLATE names must already have been validated by the caller; its resolver
/// supplies the resulting explicit label for that expression.
pub fn bind(
    expr: &Expr,
    resolve: &impl Fn(&Expr) -> Option<Declaration>,
) -> Result<Option<Plan>, Error> {
    if let Expr::Nested(inner) = expr {
        return bind(inner, resolve);
    }
    if let Expr::Collate { expr: inner, .. } = expr {
        let Some(child) = bind(inner, resolve)? else {
            return Ok(None);
        };
        let Some(declaration) = resolve(expr) else {
            return Ok(None);
        };
        if !declaration.valid()
            || !matches!(declaration.collation, Label::Explicit(_))
            || declaration.family != child.declaration.family
            || declaration.length != child.declaration.length
        {
            return Err(Error::InvalidDeclaration);
        }
        return Ok(Some(Plan {
            declaration,
            node: Node::Collate {
                child: Box::new(child),
            },
        }));
    }
    if let Expr::BinaryOp {
        left,
        op: BinaryOperator::Plus | BinaryOperator::StringConcat,
        right,
    } = expr
    {
        let mut a = bind(left, resolve)?;
        let mut b = bind(right, resolve)?;
        fn null(expr: &Expr) -> bool {
            match expr {
                Expr::Nested(e) => null(e),
                Expr::Value(v) => matches!(v.value, Value::Null),
                _ => false,
            }
        }
        let contextual_null = |expr: &Expr, sibling: &Plan| {
            let mut declaration = sibling.declaration.clone();
            declaration.length = Length::Bounded(1);
            Plan {
                declaration,
                node: Node::Leaf(Box::new(expr.clone())),
            }
        };
        if a.is_none() && null(left) {
            a = b.as_ref().map(|b| contextual_null(left, b));
        }
        if b.is_none() && null(right) {
            b = a.as_ref().map(|a| contextual_null(right, a));
        }
        let (Some(left), Some(right)) = (a, b) else {
            return Ok(None);
        };
        let collation = left
            .declaration
            .collation
            .combine(&right.declaration.collation)
            .map_err(Error::Collation)?;
        if let Label::NoCollation { left, right } = collation {
            return Err(Error::NoCollation { left, right });
        }
        let (family, length) = msduck_core::concat::shape(
            (left.declaration.family, left.declaration.length),
            (right.declaration.family, right.declaration.length),
        );
        return Ok(Some(Plan {
            declaration: Declaration {
                family,
                length,
                collation,
            },
            node: Node::Concat {
                left: Box::new(left),
                right: Box::new(right),
            },
        }));
    }
    let Some(declaration) = resolve(expr) else {
        return Ok(None);
    };
    if !declaration.valid() {
        return Err(Error::InvalidDeclaration);
    }
    Ok(Some(Plan {
        declaration,
        node: Node::Leaf(Box::new(expr.clone())),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::parser::Parser;
    fn expr(sql: &str) -> Expr {
        Parser::new(&crate::dialect::ServerDialect)
            .try_with_sql(sql)
            .unwrap()
            .parse_expr()
            .unwrap()
    }
    fn declaration(length: Length) -> Declaration {
        Declaration {
            family: Family::Nvarchar,
            length,
            collation: Label::CoercibleDefault("Latin1_General_100_BIN2".into()),
        }
    }
    #[test]
    fn nested_nodes_keep_intermediate_caps_and_max_grouping() {
        let resolve = |e: &Expr| {
            Some(declaration(if e.to_string() == "c" {
                Length::Max
            } else {
                Length::Bounded(4000)
            }))
        };
        let input = expr("a+b+c");
        let before = input.clone();
        let p = bind(&input, &resolve).unwrap().unwrap();
        assert_eq!(input, before);
        assert_eq!(p.declaration.length, Length::Max);
        let Node::Concat { left, .. } = p.node else {
            panic!()
        };
        assert_eq!(left.declaration.length, Length::Bounded(4000));
        let p = bind(&expr("a+(b+c)"), &resolve).unwrap().unwrap();
        let Node::Concat { right, .. } = p.node else {
            panic!()
        };
        assert_eq!(right.declaration.length, Length::Max);
    }
    #[test]
    fn unknown_and_null_are_distinct_and_source_occurrences_remain_single() {
        let calls = std::cell::RefCell::new(Vec::new());
        let resolve = |e: &Expr| {
            calls.borrow_mut().push(e.to_string());
            (e.to_string() == "volatile()").then(|| declaration(Length::Bounded(3)))
        };
        let p = bind(&expr("volatile()+NULL"), &resolve).unwrap().unwrap();
        assert_eq!(p.declaration.length, Length::Bounded(4));
        assert_eq!(*calls.borrow(), ["volatile()", "NULL"]);
        assert!(
            bind(&expr("volatile()+unknown"), &resolve)
                .unwrap()
                .is_none()
        );
        assert!(bind(&expr("NULL+NULL"), &|_| None).unwrap().is_none());
    }
    #[test]
    fn explicit_collation_preserves_nested_plan_and_cannot_repair_child_failure() {
        let resolve = |e: &Expr| {
            let mut d = declaration(Length::Bounded(2));
            if matches!(e, Expr::Collate { .. }) {
                d.length = Length::Bounded(4);
                d.collation = Label::Explicit("Latin1_General_100_BIN2".into());
            }
            Some(d)
        };
        let p = bind(&expr("(a+b) COLLATE Latin1_General_100_BIN2"), &resolve)
            .unwrap()
            .unwrap();
        let Node::Collate { child } = p.node else {
            panic!()
        };
        assert!(matches!(child.node, Node::Concat { .. }));
        let conflict = |e: &Expr| {
            let mut d = resolve(e).unwrap();
            if let Expr::Identifier(id) = e {
                d.collation = Label::Implicit(id.value.clone());
            }
            Some(d)
        };
        assert!(matches!(
            bind(&expr("(a+b) COLLATE Latin1_General_100_BIN2"), &conflict),
            Err(Error::NoCollation { .. })
        ));
    }

    #[test]
    fn conflicting_collations_and_invalid_shapes_fail_without_mutation() {
        let input = expr("a+b");
        let before = input.clone();
        let resolve = |e: &Expr| {
            let mut d = declaration(Length::Bounded(2));
            d.collation = Label::Implicit(e.to_string());
            Some(d)
        };
        assert!(matches!(
            bind(&input, &resolve),
            Err(Error::NoCollation { .. })
        ));
        assert_eq!(input, before);
        assert_eq!(
            bind(&input, &|_| Some(declaration(Length::Bounded(4001)))),
            Err(Error::InvalidDeclaration)
        );
        let resolve = |e: &Expr| {
            let mut d = declaration(Length::Bounded(2));
            d.collation = Label::Explicit(e.to_string());
            Some(d)
        };
        assert!(matches!(
            bind(&input, &resolve),
            Err(Error::Collation(Conflict::Explicit { .. }))
        ));
    }
}
