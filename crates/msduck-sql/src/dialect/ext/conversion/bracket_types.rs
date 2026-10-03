//! Delimited system type names such as `[nvarchar](200)`, `"int"` or
//! `[sys].[int]`.
//!
//! sqlparser reads a delimited type name as `DataType::Custom`, which no
//! lowering recognizes, so `CONVERT([nvarchar](200), x)` reached DuckDB as a
//! call of an unknown `convert` function. SQL Server resolves such a name like
//! the plain spelling, ignoring case and trailing spaces, and accepts the
//! `sys` schema as a qualifier. Every delimited system type name in a parsed
//! statement is therefore replaced by the type the plain spelling parses to. Any other delimited name
//! (`[notatype]`, `[dbo].[int]`) keeps its `Custom` type and current error.
//! The alias type `sysname`, delimited or not, becomes its base type
//! `nvarchar(128)` as a CAST or CONVERT target, variable, parameter or return
//! type; a `sysname` column keeps its name for the catalog.
//! See docs/bracket-types.md.
//!
//! `msduck_sql::batch::parse` applies [`normalize`] to every statement it
//! parses. The statement parse hook cannot: it would have to call
//! `parse_statement` on its own parser, and the dialect would dispatch back
//! to the hook at the same token.
use sqlparser::{ast::*, parser::Parser, tokenizer::Token};
use std::ops::ControlFlow;

/// SQL Server system type names, lower case. A delimited name resolves to a
/// system type only through this list; aliases such as `integer` or
/// `character varying` are recognized only unquoted.
const SYSTEM_TYPES: &[&str] = &[
    "bigint",
    "binary",
    "bit",
    "char",
    "date",
    "datetime",
    "datetime2",
    "datetimeoffset",
    "decimal",
    "float",
    "geography",
    "geometry",
    "hierarchyid",
    "image",
    "int",
    "json",
    "money",
    "nchar",
    "ntext",
    "numeric",
    "nvarchar",
    "real",
    "rowversion",
    "smalldatetime",
    "smallint",
    "smallmoney",
    "sql_variant",
    "sysname",
    "text",
    "time",
    "timestamp",
    "tinyint",
    "uniqueidentifier",
    "varbinary",
    "varchar",
    "vector",
    "xml",
];

/// The lower-case system type a type name part names, ignoring trailing
/// spaces as SQL Server does.
fn system_type(name: &str) -> Option<&'static str> {
    let name = name.trim_end_matches(' ');
    SYSTEM_TYPES
        .iter()
        .copied()
        .find(|candidate| candidate.eq_ignore_ascii_case(name))
}

/// Replace every delimited system type name in `statement`, at any depth,
/// with the type its plain spelling parses to, and `sysname` outside column
/// definitions with `nvarchar(128)`.
pub fn normalize(statement: &mut Statement) {
    let _ = VisitMut::visit(statement, &mut Normalize);
}

struct Normalize;

impl VisitorMut for Normalize {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
        match expr {
            Expr::Cast { data_type, .. }
            | Expr::Convert {
                data_type: Some(data_type),
                ..
            } => resolve(data_type, Use::Value),
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<()> {
        if let TableFactor::OpenJsonTable { columns, .. } = factor {
            for column in columns {
                resolve(&mut column.r#type, Use::Value);
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_statement(&mut self, statement: &mut Statement) -> ControlFlow<()> {
        match statement {
            Statement::CreateTable(table) => {
                for column in &mut table.columns {
                    resolve(&mut column.data_type, Use::Column);
                }
            }
            Statement::AlterTable(alter) => {
                for operation in &mut alter.operations {
                    match operation {
                        AlterTableOperation::AddColumn { column_def, .. } => {
                            resolve(&mut column_def.data_type, Use::Column)
                        }
                        AlterTableOperation::AlterColumn {
                            op: AlterColumnOperation::SetDataType { data_type, .. },
                            ..
                        } => resolve(data_type, Use::Column),
                        _ => {}
                    }
                }
            }
            Statement::Declare { stmts } => {
                for declaration in stmts {
                    if let Some(data_type) = &mut declaration.data_type {
                        resolve(data_type, Use::Value);
                    }
                }
            }
            Statement::CreateProcedure { params, .. } => {
                for param in params.iter_mut().flatten() {
                    resolve(&mut param.data_type, Use::Value);
                }
            }
            Statement::CreateFunction(function) => {
                for arg in function.args.iter_mut().flatten() {
                    resolve(&mut arg.data_type, Use::Value);
                }
                if let Some(FunctionReturnType::DataType(data_type)) = &mut function.return_type {
                    resolve(data_type, Use::Value);
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

/// Where a data type appears.
#[derive(Clone, Copy, PartialEq)]
enum Use {
    /// A table column, whose catalog keeps the alias type `sysname`.
    Column,
    /// A CAST or CONVERT target, a variable, a parameter, a return type or an
    /// OPENJSON column.
    Value,
}

/// The type a CAST or CONVERT target, variable, parameter or return type
/// written as `kind` resolves to, when it is a delimited or `sys`-qualified
/// system type name or `sysname`; `None` for every other type.
pub fn value_type(kind: &DataType) -> Option<DataType> {
    let mut resolved = kind.clone();
    resolve(&mut resolved, Use::Value);
    (resolved != *kind).then_some(resolved)
}

/// The type a delimited or `sys`-qualified system type name written as
/// `kind` parses to without delimiters, keeping `sysname` as a name; `None`
/// for every other type. Catalog text renders such a type like the plain one.
pub fn plain_type(kind: &DataType) -> Option<DataType> {
    plain(&delimited(kind)?)
}

/// Replace a delimited or `sys`-qualified system type name with the type of
/// its plain spelling, and for a value `sysname` with `nvarchar(128)`, its
/// base type; leave every other type unchanged.
fn resolve(data_type: &mut DataType, usage: Use) {
    if let Some(spelling) = delimited(data_type)
        && let Some(resolved) = plain(&spelling)
    {
        *data_type = resolved;
    }
    if usage == Use::Value && is_sysname(data_type) {
        *data_type = DataType::Nvarchar(Some(CharacterLength::IntegerLength {
            length: 128,
            unit: None,
        }));
    }
}

/// The plain spelling of a delimited or `sys`-qualified system type name.
fn delimited(data_type: &DataType) -> Option<String> {
    let DataType::Custom(name, modifiers) = data_type else {
        return None;
    };
    let ident = match name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] if ident.quote_style.is_some() => ident,
        [
            ObjectNamePart::Identifier(schema),
            ObjectNamePart::Identifier(ident),
        ] if schema.value.eq_ignore_ascii_case("sys") => ident,
        _ => return None,
    };
    let system = system_type(&ident.value)?;
    Some(if modifiers.is_empty() {
        system.to_owned()
    } else {
        format!("{system}({})", modifiers.join(","))
    })
}

fn is_sysname(data_type: &DataType) -> bool {
    matches!(data_type, DataType::Custom(name, modifiers)
        if modifiers.is_empty()
            && matches!(name.0.as_slice(), [ObjectNamePart::Identifier(ident)]
                if ident.quote_style.is_none() && ident.value.eq_ignore_ascii_case("sysname")))
}

/// The data type `spelling` parses to, when it is exactly one data type.
fn plain(spelling: &str) -> Option<DataType> {
    let mut parser = Parser::new(&crate::dialect::ServerDialect)
        .try_with_sql(spelling)
        .ok()?;
    let data_type = parser.parse_data_type().ok()?;
    (parser.peek_token_ref().token == Token::EOF).then_some(data_type)
}
