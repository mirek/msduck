//! Lower CHECK and DEFAULT expressions exactly as the engine lowers them in
//! CREATE and ALTER TABLE, so stored definitions evaluate natively.
use super::catalog::{Column, Table};
use crate::engine::{Parameter, Session, Translator, ext};
use anyhow::{Result, bail, ensure};
use sqlparser::ast::*;
use std::{collections::HashMap, ops::ControlFlow};

fn alter(table: &Table, operation: AlterTableOperation) -> Statement {
    Statement::AlterTable(AlterTable {
        name: table.object_name(),
        if_exists: false,
        only: false,
        operations: vec![operation],
        location: None,
        on_cluster: None,
        table_type: None,
        end_token: helpers::attached_token::AttachedToken::empty(),
    })
}

fn lower(session: &Session, statement: &mut Statement) -> Result<()> {
    let mut parameters = HashMap::<String, Parameter>::new();
    session.lower_database_functions(statement)?;
    session.lower_session_functions(statement, &mut parameters)?;
    ext::rewrite(session, statement, &parameters)?;
    let mut translator = Translator {
        parameters: &parameters,
        values: vec![],
        parameter_slots: HashMap::new(),
        transactions: session.transactions,
        transaction_doomed: session.transaction_doomed,
        original_login: &session.original_login,
        clock: crate::current_time::now(),
        rowcount: session.rowcount,
        last_error: session.last_error,
        caught_error: session.caught_error.as_ref(),
        spid: session.process.spid(),
    };
    if let ControlFlow::Break(error) = VisitMut::visit(statement, &mut translator) {
        bail!(error);
    }
    ensure!(
        translator.values.is_empty(),
        "unsupported bound values in a constraint definition"
    );
    Ok(())
}

/// The native predicate of a CHECK constraint on `table`.
pub(crate) fn check(session: &Session, table: &Table, expr: &Expr) -> Result<String> {
    let mut statement = alter(
        table,
        AlterTableOperation::AddConstraint {
            constraint: TableConstraint::Check(CheckConstraint {
                name: None,
                expr: Box::new(expr.clone()),
                no_inherit: false,
                enforced: None,
            }),
            not_valid: false,
        },
    );
    lower(session, &mut statement)?;
    let Statement::AlterTable(AlterTable { operations, .. }) = statement else {
        unreachable!()
    };
    match operations.into_iter().next() {
        Some(AlterTableOperation::AddConstraint {
            constraint: TableConstraint::Check(check),
            ..
        }) => Ok(check.expr.to_string()),
        _ => bail!("CHECK lowering changed its statement shape"),
    }
}

/// The T-SQL declaration of a column, from its catalog description.
pub(crate) fn declared_type(session: &Session, table: &Table, column: &Column) -> Result<DataType> {
    let name: String = session.db.query_row(
        "SELECT t.name FROM sys.types t WHERE t.user_type_id=?",
        [column.user_type],
        |row| row.get(0),
    )?;
    let lower = name.to_ascii_lowercase();
    let length = |units: i32| {
        if column.max_length == -1 {
            "max".to_string()
        } else {
            (column.max_length / units).to_string()
        }
    };
    let text = match lower.as_str() {
        "char" | "varchar" | "binary" | "varbinary" => format!("{name}({})", length(1)),
        "nchar" | "nvarchar" => format!("{name}({})", length(2)),
        "decimal" | "numeric" => format!("{name}({},{})", column.precision, column.scale),
        "datetime2" | "time" | "datetimeoffset" => format!("{name}({})", column.scale),
        _ => name.clone(),
    };
    let mut parser =
        sqlparser::parser::Parser::new(&crate::dialect::ServerDialect).try_with_sql(&text)?;
    let kind = parser.parse_data_type()?;
    let _ = table;
    Ok(kind)
}

/// The native default expression of `column` for `value`.
pub(crate) fn default(
    session: &Session,
    table: &Table,
    column: &Column,
    value: &Expr,
) -> Result<String> {
    let data_type = declared_type(session, table, column)?;
    let mut statement = alter(
        table,
        AlterTableOperation::AddColumn {
            column_keyword: false,
            if_not_exists: false,
            column_def: ColumnDef {
                name: Ident::new(&column.name),
                data_type,
                options: vec![ColumnOptionDef {
                    name: None,
                    option: ColumnOption::Default(value.clone()),
                }],
            },
            column_position: None,
        },
    );
    lower(session, &mut statement)?;
    let Statement::AlterTable(AlterTable { operations, .. }) = statement else {
        unreachable!()
    };
    if let Some(AlterTableOperation::AddColumn { column_def, .. }) = operations.into_iter().next() {
        for option in column_def.options {
            if let ColumnOption::Default(expr) = option.option {
                return Ok(expr.to_string());
            }
        }
    }
    bail!("DEFAULT lowering changed its statement shape")
}
