//! System procedure calls: names, argument binding, diagnostics and result
//! sets with SQL Server's descriptors.
use crate::engine::{Parameter, Session};
use crate::tds::{self, Column, Type};
use anyhow::{Result, anyhow};
use msduck_core::diagnostic::SqlError;
use msduck_core::result::{Origin, Properties};
use sqlparser::ast::{CharacterLength, DataType, Expr, Statement};
use std::collections::HashMap;

/// The system procedure a call names (`sp_pkeys`, `sys.sp_pkeys`,
/// `dbo.sp_pkeys`, `db..sp_pkeys`), lower-cased.
pub(super) fn procedure(statement: &Statement) -> Option<String> {
    let Statement::Execute {
        name: Some(name), ..
    } = statement
    else {
        return None;
    };
    let parts = name
        .0
        .iter()
        .map(|part| part.as_ident().map(|ident| ident.value.clone()))
        .collect::<Option<Vec<_>>>()?;
    let last = parts.last()?.to_ascii_lowercase();
    if !matches!(last.as_str(), "sp_pkeys" | "sp_fkeys" | "sp_rename") {
        return None;
    }
    match parts.len() {
        1 => {}
        2 | 3 => {
            let schema = &parts[parts.len() - 2];
            if !(schema.is_empty()
                || schema.eq_ignore_ascii_case("sys")
                || schema.eq_ignore_ascii_case("dbo"))
            {
                return None;
            }
        }
        _ => return None,
    }
    Some(last)
}

/// The database part of a call's name, if any.
pub(super) fn database_part(statement: &Statement) -> Option<String> {
    let Statement::Execute {
        name: Some(name), ..
    } = statement
    else {
        return None;
    };
    (name.0.len() == 3)
        .then(|| name.0[0].as_ident().map(|ident| ident.value.clone()))
        .flatten()
}

pub(super) fn sql_error(number: i32, state: u8, message: impl Into<String>) -> anyhow::Error {
    anyhow!(SqlError::new(number, state, message.into()))
}

/// A diagnostic raised in a procedure body: ERROR or INFO with the
/// procedure's name and the line SQL Server reports.
pub(super) struct Diagnostic {
    pub number: i32,
    pub state: u8,
    pub severity: u8,
    pub message: String,
    pub procedure: &'static str,
    pub line: i32,
}

impl Diagnostic {
    pub(super) fn write(&self, out: &mut Vec<u8>) {
        let error = self.severity > 10;
        let mut body = Vec::new();
        body.extend(self.number.to_le_bytes());
        body.extend([self.state, if error { self.severity } else { 0 }]);
        let units: Vec<u16> = self.message.encode_utf16().take(16000).collect();
        body.extend((units.len() as u16).to_le_bytes());
        body.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
        for text in ["msduck", self.procedure] {
            let units: Vec<u16> = text.encode_utf16().collect();
            body.push(units.len() as u8);
            body.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
        }
        body.extend(self.line.to_le_bytes());
        out.push(if error { 0xaa } else { 0xab });
        out.extend((body.len() as u16).to_le_bytes());
        out.extend(body);
    }

    pub(super) fn sql_error(&self) -> SqlError {
        let mut error = SqlError::new(self.number, self.state, self.message.clone());
        error.severity = self.severity;
        error
    }
}

/// A DONEINPROC token of a procedure body statement.
pub(super) fn done_in_proc(out: &mut Vec<u8>, status: u16, command: u16, count: u64) {
    tds::done(out, 0xff, status, command, count);
}

/// A bound argument: absent (the parameter's default), NULL or text.
pub(super) type Arguments = HashMap<&'static str, Option<String>>;

/// The text of an argument converted to `nvarchar`.
fn text(
    session: &Session,
    expr: &Expr,
    variables: &HashMap<String, Parameter>,
) -> Result<Option<String>> {
    use sqlparser::ast::Value as Literal;
    if let Expr::Value(value) = expr {
        match &value.value {
            Literal::Null => return Ok(None),
            Literal::SingleQuotedString(text) | Literal::NationalStringLiteral(text) => {
                return Ok(Some(text.clone()));
            }
            _ => {}
        }
    }
    let value = session.evaluate_scalar(
        expr.clone(),
        DataType::Nvarchar(Some(CharacterLength::IntegerLength {
            length: 4000,
            unit: None,
        })),
        variables,
    )?;
    Ok(match crate::backend_value::from_backend(value)? {
        msduck_core::value::Value::Null => None,
        msduck_core::value::Value::Text(text) => Some(text),
        msduck_core::value::Value::Unicode(units) => Some(String::from_utf16_lossy(&units)),
        other => Some(format!("{other:?}")),
    })
}

/// A decoded call: its arguments bound to `declared` parameters (name and
/// whether it has a default) with SQL Server's binding errors, its
/// `EXEC @status =` variable and whether a CATCH handler receives its errors.
pub(super) struct Bound {
    pub arguments: Arguments,
    pub status: Option<String>,
    pub in_try: bool,
}

pub(super) fn bind(
    session: &Session,
    procedure: &str,
    declared: &[(&'static str, bool)],
    statement: &Statement,
    variables: &HashMap<String, Parameter>,
) -> Result<Bound> {
    let call = msduck_sql::dialect::ext::procedures::call(statement)
        .ok_or_else(|| anyhow!("unsupported procedure call form"))?;
    let status = call.status.map(str::to_lowercase);
    if let Some(variable) = &status
        && !variables.contains_key(variable)
    {
        return Err(sql_error(
            137,
            2,
            format!("Must declare the scalar variable \"{variable}\"."),
        ));
    }
    let mut arguments = Arguments::new();
    let mut named = false;
    for (position, argument) in call.arguments.iter().enumerate() {
        let name = match argument.name {
            Some(written) => {
                named = true;
                let Some((name, _)) = declared
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(written))
                else {
                    return Err(sql_error(
                        8145,
                        1,
                        format!("{written} is not a parameter for procedure {procedure}."),
                    ));
                };
                if arguments.contains_key(name) {
                    return Err(sql_error(
                        8143,
                        1,
                        format!("Parameter '{name}' was supplied multiple times."),
                    ));
                }
                *name
            }
            None => {
                if named {
                    let mut error = SqlError::new(
                        119,
                        1,
                        format!(
                            "Must pass parameter number {} and subsequent parameters as '@name = value'. After the form '@name = value' has been used, all subsequent parameters must be passed in the form '@name = value'.",
                            position + 1
                        ),
                    );
                    error.severity = 15;
                    return Err(anyhow!(error));
                }
                let Some((name, _)) = declared.get(position) else {
                    return Err(sql_error(
                        8144,
                        2,
                        format!(
                            "Procedure or function {procedure} has too many arguments specified."
                        ),
                    ));
                };
                *name
            }
        };
        let Some(value) = argument.value else {
            continue;
        };
        arguments.insert(name, text(session, value, variables)?);
    }
    for (name, has_default) in declared {
        if !has_default && !arguments.contains_key(name) {
            return Err(sql_error(
                201,
                4,
                format!(
                    "Procedure or function '{procedure}' expects parameter '{name}', which was not supplied."
                ),
            ));
        }
    }
    Ok(Bound {
        arguments,
        status,
        in_try: call.in_try,
    })
}

/// Store a procedure's return status in the `EXEC @status =` variable,
/// converting it to the variable's declared type.
pub(super) fn assign(
    session: &Session,
    variable: &str,
    status: i32,
    variables: &mut HashMap<String, Parameter>,
) -> Result<()> {
    let kind = variables[variable].data_type;
    let value = session.evaluate_scalar(
        Expr::value(sqlparser::ast::Value::Number(status.to_string(), false)),
        msduck_sql::sql_type::ast(kind),
        variables,
    )?;
    variables.insert(
        variable.into(),
        Parameter {
            value: crate::backend_value::from_backend(value)?,
            data_type: kind,
        },
    );
    Ok(())
}

/// Finish a call whose body raised `diagnostics` (errors and messages, in
/// order) after producing `tokens`, returning `status`. SQL Server's system
/// procedures report errors with RAISERROR and continue to RETURN, so the
/// batch goes on; a CATCH handler around the call, or XACT_ABORT, receives
/// the first error instead.
pub(super) fn finish(
    session: &Session,
    bound: &Bound,
    mut tokens: Vec<u8>,
    diagnostics: Vec<Diagnostic>,
    status: i32,
    variables: &mut HashMap<String, Parameter>,
) -> Result<super::super::Exec> {
    if let Some(error) = diagnostics.iter().find(|d| d.severity > 10)
        && (bound.in_try || session.xact_abort)
    {
        return Err(super::super::Partial {
            tokens,
            error: anyhow!(error.sql_error()),
        }
        .into());
    }
    let failed = diagnostics.iter().any(|d| d.severity > 10);
    for diagnostic in &diagnostics {
        diagnostic.write(&mut tokens);
    }
    if failed {
        done_in_proc(&mut tokens, 0x03, 246, 0);
    }
    if let Some(variable) = &bound.status {
        assign(session, variable, status, variables)?;
    }
    Ok(super::super::Exec { tokens, status })
}

/// A result column: name, type, whether it is nullable and whether SQL
/// Server describes it as an expression (rather than a stored column).
pub(super) struct ResultColumn {
    pub name: &'static str,
    pub kind: Kind,
    pub nullable: bool,
    pub expression: bool,
}

#[derive(Clone, Copy)]
pub(super) enum Kind {
    /// nvarchar(128)
    Name,
    SmallInt,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Cell {
    Null,
    Text(String),
    Int(i64),
}

/// COLMETADATA and ROW tokens.
pub(super) fn result_set(
    out: &mut Vec<u8>,
    columns: &[ResultColumn],
    rows: &[Vec<Cell>],
) -> Result<()> {
    let described: Vec<Column> = columns
        .iter()
        .map(|column| Column {
            collation: None,
            properties: Properties {
                nullable: Some(column.nullable),
                origin: if column.expression {
                    Origin::Expression
                } else {
                    Origin::Stored
                },
            },
            name: column.name.into(),
            kind: match column.kind {
                Kind::Name => Type::Nvarchar(128),
                Kind::SmallInt => Type::Int(2),
            },
        })
        .collect();
    tds::metadata(out, &described)?;
    for row in rows {
        anyhow::ensure!(row.len() == columns.len(), "result row width");
        out.push(0xd1);
        for (column, cell) in described.iter().zip(row) {
            match (&column.kind, cell) {
                (Type::Nvarchar(_), Cell::Null) => tds::unicode_value(out, &column.kind, None)?,
                (Type::Nvarchar(_), Cell::Text(text)) => {
                    let units: Vec<u16> = text.encode_utf16().collect();
                    tds::unicode_value(out, &column.kind, Some(&units))?
                }
                (Type::Int(2), Cell::Int(value)) => {
                    if column.fixed_scalar_type().is_none() {
                        out.push(2);
                    }
                    out.extend(i16::try_from(*value)?.to_le_bytes());
                }
                (Type::Int(2), Cell::Null) => {
                    anyhow::ensure!(
                        column.fixed_scalar_type().is_none(),
                        "NULL in NOT NULL column"
                    );
                    out.push(0);
                }
                _ => anyhow::bail!("result cell does not fit its column"),
            }
        }
    }
    Ok(())
}
