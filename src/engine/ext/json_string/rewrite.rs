//! AST lowering of HASHBYTES, JSON_MODIFY, STRING_AGG and STRING_SPLIT.
//!
//! The lowerings run before result metadata is derived, so each result is
//! wrapped in a CAST that carries SQL Server's declaration (VARBINARY(8000),
//! NVARCHAR(MAX), the STRING_AGG and STRING_SPLIT families). Compile-time
//! diagnostics (arity, argument types, separator and ordinal rules) are
//! raised here, before any result metadata; runtime ones come from the
//! native functions.
use crate::engine::{Parameter, Session};
use anyhow::Result;
use msduck_core::{
    character::{Family, Length},
    diagnostic::SqlError,
    types::Type,
};
use msduck_sql::dialect::ext::json_string::auto::SPLIT_RELATION;
use sqlparser::{ast::*, dialect::GenericDialect, parser::Parser};
use std::{collections::HashMap, ops::ControlFlow};

/// Lower every json_string construct in `node`, innermost first.
pub(super) fn lower<T: VisitMut>(
    session: &Session,
    node: &mut T,
    parameters: &HashMap<String, Parameter>,
) -> Result<()> {
    let mut lower = Lower {
        session,
        parameters,
        scopes: Vec::new(),
        probing: std::cell::Cell::new(false),
    };
    match node.visit(&mut lower) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

/// A query's row sources and visible CTEs, for typing column operands.
struct QueryScope {
    with: Vec<Cte>,
    from: Vec<TableWithJoins>,
    /// The WITHIN GROUP ordering of the scope's first ordered STRING_AGG.
    ordering: Option<String>,
}

struct Lower<'a> {
    session: &'a Session,
    parameters: &'a HashMap<String, Parameter>,
    scopes: Vec<QueryScope>,
    /// Set while a typing probe lowers STRING_SPLIT sources, so nested
    /// probes cannot recurse.
    probing: std::cell::Cell<bool>,
}

impl VisitorMut for Lower<'_> {
    type Break = anyhow::Error;

    fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<Self::Break> {
        let mut with = self
            .scopes
            .last()
            .map(|scope| scope.with.clone())
            .unwrap_or_default();
        if let Some(local) = &query.with {
            with.retain(|cte| {
                !local.cte_tables.iter().any(|own| {
                    own.alias
                        .name
                        .value
                        .eq_ignore_ascii_case(&cte.alias.name.value)
                })
            });
            with.extend(local.cte_tables.iter().cloned());
        }
        let from = msduck_sql::dialect::ext::json_string::auto::first_select(&query.body)
            .map(|select| select.from.clone())
            .unwrap_or_default();
        self.scopes.push(QueryScope {
            with,
            from,
            ordering: None,
        });
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut Query) -> ControlFlow<Self::Break> {
        self.scopes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<Self::Break> {
        match self.split(factor) {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    }

    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        let Expr::Function(function) = expr else {
            return ControlFlow::Continue(());
        };
        let result = if named(function, "HASHBYTES") {
            self.hashbytes(function).map(Some)
        } else if named(function, "JSON_MODIFY") {
            self.json_modify(function).map(Some)
        } else if named(function, "STRING_AGG") && !lowered_string_agg(function) {
            self.order(function)
                .and_then(|()| self.string_agg(function))
                .map(Some)
        } else {
            Ok(None)
        };
        match result {
            Ok(Some(lowered)) => {
                *expr = lowered;
                ControlFlow::Continue(())
            }
            Ok(None) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(error),
        }
    }
}

/// A STRING_AGG this module already lowered (rewrites must be idempotent).
fn lowered_string_agg(function: &Function) -> bool {
    matches!(&function.args, FunctionArguments::List(list)
        if matches!(list.args.first(), Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Function(input))))
            if named(input, "__msduck_json_string_input")))
}

fn named(function: &Function, name: &str) -> bool {
    matches!(function.name.0.as_slice(), [part]
        if part.as_ident().is_some_and(|ident| ident.value.eq_ignore_ascii_case(name)))
}

/// Plain positional arguments; None for named or wildcard arguments.
fn arguments(function: &Function) -> Option<Vec<Expr>> {
    match &function.args {
        FunctionArguments::None => Some(Vec::new()),
        FunctionArguments::List(list) => list
            .args
            .iter()
            .map(|arg| match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr.clone()),
                _ => None,
            })
            .collect(),
        FunctionArguments::Subquery(_) => None,
    }
}

pub(super) fn call(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Function(Function {
        name: ObjectName::from(vec![Ident::new(name)]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: args
                .into_iter()
                .map(|arg| FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)))
                .collect(),
            clauses: Vec::new(),
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: Vec::new(),
    })
}

fn cast(expr: Expr, data_type: DataType) -> Expr {
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(expr),
        data_type,
        format: None,
    }
}

fn string(text: &str) -> Expr {
    Expr::Value(Value::SingleQuotedString(text.into()).into())
}

fn boolean(value: bool) -> Expr {
    Expr::Value(Value::Boolean(value).into())
}

fn is_null(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => is_null(inner),
        Expr::Value(value) => matches!(value.value, Value::Null),
        _ => false,
    }
}

fn character(family: Family, length: Length) -> DataType {
    msduck_sql::sql_type::ast(Type::Character(
        msduck_core::character::CharacterType::new(family, length).expect("valid declaration"),
    ))
}

fn unicode(family: Family) -> bool {
    matches!(family, Family::Nvarchar | Family::Nchar)
}

/// SQL Server's name of a type in error 8116.
fn type_name(kind: &Type, literal: bool) -> &'static str {
    match kind {
        Type::Bit => "bit",
        Type::TinyInt => "tinyint",
        Type::SmallInt => "smallint",
        Type::Int => "int",
        Type::BigInt => "bigint",
        Type::Real => "real",
        Type::Float => "float",
        Type::Decimal(_) if literal => "numeric",
        Type::Decimal(_) => "decimal",
        Type::Money => "money",
        Type::SmallMoney => "smallmoney",
        Type::Character(kind) => match kind.family() {
            Family::Varchar => "varchar",
            Family::Char => "char",
            Family::Nvarchar => "nvarchar",
            Family::Nchar => "nchar",
        },
        Type::Binary(kind) if kind.fixed() => "binary",
        Type::Binary(_) => "varbinary",
        Type::Date => "date",
        Type::DateTime => "datetime",
        Type::SmallDateTime => "smalldatetime",
        Type::Time(_) => "time",
        Type::DateTime2(_) => "datetime2",
        Type::DateTimeOffset(_) => "datetimeoffset",
        Type::UniqueIdentifier => "uniqueidentifier",
        Type::Text => "text",
        Type::Ntext => "ntext",
        Type::Image => "image",
        Type::Xml => "xml",
        Type::Variant => "sql_variant",
    }
}

fn invalid(kind: &str, position: usize, function: &str) -> anyhow::Error {
    SqlError::new(
        8116,
        1,
        format!(
            "Argument data type {kind} is invalid for argument {position} of {function} function."
        ),
    )
    .into()
}

fn literal(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => literal(inner),
        Expr::UnaryOp { expr, .. } => literal(expr),
        Expr::Value(_) => true,
        _ => false,
    }
}

/// Factors usable in a typing probe: base tables, views, CTEs and derived
/// tables. Table-valued functions are left out; joins become CROSS JOINs.
fn probe_from(from: &[TableWithJoins]) -> Vec<TableWithJoins> {
    let usable = |factor: &TableFactor| match factor {
        TableFactor::Table { args, .. } => args.is_none(),
        TableFactor::Derived { .. } | TableFactor::NestedJoin { .. } => true,
        _ => false,
    };
    let mut out = Vec::new();
    for table in from {
        let factors = std::iter::once(&table.relation)
            .chain(table.joins.iter().map(|join| &join.relation))
            .filter(|factor| usable(factor))
            .cloned()
            .collect::<Vec<_>>();
        let mut factors = factors.into_iter();
        if let Some(relation) = factors.next() {
            out.push(TableWithJoins {
                relation,
                joins: factors
                    .map(|relation| Join {
                        relation,
                        global: false,
                        join_operator: JoinOperator::CrossJoin(JoinConstraint::None),
                    })
                    .collect(),
            });
        }
    }
    out
}

impl Lower<'_> {
    /// Ordered aggregates of one scope must agree on their ordering (8711).
    fn order(&mut self, function: &Function) -> Result<()> {
        if function.within_group.is_empty() {
            return Ok(());
        }
        let ordering = function
            .within_group
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
            .to_lowercase();
        let Some(scope) = self.scopes.last_mut() else {
            return Ok(());
        };
        match &scope.ordering {
            Some(first) if *first != ordering => Err(SqlError::new(
                8711,
                1,
                "Multiple ordered aggregate functions in the same scope have mutually incompatible orderings.",
            )
            .into()),
            Some(_) => Ok(()),
            None => {
                scope.ordering = Some(ordering);
                Ok(())
            }
        }
    }

    /// The declared type of an operand, from its own syntax (literals,
    /// CASTs, variables, concatenation) or, for column references, from the
    /// enclosing query's row sources. None when unknown.
    fn static_type(&self, expr: &Expr) -> Option<Type> {
        if is_null(expr) {
            return None;
        }
        if let Some(kind) = msduck_sql::datalength::kind(expr, self.parameters, &|_| None)
            && let Ok(kind) = msduck_sql::sql_type::declaration(&kind)
        {
            return Some(kind);
        }
        match expr {
            Expr::Nested(inner) | Expr::Collate { expr: inner, .. } => self.static_type(inner),
            Expr::BinaryOp {
                left,
                op: BinaryOperator::Plus | BinaryOperator::StringConcat,
                right,
            } => {
                let (Type::Character(left), Type::Character(right)) =
                    (self.static_type(left)?, self.static_type(right)?)
                else {
                    return None;
                };
                let unicode = unicode(left.family()) || unicode(right.family());
                let family = if unicode {
                    Family::Nvarchar
                } else {
                    Family::Varchar
                };
                let limit = if unicode { 4000 } else { 8000 };
                let length = match (left.length(), right.length()) {
                    (Length::Bounded(a), Length::Bounded(b))
                        if u32::from(a) + u32::from(b) <= limit =>
                    {
                        Length::Bounded(a + b)
                    }
                    _ => Length::Max,
                };
                msduck_core::character::CharacterType::new(family, length)
                    .ok()
                    .map(Type::Character)
            }
            _ => self.probe(expr),
        }
    }

    /// Type a column operand by binding `SELECT expr FROM <scope>`.
    fn probe(&self, expr: &Expr) -> Option<Type> {
        if self.probing.get() {
            return None;
        }
        let scope = self.scopes.last()?;
        // Type STRING_SPLIT sources through their lowered form.
        let mut from = scope.from.clone();
        struct Split<'a, 'b>(&'a Lower<'b>);
        impl VisitorMut for Split<'_, '_> {
            type Break = ();
            fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<()> {
                if self.0.split(factor).is_err() {
                    return ControlFlow::Break(());
                }
                ControlFlow::Continue(())
            }
        }
        self.probing.set(true);
        let split = from
            .iter_mut()
            .all(|table| table.visit(&mut Split(self)).is_continue());
        self.probing.set(false);
        if !split {
            return None;
        }
        let from = probe_from(&from);
        if from.is_empty() {
            return None;
        }
        let Statement::Query(mut query) =
            Parser::parse_sql(&GenericDialect {}, "SELECT 1 AS __msduck_probe")
                .ok()?
                .remove(0)
        else {
            return None;
        };
        if !scope.with.is_empty() {
            query.with = Some(With {
                with_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                recursive: false,
                cte_tables: scope.with.clone(),
            });
        }
        let SetExpr::Select(select) = query.body.as_mut() else {
            return None;
        };
        select.from = from;
        select.projection = vec![SelectItem::ExprWithAlias {
            expr: expr.clone(),
            alias: Ident::new("__msduck_probe"),
        }];
        let fields = crate::query_catalog::projection_with_parameters(
            &self.session.db,
            &query,
            self.parameters,
        )
        .ok()??;
        fields.first()?.info.as_ref()?.logical_type()
    }

    fn hashbytes(&self, function: &Function) -> Result<Expr> {
        let args = arguments(function)
            .ok_or_else(|| anyhow::anyhow!("unsupported HASHBYTES arguments"))?;
        if args.len() != 2 {
            return Err(
                SqlError::syntax(174, 1, "The hashbytes function requires 2 argument(s).").into(),
            );
        }
        for (position, arg) in args.iter().enumerate() {
            if is_null(arg) {
                return Err(invalid("NULL", position + 1, "hashbytes"));
            }
        }
        if let Some(kind) = self.static_type(&args[0])
            && !matches!(&kind, Type::Character(c) if matches!(c.family(), Family::Varchar | Family::Nvarchar | Family::Char | Family::Nchar))
        {
            return Err(invalid(type_name(&kind, literal(&args[0])), 1, "hashbytes"));
        }
        let encoding = match self.static_type(&args[1]) {
            Some(Type::Character(kind)) if unicode(kind.family()) => "n",
            Some(Type::Character(_)) => "v",
            Some(Type::Binary(_)) | None => "",
            Some(kind) => return Err(invalid(type_name(&kind, literal(&args[1])), 2, "hashbytes")),
        };
        let mut args = args;
        args.push(string(encoding));
        Ok(cast(
            call("__msduck_hashbytes", args),
            DataType::Varbinary(Some(BinaryLength::IntegerLength { length: 8000 })),
        ))
    }

    fn json_modify(&self, function: &Function) -> Result<Expr> {
        let args = arguments(function)
            .ok_or_else(|| anyhow::anyhow!("unsupported JSON_MODIFY arguments"))?;
        if args.len() != 3 {
            return Err(SqlError::syntax(
                174,
                1,
                "The json_modify function requires 3 argument(s).",
            )
            .into());
        }
        if is_null(&args[1]) {
            return Err(invalid("NULL", 2, "json_modify"));
        }
        for (position, arg) in args.iter().enumerate().take(2) {
            if let Some(kind) = self.static_type(arg)
                && !matches!(kind, Type::Character(_))
            {
                return Err(invalid(
                    type_name(&kind, literal(arg)),
                    position + 1,
                    "json_modify",
                ));
            }
        }
        if let Some(kind) = self.static_type(&args[2])
            && !matches!(
                kind,
                Type::Bit
                    | Type::TinyInt
                    | Type::SmallInt
                    | Type::Int
                    | Type::BigInt
                    | Type::Real
                    | Type::Float
                    | Type::Decimal(_)
                    | Type::Character(_)
            )
        {
            return Err(invalid(
                type_name(&kind, literal(&args[2])),
                3,
                "json_modify",
            ));
        }
        let json = json_result(&args[2]);
        let mut args = args;
        args.push(boolean(json));
        Ok(cast(
            call("__msduck_json_modify", args),
            DataType::Nvarchar(Some(CharacterLength::Max)),
        ))
    }

    fn string_agg(&self, function: &Function) -> Result<Expr> {
        let FunctionArguments::List(list) = &function.args else {
            anyhow::bail!("unsupported STRING_AGG arguments");
        };
        if list.duplicate_treatment.is_some() {
            return Err(SqlError::syntax(102, 1, "Incorrect syntax near ','.").into());
        }
        if function.over.is_some() {
            return Err(SqlError::syntax(
                4113,
                4,
                "The function 'STRING_AGG' is not a valid windowing function, and cannot be used with the OVER clause.",
            )
            .into());
        }
        let args = arguments(function)
            .ok_or_else(|| anyhow::anyhow!("unsupported STRING_AGG arguments"))?;
        let [value, separator] = args.as_slice() else {
            return Err(SqlError::syntax(
                174,
                1,
                "The string_agg function requires 2 argument(s).",
            )
            .into());
        };
        let separator_type = match separator {
            _ if is_null_constant(separator) => None,
            Expr::Value(literal) => match &literal.value {
                Value::SingleQuotedString(_) | Value::NationalStringLiteral(_) | Value::Null => {
                    self.static_type(separator)
                }
                Value::Number(..) => return Err(invalid("int", 2, "string_agg")),
                _ => return Err(separator_error()),
            },
            Expr::Identifier(ident) if ident.value.starts_with('@') => {
                let kind = self.static_type(separator);
                if let Some(Type::Character(kind)) = &kind
                    && kind.length() == Length::Max
                {
                    return Err(SqlError::new(
                        8734,
                        1,
                        "Separator parameter for STRING_AGG cannot be large object type such as VARCHAR(MAX) or NVARCHAR(MAX).",
                    )
                    .into());
                }
                kind
            }
            _ => return Err(separator_error()),
        };
        let value_type = self.static_type(value);
        let declaration = match &value_type {
            Some(Type::Binary(kind)) => {
                return Err(invalid(
                    if kind.fixed() { "binary" } else { "varbinary" },
                    1,
                    "string_agg",
                ));
            }
            Some(Type::Character(kind)) => {
                if !unicode(kind.family())
                    && matches!(&separator_type, Some(Type::Character(s)) if unicode(s.family()))
                {
                    return Err(invalid("nvarchar", 2, "string_agg"));
                }
                Some((unicode(kind.family()), kind.length() == Length::Max))
            }
            Some(_) => Some((true, false)),
            None => None,
        };
        let separator = if is_null_constant(separator) {
            string("")
        } else if matches!(separator, Expr::Identifier(_)) {
            call("coalesce", vec![separator.clone(), string("")])
        } else {
            separator.clone()
        };
        // Other known types are formatted as text by SQL conversion rules.
        let input = match &value_type {
            Some(Type::Character(_)) | None => value.clone(),
            Some(_) => cast(
                value.clone(),
                character(Family::Nvarchar, Length::Bounded(4000)),
            ),
        };
        let mut aggregate = call(
            "string_agg",
            vec![call("__msduck_json_string_input", vec![input]), separator],
        );
        if let Expr::Function(lowered) = &mut aggregate {
            lowered.filter = function.filter.clone();
            if !function.within_group.is_empty()
                && let FunctionArguments::List(list) = &mut lowered.args
            {
                list.clauses.push(FunctionArgumentClause::OrderBy(
                    function.within_group.clone(),
                ));
            }
        }
        Ok(match declaration {
            Some((unicode, true)) => cast(
                aggregate,
                character(
                    if unicode {
                        Family::Nvarchar
                    } else {
                        Family::Varchar
                    },
                    Length::Max,
                ),
            ),
            Some((unicode, false)) => cast(
                call(
                    "__msduck_string_agg_limit",
                    vec![aggregate, boolean(unicode)],
                ),
                if unicode {
                    character(Family::Nvarchar, Length::Bounded(4000))
                } else {
                    character(Family::Varchar, Length::Bounded(8000))
                },
            ),
            None => aggregate,
        })
    }

    fn split(&self, factor: &mut TableFactor) -> Result<()> {
        let TableFactor::Table {
            name,
            alias,
            args: Some(table_args),
            ..
        } = factor
        else {
            return Ok(());
        };
        if !matches!(name.0.as_slice(), [part]
            if part.as_ident().is_some_and(|ident| ident.value.eq_ignore_ascii_case("STRING_SPLIT")))
        {
            return Ok(());
        }
        let args = table_args
            .args
            .iter()
            .map(|arg| match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr.clone()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow::anyhow!("unsupported STRING_SPLIT arguments"))?;
        if args.len() < 2 {
            return Err(SqlError::new(
                313,
                3,
                "An insufficient number of arguments were supplied for the procedure or function STRING_SPLIT.",
            )
            .into());
        }
        if args.len() > 3 {
            return Err(SqlError::new(
                8144,
                3,
                "Procedure or function STRING_SPLIT has too many arguments specified.",
            )
            .into());
        }
        let source_type = self.static_type(&args[0]);
        let separator_type = self.static_type(&args[1]);
        for (position, kind) in [(1, &source_type), (2, &separator_type)] {
            if let Some(kind) = kind
                && !matches!(kind, Type::Character(_))
            {
                return Err(invalid(
                    type_name(kind, literal(&args[position - 1])),
                    position,
                    "string_split",
                ));
            }
        }
        let ordinal = match args.get(2) {
            None => false,
            Some(expr) => ordinal(expr)?,
        };
        let value_type = match &source_type {
            Some(Type::Character(kind)) => {
                let wide = unicode(kind.family())
                    || matches!(&separator_type, Some(Type::Character(s)) if unicode(s.family()));
                Some(character(
                    if wide {
                        Family::Nvarchar
                    } else {
                        Family::Varchar
                    },
                    kind.length(),
                ))
            }
            _ => None,
        };
        let Statement::Query(mut subquery) = Parser::parse_sql(
            &GenericDialect {},
            &format!(
                "SELECT {SPLIT_RELATION}.r.value AS value, ISNULL(CAST({SPLIT_RELATION}.r.ordinal AS BIGINT), 0) AS ordinal FROM (SELECT unnest(__msduck_string_split(NULL, NULL)) AS r) AS {SPLIT_RELATION}"
            ),
        )?
        .remove(0) else {
            unreachable!()
        };
        let SetExpr::Select(select) = subquery.body.as_mut() else {
            unreachable!()
        };
        if !ordinal {
            select.projection.truncate(1);
        }
        if let (Some(kind), SelectItem::ExprWithAlias { expr, .. }) =
            (value_type, &mut select.projection[0])
        {
            *expr = cast(expr.clone(), kind);
            // Tokens of a string literal are never NULL (TDS flags 0).
            if string_literal(&args[0]) {
                *expr = call("ISNULL", vec![expr.clone(), string("")]);
            }
        }
        let TableFactor::Derived {
            lateral,
            subquery: inner,
            ..
        } = &mut select.from[0].relation
        else {
            unreachable!()
        };
        *lateral = true;
        let SetExpr::Select(inner) = inner.body.as_mut() else {
            unreachable!()
        };
        let SelectItem::ExprWithAlias { expr, .. } = &mut inner.projection[0] else {
            unreachable!()
        };
        *expr = call(
            "unnest",
            vec![call(
                "__msduck_string_split",
                vec![args[0].clone(), args[1].clone()],
            )],
        );
        *factor = TableFactor::Derived {
            lateral: true,
            subquery,
            alias: Some(alias.clone().unwrap_or_else(|| TableAlias {
                explicit: true,
                name: Ident::new("STRING_SPLIT"),
                columns: Vec::new(),
                at: None,
            })),
            sample: None,
        };
        Ok(())
    }
}

/// NULL, or a CAST of NULL (accepted as a STRING_AGG separator).
fn is_null_constant(expr: &Expr) -> bool {
    match expr {
        Expr::Cast { expr, .. } | Expr::Nested(expr) => is_null_constant(expr),
        _ => is_null(expr),
    }
}

fn string_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => string_literal(inner),
        Expr::Value(value) => matches!(
            value.value,
            Value::SingleQuotedString(_) | Value::NationalStringLiteral(_)
        ),
        _ => false,
    }
}

fn separator_error() -> anyhow::Error {
    SqlError::new(
        8733,
        1,
        "Separator parameter for STRING_AGG must be a string literal or variable.",
    )
    .into()
}

/// The constant `enable_ordinal` argument of STRING_SPLIT.
fn ordinal(expr: &Expr) -> Result<bool> {
    let constant = |text: &str| -> Result<bool> {
        if text.contains(['.', 'e', 'E']) {
            return Err(invalid("numeric", 3, "string_split"));
        }
        match text.parse::<i64>() {
            Ok(0) => Ok(false),
            Ok(1) => Ok(true),
            _ => Err(SqlError::new(
                4199,
                1,
                format!(
                    "Argument value {text} is invalid for argument 3 of string_split function."
                ),
            )
            .into()),
        }
    };
    match expr {
        Expr::Nested(inner) => ordinal(inner),
        Expr::Value(value) => match &value.value {
            Value::Null => Ok(false),
            Value::Number(text, _) => constant(text),
            _ => Err(invalid("varchar", 3, "string_split")),
        },
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => match expr.as_ref() {
            Expr::Value(value) if matches!(value.value, Value::Number(..)) => {
                let Value::Number(text, _) = &value.value else {
                    unreachable!()
                };
                constant(&format!("-{text}"))
            }
            _ => Err(ordinal_constant()),
        },
        Expr::Cast { expr, .. } => {
            let source = msduck_sql::variant_cast::source(expr).unwrap_or(expr);
            if literal(source) {
                ordinal(source)
            } else {
                Err(ordinal_constant())
            }
        }
        Expr::Function(_) => match msduck_sql::variant_cast::source(expr) {
            Some(source) => ordinal(source),
            None => Err(ordinal_constant()),
        },
        _ => Err(ordinal_constant()),
    }
}

fn ordinal_constant() -> anyhow::Error {
    SqlError::new(
        8748,
        1,
        "The enable_ordinal argument for string_split only supports constant values (not variables or columns).",
    )
    .into()
}

/// Whether a JSON_MODIFY value is JSON text that is embedded unescaped.
fn json_result(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => json_result(inner),
        Expr::Cast { expr, .. } => {
            matches!(expr.as_ref(), Expr::Function(f) if named(f, "__msduck_json_modify"))
        }
        Expr::Function(function) => [
            "JSON_QUERY",
            "JSON_MODIFY",
            "JSON_OBJECT",
            "JSON_ARRAY",
            "__msduck_json_query",
        ]
        .iter()
        .any(|name| named(function, name)),
        Expr::Subquery(query) => matches!(query.for_clause, Some(ForClause::Json { .. })),
        _ => false,
    }
}
