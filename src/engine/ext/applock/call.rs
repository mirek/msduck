//! sp_getapplock and sp_releaseapplock: argument binding, validation and the
//! completion tokens SQL Server's procedure bodies produce.
//!
//! Both procedures are T-SQL wrappers around `sys.xp_userlock`
//! (`OBJECT_DEFINITION(OBJECT_ID('sys.sp_getapplock'))`). Each statement of
//! the body ends with a DONEINPROC token, so the stream below follows the
//! body's control flow exactly; see reference/gaps-applock.json.
use super::table::{self, Key, Mode, Owner};
use super::{Value, principal, resource_units};
use crate::engine::{Parameter, Session, ext};
use anyhow::{Result, anyhow};
use msduck_core::diagnostic::SqlError;
use sqlparser::ast::{BinaryOperator, Expr, Statement};
use std::{collections::HashMap, time::Duration};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Procedure {
    Get,
    Release,
}

impl Procedure {
    fn name(self) -> &'static str {
        match self {
            Self::Get => "sp_getapplock",
            Self::Release => "sp_releaseapplock",
        }
    }

    /// Declared parameters in order: name, and whether it has a default.
    fn parameters(self) -> &'static [(&'static str, bool)] {
        match self {
            Self::Get => &[
                ("@Resource", true),
                ("@LockMode", false),
                ("@LockOwner", true),
                ("@LockTimeout", true),
                ("@DbPrincipal", true),
            ],
            Self::Release => &[
                ("@Resource", true),
                ("@LockOwner", true),
                ("@DbPrincipal", true),
            ],
        }
    }
}

/// A diagnostic as raised inside the procedure body, with its procedure name
/// and line number.
pub(super) struct Diagnostic {
    pub error: bool,
    pub number: i32,
    pub state: u8,
    pub message: String,
    pub procedure: &'static str,
    pub line: i32,
}

impl Diagnostic {
    /// An error raised by `sys.xp_userlock`.
    fn xp(number: i32, state: u8, message: String) -> Self {
        Self {
            error: true,
            number,
            state,
            message,
            procedure: "sys.xp_userlock",
            line: 1,
        }
    }

    pub(super) fn sql_error(&self) -> SqlError {
        let mut error = SqlError::new(self.number, self.state, self.message.clone());
        error.severity = if self.error { 16 } else { 10 };
        error
    }

    /// ERROR or INFO token. Severity 10 messages travel as class 0.
    pub(super) fn write(&self, out: &mut Vec<u8>) {
        let mut body = Vec::new();
        body.extend(self.number.to_le_bytes());
        body.extend([self.state, if self.error { 16 } else { 0 }]);
        let units: Vec<u16> = self.message.encode_utf16().take(16000).collect();
        body.extend((units.len() as u16).to_le_bytes());
        body.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
        for text in ["msduck", self.procedure] {
            let units: Vec<u16> = text.encode_utf16().collect();
            body.push(units.len() as u8);
            body.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
        }
        body.extend(self.line.to_le_bytes());
        out.push(if self.error { 0xaa } else { 0xab });
        out.extend((body.len() as u16).to_le_bytes());
        out.extend(body);
    }
}

/// The DONEINPROC tokens of the procedure body. With NOCOUNT ON only the
/// failed `EXEC xp_userlock` statement still reports its completion.
struct Body {
    tokens: Vec<u8>,
    nocount: bool,
}

impl Body {
    fn done(&mut self, status: u16, command: u16, count: u64) {
        if !self.nocount || status & 2 != 0 {
            crate::tds::done(&mut self.tokens, 0xff, status, command, count);
        }
    }
    /// SELECT/SET assignment of a local variable, or RETURN with a value.
    fn select(&mut self) {
        self.done(0x11, 193, 1);
    }
    fn condition(&mut self) {
        self.done(0x01, 192, 0);
    }
    fn raiserror(&mut self, diagnostic: Diagnostic) {
        diagnostic.write(&mut self.tokens);
        self.done(0x01, 246, 0);
    }
    fn exec(&mut self, failure: Option<&Diagnostic>) {
        match failure {
            Some(diagnostic) => {
                diagnostic.write(&mut self.tokens);
                self.done(0x03, 224, 0);
            }
            None => self.done(0x01, 224, 0),
        }
    }
}

/// A bound argument: absent (the parameter default) or a value.
type Arguments = HashMap<&'static str, Value>;

fn sql_error(number: i32, state: u8, message: String) -> anyhow::Error {
    anyhow!(SqlError::new(number, state, message))
}

/// Bind positional and named arguments to the procedure's parameters.
fn bind(
    session: &Session,
    procedure: Procedure,
    arguments: &[Expr],
    variables: &HashMap<String, Parameter>,
) -> Result<Arguments> {
    let declared = procedure.parameters();
    let mut bound = Arguments::new();
    let mut named = false;
    for (position, argument) in arguments.iter().enumerate() {
        let (name, value) = match argument {
            Expr::BinaryOp {
                left,
                op: BinaryOperator::Eq,
                right,
            } if matches!(left.as_ref(), Expr::Identifier(id) if id.value.starts_with('@')) => {
                let Expr::Identifier(id) = left.as_ref() else {
                    unreachable!()
                };
                named = true;
                let Some((name, _)) = declared
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(&id.value))
                else {
                    return Err(sql_error(
                        8145,
                        1,
                        format!(
                            "{} is not a parameter for procedure {}.",
                            id.value,
                            procedure.name()
                        ),
                    ));
                };
                if bound.contains_key(name) {
                    return Err(sql_error(
                        8143,
                        1,
                        format!("Parameter '{name}' was supplied multiple times."),
                    ));
                }
                (*name, right.as_ref())
            }
            _ => {
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
                            "Procedure or function {} has too many arguments specified.",
                            procedure.name()
                        ),
                    ));
                };
                (*name, argument)
            }
        };
        if is_default(value) {
            continue;
        }
        let mut value = super::evaluate(session, value, variables)?;
        if name == "@LockTimeout" {
            value = timeout(value)?;
        }
        bound.insert(name, value);
    }
    for (name, has_default) in declared {
        if !has_default && !bound.contains_key(name) {
            return Err(sql_error(
                201,
                4,
                format!(
                    "Procedure or function '{}' expects parameter '{name}', which was not supplied.",
                    procedure.name()
                ),
            ));
        }
    }
    Ok(bound)
}

/// `@LockTimeout int`: the implicit conversion of the argument.
fn timeout(value: Value) -> Result<Value> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::Int(number) => i32::try_from(number)
            .map(|n| Value::Int(n.into()))
            .map_err(|_| {
                sql_error(
                    8115,
                    2,
                    "Arithmetic overflow error converting expression to data type int.".into(),
                )
            }),
        Value::Text { ref units, unicode } => String::from_utf16_lossy(units)
            .trim()
            .parse::<i32>()
            .map(|n| Value::Int(n.into()))
            .map_err(|_| {
                sql_error(
                    8114,
                    1,
                    format!(
                        "Error converting data type {} to int.",
                        if unicode { "nvarchar" } else { "varchar" }
                    ),
                )
            }),
    }
}

fn is_default(expr: &Expr) -> bool {
    matches!(expr, Expr::Identifier(id) if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("DEFAULT"))
}

/// A `varchar(32)` option: its text, truncated, or None for NULL.
fn option(value: Option<&Value>, default: &str) -> Option<String> {
    match value {
        None => Some(default.into()),
        Some(Value::Null) => None,
        Some(value) => Some(value.text().chars().take(32).collect()),
    }
}

fn not_recognized(
    procedure: Procedure,
    value: Option<String>,
    parameter: &str,
    line: i32,
) -> Diagnostic {
    Diagnostic {
        error: false,
        number: 15625,
        state: 1,
        message: format!(
            "Option '{}' not recognized for '{parameter}' parameter.",
            value.as_deref().unwrap_or("(null)")
        ),
        procedure: procedure.name(),
        line,
    }
}

/// Is `owner` the transaction owner? `None` for an unrecognized owner.
fn owner_kind(text: Option<&str>) -> Option<bool> {
    let text = text?.trim_end_matches(' ');
    if text.eq_ignore_ascii_case("Transaction") {
        Some(true)
    } else if text.eq_ignore_ascii_case("Session") {
        Some(false)
    } else {
        None
    }
}

/// Run an application lock procedure call. Returns `None` when the
/// statement is some other procedure call.
pub(super) fn exec(
    session: &mut Session,
    statement: &Statement,
    variables: &mut HashMap<String, Parameter>,
) -> Option<Result<ext::Exec>> {
    let Statement::Execute {
        name: Some(name),
        parameters,
        ..
    } = statement
    else {
        return None;
    };
    let procedure = match msduck_sql::dialect::ext::applock::procedure(&name.to_string())? {
        "sp_getapplock" => Procedure::Get,
        _ => Procedure::Release,
    };
    let status_variable = msduck_sql::dialect::ext::applock::status_variable(statement)
        .map(|ident| ident.value.to_lowercase());
    Some((|| {
        if let Some(variable) = &status_variable
            && !variables.contains_key(variable)
        {
            return Err(anyhow!(SqlError::new(
                137,
                2,
                format!("Must declare the scalar variable \"{variable}\".")
            )));
        }
        let arguments = bind(session, procedure, parameters, variables)?;
        let (tokens, status, failure) = run(session, procedure, &arguments);
        // RETURN is the body's last statement.
        session.rowcount = 1;
        if let Some(variable) = status_variable {
            super::assign(session, &variable, status, variables)?;
        }
        match failure {
            // Under XACT_ABORT an error inside the procedure ends the batch
            // and dooms the transaction, so it travels as the call's error.
            Some(diagnostic) if session.xact_abort => Err(ext::Partial {
                tokens,
                error: anyhow!(diagnostic.sql_error()),
            }
            .into()),
            _ => Ok(ext::Exec { tokens, status }),
        }
    })())
}

/// The procedure body. Returns its tokens, the return status and the
/// xp_userlock error, if one was raised.
fn run(
    session: &mut Session,
    procedure: Procedure,
    arguments: &Arguments,
) -> (Vec<u8>, i32, Option<Diagnostic>) {
    let mut body = Body {
        tokens: Vec::new(),
        nocount: session.nocount,
    };
    let (status, failure) = match procedure {
        Procedure::Get => get(session, arguments, &mut body),
        Procedure::Release => release(session, arguments, &mut body),
    };
    if let Some(failure) = &failure {
        if session.xact_abort {
            // The error ends the batch; the engine reports it.
            return (body.tokens, status, Some(failure_owned(failure)));
        }
        body.exec(Some(failure));
    }
    // RETURN (@result) or RETURN (-999).
    body.select();
    (body.tokens, status, failure)
}

fn failure_owned(failure: &Diagnostic) -> Diagnostic {
    Diagnostic {
        message: failure.message.clone(),
        ..*failure
    }
}

/// The lock key and owner of a call, validated as xp_userlock does.
fn target(
    session: &Session,
    arguments: &Arguments,
    transaction: bool,
) -> Result<(Key, Owner, String, Vec<u16>), Diagnostic> {
    let resource = match arguments.get("@Resource") {
        None | Some(Value::Null) => {
            return Err(Diagnostic::xp(
                1224,
                5,
                "An invalid application lock resource was passed to xp_userlock.".into(),
            ));
        }
        Some(value) => resource_units(value),
    };
    let principal = match arguments.get("@DbPrincipal") {
        None => "public".to_string(),
        Some(Value::Null) => {
            return Err(Diagnostic::xp(
                1230,
                1,
                "An invalid database principal was passed to xp_userlock.".into(),
            ));
        }
        Some(value) => value.text().chars().take(128).collect(),
    };
    let canonical = principal::canonical(&principal).ok_or_else(|| {
        Diagnostic::xp(
            1202,
            1,
            format!("The database-principal '{principal}' does not exist or user is not a member."),
        )
    })?;
    let key = Key::new(&session.database().name, canonical, &resource);
    let owner = Owner {
        session: session.ext.token,
        transaction,
    };
    Ok((key, owner, principal, resource))
}

fn get(session: &mut Session, arguments: &Arguments, body: &mut Body) -> (i32, Option<Diagnostic>) {
    // select @mode = CASE @LockMode ...; if @mode = -1
    body.select();
    body.condition();
    let mode_text = option(arguments.get("@LockMode"), "");
    let Some(mode) = mode_text.as_deref().and_then(Mode::requested) else {
        body.raiserror(not_recognized(Procedure::Get, mode_text, "@LockMode", 26));
        return (-999, None);
    };
    // select @owner = CASE @LockOwner ...; if @owner = -1
    body.select();
    body.condition();
    let owner_text = option(arguments.get("@LockOwner"), "Transaction");
    let Some(transaction) = owner_kind(owner_text.as_deref()) else {
        body.raiserror(not_recognized(Procedure::Get, owner_text, "@LockOwner", 39));
        return (-999, None);
    };
    // if @LockTimeout is null set @LockTimeout = @@LOCK_TIMEOUT
    body.condition();
    let timeout = match arguments.get("@LockTimeout") {
        None | Some(Value::Null) => {
            body.select();
            // msduck has no SET LOCK_TIMEOUT, so @@LOCK_TIMEOUT is -1.
            -1
        }
        Some(Value::Int(timeout)) => *timeout,
        Some(_) => unreachable!("@LockTimeout is bound as int"),
    };
    // select @dbid = db_id (); if @owner = 1 and @@trancount = 0
    body.select();
    body.condition();
    if transaction && session.transactions == 0 {
        body.raiserror(Diagnostic {
            error: false,
            number: 15626,
            state: 1,
            message: "You attempted to acquire a transactional application lock without an active transaction.".into(),
            procedure: "sp_getapplock",
            line: 52,
        });
        return (-999, None);
    }
    // exec @result = sys.xp_userlock 0, @dbid, @DbPrincipal, @Resource, @mode, @owner, @LockTimeout
    if timeout < -1 {
        return (
            -999,
            Some(Diagnostic::xp(
                1227,
                2,
                "An invalid application lock time-out was passed to xp_userlock.".into(),
            )),
        );
    }
    let (key, owner, _, _) = match target(session, arguments, transaction) {
        Ok(target) => target,
        Err(diagnostic) => return (-999, Some(diagnostic)),
    };
    let cancel = session.read_cancel.clone();
    let cancelled = move || {
        cancel
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
    };
    let timeout = u64::try_from(timeout).ok().map(Duration::from_millis);
    let status = table::acquire(&key, owner, mode, timeout, &cancelled) as i32;
    body.exec(None);
    (status, None)
}

fn release(
    session: &mut Session,
    arguments: &Arguments,
    body: &mut Body,
) -> (i32, Option<Diagnostic>) {
    // select @owner = CASE @LockOwner ...; if @owner = -1
    body.select();
    body.condition();
    let owner_text = option(arguments.get("@LockOwner"), "Transaction");
    let Some(transaction) = owner_kind(owner_text.as_deref()) else {
        body.raiserror(not_recognized(
            Procedure::Release,
            owner_text,
            "@LockOwner",
            20,
        ));
        return (-999, None);
    };
    // select @dbid = db_id ()
    body.select();
    // exec @result = sys.xp_userlock 1, @dbid, @DbPrincipal, @Resource, 0, @owner
    let (key, owner, principal, resource) = match target(session, arguments, transaction) {
        Ok(target) => target,
        Err(diagnostic) => return (-999, Some(diagnostic)),
    };
    if transaction && session.transactions == 0 {
        return (
            -999,
            Some(Diagnostic::xp(
                3918,
                1,
                "The statement or function must be executed in the context of a user transaction."
                    .into(),
            )),
        );
    }
    if !table::release(&key, owner) {
        return (
            -999,
            Some(Diagnostic::xp(
                1223,
                1,
                format!(
                    "Cannot release the application lock (Database Principal: '{principal}', Resource: '{}') because it is not currently held.",
                    String::from_utf16_lossy(&resource)
                ),
            )),
        );
    }
    body.exec(None);
    (0, None)
}
