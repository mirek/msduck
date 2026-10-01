//! SERVERPROPERTY, DATABASEPROPERTYEX and ROWCOUNT_BIG.
//!
//! Property values are known when the statement is compiled, so calls with
//! constant or variable arguments become typed constants before binding.
//! A call projected directly by a query's outermost SELECT (or read by
//! SQL_VARIANT_PROPERTY) keeps SQL Server's sql_variant shape; anywhere else
//! (CAST, comparisons, assignments) it becomes the value's base type, which
//! is what the implicit conversion from sql_variant would produce. Arguments
//! that depend on rows select among the possible values with a CASE.
use super::{arguments, call, name, syntax_error};
use crate::engine::{Parameter, Session};
use anyhow::{Result, bail};
use sqlparser::ast::{
    CastKind, CharacterLength, DataType, Expr, Function, Ident, ObjectName, Query, SelectItem,
    SetExpr, Statement, Value as Literal, VisitMut, VisitorMut,
};
use std::{collections::HashMap, ops::ControlFlow};

/// The collation msduck reports for the server and every database, as
/// `sys.databases` and the login collation do. msduck's default comparisons
/// are currently binary (case sensitive); see docs/gaps-conversion.md.
pub(super) const COLLATION: &str = "SQL_Latin1_General_CP1_CI_AS";

const SERVER_VARIANT: &str = "__MSDUCK_VARIANT_SERVERPROPERTY";
const DATABASE_VARIANT: &str = "__MSDUCK_VARIANT_DATABASEPROPERTYEX";
const PROPERTY_VARIANT: &str = "__MSDUCK_VARIANT_SQL_VARIANT_PROPERTY";

/// A property value with its SQL Server base type.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Property {
    Null,
    /// nvarchar(128).
    Text(String),
    Int(i32),
    TinyInt(u8),
}

impl Property {
    /// The value as its base type.
    fn base(&self) -> Expr {
        let cast = |expr, data_type| Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(expr),
            data_type,
            format: None,
        };
        let nvarchar = || {
            DataType::Nvarchar(Some(CharacterLength::IntegerLength {
                length: 128,
                unit: None,
            }))
        };
        match self {
            Self::Null => cast(Expr::value(Literal::Null), nvarchar()),
            Self::Text(text) => cast(
                Expr::value(Literal::NationalStringLiteral(text.clone())),
                nvarchar(),
            ),
            Self::Int(value) => cast(msduck_sql::expr::number(value), DataType::Int(None)),
            Self::TinyInt(value) => cast(msduck_sql::expr::number(value), DataType::TinyInt(None)),
        }
    }

    /// The value as a sql_variant: the integer and sysname shapes the wire
    /// encoder supports (`crate::variant`).
    fn variant(&self) -> Expr {
        let null = || Expr::value(Literal::Null);
        let (tag, integer, text) = match self {
            Self::Null => (null(), null(), null()),
            Self::Text(text) => (
                msduck_sql::expr::number(231),
                null(),
                Expr::value(Literal::SingleQuotedString(text.clone())),
            ),
            Self::Int(value) => (
                msduck_sql::expr::number(56),
                msduck_sql::expr::number(value),
                null(),
            ),
            Self::TinyInt(value) => (
                msduck_sql::expr::number(48),
                msduck_sql::expr::number(value),
                null(),
            ),
        };
        call("__msduck_conversion_variant", vec![tag, integer, text])
    }

    /// SQL_VARIANT_PROPERTY of this value.
    fn variant_property(&self, property: &str) -> Property {
        let (base, precision, max_length, total) = match self {
            Self::Null => return Self::Null,
            Self::Text(text) => (
                "nvarchar",
                0,
                256,
                8 + 2 * text.encode_utf16().count() as i32,
            ),
            Self::Int(_) => ("int", 10, 4, 6),
            Self::TinyInt(_) => ("tinyint", 3, 1, 3),
        };
        match property.to_ascii_lowercase().as_str() {
            "basetype" => Self::Text(base.into()),
            "precision" => Self::Int(precision),
            "scale" => Self::Int(0),
            "totalbytes" => Self::Int(total),
            "maxlength" => Self::Int(max_length),
            "collation" if matches!(self, Self::Text(_)) => Self::Text(COLLATION.into()),
            _ => Self::Null,
        }
    }
}

fn host_name() -> String {
    #[cfg(unix)]
    {
        let mut buffer = [0u8; 256];
        // The buffer outlives the call and its length bounds the write.
        let status =
            unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len() as libc::size_t) };
        if status == 0 {
            let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
            if let Ok(name) = std::str::from_utf8(&buffer[..end])
                && !name.is_empty()
            {
                return name.split('.').next().unwrap_or(name).to_owned();
            }
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "localhost".into())
}

/// SERVERPROPERTY(name). Unknown names give NULL, as in SQL Server.
pub(super) fn server(property: &str) -> Property {
    use Property::*;
    let text = |value: &str| Text(value.into());
    match property.to_ascii_lowercase().as_str() {
        "collation" => text(COLLATION),
        "collationid" => Int(872468488),
        "comparisonstyle" => Int(196609),
        "computernamephysicalnetbios" | "machinename" | "servername" => Text(host_name()),
        "edition" => text("Developer Edition (64-bit)"),
        "editionid" => Int(-2117995310),
        "engineedition" => Int(3),
        "filestreamconfiguredlevel" | "filestreameffectivelevel" => Int(0),
        "isadvancedanalyticsinstalled"
        | "isbigdatacluster"
        | "isclustered"
        | "isexternalgovernanceenabled"
        | "isfulltextinstalled"
        | "ishadrenabled"
        | "isintegratedsecurityonly"
        | "islocaldb"
        | "ispolybaseinstalled"
        | "isserversuspendedforsnapshotbackup"
        | "issingleuser"
        | "istempdbmetadatamemoryoptimized"
        | "isxtpsupported" => Int(0),
        "lcid" => Int(1033),
        "licensetype" => text("DISABLED"),
        "processid" => Int(std::process::id() as i32),
        "productbuild" => text("0"),
        "productlevel" => text("RTM"),
        "productmajorversion" => text("16"),
        "productminorversion" => text("0"),
        "productversion" => text("16.0.0.0"),
        "resourceversion" => text("16.00.0000"),
        "sqlcharset" => TinyInt(1),
        "sqlcharsetname" => text("iso_1"),
        "sqlsortorder" => TinyInt(52),
        "sqlsortordername" => text("nocase_iso"),
        _ => Null,
    }
}

/// One database's catalog row, as `sys.databases` reports it.
struct DatabaseRow {
    name: String,
    state: String,
    recovery: String,
    read_only: bool,
    user_access: String,
}

fn databases(session: &Session) -> Result<Vec<DatabaseRow>> {
    let primary = session.database().catalog().primary().replace('"', "\"\"");
    let mut query = session.db.prepare(&format!(
        "SELECT name, state_desc, recovery_model_desc, is_read_only, user_access_desc FROM \"{primary}\".sys.databases ORDER BY database_id"
    ))?;
    let rows = query
        .query_map([], |row| {
            Ok(DatabaseRow {
                name: row.get(0)?,
                state: row.get(1)?,
                recovery: row.get(2)?,
                read_only: row.get(3)?,
                user_access: row.get(4)?,
            })
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;
    Ok(rows)
}

/// DATABASEPROPERTYEX(database, name) for an existing database.
fn database(row: &DatabaseRow, property: &str) -> Property {
    use Property::*;
    let text = |value: &str| Text(value.into());
    match property.to_ascii_lowercase().as_str() {
        "collation" => text(COLLATION),
        "comparisonstyle" => Int(196609),
        "isautocreatestatistics" | "isautoupdatestatistics" => Int(1),
        "isautoclose"
        | "isautocreatestatisticsincremental"
        | "isautoshrink"
        | "isansinulldefault"
        | "isansinullsenabled"
        | "isansipaddingenabled"
        | "isansiwarningsenabled"
        | "isarithmeticabortenabled"
        | "isclosecursorsoncommitenabled"
        | "isfulltextenabled"
        | "isinstandby"
        | "islocalcursorsdefault"
        | "ismemoryoptimizedelevatetosnapshotenabled"
        | "ismergepublished"
        | "isnullconcat"
        | "isnumericroundabortenabled"
        | "isparameterizationforced"
        | "isquotedidentifiersenabled"
        | "ispublished"
        | "isrecursivetriggersenabled"
        | "issubscribed"
        | "issyncwithbackup"
        | "istornpagedetectionenabled"
        | "isverifiedclone"
        | "isxtpsupported" => Int(0),
        "lcid" => Int(1033),
        "recovery" => Text(row.recovery.clone()),
        "sqlsortorder" => TinyInt(52),
        "status" => Text(row.state.clone()),
        "updateability" => text(if row.read_only {
            "READ_ONLY"
        } else {
            "READ_WRITE"
        }),
        "useraccess" => Text(row.user_access.clone()),
        "version" => Int(957),
        _ => Null,
    }
}

/// The value of a constant argument: `Some(None)` for NULL, `None` when the
/// argument depends on rows or is an expression this rewrite does not fold.
fn constant(expr: &Expr, parameters: &HashMap<String, Parameter>) -> Option<Option<String>> {
    use msduck_core::value::Value as V;
    match expr {
        Expr::Nested(inner) => constant(inner, parameters),
        Expr::Value(value) => match &value.value {
            Literal::Null => Some(None),
            Literal::SingleQuotedString(text) | Literal::NationalStringLiteral(text) => {
                Some(Some(text.clone()))
            }
            Literal::Number(text, _) => Some(Some(text.clone())),
            _ => None,
        },
        Expr::Identifier(ident)
            if ident.value.starts_with('@') && !ident.value.starts_with("@@") =>
        {
            let parameter = parameters.get(&ident.value.to_lowercase())?;
            Some(match &parameter.value {
                V::Null => None,
                V::Text(text) => Some(text.clone()),
                V::Unicode(units) => Some(String::from_utf16_lossy(units)),
                V::TinyInt(v) => Some(v.to_string()),
                V::UTinyInt(v) => Some(v.to_string()),
                V::SmallInt(v) => Some(v.to_string()),
                V::Int(v) => Some(v.to_string()),
                V::BigInt(v) => Some(v.to_string()),
                _ => return None,
            })
        }
        _ => None,
    }
}

/// Mark property calls that a query's outermost SELECT projects directly, so
/// they keep the sql_variant type; reject row-count and database-state
/// properties in stored definitions, which would otherwise freeze them.
pub(super) fn prepare(statement: &mut Statement) -> Result<()> {
    if matches!(
        statement,
        Statement::CreateView(_)
            | Statement::AlterView { .. }
            | Statement::CreateTable(_)
            | Statement::AlterTable(_)
    ) {
        struct Find(Option<&'static str>);
        impl VisitorMut for Find {
            type Break = ();
            fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
                if let Expr::Function(function) = expr {
                    match name(function).as_deref() {
                        Some("ROWCOUNT_BIG") => self.0 = Some("ROWCOUNT_BIG"),
                        Some("DATABASEPROPERTYEX") => self.0 = Some("DATABASEPROPERTYEX"),
                        _ => {}
                    }
                }
                ControlFlow::Continue(())
            }
        }
        let mut find = Find(None);
        let _ = statement.visit(&mut find);
        if let Some(function) = find.0 {
            bail!("unsupported {function} in a stored definition");
        }
        return Ok(());
    }
    fn mark(query: &mut Query) {
        fn body(set: &mut SetExpr) {
            match set {
                SetExpr::Select(select) if select.into.is_none() => {
                    for item in &mut select.projection {
                        let (SelectItem::UnnamedExpr(expr)
                        | SelectItem::ExprWithAlias { expr, .. }) = item
                        else {
                            continue;
                        };
                        if let Expr::Function(function) = expr {
                            let marker = match name(function).as_deref() {
                                Some("SERVERPROPERTY") => SERVER_VARIANT,
                                Some("DATABASEPROPERTYEX") => DATABASE_VARIANT,
                                Some("SQL_VARIANT_PROPERTY")
                                    if arguments(function).is_some_and(|args| {
                                        matches!(args.first(), Some(Expr::Function(inner)) if kind(inner).is_some())
                                    }) =>
                                {
                                    PROPERTY_VARIANT
                                }
                                _ => continue,
                            };
                            function.name = ObjectName::from(vec![Ident::new(marker)]);
                        }
                    }
                }
                SetExpr::SetOperation { left, right, .. } => {
                    body(left);
                    body(right);
                }
                SetExpr::Query(query) => mark(query),
                _ => {}
            }
        }
        body(&mut query.body);
    }
    if let Statement::Query(query) = statement {
        mark(query);
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Server,
    Database,
}

fn kind(function: &Function) -> Option<(Kind, bool)> {
    Some(match name(function)?.as_str() {
        "SERVERPROPERTY" => (Kind::Server, false),
        SERVER_VARIANT => (Kind::Server, true),
        "DATABASEPROPERTYEX" => (Kind::Database, false),
        DATABASE_VARIANT => (Kind::Database, true),
        _ => return None,
    })
}

fn arity(function: &Function, kind: Kind, count: usize) -> Result<()> {
    let (name, expected) = match kind {
        Kind::Server => ("serverproperty", 1),
        Kind::Database => ("databasepropertyex", 2),
    };
    if count != expected {
        return Err(syntax_error(
            174,
            1,
            format!("The {name} function requires {expected} argument(s)."),
        ));
    }
    let _ = function;
    Ok(())
}

/// A property call whose arguments are all constants.
fn evaluate(session: &Session, kind: Kind, values: &[Option<String>]) -> Result<Property> {
    Ok(match kind {
        Kind::Server => match &values[0] {
            None => Property::Null,
            Some(property) => server(property),
        },
        Kind::Database => {
            let (Some(database_name), Some(property)) = (&values[0], &values[1]) else {
                return Ok(Property::Null);
            };
            let rows = databases(session)?;
            match rows.iter().find(|row| {
                row.name
                    .eq_ignore_ascii_case(database_name.trim_end_matches(' '))
            }) {
                Some(row) => database(row, property),
                None => Property::Null,
            }
        }
    })
}

/// Constant values chosen by row-dependent keys.
enum Choice {
    Value(Property),
    /// The key, then each case-insensitive key value with its choice.
    Case(Box<Expr>, Vec<(String, Choice)>),
}

impl Choice {
    fn values(&self, out: &mut Vec<Property>) {
        match self {
            Self::Value(value) => out.push(value.clone()),
            Self::Case(_, choices) => choices.iter().for_each(|(_, c)| c.values(out)),
        }
    }

    /// Render as sql_variant values, or as the values' common base type
    /// when they have one (NULL takes the type of the others).
    fn render(&self, variant: bool) -> Expr {
        let mut values = Vec::new();
        self.values(&mut values);
        let kinds: Vec<std::mem::Discriminant<Property>> = values
            .iter()
            .filter(|v| **v != Property::Null)
            .map(std::mem::discriminant)
            .collect();
        let null = if variant || kinds.windows(2).any(|w| w[0] != w[1]) {
            None
        } else {
            match values.iter().find(|v| **v != Property::Null) {
                Some(Property::Int(_)) => Some(Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(Expr::value(Literal::Null)),
                    data_type: DataType::Int(None),
                    format: None,
                }),
                Some(Property::TinyInt(_)) => Some(Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(Expr::value(Literal::Null)),
                    data_type: DataType::TinyInt(None),
                    format: None,
                }),
                _ => Some(Property::Null.base()),
            }
        };
        self.render_with(null.as_ref())
    }

    fn render_with(&self, base_null: Option<&Expr>) -> Expr {
        let leaf = |value: &Property| match (base_null, value) {
            (None, value) => value.variant(),
            (Some(null), Property::Null) => null.clone(),
            (Some(_), value) => value.base(),
        };
        match self {
            Self::Value(value) => leaf(value),
            Self::Case(key, choices) => Expr::Case {
                case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                operand: Some(Box::new(call(
                    "__msduck_collation_upper",
                    vec![(**key).clone()],
                ))),
                conditions: choices
                    .iter()
                    .map(|(when, then)| sqlparser::ast::CaseWhen {
                        condition: Expr::value(Literal::SingleQuotedString(when.to_uppercase())),
                        result: then.render_with(base_null),
                    })
                    .collect(),
                else_result: Some(Box::new(leaf(&Property::Null))),
            },
        }
    }
}

const SERVER_NAMES: &[&str] = &[
    "Collation",
    "CollationID",
    "ComparisonStyle",
    "ComputerNamePhysicalNetBIOS",
    "Edition",
    "EditionID",
    "EngineEdition",
    "FilestreamConfiguredLevel",
    "FilestreamEffectiveLevel",
    "IsAdvancedAnalyticsInstalled",
    "IsBigDataCluster",
    "IsClustered",
    "IsExternalGovernanceEnabled",
    "IsFullTextInstalled",
    "IsHadrEnabled",
    "IsIntegratedSecurityOnly",
    "IsLocalDB",
    "IsPolyBaseInstalled",
    "IsServerSuspendedForSnapshotBackup",
    "IsSingleUser",
    "IsTempDbMetadataMemoryOptimized",
    "IsXTPSupported",
    "LCID",
    "LicenseType",
    "MachineName",
    "ProcessID",
    "ProductBuild",
    "ProductLevel",
    "ProductMajorVersion",
    "ProductMinorVersion",
    "ProductVersion",
    "ResourceVersion",
    "ServerName",
    "SqlCharSet",
    "SqlCharSetName",
    "SqlSortOrder",
    "SqlSortOrderName",
];

const DATABASE_NAMES: &[&str] = &[
    "Collation",
    "ComparisonStyle",
    "IsAutoClose",
    "IsAutoCreateStatistics",
    "IsAutoCreateStatisticsIncremental",
    "IsAutoShrink",
    "IsAutoUpdateStatistics",
    "IsAnsiNullDefault",
    "IsAnsiNullsEnabled",
    "IsAnsiPaddingEnabled",
    "IsAnsiWarningsEnabled",
    "IsArithmeticAbortEnabled",
    "IsCloseCursorsOnCommitEnabled",
    "IsFulltextEnabled",
    "IsInStandBy",
    "IsLocalCursorsDefault",
    "IsMemoryOptimizedElevateToSnapshotEnabled",
    "IsMergePublished",
    "IsNullConcat",
    "IsNumericRoundAbortEnabled",
    "IsParameterizationForced",
    "IsQuotedIdentifiersEnabled",
    "IsPublished",
    "IsRecursiveTriggersEnabled",
    "IsSubscribed",
    "IsSyncWithBackup",
    "IsTornPageDetectionEnabled",
    "IsVerifiedClone",
    "IsXTPSupported",
    "LCID",
    "Recovery",
    "SQLSortOrder",
    "Status",
    "Updateability",
    "UserAccess",
    "Version",
];

/// A property call with a row-dependent argument.
fn dynamic(
    session: &Session,
    kind: Kind,
    args: &[&Expr],
    values: &[Option<Option<String>>],
    map: &dyn Fn(Property) -> Property,
) -> Result<Choice> {
    Ok(match kind {
        Kind::Server => Choice::Case(
            Box::new(args[0].clone()),
            SERVER_NAMES
                .iter()
                .map(|name| (name.to_string(), Choice::Value(map(server(name)))))
                .collect(),
        ),
        Kind::Database => {
            let rows = databases(session)?;
            let for_database = |row: &DatabaseRow| -> Choice {
                match &values[1] {
                    Some(None) => Choice::Value(Property::Null),
                    Some(Some(property)) => Choice::Value(map(database(row, property))),
                    None => Choice::Case(
                        Box::new(args[1].clone()),
                        DATABASE_NAMES
                            .iter()
                            .map(|name| (name.to_string(), Choice::Value(map(database(row, name)))))
                            .collect(),
                    ),
                }
            };
            match &values[0] {
                Some(None) => Choice::Value(Property::Null),
                Some(Some(database_name)) => match rows.iter().find(|row| {
                    row.name
                        .eq_ignore_ascii_case(database_name.trim_end_matches(' '))
                }) {
                    Some(row) => for_database(row),
                    None => Choice::Value(Property::Null),
                },
                None => Choice::Case(
                    Box::new(args[0].clone()),
                    rows.iter()
                        .map(|row| (row.name.clone(), for_database(row)))
                        .collect(),
                ),
            }
        }
    })
}

pub(super) fn rewrite(
    session: &Session,
    expr: &mut Expr,
    parameters: &HashMap<String, Parameter>,
) -> Result<()> {
    let Expr::Function(function) = expr else {
        return Ok(());
    };
    let Some(function_name) = name(function) else {
        return Ok(());
    };
    if function_name == "ROWCOUNT_BIG" {
        let Some(args) = arguments(function) else {
            bail!("unsupported ROWCOUNT_BIG modifiers");
        };
        if !args.is_empty() {
            return Err(syntax_error(
                174,
                1,
                "The rowcount_big function requires 0 argument(s).",
            ));
        }
        *expr = Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(msduck_sql::expr::number(session.rowcount)),
            data_type: DataType::BigInt(None),
            format: None,
        };
        return Ok(());
    }
    if (function_name == "SQL_VARIANT_PROPERTY" || function_name == PROPERTY_VARIANT)
        && let Some(args) = arguments(function)
        && let [value, property] = args.as_slice()
        && let Expr::Function(inner) = value
        && let Some((kind, _)) = kind(inner)
    {
        let Some(inner_args) = arguments(inner) else {
            return Ok(());
        };
        arity(inner, kind, inner_args.len())?;
        let constants = inner_args
            .iter()
            .map(|arg| constant(arg, parameters))
            .collect::<Option<Vec<_>>>();
        let variant = function_name == PROPERTY_VARIANT;
        if let (Some(constants), Some(property)) = (constants, constant(property, parameters)) {
            let value = evaluate(session, kind, &constants)?;
            let value = match property {
                None => Property::Null,
                Some(property) => value.variant_property(&property),
            };
            *expr = Choice::Value(value).render(variant);
            return Ok(());
        }
        // A constant property name: SQL_VARIANT_PROPERTY of each candidate.
        if let Some(Some(property)) = constant(property, parameters) {
            let values: Vec<Option<Option<String>>> = inner_args
                .iter()
                .map(|arg| constant(arg, parameters))
                .collect();
            *expr = dynamic(session, kind, &inner_args, &values, &|value| {
                value.variant_property(&property)
            })?
            .render(variant);
            return Ok(());
        }
        // Not foldable: keep the argument a sql_variant for the built-in.
        let marker = match kind {
            Kind::Server => SERVER_VARIANT,
            Kind::Database => DATABASE_VARIANT,
        };
        if let Expr::Function(function) = expr
            && let sqlparser::ast::FunctionArguments::List(list) = &mut function.args
            && let Some(sqlparser::ast::FunctionArg::Unnamed(
                sqlparser::ast::FunctionArgExpr::Expr(Expr::Function(inner)),
            )) = list.args.first_mut()
        {
            inner.name = ObjectName::from(vec![Ident::new(marker)]);
        }
        return Ok(());
    }
    let Some((kind, variant)) = kind(function) else {
        return Ok(());
    };
    let Some(args) = arguments(function) else {
        bail!("unsupported {function_name} modifiers");
    };
    arity(function, kind, args.len())?;
    let values: Vec<Option<Option<String>>> =
        args.iter().map(|arg| constant(arg, parameters)).collect();
    if values.iter().all(Option::is_some) {
        let constants: Vec<Option<String>> = values.into_iter().map(Option::unwrap).collect();
        let value = evaluate(session, kind, &constants)?;
        *expr = Choice::Value(value).render(variant);
        return Ok(());
    }
    *expr = dynamic(session, kind, &args, &values, &|value| value)?.render(variant);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_properties_have_sql_server_base_types() {
        assert_eq!(server("Collation"), Property::Text(COLLATION.into()));
        assert_eq!(server("COLLATION"), Property::Text(COLLATION.into()));
        assert_eq!(server("EngineEdition"), Property::Int(3));
        assert_eq!(server("SqlCharSet"), Property::TinyInt(1));
        assert_eq!(server("InstanceName"), Property::Null);
        assert_eq!(server("IsCaseSensitive"), Property::Null);
        assert_eq!(server("ProductVersion"), Property::Text("16.0.0.0".into()));
        assert_eq!(server("ServerName"), server("MachineName"));
        for name in SERVER_NAMES {
            assert_ne!(server(name), Property::Null, "{name}");
        }
    }

    #[test]
    fn variant_properties_describe_base_types() {
        let text = Property::Text("RTM".into());
        assert_eq!(
            text.variant_property("BaseType"),
            Property::Text("nvarchar".into())
        );
        assert_eq!(text.variant_property("MaxLength"), Property::Int(256));
        assert_eq!(
            Property::Int(3).variant_property("basetype"),
            Property::Text("int".into())
        );
        assert_eq!(
            Property::Int(3).variant_property("Precision"),
            Property::Int(10)
        );
        assert_eq!(
            Property::TinyInt(1).variant_property("MaxLength"),
            Property::Int(1)
        );
        assert_eq!(Property::Null.variant_property("BaseType"), Property::Null);
        assert_eq!(Property::Int(3).variant_property("Bogus"), Property::Null);
    }
}
