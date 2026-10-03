//! Session-owned DuckDB execution with AST-based T-SQL translation.
pub(crate) mod ext;
mod joined_output;
pub(crate) mod rand;
mod session_context;
use crate::tds::{self, Column, Type};
use anyhow::{Result, bail, ensure};
use duckdb::{
    Connection,
    arrow::datatypes::DataType as ArrowType,
    types::{TimeUnit, Value},
};
pub use joined_output::{BoundOutputQuery, BoundOutputUpdate, PreparedJoinedOutput};
use msduck_core::diagnostic::SqlError;
pub use msduck_sql::batch::parameter_declarations;
use msduck_sql::batch::variable_type;
use msduck_sql::expr::number;
pub(crate) use msduck_sql::expr::{binary_function, unary_function};
use msduck_sql::preflight::{
    try_catch_parts, validate_transaction_syntax, variables as batch_variables,
};
pub(crate) use msduck_sql::sql_type::integral_type;
use sqlparser::{ast::*, parser::Parser};
use std::{
    collections::HashMap,
    ops::ControlFlow,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT_TRANSACTION: AtomicU64 = AtomicU64::new(1);

pub use crate::parameter::Parameter;
use msduck_core::{types::Type as SqlType, value::Value as ParameterValue};

fn percentile_plans<T: Visit>(
    node: &T,
    parameters: &HashMap<String, Parameter>,
) -> Result<Vec<msduck_sql::percentile::RuntimePlan>> {
    let declarations = msduck_sql::binding_scope::Scope {
        parameters: parameters
            .keys()
            .map(|name| (name.clone(), Default::default()))
            .collect(),
        ..Default::default()
    };
    struct Plans<'a> {
        declarations: &'a msduck_sql::binding_scope::Scope,
        plans: Vec<msduck_sql::percentile::RuntimePlan>,
    }
    impl Visitor for Plans<'_> {
        type Break = anyhow::Error;
        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
            use msduck_sql::percentile::PlanError;
            match msduck_sql::percentile::runtime_plan(expr, self.declarations) {
                Ok(Some(plan)) => self.plans.push(plan),
                Ok(None) => {}
                Err(PlanError::Diagnostic(error)) => return ControlFlow::Break(error.into()),
                Err(PlanError::Shape(message)) => {
                    return ControlFlow::Break(anyhow::anyhow!(message));
                }
                Err(PlanError::Unknown) => {
                    return ControlFlow::Break(anyhow::anyhow!(
                        "unsupported statement-wide percentile fraction source"
                    ));
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut visitor = Plans {
        declarations: &declarations,
        plans: Vec::new(),
    };
    if let ControlFlow::Break(error) = node.visit(&mut visitor) {
        return Err(error);
    }
    Ok(visitor.plans)
}

// Backend lowering owns binding slots and native fault tickets. Logical result
// metadata must be acquired from the original percentile expression beforehand.
fn lower_runtime_percentiles<T: VisitMut>(
    node: &mut T,
    bindings: &[(String, Option<String>, bool)],
) -> Result<()> {
    struct Lower<'a> {
        bindings: &'a [(String, Option<String>, bool)],
        index: usize,
    }
    impl VisitorMut for Lower<'_> {
        type Break = anyhow::Error;
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
            let Expr::Function(function) = expr else {
                return ControlFlow::Continue(());
            };
            let name = function.name.to_string().to_ascii_lowercase();
            if !matches!(name.as_str(), "percentile_cont" | "percentile_disc") {
                return ControlFlow::Continue(());
            }
            let Some((fraction, fault, zero)) = self.bindings.get(self.index) else {
                return ControlFlow::Break(anyhow::anyhow!("percentile plan traversal changed"));
            };
            self.index += 1;
            let Some(order) = function.within_group.first() else {
                return ControlFlow::Break(anyhow::anyhow!("percentile ordering disappeared"));
            };
            let descending = order.options.sort == Some(OrderBySort::Desc);
            let mut input = order.expr.clone();
            if let Some(ticket) = fault {
                let guard = binary_function(
                    "__msduck_percentile_invalid_input",
                    input.clone(),
                    Expr::Identifier(Ident::new(ticket)),
                );
                input = Expr::Case {
                    case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                    end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                    operand: None,
                    conditions: vec![sqlparser::ast::CaseWhen {
                        condition: guard,
                        result: Expr::Value(sqlparser::ast::Value::Null.into()),
                    }],
                    else_result: Some(Box::new(input)),
                };
            }
            if name == "percentile_cont" {
                input = unary_function("__msduck_percentile_input", input);
            }
            let target = if descending && *zero {
                "max"
            } else if name == "percentile_cont" {
                "quantile_cont"
            } else {
                "quantile_disc"
            };
            let mut fraction = Expr::Identifier(Ident::new(fraction));
            if descending && !zero {
                fraction = Expr::UnaryOp {
                    op: UnaryOperator::Minus,
                    expr: Box::new(fraction),
                };
            }
            function.name = ObjectName::from(vec![Ident::new(target)]);
            function.within_group.clear();
            if let FunctionArguments::List(args) = &mut function.args {
                args.args = vec![FunctionArg::Unnamed(FunctionArgExpr::Expr(input))];
                if target != "max" {
                    args.args
                        .push(FunctionArg::Unnamed(FunctionArgExpr::Expr(fraction)));
                }
            }
            ControlFlow::Continue(())
        }
    }
    let mut lower = Lower { bindings, index: 0 };
    if let ControlFlow::Break(error) = node.visit(&mut lower) {
        return Err(error);
    }
    ensure!(
        lower.index == bindings.len(),
        "percentile plan traversal changed"
    );
    Ok(())
}

fn percentile_binding(
    parameters: &mut HashMap<String, Parameter>,
    value: ParameterValue,
    data_type: SqlType,
) -> String {
    let mut index = parameters.len();
    loop {
        let name = format!("@__msduck_percentile_{index}");
        if !parameters.keys().any(|key| key.eq_ignore_ascii_case(&name)) {
            parameters.insert(name.clone(), Parameter { value, data_type });
            return name;
        }
        index += 1;
    }
}

fn runtime_diagnostic(message: &str) -> Option<SqlError> {
    crate::guid_assignment::diagnostic(message)
        .or_else(|| rand::diagnostic(message))
        .or_else(|| crate::json_extract::diagnostic(message))
        .or_else(|| crate::integer_conversion::diagnostic(message))
        .or_else(|| crate::binary_unicode::diagnostic(message))
        .or_else(|| crate::storage_diagnostic::diagnostic(message))
        .or_else(|| crate::query_error::integer_overflow(message))
        .or_else(|| {
            msduck_core::left_right::diagnostic(
                message
                    .strip_prefix("Invalid Input Error: ")
                    .unwrap_or(message),
            )
        })
        .or_else(|| msduck_sql::recursive_lower::diagnostic(message))
        .or_else(|| msduck_sql::top::diagnostic(message))
        .or_else(|| crate::money_range::diagnostic(message))
        .or_else(|| crate::ntile::diagnostic(message))
        .or_else(|| msduck_sql::percentile::literal_diagnostic(message))
        .or_else(|| {
            msduck_core::diagnostic::numeric(
                message
                    .strip_prefix("Invalid Input Error: ")
                    .unwrap_or(message),
            )
        })
}

fn sql_error_from_message(message: &str) -> SqlError {
    runtime_diagnostic(message).unwrap_or_else(|| SqlError::new(error_number(message), 1, message))
}

/// Several diagnostics from one statement, reported in order. SQL Server's
/// multi-name DROP DATABASE drops what it can and reports each failure; the
/// last diagnostic is the statement's error.
#[derive(Debug)]
struct StatementErrors(Vec<SqlError>);
impl std::fmt::Display for StatementErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.last() {
            Some(error) => f.write_str(&error.message),
            None => Ok(()),
        }
    }
}
impl std::error::Error for StatementErrors {}

pub(crate) fn emit_error(out: &mut Vec<u8>, error: &anyhow::Error) -> i32 {
    // Output a hook produced before failing precedes its diagnostic.
    if let Some(partial) = error.downcast_ref::<ext::Partial>() {
        out.extend_from_slice(&partial.tokens);
        return emit_error(out, &partial.error);
    }
    if let Some(StatementErrors(errors)) = error.downcast_ref::<StatementErrors>() {
        for error in errors {
            tds::sql_error(out, error);
        }
        return errors.last().map_or(0, |error| error.number);
    }
    if let Some(error) = error.downcast_ref::<SqlError>() {
        tds::sql_error(out, error);
        error.number
    } else {
        let message = error.to_string();
        if let Some(error) = runtime_diagnostic(&message) {
            tds::sql_error(out, &error);
            error.number
        } else {
            let number = error_number(&message);
            tds::error(out, number, &message);
            number
        }
    }
}

// Control boundaries carry no row count and do not change session counters.
// SQL batches retain them under NOCOUNT; RPCs suppress non-result completions.
fn control_done(out: &mut Vec<u8>, rpc: bool, nocount: bool, command: u16) -> Option<usize> {
    if rpc && nocount {
        return None;
    }
    let offset = out.len();
    tds::done(out, if rpc { 0xff } else { 0xfd }, 1, command, 0);
    Some(offset)
}

fn binding_failure(error: &anyhow::Error) -> bool {
    // Runtime diagnostics gathered from one statement are catchable.
    if error.downcast_ref::<StatementErrors>().is_some() {
        return false;
    }
    if error
        .downcast_ref::<crate::query_error::CompilationFailure>()
        .is_some()
    {
        return true;
    }
    let message = error.to_string();
    let number = sql_error_from_message(&message).number;
    error.downcast_ref::<SqlError>().is_none()
        && (matches!(number, 102 | 137 | 208 | 8117 | 40515)
            || message.contains("Binder Error")
            || message.contains("Parser Error")
            || message.contains("Catalog Error"))
}

// Result-set presence is recorded when metadata is produced, including an
// empty result. It is independent of row counts and encoded token bytes.
struct Execution {
    tokens: Vec<u8>,
    count: Option<u64>,
    command: u16,
    kind: msduck_core::completion::Kind,
}
impl Execution {
    fn statement(tokens: Vec<u8>, count: Option<u64>, command: u16) -> Self {
        Self {
            tokens,
            count,
            command,
            kind: msduck_core::completion::Kind::Statement,
        }
    }
    fn result_set(tokens: Vec<u8>, count: Option<u64>, command: u16) -> Self {
        Self {
            tokens,
            count,
            command,
            kind: msduck_core::completion::Kind::ResultSet,
        }
    }
}

// Command identities captured from SQL Server for successful DDL. Backend
// affected-row counts do not represent SQL Server DDL completion row counts.
fn ddl_completion_command(statement: &Statement) -> Option<u16> {
    Some(match statement {
        Statement::CreateTable(_) => 198,
        Statement::CreateView(_) | Statement::AlterView { .. } => 207,
        Statement::CreateIndex(_) => 200,
        Statement::AlterTable(_) => 216,
        Statement::Truncate(_) => 234,
        Statement::CreateSchema { .. } => 253,
        Statement::Drop { object_type, .. } => match object_type {
            ObjectType::Table => 199,
            ObjectType::View => 208,
            ObjectType::Index => 201,
            ObjectType::Schema => 253,
            _ => return None,
        },
        _ => return None,
    })
}

struct Work<'a> {
    statement: &'a Statement,
    loop_boundary: bool,
    catch_handler: Option<&'a [Statement]>,
    restore_error: Option<Option<SqlError>>,
    completion: Option<u16>,
}
impl<'a> Work<'a> {
    fn leaf(statement: &'a Statement) -> Self {
        Self {
            statement,
            loop_boundary: false,
            catch_handler: None,
            restore_error: None,
            completion: None,
        }
    }
}
#[derive(Clone, Copy)]
enum RpcExecution {
    Direct,
    Prepared,
}

pub struct Session {
    pub db: Connection,
    diagnostics: crate::statement_diagnostics::Registry,
    rand: std::sync::Arc<std::sync::Mutex<rand::Generator>>,
    pub nocount: bool,
    pub transactions: u32,
    pub rowcount: u64,
    pub last_error: i32,
    pub transaction_descriptor: u64,
    pub original_login: String,
    transaction_name: String,
    xact_abort: bool,
    ansi_warnings: bool,
    datefirst: i32,
    backend_datefirst: std::cell::Cell<i32>,
    /// Statements running in another database (see `enter_home`).
    catalog_homes: std::cell::Cell<u32>,
    /// Set by `execute_routed` for the `execute` call it makes.
    routed: std::cell::Cell<bool>,
    /// Databases that running statements use besides the current one
    /// (`use_other`, `enter_home`); they count as the session's own uses.
    other_uses: OtherUses,
    /// Set when such a statement could not restore the session database
    /// as the connection's DuckDB default catalog.
    catalog_displaced: std::cell::Cell<bool>,
    transaction_doomed: bool,
    caught_error: Option<SqlError>,
    read_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    read_cancelled: bool,
    read_cancel_cleanup_failed: bool,
    /// The session's current database; it stays in use while selected.
    database: crate::database_catalog::Use,
    /// SINGLE_USER databases this session set and still holds.
    holds: Vec<crate::database_catalog::Hold>,
    /// The session's SPID and sys.dm_exec_sessions entry.
    process: crate::sessions::Registration,
    /// Keys set by sp_set_session_context; RESETCONNECTION starts a new session.
    session_context: msduck_sql::session_function::SessionContext,
    /// State of extension features (see `ext`).
    ext: ext::State,
}
impl Drop for Session {
    fn drop(&mut self) {
        ext::session_end(self);
    }
}
impl Session {
    /// Current supported SET state. All remaining bits are fixed login options
    /// because session_setting explicitly refuses changes to those options.
    fn options_mask(&self) -> i32 {
        let options = self.session_options();
        1024 // ANSI_NULL_DFLT_ON, established at login
            | if options.ansi_warnings { 8 } else { 0 }
            | if options.ansi_padding { 16 } else { 0 }
            | if options.ansi_nulls { 32 } else { 0 }
            | if options.arithabort { 64 } else { 0 }
            | if options.quoted_identifier { 256 } else { 0 }
            | if self.nocount { 512 } else { 0 }
            | if options.concat_null_yields_null { 4096 } else { 0 }
            | if options.numeric_roundabort { 8192 } else { 0 }
            | if self.xact_abort { 16384 } else { 0 }
    }

    pub fn new(connection: crate::server::Connection) -> Result<Self> {
        let (db, diagnostics, databases, sessions) = connection.into_parts();
        let database = databases.enter(&db, crate::database_catalog::MASTER)?;
        let process = sessions.register(database.database_id, database.alias())?;
        process.set_interrupt(db.interrupt_handle());
        db.execute_batch("SET schema = 'dbo'; SET arrow_lossless_conversion = true; SET VARIABLE __msduck_datefirst = 7")?;
        db.execute_batch(
            "CREATE TEMP MACRO __msduck_time_round(value, quantum) AS
             CAST(substr(CAST(make_timestamp_ns(
                 (((epoch_ns(value) + quantum // 2) // quantum) * quantum)
                 % 86400000000000
             ) AS VARCHAR), 12) AS TIME_NS)",
        )?;
        // Microseconds since the epoch on SQL Server's datetime grid (1/300 s,
        // shown as .000/.003/.007), for current-time functions in stored
        // definitions.
        db.execute_batch(
            "CREATE TEMP MACRO __msduck_legacy_datetime(us) AS make_timestamp(
                 ((us * 3 + 5000) // 10000) // 300 * 1000000
                 + ((((us * 3 + 5000) // 10000) % 300) * 10 + 1) // 3 * 1000)",
        )?;
        db.execute_batch("CREATE TEMP MACRO __msduck_int_div(a,b) AS CASE WHEN b=0 THEN error('Divide by zero error encountered.') ELSE a // b END;
            CREATE TEMP MACRO __msduck_int_mod(a,b) AS CASE WHEN b=0 THEN error('Divide by zero error encountered.') ELSE a % b END")?;
        let mut session = Self {
            db,
            diagnostics,
            rand: rand::Generator::new()?,
            nocount: false,
            transactions: 0,
            rowcount: 0,
            last_error: 0,
            transaction_descriptor: 0,
            original_login: "sa".into(),
            transaction_name: String::new(),
            xact_abort: false,
            ansi_warnings: true,
            datefirst: 7,
            backend_datefirst: std::cell::Cell::new(7),
            catalog_homes: std::cell::Cell::new(0),
            routed: std::cell::Cell::new(false),
            other_uses: Default::default(),
            catalog_displaced: std::cell::Cell::new(false),
            transaction_doomed: false,
            caught_error: None,
            read_cancel: None,
            read_cancelled: false,
            read_cancel_cleanup_failed: false,
            database,
            holds: Vec::new(),
            process,
            session_context: Self::empty_session_context(),
            ext: ext::State::default(),
        };
        ext::session_start(&mut session)?;
        Ok(session)
    }

    /// The session's SPID.
    pub fn spid(&self) -> i16 {
        self.process.spid()
    }

    /// The session's sys.dm_exec_sessions entry.
    pub fn process(&self) -> &crate::sessions::Registration {
        &self.process
    }

    /// Continue `other`'s session in this one after RESETCONNECTION: keep
    /// its SPID, login and client names, and the SINGLE_USER databases it
    /// holds, as SQL Server does for a reset connection.
    pub fn continue_session(&mut self, other: &mut Session) {
        self.process.exchange(&mut other.process);
        self.process
            .set_database(self.database.database_id, self.database.alias());
        self.process.set_interrupt(self.db.interrupt_handle());
        self.holds = std::mem::take(&mut other.holds);
        self.original_login = std::mem::take(&mut other.original_login);
    }

    /// How many of this session's uses and holds are on a catalog alias.
    fn own_uses(&self, alias: &str) -> usize {
        usize::from(self.database.alias() == alias)
            + self.held(alias)
            + self.other_uses.count(alias)
    }

    fn held(&self, alias: &str) -> usize {
        self.holds
            .iter()
            .filter(|hold| hold.alias() == alias)
            .count()
    }

    /// CREATE DATABASE, DROP DATABASE and USE run against the database
    /// catalog. Tokens, errors and completion commands follow SQL Server.
    fn database_statement(&mut self, statement: &Statement) -> Result<Option<Execution>> {
        if let Some(request) = msduck_sql::dialect::alter_database::request(statement) {
            return self.alter_database(request).map(Some);
        }
        match statement {
            Statement::Use(target) => {
                let sqlparser::ast::Use::Object(name) = target else {
                    bail!("unsupported USE statement: {statement}");
                };
                let name = database_name(name)?;
                let old = self.database.name.clone();
                self.use_database(&name)?;
                let mut tokens = Vec::new();
                tds::env_text(&mut tokens, 1, &self.database.name, &old);
                let message: Vec<u16> =
                    format!("Changed database context to '{}'.", self.database.name)
                        .encode_utf16()
                        .collect();
                tds::diagnostic_utf16(
                    &mut tokens,
                    tds::DiagnosticKind::Information,
                    0,
                    1,
                    5701,
                    &message,
                );
                tds::collation_change(&mut tokens);
                Ok(Some(Execution::statement(tokens, None, 226)))
            }
            Statement::CreateDatabase {
                db_name,
                if_not_exists,
                default_collation,
                ..
            } => {
                // Only the name, the server collation and IF NOT EXISTS are
                // supported; any other parsed option is refused explicitly.
                let mut plain = statement.clone();
                if let Statement::CreateDatabase {
                    if_not_exists,
                    default_collation,
                    ..
                } = &mut plain
                {
                    *if_not_exists = false;
                    *default_collation = None;
                }
                if plain.to_string() != format!("CREATE DATABASE {db_name}") {
                    bail!("unsupported CREATE DATABASE options: {statement}");
                }
                if let Some(collation) = default_collation
                    && !collation.eq_ignore_ascii_case("SQL_Latin1_General_CP1_CI_AS")
                {
                    bail!(
                        "unsupported CREATE DATABASE collation {collation}; only SQL_Latin1_General_CP1_CI_AS is available"
                    );
                }
                if self.transactions > 0 {
                    bail!(SqlError::new(
                        226,
                        5,
                        "CREATE DATABASE statement not allowed within multi-statement transaction."
                    ));
                }
                let name = database_name(db_name)?;
                match self.database.catalog().create(&self.db, &name) {
                    Err(error)
                        if *if_not_exists
                            && error
                                .downcast_ref::<SqlError>()
                                .is_some_and(|error| error.number == 1801) => {}
                    result => {
                        result?;
                    }
                }
                Ok(Some(Execution::statement(Vec::new(), None, 203)))
            }
            Statement::Drop {
                object_type: ObjectType::Database,
                if_exists,
                names,
                cascade,
                restrict,
                purge,
                temporary,
                table,
            } => {
                if *cascade || *restrict || *purge || *temporary || table.is_some() {
                    bail!("unsupported DROP DATABASE options: {statement}");
                }
                if self.transactions > 0 {
                    bail!(SqlError::new(
                        574,
                        0,
                        "DROP DATABASE statement cannot be used inside a user transaction."
                    ));
                }
                // Like SQL Server, drop every database that can be dropped
                // and report each failure; the statement fails if any does.
                // Malformed names fail the statement before anything is dropped.
                let names = names
                    .iter()
                    .map(database_name)
                    .collect::<Result<Vec<_>>>()?;
                let mut errors = Vec::new();
                for name in names {
                    let alias = self
                        .database
                        .catalog()
                        .resolve(&self.db, &name)
                        .ok()
                        .flatten();
                    let removed = self
                        .database
                        .catalog()
                        .remove_as(&self.db, &name, &|alias| self.held(alias));
                    // Holds on a dropped database end with it.
                    if removed.is_ok()
                        && let Some(alias) = alias
                    {
                        self.holds.retain(|hold| hold.alias() != alias);
                    }
                    if let Err(error) = removed {
                        let error = error
                            .downcast_ref::<SqlError>()
                            .cloned()
                            .unwrap_or_else(|| sql_error_from_message(&format!("{error:#}")));
                        if !(*if_exists && error.number == 3701) {
                            errors.push(error);
                        }
                    }
                }
                match errors.len() {
                    0 => Ok(Some(Execution::statement(Vec::new(), None, 204))),
                    1 => Err(errors.remove(0).into()),
                    _ => Err(StatementErrors(errors).into()),
                }
            }
            _ => Ok(None),
        }
    }

    /// ALTER DATABASE ... SET, following reference/alter-database-sessions.json.
    /// ROLLBACK IMMEDIATE closes the other sessions using the database; the
    /// session that sets SINGLE_USER holds the database until it disconnects
    /// or sets another user access mode.
    fn alter_database(
        &mut self,
        request: msduck_sql::dialect::alter_database::Request,
    ) -> Result<Execution> {
        use crate::database_catalog::{Setting, Termination, UserAccess};
        use msduck_sql::dialect::alter_database as syntax;
        if self.transactions > 0 {
            bail!(SqlError::new(
                226,
                6,
                "ALTER DATABASE statement not allowed within multi-statement transaction."
            ));
        }
        let name = match &request.database {
            Some(name) => name.value.clone(),
            None if self.database.database_id == crate::database_catalog::MASTER_ID => {
                bail!(SqlError::new(
                    12104,
                    2,
                    "ALTER DATABASE CURRENT failed because 'master' is a system database. System databases cannot be altered by using the CURRENT keyword. Use the database name to alter a system database."
                ));
            }
            None => self.database.name.clone(),
        };
        let settings = request
            .settings
            .iter()
            .map(|setting| match setting {
                syntax::Setting::ReadCommittedSnapshot(on) => Setting::ReadCommittedSnapshot(*on),
                syntax::Setting::SingleUser => Setting::UserAccess(UserAccess::Single),
                syntax::Setting::RestrictedUser => Setting::UserAccess(UserAccess::Restricted),
                syntax::Setting::MultiUser => Setting::UserAccess(UserAccess::Multi),
            })
            .collect::<Vec<_>>();
        let termination = match request.termination {
            syntax::Termination::Wait => Termination::Wait,
            syntax::Termination::NoWait => Termination::NoWait,
            syntax::Termination::RollbackImmediate => Termination::RollbackImmediate,
            syntax::Termination::RollbackAfterSeconds(seconds) => {
                Termination::RollbackAfter(std::time::Duration::from_secs(seconds))
            }
        };
        let catalog = self.database.catalog().clone();
        let altered = catalog
            .alter(
                &self.db,
                &name,
                &settings,
                termination,
                &|alias| self.own_uses(alias),
                &|alias| self.process.terminate_others(alias),
            )
            .map_err(|error| match error.downcast_ref::<SqlError>() {
                Some(first) if matches!(first.number, 5011 | 5064 | 5070) => StatementErrors(vec![
                    first.clone(),
                    SqlError::new(5069, 1, "ALTER DATABASE statement failed."),
                ])
                .into(),
                _ => error,
            })?;
        let access = settings.iter().rev().find_map(|setting| match setting {
            Setting::UserAccess(access) => Some(*access),
            Setting::ReadCommittedSnapshot(_) => None,
        });
        if access.is_some_and(|access| access != UserAccess::Single) {
            self.holds.retain(|hold| hold.alias() != altered.alias);
        }
        if let Some(hold) = altered.hold
            && self.held(hold.alias()) == 0
        {
            self.holds.push(hold);
        }
        let mut tokens = Vec::new();
        if altered.terminated {
            for (state, progress) in [(2, 0), (1, 100)] {
                let message: Vec<u16> = format!(
                    "Nonqualified transactions are being rolled back. Estimated rollback completion: {progress}%."
                )
                .encode_utf16()
                .collect();
                tds::diagnostic_utf16(
                    &mut tokens,
                    tds::DiagnosticKind::Information,
                    0,
                    state,
                    5060,
                    &message,
                );
            }
        }
        Ok(Execution::statement(tokens, None, 215))
    }

    /// Replace DB_NAME and DB_ID with the session's current database or a
    /// catalog lookup. Runs per statement, so a USE earlier in the batch
    /// applies to later statements.
    fn lower_database_functions<T: VisitMut>(&self, node: &mut T) -> Result<()> {
        struct Lower<'a>(&'a crate::database_catalog::Use);
        impl VisitorMut for Lower<'_> {
            type Break = String;
            fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
                let Expr::Function(f) = expr else {
                    return ControlFlow::Continue(());
                };
                let Some(function) = msduck_sql::session_function::database_function(f) else {
                    return ControlFlow::Continue(());
                };
                let argument = match &f.args {
                    FunctionArguments::List(args) => match args.args.as_slice() {
                        [] => None,
                        [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))] => {
                            Some(argument.clone())
                        }
                        _ => {
                            return ControlFlow::Break(format!("unsupported arguments in {f}"));
                        }
                    },
                    _ => None,
                };
                *expr = msduck_sql::session_function::database(
                    function,
                    argument,
                    &self.0.name,
                    self.0.database_id,
                );
                ControlFlow::Continue(())
            }
        }
        match node.visit(&mut Lower(&self.database)) {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(message) => bail!(message),
        }
    }

    /// Resolve the database part of `database.schema.object` relations.
    /// Only published databases resolve, so one being created or dropped
    /// cannot be read or modified; like SQL Server, an unknown database makes
    /// the object name invalid (208). A name in the current database becomes
    /// `schema.object`, so binding, metadata and storage coercions apply as
    /// usual. Features that bind against the current database's catalog
    /// objects use this form, which refuses a reference to another database
    /// rather than binding it without its declared metadata.
    fn qualify_databases<T: VisitMut>(&self, node: &mut T) -> Result<()> {
        self.qualify_relations(node, false)
    }

    /// `qualify_databases` for statements the engine plans itself, which may
    /// read another database: such a relation names that database's quoted
    /// DuckDB catalog, whose catalog objects supply its metadata (see
    /// `cross_database`). Already qualified names are kept, so this
    /// can run again on its own output.
    fn qualify_databases_across<T: VisitMut>(&self, node: &mut T) -> Result<()> {
        self.qualify_relations(node, true)
    }

    fn qualify_relations<T: VisitMut>(&self, node: &mut T, across: bool) -> Result<()> {
        struct Qualify<'a> {
            session: &'a Session,
            across: bool,
            current: Option<String>,
        }
        impl VisitorMut for Qualify<'_> {
            type Break = anyhow::Error;
            fn pre_visit_relation(
                &mut self,
                relation: &mut ObjectName,
            ) -> ControlFlow<anyhow::Error> {
                let parts = relation.0.len();
                if parts < 3 {
                    return ControlFlow::Continue(());
                }
                let written = relation
                    .0
                    .iter()
                    .map(|part| match part {
                        ObjectNamePart::Identifier(ident) => ident.value.clone(),
                        part => part.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                if parts > 3 {
                    return ControlFlow::Break(anyhow::anyhow!(
                        "unsupported server-qualified object name {written}"
                    ));
                }
                // `database..object` names the default schema, which is dbo.
                if let ObjectNamePart::Identifier(schema) = &mut relation.0[1]
                    && schema.value.is_empty()
                    && schema.quote_style.is_none()
                {
                    *schema = Ident::new("dbo");
                }
                let ObjectNamePart::Identifier(database) = &mut relation.0[0] else {
                    return ControlFlow::Break(anyhow::anyhow!(
                        "unsupported object name {written}"
                    ));
                };
                let catalog = self.session.database.catalog();
                let resolved = match catalog.resolve(&self.session.db, &database.value) {
                    // A name the engine itself qualified with master's
                    // catalog; client SQL carries source spans and cannot
                    // name DuckDB's catalog.
                    Ok(None)
                        if database.quote_style == Some('"')
                            && database.span == sqlparser::tokenizer::Span::empty()
                            && database.value == catalog.primary() =>
                    {
                        Ok(Some(catalog.primary().to_string()))
                    }
                    resolved => resolved,
                };
                let current = match &self.current {
                    Some(current) => current,
                    None => match self.session.current_alias() {
                        Ok(current) => self.current.insert(current),
                        Err(error) => return ControlFlow::Break(error),
                    },
                };
                match resolved {
                    Ok(Some(alias)) if alias == *current => {
                        relation.0.remove(0);
                        ControlFlow::Continue(())
                    }
                    Ok(Some(alias)) if self.across => {
                        *database = Ident::with_quote('"', alias);
                        ControlFlow::Continue(())
                    }
                    Ok(Some(_)) => ControlFlow::Break(anyhow::anyhow!(
                        "unsupported reference to {written} in another database; USE {} first",
                        database.value
                    )),
                    Ok(None) => ControlFlow::Break(
                        SqlError::new(208, 1, format!("Invalid object name '{written}'.")).into(),
                    ),
                    Err(error) => ControlFlow::Break(error),
                }
            }
        }
        match node.visit(&mut Qualify {
            session: self,
            across,
            current: None,
        }) {
            ControlFlow::Continue(()) => Ok(()),
            ControlFlow::Break(error) => Err(error),
        }
    }

    /// Qualify a statement's relations: queries, DML and variable
    /// assignments may read other databases (see `cross_database`); other
    /// statements, such as DDL, are refused there.
    fn qualify_statement(&self, statement: &mut Statement) -> Result<()> {
        if matches!(
            statement,
            Statement::Query(_)
                | Statement::Insert(_)
                | Statement::Update(_)
                | Statement::Delete(_)
                | Statement::Set(_)
                | Statement::Declare { .. }
        ) {
            self.qualify_databases_across(statement)
        } else {
            // DROP names objects outside relation positions. Another
            // database's objects are refused like other DDL on them.
            if let Statement::Drop { names, .. } = statement {
                let catalog = self.database.catalog();
                for name in names.iter_mut().filter(|name| name.0.len() == 3) {
                    let Some(database) = name.0[0].as_ident() else {
                        continue;
                    };
                    match catalog.resolve(&self.db, &database.value)? {
                        Some(alias) if alias == self.current_alias()? => {
                            name.0.remove(0);
                        }
                        Some(_) => bail!(
                            "unsupported reference to {} in another database; USE {} first",
                            name.0
                                .iter()
                                .filter_map(|part| part.as_ident().map(|ident| ident.value.clone()))
                                .collect::<Vec<_>>()
                                .join("."),
                            database.value
                        ),
                        None => {}
                    }
                }
            }
            self.qualify_databases(statement)
        }
    }

    /// The DuckDB catalog names resolve against: the session's database, or
    /// the one a statement runs in (see `cross_database`).
    fn current_alias(&self) -> Result<String> {
        if self.catalog_homes.get() > 0 || self.catalog_displaced.get() {
            Ok(self
                .db
                .query_row("SELECT current_database()", [], |row| row.get(0))?)
        } else {
            Ok(self.database.alias().to_string())
        }
    }

    /// The database a query or DML statement qualified by
    /// `qualify_databases_across` runs in, when that is not the current one.
    ///
    /// A statement whose relations all belong to one other database runs
    /// with that database as the DuckDB default catalog, so every metadata
    /// lookup, storage coercion and write sees it as it would after USE.
    /// Other statements run in the current database: they may read other
    /// databases, whose metadata comes from their own catalogs, but may not
    /// write them. Other statement kinds are refused, as before.
    fn cross_database<T: Visit>(
        &self,
        node: &T,
        statement: Option<&Statement>,
    ) -> Result<CrossDatabase> {
        let relations = Relations::collect(
            node,
            self.current_alias()?,
            &statement.map(alias_targets).unwrap_or_default(),
        );
        let Some((database, written)) = relations.foreign.first_key_value() else {
            return Ok(CrossDatabase::Local);
        };
        let display = |alias: &str| self.database.catalog().display_name(alias);
        let dml = match statement {
            None => None,
            Some(Statement::Query(query)) => match query.body.as_ref() {
                SetExpr::Insert(statement)
                | SetExpr::Update(statement)
                | SetExpr::Delete(statement) => Some(statement),
                _ => None,
            },
            Some(
                statement @ (Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_)),
            ) => Some(statement),
            Some(_) => {
                bail!(
                    "unsupported reference to {} in another database; USE {} first",
                    written,
                    display(database)
                )
            }
        };
        if relations.foreign.len() == 1 && relations.local.is_empty() && !relations.into {
            return Ok(CrossDatabase::Home(database.clone(), vec![]));
        }
        // A statement that writes another database runs there, reading the
        // current database and any others through three-part names.
        let target = dml.and_then(dml_target);
        if let Some(target) = target
            && let [ObjectNamePart::Identifier(alias), _, _] = target.0.as_slice()
            && relations.foreign.contains_key(&alias.value)
        {
            // Temporary objects belong to the session's database.
            if relations.temporary {
                bail!(
                    "unsupported cross-database statement: it writes {} in database '{}' and also references temporary tables or table variables",
                    target.0[1..]
                        .iter()
                        .map(|part| part.to_string())
                        .collect::<Vec<_>>()
                        .join("."),
                    display(&alias.value)
                );
            }
            let home = alias.value.clone();
            let others = relations
                .foreign
                .into_keys()
                .filter(|other| *other != home)
                .collect();
            return Ok(CrossDatabase::Home(home, others));
        }
        Ok(CrossDatabase::Mixed(
            relations.foreign.into_keys().collect(),
        ))
    }

    /// Make `home`, another database, the connection's DuckDB default
    /// catalog for one statement, after the checks USE makes (user access,
    /// transitions). Like USE, the returned guard keeps the database from
    /// being dropped meanwhile. `leave_home` restores the previous catalog.
    /// USE cannot run in an aborted DuckDB transaction, so a failed restore
    /// is retried by `restore_catalog` before the next statement and after
    /// ROLLBACK.
    fn enter_home(&self, home: &str) -> Result<Home> {
        let previous =
            self.db
                .query_row("SELECT current_database(),current_schema()", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
        // While the statement runs, `execute_in_home` makes the other
        // database the session's and the session's database a use of the
        // statement, as `own_uses` counts them.
        let OtherUse { guard, .. } = self.use_other(home)?;
        self.catalog_homes.set(self.catalog_homes.get() + 1);
        Ok(Home {
            previous,
            guard,
            _counted: self.other_uses.add(self.database.alias()),
        })
    }

    fn leave_home(&self, home: Home) {
        self.catalog_homes.set(self.catalog_homes.get() - 1);
        if crate::query_catalog::leave_catalog(&self.db, &home.previous).is_err() {
            self.catalog_displaced.set(true);
        }
    }

    /// Hold the other databases a statement reads, with the checks USE
    /// makes, while it runs in the current database.
    fn use_others(&self, aliases: &[String]) -> Result<Vec<OtherUse>> {
        let previous =
            self.db
                .query_row("SELECT current_database(),current_schema()", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
        let guards = aliases
            .iter()
            .map(|alias| self.use_other(alias))
            .collect::<Result<Vec<_>>>();
        if crate::query_catalog::leave_catalog(&self.db, &previous).is_err() {
            self.catalog_displaced.set(true);
        }
        guards
    }

    fn use_other(&self, alias: &str) -> Result<OtherUse> {
        let catalog = self.database.catalog();
        let guard = catalog.enter_as(&self.db, &catalog.display_name(alias), &|alias| {
            self.own_uses(alias)
        })?;
        Ok(OtherUse {
            _counted: self.other_uses.add(alias),
            guard,
        })
    }

    /// Make the session's database the DuckDB default catalog again after a
    /// statement in another database could not restore it.
    fn restore_catalog(&self) {
        if self.catalog_homes.get() == 0
            && self.catalog_displaced.get()
            && crate::query_catalog::leave_catalog(
                &self.db,
                &(self.database.alias().to_string(), "dbo".to_string()),
            )
            .is_ok()
        {
            self.catalog_displaced.set(false);
        }
    }

    /// DuckDB writes one attached database per transaction. Name the
    /// databases instead of DuckDB's catalogs. DuckDB has aborted the
    /// transaction, so its work is rolled back now and the session's
    /// transaction is doomed, with an empty DuckDB transaction in its place:
    /// outside TRY the batch ends and rolls it back, inside TRY the handler
    /// runs and must roll it back. Nothing it wrote commits.
    fn single_database_writes(&mut self, error: anyhow::Error) -> anyhow::Error {
        let message = format!("{error:#}");
        let Some(rest) = message
            .split_once("Attempting to write to database \"")
            .map(|(_, rest)| rest)
        else {
            return error;
        };
        let names = rest.split_once('"').and_then(|(written, rest)| {
            rest.split_once("already modified database \"")
                .and_then(|(_, rest)| rest.split_once('"'))
                .map(|(modified, _)| (written, modified))
        });
        let Some((written, modified)) = names else {
            return error;
        };
        if self.transactions > 0 {
            self.transaction_doomed = true;
            if self.db.execute_batch("ROLLBACK; BEGIN TRANSACTION").is_ok() {
                self.restore_catalog();
            }
        }
        let catalog = self.database.catalog();
        // A runtime error, which TRY catches, unlike unsupported syntax.
        SqlError::new(
            40515,
            1,
            format!(
                "unsupported cross-database transaction: database '{}' cannot be modified in a transaction that has already modified database '{}'; a transaction may write only one database",
                catalog.display_name(written),
                catalog.display_name(modified)
            ),
        )
        .into()
    }

    /// The session's current database.
    pub fn database(&self) -> &crate::database_catalog::Use {
        &self.database
    }

    /// Make `name` the session's current database, as USE and the login do.
    /// The previous database is released only once the new one is in use.
    pub fn use_database(&mut self, name: &str) -> Result<()> {
        self.enter_database(name, None)
    }

    /// Enter the login database for a session that replaces `previous` after
    /// RESETCONNECTION. The previous session's uses and holds count as this
    /// session's own, because it continues the same SQL Server session.
    pub fn use_database_continuing(&mut self, name: &str, previous: &Session) -> Result<()> {
        self.enter_database(name, Some(previous))
    }

    fn enter_database(&mut self, name: &str, previous: Option<&Session>) -> Result<()> {
        let catalog = self.database.catalog().clone();
        let entered = catalog.enter_as(&self.db, name, &|alias| {
            self.own_uses(alias) + previous.map_or(0, |previous| previous.own_uses(alias))
        })?;
        self.database = entered;
        self.process
            .set_database(self.database.database_id, self.database.alias());
        Ok(())
    }

    /// Open an explicit context for one execution. Preparing SQL must not open
    /// or mutate a current-session context in the shared native catalog.
    pub fn diagnostic_scope(&self) -> Result<crate::statement_diagnostics::Scope> {
        self.diagnostics.begin().map_err(anyhow::Error::msg)
    }
    pub fn batch(
        &mut self,
        sql: &str,
        parameters: &HashMap<String, Parameter>,
        rpc: bool,
    ) -> Vec<u8> {
        self.batch_response(sql, parameters, rpc, None).0
    }
    /// Compile without stepping statements. Preparation must never execute DML.
    pub fn validate_prepared_sql(
        &self,
        sql: &str,
        declarations: &[(String, SqlType)],
    ) -> Result<()> {
        self.validate_prepared_statements(parse_batch(sql)?, declarations)
    }
    fn validate_prepared_statements(
        &self,
        statements: Vec<Statement>,
        declarations: &[(String, SqlType)],
    ) -> Result<()> {
        let parameters = declarations
            .iter()
            .map(|(name, kind)| {
                (
                    name.clone(),
                    Parameter {
                        value: ParameterValue::Null,
                        data_type: *kind,
                    },
                )
            })
            .collect();
        let mut parameters = batch_variables(&statements, &parameters)?;
        let mut pending = statements.into_iter().rev().collect::<Vec<_>>();
        while let Some(statement) = pending.pop() {
            crate::query_catalog::validate_cte_columns(&self.db, &statement)?;
            if let Some((body, handler)) = try_catch_parts(&statement) {
                pending.extend(handler.iter().rev().cloned());
                pending.extend(body.iter().rev().cloned());
                continue;
            }
            if crate::dialect::loop_control(&statement).is_some() {
                continue; // Placement was checked by batch_variables.
            }
            if msduck_sql::drop_index_syntax::request(&statement).is_some() {
                continue; // Bind table/index identities only when executed.
            }
            if msduck_sql::dialect::alter_database::request(&statement).is_some() {
                continue; // Databases are resolved only when executed.
            }
            if msduck_sql::dialect::ext::custom(&statement).is_some()
                || matches!(statement, Statement::Set(Set::SetTransaction { .. }))
            {
                continue; // Extension statements and session settings bind when executed.
            }
            if let Some(call) = msduck_sql::raiserror::call(&statement) {
                for expression in [call.message, call.severity, call.state]
                    .into_iter()
                    .chain(call.substitutions)
                {
                    msduck_sql::raiserror::argument(expression, &parameters)?;
                }
                continue;
            }
            match &statement {
                Statement::If(condition) => {
                    self.validate_prepared_predicate(
                        condition
                            .if_block
                            .condition
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("missing IF condition"))?,
                        &parameters,
                    )?;
                    if let Some(block) = &condition.else_block {
                        pending.extend(block.statements().iter().rev().cloned());
                    }
                    pending.extend(condition.if_block.statements().iter().rev().cloned());
                    continue;
                }
                Statement::While(condition) => {
                    self.validate_prepared_predicate(
                        condition
                            .while_block
                            .condition
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("missing WHILE condition"))?,
                        &parameters,
                    )?;
                    pending.extend(condition.while_block.statements().iter().rev().cloned());
                    continue;
                }
                Statement::StartTransaction {
                    has_end_keyword: true,
                    statements,
                    ..
                } => {
                    pending.extend(statements.iter().rev().cloned());
                    continue;
                }
                Statement::Return(value) => {
                    if let Some(ReturnStatementValue::Expr(expression)) = &value.value {
                        self.validate_prepared_scalar(
                            expression.clone(),
                            DataType::Int(None),
                            &parameters,
                        )?;
                    }
                    continue;
                }
                Statement::Print(print) => {
                    self.validate_prepared_scalar(
                        *print.message.clone(),
                        print_target(&print.message, &parameters),
                        &parameters,
                    )?;
                    continue;
                }
                Statement::StartTransaction { .. }
                | Statement::Commit { .. }
                | Statement::Rollback { .. } => {
                    continue; // Syntax was checked; transaction state changes only on execution.
                }
                Statement::Throw(throw) => {
                    if let (Some(number), Some(message), Some(state)) =
                        (&throw.error_number, &throw.message, &throw.state)
                    {
                        self.validate_prepared_scalar(
                            *number.clone(),
                            DataType::Int(None),
                            &parameters,
                        )?;
                        self.validate_prepared_scalar(
                            *message.clone(),
                            DataType::Varchar(None),
                            &parameters,
                        )?;
                        self.validate_prepared_scalar(
                            *state.clone(),
                            DataType::Int(None),
                            &parameters,
                        )?;
                    }
                    continue;
                }
                _ => {}
            }
            if let Statement::Declare { stmts } = &statement {
                for declaration in stmts {
                    let kind = variable_type(
                        declaration
                            .data_type
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("missing variable type"))?,
                    )?;
                    let expression = match &declaration.assignment {
                        None => Expr::Value(sqlparser::ast::Value::Null.into()),
                        Some(DeclareAssignment::MsSqlAssignment(value)) => *value.clone(),
                        _ => bail!("unsupported variable initializer"),
                    };
                    self.validate_prepared_scalar(
                        expression,
                        crate::sql_type::ast(kind),
                        &parameters,
                    )?;
                }
                continue;
            }
            if matches!(&statement, Statement::Set(_))
                && !matches!(&statement, Statement::Set(Set::SingleAssignment { variable, .. }) if variable.to_string().starts_with('@'))
            {
                if let Some(value) =
                    crate::datepart::setting(&statement).map_err(anyhow::Error::msg)?
                {
                    self.validate_prepared_scalar(value, DataType::Int(None), &parameters)?;
                } else {
                    session_setting(&statement)?;
                }
                continue;
            }
            if let Statement::Set(Set::SingleAssignment {
                scope,
                hivevar,
                variable,
                values,
            }) = &statement
            {
                let name = variable.to_string().to_lowercase();
                ensure!(
                    name.starts_with('@') && scope.is_none() && !hivevar && values.len() == 1,
                    "unsupported prepared variable assignment"
                );
                let kind = parameters
                    .get(&name)
                    .ok_or_else(|| anyhow::anyhow!("Must declare the scalar variable {name}"))?
                    .data_type;
                self.validate_prepared_scalar(
                    values[0].clone(),
                    crate::sql_type::ast(kind),
                    &parameters,
                )?;
                continue;
            }
            ensure!(
                matches!(
                    statement,
                    Statement::Query(_)
                        | Statement::Insert(_)
                        | Statement::Update { .. }
                        | Statement::Delete(_)
                ),
                "unsupported prepared statement kind"
            );
            // Databases resolve as execution resolves them (see `execute`).
            let mut qualified = statement.clone();
            if self.qualify_databases_across(&mut qualified).is_ok() {
                match self.cross_database(&qualified, Some(&qualified))? {
                    CrossDatabase::Local => {}
                    CrossDatabase::Home(home, others) => {
                        let targets = alias_targets(&qualified);
                        rehome(&mut qualified, &home, &self.current_alias()?, &targets);
                        let _others = self.use_others(&others)?;
                        let home = self.enter_home(&home)?;
                        let result = self.validate_prepared_dml(qualified, &mut parameters);
                        self.leave_home(home);
                        result?;
                        continue;
                    }
                    CrossDatabase::Mixed(others) => {
                        let _others = self.use_others(&others)?;
                        self.validate_prepared_dml(qualified, &mut parameters)?;
                        continue;
                    }
                }
            }
            self.validate_prepared_dml(statement, &mut parameters)?;
        }
        Ok(())
    }
    /// Bind one query or DML statement of `validate_prepared_statements`.
    fn validate_prepared_dml(
        &self,
        mut statement: Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<()> {
        // Lower assignments exactly as execution does, but only bind the
        // resulting query. Preparation must not evaluate or store values.
        if let Some((update, with)) = msduck_sql::output::joined_update(&statement) {
            return self
                .plan_joined_execution(update, with.cloned(), parameters)
                .map(|_| ());
        }
        if let Statement::Query(query) = &mut statement {
            crate::query_catalog::bind_query_with_parameters(&self.db, query, parameters)?;
        }
        self.lower_output(&mut statement, parameters)?;
        let runtime_percentile_plans = percentile_plans(&statement, parameters)?;
        self.bind_dml(&mut statement, parameters)?;
        crate::query_catalog::lower_recursion(&self.db, &mut statement)?;
        let json = crate::for_json::Output::take(&self.db, &mut statement)?;
        self.lower_database_functions(&mut statement)?;
        self.qualify_databases_across(&mut statement)?;
        self.lower_session_functions(&mut statement, parameters)?;
        ext::rewrite(self, &mut statement, parameters)?;
        select_assignments(&mut statement, parameters)?;
        let into = crate::select_into::take(&mut statement)?;
        let money_columns = crate::insert::money_columns(&statement, parameters);
        crate::update::expand_compound(&self.db, &mut statement)?;
        let money_assignments = crate::update::money_assignments(&statement, parameters);
        crate::aggregate_columns::annotate(&self.db, &mut statement, parameters)
            .map_err(anyhow::Error::msg)?;
        crate::query_catalog::bind_unicode_operations(&self.db, &mut statement, parameters)?;
        crate::concat_lower::annotated_unicode_casts(&mut statement);
        crate::for_json::lower_nested(&self.db, &mut statement, parameters)?;
        let mut percentile_parameters = parameters.clone();
        let bindings = runtime_percentile_plans
            .iter()
            .map(|_| {
                (
                    percentile_binding(
                        &mut percentile_parameters,
                        ParameterValue::Double(0.0),
                        SqlType::Float,
                    ),
                    None,
                    false,
                )
            })
            .collect::<Vec<_>>();
        lower_runtime_percentiles(&mut statement, &bindings)?;
        let mut translator = Translator {
            parameters: &percentile_parameters,
            values: Vec::new(),
            parameter_slots: HashMap::new(),
            transactions: self.transactions,
            options_mask: self.options_mask(),
            transaction_doomed: self.transaction_doomed,
            original_login: &self.original_login,
            clock: crate::current_time::now(),
            rowcount: self.rowcount,
            last_error: self.last_error,
            caught_error: self.caught_error.as_ref(),
            spid: self.process.spid(),
        };
        if let ControlFlow::Break(error) = VisitMut::visit(&mut statement, &mut translator) {
            bail!(error);
        }
        let _rand_scopes = rand::lower(
            &mut statement,
            &mut translator.values,
            &self.diagnostics.rand,
            &self.rand,
        )?;
        crate::insert::lower(&self.db, &mut statement, &money_columns)?;
        crate::update::lower(&self.db, &mut statement, &money_assignments)?;
        if let Some(target) = into {
            crate::select_into::definition(
                &self.db,
                &target,
                &statement.to_string(),
                &translator.values,
            )?;
        }
        self.db.prepare(&statement.to_string())?;
        if let Some(json) = json {
            let names = crate::for_json::Output::names(
                &self.db,
                &statement.to_string(),
                &translator.values,
            )?;
            json.plan(&names)?;
        }
        Ok(())
    }
    fn bind_dml(
        &self,
        statement: &mut Statement,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        let dml = matches!(statement, Statement::Update(_) | Statement::Delete(_))
            || matches!(statement, Statement::Query(query) if matches!(query.body.as_ref(), SetExpr::Update(_) | SetExpr::Delete(_)));
        if dml {
            crate::aggregate_columns::annotate(&self.db, statement, parameters)
                .map_err(anyhow::Error::msg)?;
            crate::update::canonicalize(statement)?;
            crate::delete::canonicalize(statement)?;
        }
        Ok(())
    }
    fn validate_prepared_scalar(
        &self,
        expression: Expr,
        data_type: DataType,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        let expression = Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(expression),
            data_type,
            format: None,
        };
        self.validate_prepared_expression(expression, parameters)
            .map(|_| ())
    }
    fn validate_prepared_predicate(
        &self,
        expression: Expr,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<()> {
        let kind = self.validate_prepared_expression(expression, parameters)?;
        ensure!(
            kind == duckdb::core::LogicalTypeId::Boolean,
            "An expression of non-boolean type specified in a context where a condition is expected"
        );
        Ok(())
    }
    fn validate_prepared_expression(
        &self,
        mut expression: Expr,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<duckdb::core::LogicalTypeId> {
        self.lower_database_functions(&mut expression)?;
        self.qualify_databases_across(&mut expression)?;
        // Other databases are entered as execution enters them.
        match self.cross_database(&expression, None)? {
            CrossDatabase::Local => self.validate_prepared_expression_here(expression, parameters),
            CrossDatabase::Home(home, others) => {
                rehome(&mut expression, &home, &self.current_alias()?, &[]);
                let _others = self.use_others(&others)?;
                let home = self.enter_home(&home)?;
                let result = self.validate_prepared_expression_here(expression, parameters);
                self.leave_home(home);
                result
            }
            CrossDatabase::Mixed(others) => {
                let _others = self.use_others(&others)?;
                self.validate_prepared_expression_here(expression, parameters)
            }
        }
    }

    fn validate_prepared_expression_here(
        &self,
        mut expression: Expr,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<duckdb::core::LogicalTypeId> {
        let lowered = self.lower_session_functions_shared(&mut expression, parameters)?;
        let parameters = &*lowered;
        ext::rewrite(self, &mut expression, parameters)?;
        crate::query_catalog::lower_recursion(&self.db, &mut expression)?;
        crate::aggregate_columns::annotate(&self.db, &mut expression, parameters)
            .map_err(anyhow::Error::msg)?;
        crate::query_catalog::bind_unicode_expression(&self.db, &mut expression, parameters)?;
        crate::concat_lower::annotated_unicode_casts(&mut expression);
        crate::for_json::lower_nested(&self.db, &mut expression, parameters)?;
        let mut translator = Translator {
            parameters,
            values: vec![],
            parameter_slots: HashMap::new(),
            transactions: self.transactions,
            options_mask: self.options_mask(),
            transaction_doomed: self.transaction_doomed,
            original_login: &self.original_login,
            clock: crate::current_time::now(),
            rowcount: self.rowcount,
            last_error: self.last_error,
            caught_error: self.caught_error.as_ref(),
            spid: self.process.spid(),
        };
        if let ControlFlow::Break(error) = VisitMut::visit(&mut expression, &mut translator) {
            bail!(error);
        }
        // Bind only: evaluating an initializer here could invoke a volatile
        // function or raise an execution-time error during sp_prepare.
        let _rand_scopes = rand::lower(
            &mut expression,
            &mut translator.values,
            &self.diagnostics.rand,
            &self.rand,
        )?;
        let prepared = self.db.prepare(&format!("SELECT {expression}"))?;
        // Unlike Arrow schema access, this obtains bound logical metadata
        // directly from the prepared statement without executing it.
        Ok(prepared.column_logical_type(0).id())
    }
    /// Opt-in worker entry point for the captured active-read cancellation class.
    /// Other execution classes and live Attention reception remain separate work.
    pub fn batch_response_with_read_cancel(
        &mut self,
        sql: &str,
        parameters: &HashMap<String, Parameter>,
        mode: crate::read_cancellation::Mode,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> crate::read_cancellation::Outcome {
        use crate::read_cancellation::{Mode, Outcome};
        let previous = self.read_cancel.replace(cancel);
        self.read_cancelled = false;
        self.read_cancel_cleanup_failed = false;
        let rpc = match mode {
            Mode::Batch => None,
            Mode::Rpc => Some(RpcExecution::Direct),
            Mode::Prepared => Some(RpcExecution::Prepared),
        };
        let (tokens, success) = self.batch_response_context(sql, parameters, rpc, None);
        self.read_cancel = previous;
        if self.read_cancel_cleanup_failed {
            Outcome::CleanupFailed { tokens }
        } else if self.read_cancelled {
            Outcome::Cancelled {
                tokens,
                attention_ack: msduck_tds::attention_completion::ACK,
            }
        } else {
            Outcome::Finished { tokens, success }
        }
    }

    pub fn batch_response(
        &mut self,
        sql: &str,
        parameters: &HashMap<String, Parameter>,
        rpc: bool,
        handle: Option<(&str, i32)>,
    ) -> (Vec<u8>, bool) {
        self.batch_response_context(sql, parameters, rpc.then_some(RpcExecution::Direct), handle)
    }

    pub fn prepared_batch(
        &mut self,
        sql: &str,
        parameters: &HashMap<String, Parameter>,
    ) -> Vec<u8> {
        self.batch_response_context(sql, parameters, Some(RpcExecution::Prepared), None)
            .0
    }

    fn batch_response_context(
        &mut self,
        sql: &str,
        parameters: &HashMap<String, Parameter>,
        rpc_execution: Option<RpcExecution>,
        handle: Option<(&str, i32)>,
    ) -> (Vec<u8>, bool) {
        let saved_nocount = self.nocount;
        let saved_xact_abort = self.xact_abort;
        let saved_ansi_warnings = self.ansi_warnings;
        let saved_datefirst = self.datefirst;
        let result = self.batch_response_inner(sql, parameters, rpc_execution, handle);
        if rpc_execution.is_some() {
            self.nocount = saved_nocount;
            self.xact_abort = saved_xact_abort;
            self.ansi_warnings = saved_ansi_warnings;
            // Restoring caller state must not execute SQL in an aborted native
            // transaction or replace the request's original diagnostics.
            self.datefirst = saved_datefirst;
        }
        result
    }

    fn set_datefirst(&mut self, first: i32) -> Result<()> {
        let previous = self.datefirst;
        self.datefirst = first;
        if let Err(error) = self.sync_datefirst() {
            self.datefirst = previous;
            return Err(error);
        }
        Ok(())
    }

    fn sync_datefirst(&self) -> Result<()> {
        if self.backend_datefirst.get() != self.datefirst {
            self.db.execute_batch(&format!(
                "SET VARIABLE __msduck_datefirst = {}",
                self.datefirst
            ))?;
            self.backend_datefirst.set(self.datefirst);
        }
        Ok(())
    }

    /// Run one batch: a SQL batch, an RPC request or a nested body such as a
    /// procedure. Features observe its scope through `batch_begin` and
    /// `batch_end`, which also surround batches claimed by a batch hook.
    fn batch_response_inner(
        &mut self,
        sql: &str,
        parameters: &HashMap<String, Parameter>,
        rpc_execution: Option<RpcExecution>,
        handle: Option<(&str, i32)>,
    ) -> (Vec<u8>, bool) {
        ext::batch_begin(self, rpc_execution.is_some());
        let response = self.batch_response_body(sql, parameters, rpc_execution, handle);
        ext::batch_end(self);
        response
    }

    fn batch_response_body(
        &mut self,
        sql: &str,
        parameters: &HashMap<String, Parameter>,
        rpc_execution: Option<RpcExecution>,
        handle: Option<(&str, i32)>,
    ) -> (Vec<u8>, bool) {
        let rpc = rpc_execution.is_some();
        self.caught_error = None;
        if let Some(response) = ext::batch(self, sql, parameters, rpc) {
            return response;
        }
        let mut out = Vec::new();
        let mut had_runtime_error = false;
        let statements = match parse_batch(sql) {
            Ok(s) => s,
            Err(e) => {
                if e.downcast_ref::<SqlError>().is_some() {
                    self.last_error = emit_error(&mut out, &e);
                    let command = if matches!(self.last_error, 156 | 159) {
                        if rpc {
                            out.push(0x79);
                            out.extend(self.last_error.to_le_bytes());
                            224
                        } else {
                            253
                        }
                    } else {
                        0
                    };
                    tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, command, 0);
                    return (out, false);
                }
                let message = e.to_string();
                self.error(
                    &mut out,
                    crate::merge::error_number(&message)
                        .or_else(|| msduck_sql::query_options::error_number(&message))
                        .unwrap_or(102),
                    &message,
                );
                tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                return (out, false);
            }
        };
        let mut variables = match batch_variables(&statements, parameters) {
            Ok(variables) => variables,
            Err(error) => {
                self.last_error = emit_error(&mut out, &error);
                tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                return (out, false);
            }
        };
        let mut pending: Vec<Work<'_>> = statements.iter().rev().map(Work::leaf).collect();
        let mut steps = 0usize;
        let mut last_done = None;
        let mut return_status = 0i32;
        let mut preserve_return_status = false;
        let mut executed_leaf = false;
        while let Some(work) = pending.pop() {
            steps += 1;
            if steps > 10_000 {
                self.error(
                    &mut out,
                    50000,
                    "batch execution exceeds current 10000-step limit",
                );
                tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                return (out, false);
            }
            if let Some(command) = work.completion {
                if let Some(offset) = control_done(&mut out, rpc, self.nocount, command) {
                    last_done = Some(offset);
                }
                continue;
            }
            if let Some(previous) = work.restore_error {
                self.caught_error = previous;
                // END TRY/CATCH resets @@ROWCOUNT even when no exception was
                // raised. An informational aggregate warning is not an error.
                self.rowcount = 0;
                if let Some(offset) = control_done(&mut out, rpc, self.nocount, 351) {
                    last_done = Some(offset);
                }
                continue;
            }
            let statement = work.statement;
            if let Some((body, handler)) = try_catch_parts(statement) {
                if let Some(offset) = control_done(&mut out, rpc, self.nocount, 349) {
                    last_done = Some(offset);
                }
                pending.push(Work {
                    statement,
                    loop_boundary: false,
                    catch_handler: Some(handler),
                    restore_error: Some(self.caught_error.clone()),
                    completion: None,
                });
                pending.extend(body.iter().rev().map(Work::leaf));
                continue;
            }
            if let Some(is_continue) = crate::dialect::loop_control(statement) {
                if let Some(index) = pending.iter().rposition(|work| work.loop_boundary) {
                    self.unwind_work(&mut pending, index + usize::from(is_continue));
                    if let Some(offset) = control_done(&mut out, rpc, self.nocount, 202) {
                        last_done = Some(offset);
                    }
                    continue;
                }
                self.error(&mut out, 50000, "loop control outside WHILE");
                tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                return (out, false);
            }
            if let Statement::Return(return_statement) = statement {
                let value = match &return_statement.value {
                    None => Ok(Value::Int(self.last_error)),
                    Some(ReturnStatementValue::Expr(expression)) => {
                        self.evaluate_scalar(expression.clone(), DataType::Int(None), &variables)
                    }
                };
                match value {
                    Ok(Value::Int(value)) => return_status = value,
                    Ok(Value::Null) => {
                        tds::info(
                            &mut out,
                            282,
                            "A NULL return status is not allowed; returning 0 instead.",
                        );
                    }
                    Ok(_) => unreachable!("RETURN expression is cast to INT"),
                    Err(error) => {
                        if self.catch_error(&mut pending, &error) {
                            continue;
                        }
                        self.last_error = emit_error(&mut out, &error);
                        self.rollback_doomed(&mut out);
                        tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                        return (out, false);
                    }
                }
                self.rowcount = 1;
                self.last_error = 0;
                if let Some(offset) = control_done(&mut out, rpc, self.nocount, 219) {
                    last_done = Some(offset);
                }
                break;
            }
            let branch: Result<Option<&[Statement]>> = match statement {
                Statement::While(while_statement) => {
                    let expression = while_statement
                        .while_block
                        .condition
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("missing WHILE condition"));
                    expression
                        .and_then(|expression| {
                            self.evaluate_expression(expression, &variables, true)
                        })
                        .map(|value| {
                            if matches!(value, Value::Boolean(true)) {
                                pending.push(Work {
                                    statement,
                                    loop_boundary: true,
                                    catch_handler: None,
                                    restore_error: None,
                                    completion: None,
                                });
                                Some(while_statement.while_block.statements().as_slice())
                            } else {
                                Some(&[][..])
                            }
                        })
                }
                Statement::If(condition) => {
                    let expression = condition
                        .if_block
                        .condition
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("missing IF condition"));
                    expression
                        .and_then(|expression| {
                            self.evaluate_expression(expression, &variables, true)
                        })
                        .map(|value| {
                            if matches!(value, Value::Boolean(true)) {
                                Some(condition.if_block.statements().as_slice())
                            } else {
                                Some(
                                    condition
                                        .else_block
                                        .as_ref()
                                        .map(|block| block.statements().as_slice())
                                        .unwrap_or(&[]),
                                )
                            }
                        })
                }
                Statement::StartTransaction {
                    has_end_keyword: true,
                    statements,
                    exception,
                    modifier,
                    ..
                } => {
                    if exception.is_some() || modifier.is_some() {
                        Err(anyhow::anyhow!("unsupported exception block"))
                    } else {
                        Ok(Some(statements.as_slice()))
                    }
                }
                _ => Ok(None),
            };
            match branch {
                Ok(Some(statements)) => {
                    // SQL Server completes each condition evaluation, even
                    // when its branch is not selected. BEGIN/END is only a
                    // grouping construct and contributes no completion token.
                    if matches!(statement, Statement::If(_) | Statement::While(_))
                        && !(rpc && self.nocount)
                    {
                        last_done = Some(out.len());
                        tds::done(
                            &mut out,
                            if rpc { 0xff } else { 0xfd },
                            u16::from(!statements.is_empty() || !pending.is_empty() || rpc),
                            0xc0,
                            0,
                        );
                    }
                    pending.extend(statements.iter().rev().map(Work::leaf));
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    if self.catch_error(&mut pending, &error) {
                        if matches!(statement, Statement::If(_) | Statement::While(_))
                            && let Some(offset) = control_done(&mut out, rpc, self.nocount, 0xc0)
                        {
                            last_done = Some(offset);
                        }
                        continue;
                    }
                    self.last_error = emit_error(&mut out, &error);
                    self.rollback_doomed(&mut out);
                    tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                    return (out, false);
                }
            }
            let more = u16::from(!pending.is_empty() || rpc);
            if let Some(call) = msduck_sql::raiserror::call(statement) {
                let raised = match crate::raiserror::bind(&call, &variables) {
                    Ok(raised) => raised,
                    Err(error) => {
                        if self.catch_error(&mut pending, &error) {
                            continue;
                        }
                        self.last_error = emit_error(&mut out, &error);
                        self.rollback_doomed(&mut out);
                        tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                        return (out, false);
                    }
                };
                if out.len().saturating_add(4200) > tds::MAX_MESSAGE - 1024 {
                    self.error(
                        &mut out,
                        50000,
                        "batch result exceeds current 16 MiB response limit",
                    );
                    tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                    return (out, false);
                }
                executed_leaf = true;
                self.rowcount = 0;
                let is_error = raised.delivery == msduck_core::raiserror::Delivery::Error;
                if is_error
                    && pending.iter().any(|work| work.catch_handler.is_some())
                    && self.catch_error(&mut pending, &raised.diagnostic.clone().into())
                {
                    if let Some(offset) = control_done(&mut out, rpc, self.nocount, 246) {
                        last_done = Some(offset);
                    }
                    continue;
                }
                preserve_return_status = false;
                self.last_error = raised.error_number;
                if is_error {
                    had_runtime_error = true;
                    return_status = raised.error_number;
                    tds::sql_error(&mut out, &raised.diagnostic);
                } else {
                    let units = raised
                        .diagnostic
                        .message_utf16
                        .clone()
                        .unwrap_or_else(|| raised.diagnostic.message.encode_utf16().collect());
                    tds::diagnostic_utf16(
                        &mut out,
                        tds::DiagnosticKind::Information,
                        raised.diagnostic.severity,
                        raised.diagnostic.state,
                        raised.diagnostic.number,
                        &units,
                    );
                    return_status = 0;
                }
                if !msduck_core::completion::visible(
                    rpc,
                    self.nocount,
                    if is_error {
                        msduck_core::completion::Kind::Error
                    } else {
                        msduck_core::completion::Kind::Statement
                    },
                ) {
                    continue;
                }
                last_done = Some(out.len());
                tds::done(
                    &mut out,
                    if rpc { 0xff } else { 0xfd },
                    more | if is_error { 2 } else { 0 },
                    246,
                    0,
                );
                continue;
            }
            let procedure_call = match self.set_session_context(statement, &variables) {
                Some(result) => Some(result.map(|()| ext::Exec::status(0))),
                None => ext::exec(self, statement, &mut variables),
            };
            if let Some(result) = procedure_call {
                executed_leaf = true;
                // EXEC keeps @@ROWCOUNT. A batch reports RETURNSTATUS (0, or 1
                // after a failure) and DONEPROC; inside sp_executesql the call
                // ends with DONEINPROC and a failure becomes the RPC status.
                let mut failed = false;
                let status: i32 = match result {
                    Ok(ext::Exec { tokens, status }) => {
                        out.extend(tokens);
                        self.last_error = 0;
                        status
                    }
                    Err(error) => {
                        failed = true;
                        let error = ext::take_partial(error, &mut out);
                        if self.catch_error(&mut pending, &error) {
                            // A caught failure's DONEPROC has no RETURNSTATUS.
                            if !(rpc && self.nocount) {
                                last_done = Some(out.len());
                                tds::done(&mut out, if rpc { 0xff } else { 0xfe }, more, 224, 0);
                            }
                            continue;
                        }
                        self.last_error = emit_error(&mut out, &error);
                        if error.downcast_ref::<SqlError>().is_none() {
                            // Unsupported forms stop the batch explicitly.
                            tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                            return (out, false);
                        }
                        preserve_return_status = false;
                        had_runtime_error = true;
                        return_status = self.last_error;
                        1
                    }
                };
                if rpc {
                    if !self.nocount {
                        last_done = Some(out.len());
                        tds::done(&mut out, 0xff, more | if failed { 2 } else { 0 }, 224, 0);
                    }
                } else {
                    out.push(0x79);
                    out.extend(status.to_le_bytes());
                    last_done = Some(out.len());
                    tds::done(&mut out, 0xfe, more | if failed { 2 } else { 0 }, 224, 0);
                }
                continue;
            }
            match self
                .execute(statement.clone(), &mut variables)
                .map_err(|error| self.single_database_writes(error))
            {
                Ok(Execution {
                    tokens,
                    count,
                    command,
                    kind,
                }) => {
                    executed_leaf = true;
                    if out.len().saturating_add(tokens.len()).saturating_add(13)
                        > tds::MAX_MESSAGE - 1024
                    {
                        self.error(
                            &mut out,
                            50000,
                            "batch result exceeds current 16 MiB response limit",
                        );
                        tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                        return (out, false);
                    }
                    if !rpc
                        && matches!(statement, Statement::Set(_))
                        && statement
                            .to_string()
                            .eq_ignore_ascii_case("SET LANGUAGE US_ENGLISH")
                    {
                        tds::diagnostic_utf16(
                            &mut out,
                            tds::DiagnosticKind::Information,
                            0,
                            1,
                            5703,
                            &"Changed language setting to us_english."
                                .encode_utf16()
                                .collect::<Vec<_>>(),
                        );
                    }
                    out.extend(tokens);
                    self.last_error = 0;
                    if had_runtime_error && !preserve_return_status {
                        return_status = 0;
                    }
                    // A declaration without an initializer changes neither the
                    // affected-row state nor the intermediate completion stream.
                    // The batch epilogue still supplies a final DONE if needed.
                    if matches!(statement, Statement::Declare { stmts }
                        if stmts.iter().all(|declaration| declaration.assignment.is_none()))
                    {
                        continue;
                    }
                    self.rowcount = count.unwrap_or(0);
                    let visible = count.filter(|_| !self.nocount);
                    if (rpc && ddl_completion_command(statement) == Some(253))
                        || !msduck_core::completion::visible(rpc, self.nocount, kind)
                    {
                        continue;
                    }
                    last_done = Some(out.len());
                    tds::done(
                        &mut out,
                        if rpc { 0xff } else { 0xfd },
                        more | if visible.is_some() { 16 } else { 0 },
                        command,
                        visible.unwrap_or(0),
                    );
                }
                Err(e) => {
                    let e = ext::take_partial(e, &mut out);
                    if e.downcast_ref::<crate::read_cancellation::UnusableRead>()
                        .is_some()
                    {
                        self.read_cancel_cleanup_failed = true;
                        self.last_error = emit_error(&mut out, &e);
                        return (out, false); // no CATCH, later statements, rollback SQL, or ACK
                    }
                    if let Some(cancelled) =
                        e.downcast_ref::<crate::read_cancellation::CancelledRead>()
                    {
                        let plan = msduck_tds::attention_completion::ActiveRead::new(
                            rpc,
                            self.transactions > 0,
                            self.xact_abort,
                            pending.iter().any(|work| work.catch_handler.is_some()),
                        );
                        if out
                            .len()
                            .saturating_add(cancelled.metadata.len())
                            .saturating_add(64)
                            > tds::MAX_MESSAGE
                        {
                            self.read_cancel_cleanup_failed = true;
                            return (out, false);
                        }
                        out.extend_from_slice(&cancelled.metadata);
                        plan.before_rollback(&mut out);
                        if plan.rollback {
                            match self.rollback_transaction("") {
                                Ok(rollback) => out.extend(rollback),
                                Err(error) => {
                                    self.read_cancel_cleanup_failed = true;
                                    self.last_error = emit_error(&mut out, &error);
                                    return (out, false);
                                }
                            }
                        }
                        plan.finish(&mut out);
                        self.rowcount = 0;
                        self.read_cancelled = true;
                        return (out, false); // bypass CATCH and every later statement
                    }
                    // Boundary events are speculative until the first leaf
                    // binds. A compilation failure must precede those events.
                    let failed_binding = binding_failure(&e);
                    if !executed_leaf && failed_binding {
                        out.clear();
                    }
                    executed_leaf |= !failed_binding;
                    if let Some(failed) = e.downcast_ref::<crate::query_error::FailedQuery>() {
                        if out.len().saturating_add(failed.metadata.len()) > tds::MAX_MESSAGE - 1024
                        {
                            self.error(
                                &mut out,
                                50000,
                                "batch result exceeds current 16 MiB response limit",
                            );
                            tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                            return (out, false);
                        }
                        out.extend_from_slice(&failed.metadata);
                        if matches!(failed.command, 0xc1 | 0xc3..=0xc5) {
                            self.rowcount = 0;
                        }
                    }
                    if self.catch_error(&mut pending, &e) {
                        if let Some(failed) = e.downcast_ref::<crate::output_sink::Failed>() {
                            self.rowcount = 0;
                            if msduck_core::completion::visible(
                                rpc,
                                self.nocount,
                                msduck_core::completion::Kind::Statement,
                            ) {
                                let command = match failed.operation {
                                    msduck_sql::output::Operation::Insert => 0xc3,
                                    msduck_sql::output::Operation::Update => 0xc5,
                                    msduck_sql::output::Operation::Delete => 0xc4,
                                };
                                last_done = Some(out.len());
                                tds::done(
                                    &mut out,
                                    if rpc { 0xff } else { 0xfd },
                                    1 | if self.nocount { 0 } else { 16 },
                                    command,
                                    0,
                                );
                            }
                        }
                        if matches!(statement, Statement::Commit { .. })
                            && let Some(offset) = control_done(&mut out, rpc, self.nocount, 0)
                        {
                            last_done = Some(offset);
                        }
                        if matches!(statement, Statement::Throw(_))
                            && let Some(offset) = control_done(&mut out, rpc, self.nocount, 246)
                        {
                            last_done = Some(offset);
                        }
                        if let Some(failed) = e.downcast_ref::<crate::query_error::FailedQuery>() {
                            tds::done(
                                &mut out,
                                if rpc { 0xff } else { 0xfd },
                                1 | if self.nocount { 0 } else { 16 },
                                failed.command,
                                0,
                            );
                        }
                        continue;
                    }
                    preserve_return_status = false;
                    self.last_error = emit_error(&mut out, &e);
                    if self.last_error == 3930 && matches!(statement, Statement::Commit { .. }) {
                        // Captured GUID failure: an uncaught COMMIT rejection
                        // ends COMMIT, while subsequent reads still see prior
                        // writes until the doomed-transaction batch epilogue.
                        had_runtime_error = true;
                        return_status = self.last_error;
                        self.rowcount = 0;
                        last_done = Some(out.len());
                        tds::done(&mut out, if rpc { 0xff } else { 0xfd }, 2 | more, 213, 0);
                        continue;
                    }
                    if self.last_error == 3902
                        && !rpc
                        && matches!(statement, Statement::Commit { .. })
                    {
                        tds::done(&mut out, 0xfd, 2, 213, 0);
                        return (out, false);
                    }
                    if self.last_error == 8169 {
                        self.rowcount = 0;
                        if matches!(statement, Statement::AlterTable(_)) {
                            tds::diagnostic_utf16(
                                &mut out,
                                tds::DiagnosticKind::Information,
                                0,
                                0,
                                3621,
                                &"The statement has been terminated."
                                    .encode_utf16()
                                    .collect::<Vec<_>>(),
                            );
                        }
                        self.rollback_doomed(&mut out);
                        tds::done(
                            &mut out,
                            if rpc { 0xfe } else { 0xfd },
                            2,
                            if rpc { 224 } else { 253 },
                            0,
                        );
                        return (out, false);
                    }
                    if self.transaction_doomed {
                        self.rollback_doomed(&mut out);
                        tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 0, 0);
                        return (out, false);
                    }
                    if self.last_error == 2742 && matches!(statement, Statement::Set(_)) {
                        // Invalid DATEFIRST ends only SET. SQL Server preserves
                        // its RPC failure status even when later queries succeed.
                        had_runtime_error = true;
                        preserve_return_status =
                            matches!(rpc_execution, Some(RpcExecution::Prepared));
                        return_status = -6;
                        self.rowcount = 0;
                        last_done = Some(out.len());
                        tds::done(&mut out, if rpc { 0xff } else { 0xfd }, 2 | more, 0, 0);
                        continue;
                    }
                    let failed_dml =
                        e.downcast_ref::<crate::query_error::FailedQuery>()
                            .map(|failed| failed.command)
                            .filter(|command| matches!(command, 0xc3..=0xc5))
                            .or_else(|| {
                                e.downcast_ref::<crate::output_sink::Failed>()
                                    .map(|failed| match failed.operation {
                                        msduck_sql::output::Operation::Insert => 0xc3,
                                        msduck_sql::output::Operation::Update => 0xc5,
                                        msduck_sql::output::Operation::Delete => 0xc4,
                                    })
                            });
                    if let Some(command) = failed_dml {
                        tds::diagnostic_utf16(
                            &mut out,
                            tds::DiagnosticKind::Information,
                            0,
                            0,
                            3621,
                            &"The statement has been terminated."
                                .encode_utf16()
                                .collect::<Vec<_>>(),
                        );
                        had_runtime_error = true;
                        return_status = self.last_error;
                        self.rowcount = 0;
                        last_done = Some(out.len());
                        tds::done(
                            &mut out,
                            if rpc { 0xff } else { 0xfd },
                            2 | more,
                            command,
                            0,
                        );
                        continue;
                    }
                    if let Some(failed) = e.downcast_ref::<crate::query_error::FailedQuery>()
                        && matches!(self.last_error, 8115 | 8134)
                    {
                        // Default SQL Server arithmetic errors end this query,
                        // not the batch. Keep the failure visible even if a later
                        // statement succeeds and resets @@ERROR.
                        had_runtime_error = true;
                        return_status = self.last_error;
                        self.rowcount = 0;
                        last_done = Some(out.len());
                        tds::done(
                            &mut out,
                            if rpc { 0xff } else { 0xfd },
                            2 | more,
                            failed.command,
                            0,
                        );
                        continue;
                    }
                    if rpc && msduck_sql::drop_index_syntax::request(statement).is_some() {
                        self.rowcount = 0;
                        if self.last_error == 3701 {
                            // A missing target ends this statement, while later
                            // RPC statements still run, even with NOCOUNT ON.
                            had_runtime_error = true;
                            return_status = self.last_error;
                            last_done = Some(out.len());
                            tds::done(&mut out, 0xff, 3, 201, 0);
                            continue;
                        }
                        if self.last_error == 3748 {
                            tds::done(&mut out, 0xfe, 2, 224, 0);
                            return (out, false);
                        }
                    }
                    let command =
                        if !rpc && msduck_sql::drop_index_syntax::request(statement).is_some() {
                            self.rowcount = 0;
                            if self.last_error == 3748 { 253 } else { 201 }
                        } else if !rpc
                            && msduck_sql::dialect::alter_database::request(statement).is_some()
                        {
                            // Captured: option refusals end as CurCmd 253,
                            // the other ALTER DATABASE failures as 215.
                            if matches!(self.last_error, 5058 | 12104) {
                                253
                            } else {
                                215
                            }
                        } else if !rpc
                            && matches!(statement, Statement::CreateIndex(_))
                            && self.last_error == 1913
                        {
                            self.rowcount = 0;
                            253
                        } else if !rpc
                            && e.downcast_ref::<crate::query_error::CompilationFailure>()
                                .is_some()
                        {
                            0xfd
                        } else {
                            0
                        };
                    tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, command, 0);
                    return (out, false);
                }
            }
        }
        if self.transaction_doomed {
            if let Some(offset) = last_done {
                out[offset + 1] |= 1;
            }
            self.error(&mut out, 3998, "Uncommittable transaction is detected at the end of the batch. The transaction is rolled back.");
            self.rollback_doomed(&mut out);
            tds::done(&mut out, if rpc { 0xfe } else { 0xfd }, 2, 253, 0);
            self.caught_error = None;
            return (out, false);
        }
        if rpc {
            if let Some((name, value)) = handle.filter(|_| !had_runtime_error) {
                tds::return_handle(&mut out, name, value);
            }
            out.push(0x79);
            out.extend(return_status.to_le_bytes());
            tds::done(&mut out, 0xfe, 0, 0xe0, 0);
        } else if let Some(offset) = last_done {
            // A trailing unselected IF may leave the previous leaf marked MORE.
            out[offset + 1] &= !1;
        } else {
            tds::done(&mut out, 0xfd, 0, 0, 0);
        }
        self.caught_error = None;
        (out, !had_runtime_error)
    }
    fn unwind_work(&mut self, pending: &mut Vec<Work<'_>>, length: usize) {
        while pending.len() > length {
            if let Some(previous) = pending.pop().and_then(|work| work.restore_error) {
                self.caught_error = previous;
            }
        }
    }
    fn catch_error<'a>(&mut self, pending: &mut Vec<Work<'a>>, error: &anyhow::Error) -> bool {
        let message = error.to_string();
        let caught = error
            .downcast_ref::<SqlError>()
            .cloned()
            .or_else(|| {
                error
                    .downcast_ref::<StatementErrors>()
                    .and_then(|errors| errors.0.last().cloned())
            })
            .unwrap_or_else(|| sql_error_from_message(&message));
        // Same-level binding/compilation failures are not runtime catch targets.
        if binding_failure(error) {
            return false;
        }
        // Retain the transaction for CATCH reads and explicit ROLLBACK. The
        // pinned 17.0.4065.4 reference also dooms caught RAISERROR 11/16;
        // informational severity 10 never enters this path.
        if self.transactions > 0
            && ((self.xact_abort && caught.severity >= 11) || caught.number == 8169)
        {
            self.transaction_doomed = true;
        }
        let Some(index) = pending
            .iter()
            .rposition(|work| work.catch_handler.is_some())
        else {
            return false;
        };
        self.unwind_work(pending, index + 1);
        let marker = pending.pop().expect("catch marker exists");
        let handler = marker.catch_handler.expect("catch handler exists");
        self.last_error = caught.number;
        self.caught_error = Some(caught);
        let statement = marker.statement;
        pending.push(Work {
            catch_handler: None,
            ..marker
        });
        pending.extend(handler.iter().rev().map(Work::leaf));
        pending.push(Work {
            completion: Some(350),
            ..Work::leaf(statement)
        });
        true
    }
    fn error(&mut self, out: &mut Vec<u8>, number: i32, message: &str) {
        self.last_error = number;
        if let Some(error) = crate::json_extract::diagnostic(message)
            && number == error.number
        {
            tds::sql_error(out, &error);
        } else {
            tds::error(out, number, message);
        }
    }
    fn validate_isolation(isolation: u8) -> Result<()> {
        if let Some(result) = ext::isolation(isolation) {
            return result;
        }
        // DuckDB currently supplies snapshot isolation. Other locking modes
        // require further emulation; do not silently accept SERIALIZABLE.
        ensure!(
            matches!(isolation, 0 | 2 | 5),
            "unsupported transaction isolation level {isolation}"
        );
        Ok(())
    }
    pub fn begin_transaction(&mut self, isolation: u8, name: &str) -> Result<Vec<u8>> {
        Self::validate_isolation(isolation)?;
        ensure!(self.transactions < u32::MAX, "transaction nesting overflow");
        let mut out = Vec::new();
        if self.transactions == 0 {
            self.db.execute_batch("BEGIN TRANSACTION")?;
            self.transaction_descriptor = NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed);
            self.transaction_name = name.to_owned();
            tds::transaction_env(&mut out, 8, self.transaction_descriptor);
        }
        self.transactions += 1;
        ext::transaction_begin(self, isolation);
        Ok(out)
    }
    fn rollback_doomed(&mut self, out: &mut Vec<u8>) {
        if self.transaction_doomed {
            match self.rollback_transaction("") {
                Ok(tokens) => out.extend(tokens),
                Err(error) => {
                    emit_error(out, &error);
                }
            }
        }
    }
    fn require_committable(&self) -> Result<()> {
        if self.transaction_doomed {
            return Err(SqlError::new(3930, 1, "The current transaction cannot be committed and cannot support operations that write to the log file. Roll back the transaction.").into());
        }
        Ok(())
    }
    pub fn commit_transaction(&mut self) -> Result<Vec<u8>> {
        self.require_committable()?;
        if self.transactions == 0 {
            return Err(SqlError::new(
                3902,
                1,
                "The COMMIT TRANSACTION request has no corresponding BEGIN TRANSACTION.",
            )
            .into());
        }
        let mut out = Vec::new();
        if self.transactions == 1 {
            self.db.execute_batch("COMMIT")?;
            tds::transaction_env(&mut out, 9, self.transaction_descriptor);
            self.transaction_descriptor = 0;
            self.transaction_name.clear();
        }
        self.transactions -= 1;
        if self.transactions == 0 {
            ext::transaction_end(self, true);
        }
        self.sync_datefirst()?;
        Ok(out)
    }
    pub fn rollback_transaction(&mut self, name: &str) -> Result<Vec<u8>> {
        ensure!(
            self.transactions > 0,
            "ROLLBACK has no corresponding BEGIN TRANSACTION"
        );
        // A savepoint wins over an outer transaction of the same name.
        if !name.is_empty()
            && let Some(result) = ext::rollback_to(self, name)
        {
            return result;
        }
        ensure!(
            name.is_empty() || name == self.transaction_name,
            "Cannot roll back {name}. No transaction or savepoint of that name was found."
        );
        self.db.execute_batch("ROLLBACK")?;
        self.restore_catalog();
        let mut out = Vec::new();
        tds::transaction_env(&mut out, 10, self.transaction_descriptor);
        self.transactions = 0;
        self.transaction_doomed = false;
        self.transaction_descriptor = 0;
        self.transaction_name.clear();
        ext::transaction_end(self, false);
        self.sync_datefirst()?;
        Ok(out)
    }
    pub fn transaction_request(&mut self, request: tds::TransactionRequest) -> Result<Vec<u8>> {
        use tds::TransactionRequest;
        // Validate the restart before changing the existing transaction.
        match &request {
            TransactionRequest::Commit {
                restart: Some(begin),
            }
            | TransactionRequest::Rollback {
                restart: Some(begin),
                ..
            } => Self::validate_isolation(begin.isolation)?,
            _ => {}
        }
        let (mut out, restart) = match request {
            TransactionRequest::Begin(begin) => {
                (self.begin_transaction(begin.isolation, &begin.name)?, None)
            }
            TransactionRequest::Commit { restart } => (self.commit_transaction()?, restart),
            TransactionRequest::Rollback { name, restart } => {
                (self.rollback_transaction(&name)?, restart)
            }
            TransactionRequest::Save { name } => match ext::save_transaction(self, &name) {
                Some(result) => (result?, None),
                None => bail!("unsupported savepoint: DuckDB has no native savepoint support"),
            },
        };
        if let Some(begin) = restart {
            out.extend(self.begin_transaction(begin.isolation, &begin.name)?);
        }
        tds::done(&mut out, 0xfd, 0, 0, 0);
        Ok(out)
    }
    fn lower_output(
        &self,
        statement: &mut Statement,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<Option<msduck_sql::output::NativePlan>> {
        let plan = msduck_sql::output::lower_native(statement)?;
        if let Some(plan) = &plan {
            let mut bound = Statement::Query(plan.projection.clone());
            crate::aggregate_columns::annotate(&self.db, &mut bound, parameters)
                .map_err(anyhow::Error::msg)?;
            let Statement::Query(query) = bound else {
                unreachable!()
            };
            let SetExpr::Select(select) = query.body.as_ref() else {
                bail!("unsupported OUTPUT projection shape");
            };
            plan.bind_projection(statement, &select.projection)?;
            if plan.requires_materialization() || plan.paired {
                // Bind the original RETURNING contract with typed NULLs in
                // place of OUTPUT parameters. This retains native rejection
                // of aggregates/windows without evaluating any user values.
                let mut items = select.projection.clone();
                struct NullParameters<'a>(&'a HashMap<String, Parameter>);
                impl VisitorMut for NullParameters<'_> {
                    type Break = ();
                    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
                        if let Expr::Identifier(id) = expr
                            && let Some(parameter) = self.0.get(&id.value.to_lowercase())
                        {
                            *expr = Expr::Cast {
                                kind: CastKind::Cast,
                                expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
                                data_type: parameter.ast_type(),
                                format: None,
                            };
                        }
                        ControlFlow::Continue(())
                    }
                }
                let _ = VisitMut::visit(&mut items, &mut NullParameters(parameters));
                let mut candidate = statement.clone();
                plan.bind_projection(&mut candidate, &items)?;
                let declarations = parameters
                    .iter()
                    .map(|(name, parameter)| (name.clone(), parameter.data_type))
                    .collect::<Vec<_>>();
                self.validate_prepared_statements(vec![candidate], &declarations)?;
                plan.bind_projection(statement, &[SelectItem::Wildcard(Default::default())])?;
            }
            if let Some(sink) = &plan.sink {
                let fields = crate::query_catalog::projection_with_parameters(
                    &self.db,
                    &plan.projection,
                    parameters,
                )?
                .ok_or_else(|| anyhow::anyhow!("OUTPUT destination requires a known projection"))?;
                let sink = crate::output_sink::bind(&self.db, sink, &fields)?;
                self.validate_prepared_statements(vec![sink.statement], &sink.declarations)?;
            }
        }
        Ok(plan)
    }

    /// Route a statement that names another database (see `cross_database`)
    /// and run it through `execute` again. Statements nest deeply through
    /// triggers and procedures, so the copies stay out of `execute`'s frame.
    #[inline(never)]
    fn execute_routed(
        &mut self,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Execution> {
        let statement = Box::new(std::mem::replace(
            statement,
            Statement::Commit {
                chain: false,
                end: false,
                modifier: None,
            },
        ));
        let result = match self.route_databases(statement)? {
            Routed::Here(statement) => {
                self.routed.set(true);
                self.execute(*statement, parameters)
            }
            Routed::Home(statement, home, others) => {
                let _others = self.use_others(&others)?;
                self.execute_in_home(*statement, &home, parameters)
            }
            Routed::Mixed(statement, others) => {
                let _others = self.use_others(&others)?;
                self.routed.set(true);
                self.execute(*statement, parameters)
            }
        };
        result.map_err(|error| self.single_database_writes(error))
    }

    /// Decide where a query or DML statement that names another database
    /// runs (see `cross_database`). Statements nest deeply through triggers
    /// and procedures, so the copies stay out of `execute`'s frame.
    #[inline(never)]
    fn route_databases(&self, statement: Box<Statement>) -> Result<Routed> {
        // An unknown database may be one a feature creates on first use
        // (msdb); the feature hooks see such statements first.
        let mut qualified = statement.clone();
        if self.qualify_databases_across(&mut *qualified).is_err() {
            return Ok(Routed::Here(statement));
        }
        Ok(match self.cross_database(&*qualified, Some(&*qualified))? {
            CrossDatabase::Local => Routed::Here(statement),
            // msdb-only statements are routed by the backup feature.
            CrossDatabase::Home(home, _)
                if self
                    .database
                    .catalog()
                    .display_name(&home)
                    .eq_ignore_ascii_case(crate::database_catalog::MSDB) =>
            {
                Routed::Here(statement)
            }
            CrossDatabase::Home(home, others) => {
                // DB_NAME() and DB_ID() keep naming the session's database;
                // features see the other one as current.
                self.lower_database_functions(&mut *qualified)?;
                let targets = alias_targets(&qualified);
                rehome(&mut *qualified, &home, &self.current_alias()?, &targets);
                Routed::Home(qualified, home, others)
            }
            CrossDatabase::Mixed(others) => Routed::Mixed(qualified, others),
        })
    }

    /// Run a statement whose relations all belong to `home`, another
    /// database, as if the session used that database.
    #[inline(never)]
    fn execute_in_home(
        &mut self,
        statement: Statement,
        home: &str,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Execution> {
        let mut home = self.enter_home(home)?;
        std::mem::swap(&mut self.database, &mut home.guard);
        self.routed.set(true);
        let result = self.execute(statement, parameters);
        std::mem::swap(&mut self.database, &mut home.guard);
        self.leave_home(home);
        result
    }

    fn execute(
        &mut self,
        mut statement: Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Execution> {
        self.restore_catalog();
        if !self.routed.replace(false) && names_other_databases(&statement) {
            return self.execute_routed(&mut statement, parameters);
        }
        if let Some(execution) = ext::statement(self, &mut statement, parameters)? {
            return Ok(execution);
        }
        // Resolve database names before any DDL dispatch or catalog
        // bookkeeping, which see only two-part names.
        self.lower_database_functions(&mut statement)?;
        self.qualify_statement(&mut statement)?;
        self.lower_session_functions(&mut statement, parameters)?;
        ext::rewrite(self, &mut statement, parameters)?;
        if let Statement::CreateTable(table) = &mut statement {
            crate::computed_columns::plan(&self.db, table)?;
        }
        crate::computed_columns::check_writes(&self.db, &statement)?;
        if !matches!(
            statement,
            Statement::Rollback { .. }
                | Statement::Commit { .. }
                | Statement::StartTransaction { .. }
        ) {
            self.sync_datefirst()?;
        }
        if let Some(request) = msduck_sql::drop_index_syntax::request(&statement) {
            self.require_committable()?;
            return self.execute_drop_index(request);
        }
        if self.transaction_doomed {
            let command = match &statement {
                Statement::Insert(_) => Some(0xc3),
                Statement::Update(_) => Some(0xc5),
                Statement::Delete(_) => Some(0xc4),
                _ => None,
            };
            if let Some(command) = command {
                return Err(crate::query_error::attach_context(
                    self.require_committable().unwrap_err(),
                    vec![],
                    command,
                ));
            }
            if crate::object_catalog::is_ddl(&statement) {
                self.require_committable()?;
            }
        }
        crate::query_catalog::validate_cte_columns(&self.db, &statement)?;
        let catalog_ddl = crate::object_catalog::is_ddl(&statement);
        let own_transaction =
            (catalog_ddl || msduck_sql::output::has(&statement)) && self.transactions == 0;
        if own_transaction {
            self.db.execute_batch("BEGIN TRANSACTION")?;
        }
        let result = (|| {
            let view_columns = crate::query_catalog::view(&self.db, &statement)?;
            let mut result = if let Statement::CreateIndex(index) = &statement {
                let object_id: Option<i32> = self.db.query_row(
                    "SELECT __msduck_object_id(?,'U')",
                    [index.table_name.to_string()],
                    |r| r.get(0),
                )?;
                let object_id = object_id
                    .ok_or_else(|| anyhow::anyhow!("CREATE INDEX target table does not exist"))?;
                crate::index_catalog::create(
                    &self.db,
                    object_id,
                    index,
                    crate::index_catalog::Transaction::CallerOwned,
                )?;
                Execution::statement(vec![], None, 200)
            } else {
                self.execute_inner(
                    statement.clone(),
                    parameters,
                    self.transactions == 0 && !own_transaction,
                )?
            };
            if let Some(command) = ddl_completion_command(&statement) {
                result.count = None;
                result.command = command;
            }
            if catalog_ddl {
                crate::object_catalog::sync(&self.db)?;
                crate::query_catalog::sync(&self.db)?;
                crate::index_catalog::sync(&self.db)?;
                crate::object_catalog::touch(&self.db, &statement)?;
                crate::declared_columns::record(&self.db, &statement)?;
                crate::computed_columns::record(&self.db, &statement)?;
                crate::computed_columns::prune(&self.db)?;
                crate::object_catalog::sync_lob(&self.db)?;
                if let Some((name, fields)) = view_columns {
                    crate::query_catalog::record(&self.db, &name, &fields)?;
                    crate::query_catalog::record_view_definition(&self.db, &statement)?;
                }
            }
            if own_transaction {
                self.db.execute_batch("COMMIT")?;
            }
            Ok(result)
        })();
        if result.is_err() && own_transaction {
            let _ = self.db.execute_batch("ROLLBACK");
        }
        // Statements of procedure and trigger bodies reach DuckDB here
        // without the batch loop, so writes to a second database are
        // reported and doom the transaction here too.
        result.map_err(|error| self.single_database_writes(error))
    }
    fn execute_drop_index(
        &mut self,
        request: msduck_sql::drop_index::Request,
    ) -> Result<Execution> {
        use msduck_sql::drop_index as bind;
        let managed = crate::index_catalog::acquire_complete(&self.db)?;
        let tables = self.db.prepare("SELECT o.object_id,s.name,o.name FROM sys.objects o JOIN sys.schemas s USING(schema_id) WHERE rtrim(o.type)='U'")?
            .query_map([], |r| Ok(bind::Table { id:r.get::<_,i32>(0)? as u64, schema:r.get(1)?, name:r.get(2)? }))?
            .collect::<duckdb::Result<Vec<_>>>()?;
        let indexes = managed
            .iter()
            .map(|i| bind::Index {
                table_id: i.object_id as u64,
                id: i.index_id as u64,
                name: i.name.clone(),
                clustered: false,
                constraint_backed: false,
                backend_name: ObjectName::from(vec![
                    Ident::with_quote('"', &i.backend_schema),
                    Ident::with_quote('"', &i.backend_name),
                ]),
            })
            .collect::<Vec<_>>();
        let plan = bind::bind(
            &request,
            &tables,
            &indexes,
            &["dbo"],
            str::eq_ignore_ascii_case,
        )
        .map_err(|error| match error {
            bind::Error::Unsupported(message) => anyhow::anyhow!(message),
            bind::Error::Sql(d) => SqlError::from_utf16(
                d.number,
                d.state,
                d.class,
                d.message.encode_utf16().collect(),
            )
            .into(),
        })?;
        for drop in plan.drops {
            let index = managed
                .iter()
                .find(|i| i.object_id as u64 == drop.table_id && i.index_id as u64 == drop.index_id)
                .ok_or_else(|| {
                    anyhow::anyhow!("Bound index identity is absent from its catalog snapshot")
                })?;
            let owner = if self.transactions == 0 {
                crate::index_catalog::Transaction::Owned
            } else {
                crate::index_catalog::Transaction::CallerOwned
            };
            crate::index_catalog::drop_index(&self.db, index, owner)?;
        }
        if let Some(d) = plan.terminal {
            return Err(SqlError::from_utf16(
                d.number,
                d.state,
                d.class,
                d.message.encode_utf16().collect(),
            )
            .into());
        }
        Ok(Execution::statement(vec![], None, 201))
    }

    fn execute_inner(
        &mut self,
        statement: Statement,
        parameters: &mut HashMap<String, Parameter>,
        autocommit: bool,
    ) -> Result<Execution> {
        let diagnostics = self
            .ansi_warnings
            .then(|| self.diagnostic_scope())
            .transpose()?;
        let mut result =
            self.execute_observed(statement, parameters, autocommit, diagnostics.as_ref())?;
        if self.ansi_warnings
            && diagnostics
                .as_ref()
                .is_some_and(|scope| scope.null_eliminated())
        {
            crate::aggregate_diagnostics::append_warning(&mut result.tokens);
        }
        Ok(result)
    }

    fn execute_observed(
        &mut self,
        mut statement: Statement,
        parameters: &mut HashMap<String, Parameter>,
        autocommit: bool,
        diagnostics: Option<&crate::statement_diagnostics::Scope>,
    ) -> Result<Execution> {
        validate_transaction_syntax(&statement)?;
        crate::ntile::validate_constants(&statement)?;
        let runtime_percentile_plans = percentile_plans(&statement, parameters)?;
        if let Some(execution) = self.database_statement(&statement)? {
            return Ok(execution);
        }
        self.lower_database_functions(&mut statement)?;
        self.qualify_statement(&mut statement)?;
        self.lower_session_functions(&mut statement, parameters)?;
        if let Statement::Print(print) = &statement {
            let unicode = print_is_unicode(&print.message, parameters);
            let value = self.evaluate_scalar(
                *print.message.clone(),
                print_target(&print.message, parameters),
                parameters,
            )?;
            let mut tokens = Vec::new();
            let units = match value {
                Value::Null => None,
                Value::Text(message) => Some(message.encode_utf16().take(8000).collect::<Vec<_>>()),
                value @ Value::Struct(_) => Some(crate::unicode_carrier::units(&value)?),
                _ => bail!("unsupported PRINT value representation"),
            };
            tds::diagnostic_utf16(
                &mut tokens,
                tds::DiagnosticKind::Information,
                0,
                1,
                0,
                msduck_core::print::message(units.as_deref(), unicode),
            );
            return Ok(Execution::statement(tokens, None, 247));
        }
        if let Statement::Throw(throw) = &statement {
            let (Some(number), Some(message), Some(state)) =
                (&throw.error_number, &throw.message, &throw.state)
            else {
                if let Some(error) = &self.caught_error {
                    return Err(error.clone().into());
                }
                bail!("unsupported bare THROW outside CATCH");
            };
            let number = self.evaluate_scalar(*number.clone(), DataType::Int(None), parameters)?;
            let message =
                self.evaluate_scalar(*message.clone(), DataType::Varchar(None), parameters)?;
            let state = self.evaluate_scalar(*state.clone(), DataType::Int(None), parameters)?;
            let (Value::Int(number), Value::Text(message), Value::Int(state)) =
                (number, message, state)
            else {
                bail!("THROW arguments cannot be NULL");
            };
            ensure!(number >= 50000, "THROW error number must be at least 50000");
            ensure!(
                (0..=255).contains(&state),
                "THROW state must be between 0 and 255"
            );
            ensure!(
                message.encode_utf16().count() <= 2048,
                "THROW message exceeds 2048 UTF-16 units"
            );
            return Err(SqlError {
                number,
                state: state as u8,
                severity: 16,
                message: message.replace("%%", "%"),
                message_utf16: None,
            }
            .into());
        }
        if let Statement::Declare { stmts } = &statement {
            for declaration in stmts {
                ensure!(
                    declaration.declare_type.is_none(),
                    "unsupported non-scalar declaration"
                );
                let kind = declaration
                    .data_type
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("missing variable type"))?;
                let kind = variable_type(kind)?;
                for name in &declaration.names {
                    let name = name.value.to_lowercase();
                    ensure!(
                        name.starts_with('@') && !name.starts_with("@@"),
                        "invalid local variable name"
                    );
                    let expression = match &declaration.assignment {
                        None => Expr::Value(sqlparser::ast::Value::Null.into()),
                        Some(DeclareAssignment::MsSqlAssignment(value)) => *value.clone(),
                        _ => bail!("unsupported variable initializer"),
                    };
                    let value = self.evaluate_scalar_observed(
                        expression,
                        crate::sql_type::ast(kind),
                        parameters,
                        diagnostics,
                    )?;
                    parameters.insert(
                        name,
                        Parameter {
                            value: crate::backend_value::from_backend(value)?,
                            data_type: kind,
                        },
                    );
                }
            }
            let count = stmts
                .iter()
                .any(|declaration| declaration.assignment.is_some())
                .then_some(1);
            return Ok(Execution::statement(vec![], count, 193));
        }
        if let Statement::Set(Set::SingleAssignment {
            scope,
            hivevar,
            variable,
            values,
        }) = &statement
        {
            let name = variable.to_string().to_lowercase();
            if name.starts_with('@') {
                ensure!(
                    scope.is_none() && !hivevar && values.len() == 1,
                    "unsupported variable assignment"
                );
                let kind = parameters
                    .get(&name)
                    .ok_or_else(|| anyhow::anyhow!("Must declare the scalar variable {name}"))?
                    .data_type;
                let value = self
                    .evaluate_scalar_observed(
                        values[0].clone(),
                        crate::sql_type::ast(kind),
                        parameters,
                        diagnostics,
                    )
                    .map_err(|error| {
                        if error
                            .downcast_ref::<SqlError>()
                            .is_some_and(|error| matches!(error.number, 8115 | 8134))
                        {
                            self.rowcount = 0;
                            crate::query_error::attach_context(error, vec![], 0xc1)
                        } else {
                            error
                        }
                    })?;
                parameters.insert(
                    name,
                    Parameter {
                        value: crate::backend_value::from_backend(value)?,
                        data_type: kind,
                    },
                );
                return Ok(Execution::statement(vec![], Some(1), 193));
            }
        }
        if let Statement::Set(_) = &statement {
            if let Some(value) = crate::datepart::setting(&statement).map_err(anyhow::Error::msg)? {
                let value = self.evaluate_scalar(value, DataType::Int(None), parameters)?;
                let first = match value {
                    Value::Int(first) => first,
                    Value::Null => 0,
                    _ => bail!("DATEFIRST requires an integer value"),
                };
                if !(1..=7).contains(&first) {
                    return Err(SqlError::new(
                        2742,
                        1,
                        format!("SET DATEFIRST {first} is out of range."),
                    )
                    .into());
                }
                self.set_datefirst(first)?;
                return Ok(Execution::statement(vec![], None, 0));
            }
            if statement
                .to_string()
                .eq_ignore_ascii_case("SET LANGUAGE US_ENGLISH")
            {
                self.set_datefirst(7)?;
            }
            let normalized = statement.to_string().to_uppercase().replace(" = ", " ");
            if matches!(
                normalized.as_str(),
                "SET ANSI_WARNINGS ON" | "SET ANSI_WARNINGS OFF"
            ) {
                self.ansi_warnings = normalized.ends_with(" ON");
            }
            if matches!(
                normalized.as_str(),
                "SET XACT_ABORT ON" | "SET XACT_ABORT OFF"
            ) {
                self.xact_abort = normalized.ends_with(" ON");
            }
            if let Some(nocount) = session_setting(&statement)? {
                self.nocount = nocount;
                return Ok(Execution::statement(
                    vec![],
                    None,
                    if nocount { 185 } else { 186 },
                ));
            }
            return Ok(Execution::statement(vec![], None, 0));
        }
        match &statement {
            Statement::StartTransaction { .. } => {
                return Ok(Execution::statement(
                    self.begin_transaction(0, "")?,
                    None,
                    212,
                ));
            }
            Statement::Commit { .. } => {
                return Ok(Execution::statement(self.commit_transaction()?, None, 213));
            }
            Statement::Rollback { savepoint, .. } => {
                let name = savepoint.as_ref().map(|id| id.value.as_str()).unwrap_or("");
                return Ok(Execution::statement(
                    self.rollback_transaction(name)?,
                    None,
                    0,
                ));
            }
            _ => {}
        }
        if let Some((update, with)) = msduck_sql::output::joined_update(&statement) {
            let plan = self
                .plan_joined_execution(update, with.cloned(), parameters)
                .map_err(crate::query_error::compilation)?;
            return self.execute_joined_output(plan);
        }
        let alter_name = if let Some(view) = crate::views::alter_definition(&statement)? {
            let name = view.name.clone();
            statement = Statement::CreateView(view);
            Some(name)
        } else {
            None
        };
        let output = self.lower_output(&mut statement, parameters)?;
        let result_fields = if let Some(output) = &output {
            crate::query_catalog::checked_projection_with_parameters(
                &self.db,
                &output.projection,
                parameters,
            )
            .map_err(crate::query_error::compilation)?
            .unwrap_or_default()
        } else if let Statement::Query(query) = &mut statement {
            let mut fields =
                crate::query_catalog::bind_query_with_parameters(&self.db, query, parameters)
                    .map_err(crate::query_error::compilation)?
                    .unwrap_or_default();
            // Bind the actual query first. Known RAND declarations enrich only
            // a metadata clone; effects and argument validation stay in the
            // untouched execution tree, and unknown projection shapes stay unknown.
            let mut declared = query.clone();
            if rand::declarations(&mut declared)
                && let Some(profiles) = crate::query_catalog::projection_with_parameters(
                    &self.db, &declared, parameters,
                )?
                && profiles.len() == fields.len()
            {
                for (field, profile) in fields.iter_mut().zip(profiles) {
                    if field.info.is_none() && profile.info.is_some() {
                        field.info = profile.info;
                        field.properties = profile.properties;
                        field.collation = profile.collation;
                    }
                }
            }
            fields
        } else {
            Vec::new()
        };
        msduck_sql::projection::validate_collation_operations(&result_fields)
            .map_err(anyhow::Error::new)
            .map_err(crate::query_error::compilation)?;
        let output_sink = output
            .as_ref()
            .and_then(|plan| plan.sink.as_ref())
            .map(|sink| crate::output_sink::bind(&self.db, sink, &result_fields))
            .transpose()?;
        let materialized = if let Some(plan) = output
            .as_ref()
            .filter(|plan| plan.requires_materialization() && !plan.paired)
        {
            let name = crate::output_image::name();
            let mut materialized = plan.materialize(&mut statement, name.clone())?;
            let mut bound = Statement::Query(plan.projection.clone());
            crate::aggregate_columns::annotate(&self.db, &mut bound, parameters)
                .map_err(anyhow::Error::msg)?;
            let Statement::Query(bound) = bound else {
                unreachable!()
            };
            let (SetExpr::Select(bound), SetExpr::Select(projection)) =
                (bound.body.as_ref(), materialized.projection.body.as_mut())
            else {
                unreachable!()
            };
            projection.projection = bound.projection.clone();
            let mut translator = Translator {
                parameters,
                values: vec![],
                parameter_slots: HashMap::new(),
                transactions: self.transactions,
                options_mask: self.options_mask(),
                transaction_doomed: self.transaction_doomed,
                original_login: &self.original_login,
                clock: crate::current_time::now(),
                rowcount: self.rowcount,
                last_error: self.last_error,
                caught_error: self.caught_error.as_ref(),
                spid: self.process.spid(),
            };
            if let ControlFlow::Break(error) =
                VisitMut::visit(&mut materialized.projection, &mut translator)
            {
                bail!(error);
            }
            Some((materialized, name, translator.values))
        } else {
            None
        };
        self.bind_dml(&mut statement, parameters)?;
        crate::query_catalog::lower_recursion(&self.db, &mut statement)?;
        let json = crate::for_json::Output::take(&self.db, &mut statement)?;
        let assignments = select_assignments(&mut statement, parameters)?;
        let into = crate::select_into::take(&mut statement)?;
        let into_columns = if into.is_some() {
            if let Statement::Query(query) = &statement {
                crate::query_catalog::projection(&self.db, query)?
            } else {
                None
            }
        } else {
            None
        };
        let metadata_statement = output
            .as_ref()
            .map(|output| Statement::Query(output.projection.clone()));
        let result_types = crate::result_types::bound_projection(
            &self.db,
            metadata_statement.as_ref().unwrap_or(&statement),
            parameters,
        )?;
        let is_query = output.is_some() || crate::update::is_query(&statement);
        let write_command = match &statement {
            Statement::Insert(_) => Some(0xc3),
            Statement::Update(_) => Some(0xc5),
            Statement::Delete(_) => Some(0xc4),
            Statement::Query(query) => match query.body.as_ref() {
                SetExpr::Update(_) => Some(0xc5),
                SetExpr::Delete(_) => Some(0xc4),
                _ => None,
            },
            _ => None,
        };
        ensure!(
            matches!(
                statement,
                Statement::Query(_)
                    | Statement::CreateTable(_)
                    | Statement::Truncate(_)
                    | Statement::CreateSchema { .. }
                    | Statement::AlterTable(_)
                    | Statement::CreateView(_)
                    | Statement::Insert(_)
                    | Statement::Update { .. }
                    | Statement::Delete(_)
                    | Statement::Drop { .. }
                    | Statement::CreateIndex(_)
            ),
            "unsupported T-SQL statement"
        );
        let identity_source = match &statement {
            Statement::CreateTable(_) | Statement::AlterTable(_) => Some(statement.clone()),
            _ => None,
        };
        let money_columns = crate::insert::money_columns(&statement, parameters);
        crate::update::expand_compound(&self.db, &mut statement)?;
        let money_assignments = crate::update::money_assignments(&statement, parameters);
        crate::query_catalog::bind_binary_operations(&self.db, &mut statement, parameters)?;
        crate::concat_lower::statement(&mut statement, parameters).map_err(anyhow::Error::msg)?;
        crate::aggregate_columns::annotate(&self.db, &mut statement, parameters)
            .map_err(anyhow::Error::msg)?;
        crate::query_catalog::bind_unicode_operations(&self.db, &mut statement, parameters)?;
        crate::concat_lower::annotated_unicode_casts(&mut statement);
        crate::for_json::lower_nested(&self.db, &mut statement, parameters)?;
        let checked_projection = if output.is_none()
            && json.is_none()
            && assignments.is_empty()
            && into.is_none()
            && let Statement::Query(query) = &statement
            && let Some(plan) = crate::checked_projection::plan(&self.db, query, parameters)?
            && let Some(metadata) = crate::checked_projection::metadata(&plan, &result_fields)?
        {
            for expression in &plan.scalar_checks {
                self.evaluate_expression(expression.clone(), parameters, false)
                    .map_err(|error| {
                        crate::query_error::attach_context(error, metadata.clone(), 0xc1)
                    })?;
            }
            statement = Statement::Query(plan.query);
            Some((plan.width, metadata))
        } else {
            None
        };
        let mut percentile_parameters = parameters.clone();
        let mut percentile_faults = Vec::new();
        let mut percentile_bindings = Vec::new();
        for plan in &runtime_percentile_plans {
            let fraction =
                self.evaluate_percentile_fraction(&plan.fraction, parameters, diagnostics)?;
            let (value, ticket) = match fraction {
                Ok(value) => (value, None),
                Err(error) => {
                    let index = i64::try_from(percentile_faults.len())?;
                    percentile_faults.push(Some(error));
                    let ticket = percentile_binding(
                        &mut percentile_parameters,
                        ParameterValue::BigInt(index),
                        SqlType::BigInt,
                    );
                    (0.0, Some(ticket))
                }
            };
            let name = percentile_binding(
                &mut percentile_parameters,
                ParameterValue::Double(value),
                SqlType::Float,
            );
            percentile_bindings.push((name, ticket, value == 0.0));
        }
        lower_runtime_percentiles(&mut statement, &percentile_bindings)?;
        let mut translator = Translator {
            parameters: &percentile_parameters,
            values: vec![],
            parameter_slots: HashMap::new(),
            transactions: self.transactions,
            options_mask: self.options_mask(),
            transaction_doomed: self.transaction_doomed,
            original_login: &self.original_login,
            clock: crate::current_time::now(),
            rowcount: self.rowcount,
            last_error: self.last_error,
            caught_error: self.caught_error.as_ref(),
            spid: self.process.spid(),
        };
        if let ControlFlow::Break(error) = VisitMut::visit(&mut statement, &mut translator) {
            bail!(error);
        }
        let _rand_scopes = rand::lower(
            &mut statement,
            &mut translator.values,
            &self.diagnostics.rand,
            &self.rand,
        )?;
        crate::insert::lower(&self.db, &mut statement, &money_columns)?;
        crate::update::lower(&self.db, &mut statement, &money_assignments)?;
        if let Some(diagnostics) = diagnostics {
            let ticket = Expr::Value(
                sqlparser::ast::Value::Placeholder(format!("${}", translator.values.len() + 1))
                    .into(),
            );
            if crate::aggregate_diagnostics::instrument(&mut statement, ticket) > 0 {
                translator
                    .values
                    .push(Value::Blob(diagnostics.ticket().to_vec()));
            }
        }
        // Identity parameters are integer constants, not numeric result expressions.
        match (identity_source.as_ref(), &mut statement) {
            (Some(Statement::CreateTable(original)), Statement::CreateTable(table)) => {
                for (source, target) in original.columns.iter().zip(&mut table.columns) {
                    crate::identity::restore_options(source, target);
                }
            }
            (Some(Statement::AlterTable(original)), Statement::AlterTable(table)) => {
                for (source, target) in original.operations.iter().zip(&mut table.operations) {
                    if let (
                        AlterTableOperation::AddColumn {
                            column_def: source, ..
                        },
                        AlterTableOperation::AddColumn {
                            column_def: target, ..
                        },
                    ) = (source, target)
                    {
                        crate::identity::restore_options(source, target);
                    }
                }
            }
            _ => {}
        }
        if crate::schema_catalog::execute(&self.db, &statement, autocommit)? {
            return Ok(Execution::statement(vec![], None, 0));
        }
        if crate::identity::create(&self.db, &statement, autocommit)? {
            return Ok(Execution::statement(vec![], None, 0));
        }
        if let Statement::Truncate(truncate) = &statement {
            crate::truncate::execute(&self.db, truncate, autocommit)?;
            return Ok(Execution::statement(vec![], None, 0));
        }
        if let Statement::AlterTable(table) = &statement {
            crate::guid_assignment::validate_alter(&self.db, table).inspect_err(|error| {
                if self.transactions > 0
                    && error
                        .downcast_ref::<SqlError>()
                        .is_some_and(|e| e.number == 8169)
                {
                    self.transaction_doomed = true;
                }
            })?;
            ensure!(
                translator.values.is_empty(),
                "unsupported bound values in ALTER TABLE"
            );
            let Some(Statement::AlterTable(original)) = &identity_source else {
                unreachable!()
            };
            crate::table_alter::execute_declared(&self.db, table, original, autocommit)?;
            return Ok(Execution::statement(vec![], None, 0));
        }
        if crate::identity::drop_table(&self.db, &statement, autocommit)? {
            return Ok(Execution::statement(vec![], None, 0));
        }
        let paired = if let Some(output) = output.as_ref().filter(|output| output.paired) {
            let (update, with) = match &statement {
                Statement::Update(update) => (update, None),
                Statement::Query(query) => {
                    let SetExpr::Update(inner) = query.body.as_ref() else {
                        bail!("expected paired UPDATE")
                    };
                    let Statement::Update(update) = inner else {
                        bail!("expected paired UPDATE")
                    };
                    (update, query.with.clone())
                }
                _ => bail!("expected paired UPDATE"),
            };
            let columns = crate::output_update::columns(&self.db, update)?;
            let name = crate::output_image::name();
            let alias = name.0.last().unwrap().as_ident().unwrap().clone();
            let mut plan = msduck_sql::output_update::plan(update, &columns, name.clone(), alias)?;
            plan.capture.with = with;
            let mut bound = Statement::Query(output.projection.clone());
            crate::aggregate_columns::annotate(&self.db, &mut bound, parameters)
                .map_err(anyhow::Error::msg)?;
            let Statement::Query(bound) = bound else {
                unreachable!()
            };
            let SetExpr::Select(bound) = bound.body.as_ref() else {
                unreachable!()
            };
            let mut projection = plan.projection(&bound.projection)?;
            let mut projection_translator = Translator {
                parameters,
                values: vec![],
                parameter_slots: HashMap::new(),
                transactions: self.transactions,
                options_mask: self.options_mask(),
                transaction_doomed: self.transaction_doomed,
                original_login: &self.original_login,
                clock: crate::current_time::now(),
                rowcount: self.rowcount,
                last_error: self.last_error,
                caught_error: self.caught_error.as_ref(),
                spid: self.process.spid(),
            };
            if let ControlFlow::Break(error) =
                VisitMut::visit(&mut projection, &mut projection_translator)
            {
                bail!(error);
            }
            Some((plan, name, projection, projection_translator.values))
        } else {
            None
        };
        let rendered = paired.as_ref().map_or_else(
            || statement.to_string(),
            |(plan, _, _, _)| plan.write.to_string(),
        );
        if let Some(target) = into {
            let count = crate::select_into::execute(
                &self.db,
                &target,
                &rendered,
                &translator.values,
                autocommit,
                into_columns.as_deref(),
            )?;
            return Ok(Execution::statement(vec![], Some(count), 0xc1));
        }
        if let Some(name) = alter_name {
            crate::views::alter_existing(&self.db, &name, &rendered, autocommit)?;
            return Ok(Execution::statement(vec![], None, 0));
        }
        let image = if let Some((plan, name, _, _)) = &paired {
            Some(crate::output_image::Image::create_bound(
                &self.db,
                name.clone(),
                &plan.capture,
                &translator.values,
            )?)
        } else {
            materialized
                .as_ref()
                .map(|(plan, name, _)| {
                    crate::output_image::Image::create(&self.db, name.clone(), &plan.image)
                })
                .transpose()?
        };
        let mut projected = if let Some((_, _, projection, _)) = &paired {
            Some(self.db.prepare(&projection.to_string())?)
        } else {
            materialized
                .as_ref()
                .map(|(plan, _, _)| self.db.prepare(&plan.projection.to_string()))
                .transpose()?
        };
        if !is_query && output.is_none() {
            let checked = match write_command {
                Some(0xc3) => {
                    crate::guid_assignment::checked_insert(&self.db, &statement, &translator.values)
                }
                Some(0xc5) => {
                    crate::guid_assignment::checked_update(&self.db, &statement, &translator.values)
                }
                _ => Ok(None),
            };
            let count = checked.map_err(|error| {
                let number = error.downcast_ref::<SqlError>().map(|e| e.number);
                if self.transactions > 0 && number == Some(8169) {
                    self.transaction_doomed = true;
                }
                if matches!(number, Some(8169 | 2628)) {
                    crate::query_error::attach_context(
                        error,
                        vec![],
                        write_command.expect("checked GUID DML"),
                    )
                } else {
                    error
                }
            })?;
            if let Some(count) = count {
                return Ok(Execution::statement(
                    vec![],
                    Some(count),
                    write_command.expect("checked GUID DML"),
                ));
            }
        }
        if self.transactions > 0 && !is_query && output.is_none() {
            let checked = match write_command {
                Some(0xc3) => crate::storage_diagnostic::checked_insert(
                    &self.db,
                    &statement,
                    &translator.values,
                ),
                Some(0xc5) => crate::storage_diagnostic::checked_update(
                    &self.db,
                    &statement,
                    &translator.values,
                ),
                _ => Ok(None),
            };
            let count = checked.map_err(|error| {
                if error
                    .downcast_ref::<SqlError>()
                    .is_some_and(|e| e.number == 2628)
                {
                    crate::query_error::attach_context(
                        error,
                        vec![],
                        write_command.expect("checked DML"),
                    )
                } else {
                    error
                }
            })?;
            if let Some(count) = count {
                return Ok(Execution::statement(
                    vec![],
                    Some(count),
                    write_command.expect("checked DML"),
                ));
            }
        }
        let mut prepared = self.db.prepare(&rendered).map_err(|error| {
            if let Some(diagnostic) = crate::query_error::integer_overflow(&error.to_string()) {
                if is_query
                    && output.is_none()
                    && json.is_none()
                    && assignments.is_empty()
                    && let Some(metadata) =
                        crate::query_error::describe_fields(&result_fields, &result_types)
                {
                    return crate::query_error::attach_context(diagnostic.into(), metadata, 0xc1);
                }
                return anyhow::Error::new(diagnostic);
            }
            anyhow::Error::new(error)
        })?;
        if !is_query {
            let count = prepared
                .execute(duckdb::params_from_iter(translator.values.iter()))
                .map_err(|error| {
                    // A rejected character write terminates this statement. Keep
                    // its operation identity for 3621 and subsequent batch work;
                    // preparation/binding errors never enter this execution path.
                    if let Some(command) = write_command
                        && matches!(error_number(&error.to_string()), 8152 | 2628)
                    {
                        crate::query_error::attach(error, vec![], command)
                    } else {
                        anyhow::Error::new(error)
                    }
                })? as u64;
            return Ok(Execution::statement(
                vec![],
                Some(count),
                write_command.unwrap_or(0xc3),
            ));
        }
        let names = if json.is_some() {
            crate::for_json::Output::names(&self.db, &rendered, &translator.values)?
        } else {
            Vec::new()
        };
        let json_plan = json.as_ref().map(|json| json.plan(&names)).transpose()?;
        let error_metadata = if let Some((_, metadata)) = &checked_projection {
            Some(metadata.clone())
        } else if json.is_none() && assignments.is_empty() && output_sink.is_none() {
            crate::query_error::describe(
                projected.as_ref().unwrap_or(&prepared),
                &result_fields,
                &result_types,
            )
            .or_else(|| {
                (!runtime_percentile_plans.is_empty())
                    .then(|| crate::query_error::describe_fields(&result_fields, &result_types))
                    .flatten()
            })
        } else {
            None
        };
        let output_command = output.as_ref().map_or(0xc1, |plan| match plan.operation {
            msduck_sql::output::Operation::Insert => 0xc3,
            msduck_sql::output::Operation::Update => 0xc5,
            msduck_sql::output::Operation::Delete => 0xc4,
        });
        let batches = if let Some((plan, _, _, values)) = &paired {
            let describe = |error| match &error_metadata {
                Some(metadata) => {
                    crate::query_error::attach(error, metadata.clone(), output_command)
                }
                None if output_sink.is_some() => crate::output_sink::failed(
                    anyhow::Error::new(error),
                    msduck_sql::output::Operation::Update,
                ),
                None => anyhow::Error::new(error),
            };
            let mut capture = self.db.prepare(&plan.capture.to_string())?;
            image.as_ref().unwrap().append(
                capture
                    .query_arrow(duckdb::params_from_iter(translator.values.iter()))
                    .map_err(describe)?,
            )?;
            prepared.execute([]).map_err(describe)?;
            projected
                .as_mut()
                .unwrap()
                .query_arrow(duckdb::params_from_iter(values.iter()))
                .map_err(describe)?
        } else {
            let execution = if let Some(cancel) = &self.read_cancel
                && matches!(statement, Statement::Query(_))
                && output.is_none()
                && assignments.is_empty()
                && json.is_none()
                && error_metadata.is_some()
            {
                match prepared.query_arrow_cancellable_read(
                    duckdb::params_from_iter(translator.values.iter()),
                    cancel,
                ) {
                    Ok(Some(batches)) => Ok(batches),
                    Ok(None) => {
                        return Err(crate::read_cancellation::CancelledRead {
                            metadata: error_metadata.clone().expect("checked read metadata"),
                        }
                        .into());
                    }
                    Err(duckdb::CancellableReadError::Query(error)) => Err(error),
                    Err(duckdb::CancellableReadError::ConnectionUnusable(error)) => {
                        return Err(crate::read_cancellation::UnusableRead(error).into());
                    }
                }
            } else {
                prepared.query_arrow(duckdb::params_from_iter(translator.values.iter()))
            };
            let batches = execution.map_err(|error| {
                if let Some(ticket) = crate::percentile_input::fault_ticket(&error.to_string())
                    && let Some(Some(diagnostic)) = percentile_faults.get_mut(ticket)
                {
                    let diagnostic = diagnostic.clone();
                    return match &error_metadata {
                        Some(metadata) => crate::query_error::attach_context(
                            diagnostic.into(),
                            metadata.clone(),
                            output_command,
                        ),
                        None => diagnostic.into(),
                    };
                }
                match &error_metadata {
                    Some(metadata) => {
                        crate::query_error::attach(error, metadata.clone(), output_command)
                    }
                    None if output_sink.is_some() => crate::output_sink::failed(
                        anyhow::Error::new(error),
                        output.as_ref().unwrap().operation,
                    ),
                    None => anyhow::Error::new(error),
                }
            })?;
            if let Some((_, _, values)) = &materialized {
                image
                    .as_ref()
                    .expect("materialized image")
                    .append(batches)?;
                projected
                    .as_mut()
                    .expect("materialized projection")
                    .query_arrow(duckdb::params_from_iter(values.iter()))
                    .map_err(|error| match &error_metadata {
                        Some(metadata) => {
                            crate::query_error::attach(error, metadata.clone(), output_command)
                        }
                        None if output_sink.is_some() => crate::output_sink::failed(
                            anyhow::Error::new(error),
                            output.as_ref().unwrap().operation,
                        ),
                        None => anyhow::Error::new(error),
                    })?
            } else {
                batches
            }
        };
        if let Some(sink) = output_sink {
            let rows = Self::collect_output_rows(batches, sink.declarations.len())?;
            drop(prepared);
            drop(projected);
            crate::output_image::close(image)?;
            return self.store_output_rows(sink, rows, output.as_ref().unwrap().operation);
        }

        if let (Some(json), Some(plan)) = (json, json_plan) {
            let mut writer = json.writer(&plan)?;
            let json_types = batches
                .get_schema()
                .fields()
                .iter()
                .enumerate()
                .map(|(i, field)| {
                    result_types
                        .get(i)
                        .cloned()
                        .flatten()
                        .or_else(|| wire_type(field.data_type()).ok())
                })
                .collect::<Vec<_>>();
            for batch in batches {
                for row in 0..batch.num_rows() {
                    let values = (0..batch.num_columns())
                        .map(|i| {
                            arrow_value(batch.column(i).as_ref(), row, batch.schema().field(i))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    json.row(&mut writer, &names, &values, &json_types)?;
                }
            }
            return Ok(Execution::result_set(
                crate::for_json::Output::encode(writer.finish())?,
                Some(1),
                0xc1,
            ));
        }
        if !assignments.is_empty() {
            let mut count = 0u64;
            let mut last = None;
            for batch in batches {
                count += batch.num_rows() as u64;
                if batch.num_rows() > 0 {
                    let row = batch.num_rows() - 1;
                    last = Some(
                        (0..assignments.len())
                            .map(|i| {
                                binding_value(arrow_value(
                                    batch.column(i).as_ref(),
                                    row,
                                    batch.schema().field(i),
                                )?)
                            })
                            .collect::<Result<Vec<_>>>()?,
                    );
                }
            }
            // No rows leave existing bindings untouched. A scalar subquery
            // returning no rows instead produces one row containing NULL.
            if let Some(values) = last {
                for ((name, data_type), value) in assignments.into_iter().zip(values) {
                    parameters.insert(
                        name,
                        Parameter {
                            value: crate::backend_value::from_backend(value)?,
                            data_type,
                        },
                    );
                }
            }
            return Ok(Execution::statement(vec![], Some(count), 0xc1));
        }
        let schema = batches.get_schema();
        let (out, count) = Self::encode_batches_inner(
            batches,
            &schema,
            &result_fields,
            &result_types,
            checked_projection.map(|(width, _)| width),
        )?;
        crate::output_image::close(image)?;
        Ok(Execution::result_set(
            out,
            Some(count),
            output
                .as_ref()
                .map_or(0xc1, |output| match output.operation {
                    msduck_sql::output::Operation::Insert => 0xc3,
                    msduck_sql::output::Operation::Update => 0xc5,
                    msduck_sql::output::Operation::Delete => 0xc4,
                }),
        ))
    }

    fn collect_output_rows(
        batches: impl Iterator<Item = duckdb::arrow::record_batch::RecordBatch>,
        width: usize,
    ) -> Result<Vec<Vec<ParameterValue>>> {
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        for batch in batches {
            ensure!(
                batch.num_columns() == width,
                "OUTPUT projection width changed during execution"
            );
            for row in 0..batch.num_rows() {
                let values = (0..batch.num_columns())
                    .map(|i| {
                        crate::backend_value::from_backend(binding_value(arrow_value(
                            batch.column(i).as_ref(),
                            row,
                            batch.schema().field(i),
                        )?)?)
                    })
                    .collect::<Result<Vec<_>>>()?;
                for value in &values {
                    bytes = bytes
                        .saturating_add(std::mem::size_of::<ParameterValue>())
                        .saturating_add(match value {
                            ParameterValue::Text(value) => value.len(),
                            ParameterValue::Unicode(value) => value.len().saturating_mul(2),
                            ParameterValue::Blob(value) => value.len(),
                            _ => 0,
                        });
                }
                ensure!(
                    bytes <= 64 * 1024 * 1024,
                    "OUTPUT materialization exceeds the configured 64 MiB limit"
                );
                rows.push(values);
            }
        }
        Ok(rows)
    }

    fn store_output_rows(
        &mut self,
        sink: crate::output_sink::Bound,
        rows: Vec<Vec<ParameterValue>>,
        operation: msduck_sql::output::Operation,
    ) -> Result<Execution> {
        let count = rows.len() as u64;
        for row in rows {
            let mut bindings = sink
                .declarations
                .iter()
                .zip(row)
                .map(|((name, kind), value)| {
                    (
                        name.clone(),
                        Parameter {
                            data_type: *kind,
                            value,
                        },
                    )
                })
                .collect();
            self.execute_inner(sink.statement.clone(), &mut bindings, false)
                .map_err(|error| crate::output_sink::failed(error, operation))?;
        }
        Ok(Execution::statement(
            vec![],
            Some(count),
            match operation {
                msduck_sql::output::Operation::Insert => 0xc3,
                msduck_sql::output::Operation::Update => 0xc5,
                msduck_sql::output::Operation::Delete => 0xc4,
            },
        ))
    }

    fn encode_batches(
        batches: impl Iterator<Item = duckdb::arrow::record_batch::RecordBatch>,
        schema: &duckdb::arrow::datatypes::Schema,
        result_fields: &[crate::query_catalog::Field],
        result_types: &[Option<Type>],
    ) -> Result<(Vec<u8>, u64)> {
        Self::encode_batches_inner(batches, schema, result_fields, result_types, None)
    }

    fn encode_batches_inner(
        batches: impl Iterator<Item = duckdb::arrow::record_batch::RecordBatch>,
        schema: &duckdb::arrow::datatypes::Schema,
        result_fields: &[crate::query_catalog::Field],
        result_types: &[Option<Type>],
        checked_width: Option<usize>,
    ) -> Result<(Vec<u8>, u64)> {
        let public_schema = checked_width
            .map(|width| schema.project(&(0..width).collect::<Vec<_>>()))
            .transpose()?;
        let schema = public_schema.as_ref().unwrap_or(schema);
        let metadata = crate::result_metadata::Aligned::new(
            schema.fields().len(),
            result_fields,
            result_types,
        );
        let columns = schema
            .fields()
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let kind = if result_fields.len() == schema.fields().len()
                    && matches!(
                        result_fields[index]
                            .info
                            .as_ref()
                            .and_then(|info| info.system_type_id),
                        Some(58 | 61)
                    )
                    && matches!(field.data_type(), ArrowType::Timestamp(_, None))
                {
                    Type::LegacyDateTime(
                        if result_fields[index]
                            .info
                            .as_ref()
                            .and_then(|info| info.system_type_id)
                            == Some(58)
                        {
                            4
                        } else {
                            8
                        },
                    )
                } else if let Some(kind) = metadata.declared(index)
                    && (matches!(field.data_type(), ArrowType::Utf8 | ArrowType::LargeUtf8)
                        || crate::unicode_carrier::is_arrow(field.data_type())
                            && matches!(kind, Type::Text | Type::Nvarchar(_) | Type::Nchar(_))
                        || matches!(
                            (kind, field.data_type()),
                            (Type::Time(_), ArrowType::Time64(_))
                                | (Type::Money(_), ArrowType::Decimal128(_, 4))
                                | (
                                    Type::Varbinary(_) | Type::FixedBinary(_),
                                    ArrowType::Binary | ArrowType::LargeBinary
                                )
                        ))
                {
                    kind.clone()
                } else if is_bool_field(field) {
                    Type::Bit
                } else if is_uuid_field(field) {
                    Type::Guid
                } else {
                    wire_type(field.data_type())?
                };
                Ok(metadata.column(index, field.name().clone(), kind))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut out = vec![];
        tds::metadata(&mut out, &columns)?;
        let mut count = 0;
        for batch in batches {
            for row in 0..batch.num_rows() {
                if let Some(width) = checked_width
                    && let Some(error) = crate::checked_projection::diagnostic(&batch, row, width)?
                {
                    return Err(crate::query_error::attach_context(error.into(), out, 0xc1));
                }
                out.push(0xd1);
                for (i, column) in columns.iter().enumerate() {
                    encode_column(
                        &mut out,
                        column,
                        &arrow_value(batch.column(i).as_ref(), row, batch.schema().field(i))?,
                    )?;
                }
                ensure!(
                    out.len() <= tds::MAX_MESSAGE,
                    "result exceeds current 16 MiB response limit"
                );
                count += 1;
            }
        }
        Ok((out, count))
    }

    fn evaluate_scalar(
        &self,
        expression: Expr,
        data_type: DataType,
        parameters: &HashMap<String, Parameter>,
    ) -> Result<Value> {
        self.evaluate_scalar_observed(expression, data_type, parameters, None)
    }

    fn evaluate_scalar_observed(
        &self,
        expression: Expr,
        data_type: DataType,
        parameters: &HashMap<String, Parameter>,
        diagnostics: Option<&crate::statement_diagnostics::Scope>,
    ) -> Result<Value> {
        let expression = Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(expression),
            data_type,
            format: None,
        };
        self.evaluate_expression_observed(expression, parameters, false, diagnostics)
    }

    fn evaluate_expression(
        &self,
        expression: Expr,
        parameters: &HashMap<String, Parameter>,
        predicate: bool,
    ) -> Result<Value> {
        self.evaluate_expression_observed(expression, parameters, predicate, None)
    }

    fn evaluate_percentile_fraction(
        &self,
        expression: &Expr,
        parameters: &HashMap<String, Parameter>,
        diagnostics: Option<&crate::statement_diagnostics::Scope>,
    ) -> Result<Result<f64, SqlError>> {
        if let Some(constant) = msduck_sql::percentile::constant_fraction(expression) {
            return Ok(constant);
        }
        let mut source = expression;
        while let Expr::Nested(inner) = source {
            source = inner;
        }
        if let Expr::Subquery(query) = source
            && let SetExpr::Select(select) = query.body.as_ref()
            && select.from.is_empty()
            && let [SelectItem::UnnamedExpr(value) | SelectItem::ExprWithAlias { expr: value, .. }] =
                select.projection.as_slice()
        {
            return self.evaluate_percentile_fraction(value, parameters, diagnostics);
        }
        if let Expr::Identifier(name) = source
            && let Some(parameter) = parameters
                .iter()
                .find_map(|(key, value)| key.eq_ignore_ascii_case(&name.value).then_some(value))
        {
            return msduck_sql::percentile::runtime_fraction(&parameter.value, parameter.data_type)
                .ok_or_else(|| {
                    anyhow::anyhow!("unsupported percentile fraction binding representation")
                });
        }
        let Statement::Query(mut query) =
            Parser::parse_sql(&crate::dialect::ServerDialect, "SELECT NULL")?.remove(0)
        else {
            unreachable!()
        };
        let SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!()
        };
        let mut declared_expression = expression.clone();
        rand::declarations(&mut declared_expression);
        msduck_sql::case_types::lower(&mut declared_expression, parameters);
        select.projection = vec![SelectItem::UnnamedExpr(declared_expression)];
        let declaration =
            crate::query_catalog::projection_with_parameters(&self.db, &query, parameters)?
                .and_then(|fields| fields.into_iter().next())
                .and_then(|field| field.info)
                .and_then(|info| info.logical_type())
                .ok_or_else(|| anyhow::anyhow!("unsupported percentile fraction declaration"))?;
        let mut expression = expression.clone();
        msduck_sql::percentile::normalize_runtime_literals(&mut expression)?;
        let value =
            match self.evaluate_expression_observed(expression, parameters, false, diagnostics) {
                Ok(value) => crate::backend_value::from_backend(value)?,
                Err(error) => {
                    let diagnostic = error
                        .downcast_ref::<SqlError>()
                        .cloned()
                        .or_else(|| runtime_diagnostic(&error.to_string()));
                    if let Some(diagnostic) = diagnostic
                        && matches!(diagnostic.number, 8134 | 8114 | 8115)
                    {
                        return Ok(Err(diagnostic));
                    }
                    return Err(error);
                }
            };
        msduck_sql::percentile::runtime_fraction(&value, declaration).ok_or_else(|| {
            anyhow::anyhow!("unsupported evaluated percentile fraction representation")
        })
    }

    fn evaluate_expression_observed(
        &self,
        mut expression: Expr,
        parameters: &HashMap<String, Parameter>,
        predicate: bool,
        diagnostics: Option<&crate::statement_diagnostics::Scope>,
    ) -> Result<Value> {
        self.restore_catalog();
        self.lower_database_functions(&mut expression)?;
        self.qualify_databases_across(&mut expression)?;
        match self.cross_database(&expression, None)? {
            CrossDatabase::Local => {}
            CrossDatabase::Home(home, others) => {
                rehome(&mut expression, &home, &self.current_alias()?, &[]);
                let _others = self.use_others(&others)?;
                let home = self.enter_home(&home)?;
                let result =
                    self.evaluate_expression_here(expression, parameters, predicate, diagnostics);
                self.leave_home(home);
                return result;
            }
            CrossDatabase::Mixed(others) => {
                let _others = self.use_others(&others)?;
                return self.evaluate_expression_here(
                    expression,
                    parameters,
                    predicate,
                    diagnostics,
                );
            }
        }
        self.evaluate_expression_here(expression, parameters, predicate, diagnostics)
    }

    fn evaluate_expression_here(
        &self,
        mut expression: Expr,
        parameters: &HashMap<String, Parameter>,
        predicate: bool,
        diagnostics: Option<&crate::statement_diagnostics::Scope>,
    ) -> Result<Value> {
        self.sync_datefirst()?;
        let lowered = self.lower_session_functions_shared(&mut expression, parameters)?;
        let parameters = &*lowered;
        ext::rewrite(self, &mut expression, parameters)?;
        crate::query_catalog::lower_recursion(&self.db, &mut expression)?;
        crate::aggregate_columns::annotate(&self.db, &mut expression, parameters)
            .map_err(anyhow::Error::msg)?;
        crate::query_catalog::bind_unicode_expression(&self.db, &mut expression, parameters)?;
        crate::concat_lower::annotated_unicode_casts(&mut expression);
        crate::for_json::lower_nested(&self.db, &mut expression, parameters)?;
        let mut translator = Translator {
            parameters,
            values: vec![],
            parameter_slots: HashMap::new(),
            transactions: self.transactions,
            options_mask: self.options_mask(),
            transaction_doomed: self.transaction_doomed,
            original_login: &self.original_login,
            clock: crate::current_time::now(),
            rowcount: self.rowcount,
            last_error: self.last_error,
            caught_error: self.caught_error.as_ref(),
            spid: self.process.spid(),
        };
        use msduck_sql::checked_expression::{Kind, plan};
        let mut declarations = parameters
            .iter()
            .filter_map(|(name, parameter)| {
                let kind = match parameter.ast_type() {
                    DataType::Int(_) | DataType::Integer(_) => Kind::Int,
                    DataType::BigInt(_) => Kind::BigInt,
                    _ => return None,
                };
                Some((name.clone(), kind))
            })
            .collect::<HashMap<_, _>>();
        for name in ["@@trancount", "@@rowcount", "@@error"] {
            declarations.insert(name.into(), Kind::Int);
        }
        let mut rand_scopes = Vec::new();
        let (sql, checked) = if let Some(mut checked) = plan(&expression, &declarations) {
            if let ControlFlow::Break(error) = VisitMut::visit(&mut checked.query, &mut translator)
            {
                bail!(error);
            }
            rand_scopes.extend(rand::lower(
                &mut checked.query,
                &mut translator.values,
                &self.diagnostics.rand,
                &self.rand,
            )?);
            if let Some(diagnostics) = diagnostics {
                crate::aggregate_diagnostics::bind_expressions(
                    &mut checked.query,
                    diagnostics,
                    &mut translator.values,
                );
            }
            (checked.query.to_string(), true)
        } else {
            if let ControlFlow::Break(error) = VisitMut::visit(&mut expression, &mut translator) {
                bail!(error);
            }
            rand_scopes.extend(rand::lower(
                &mut expression,
                &mut translator.values,
                &self.diagnostics.rand,
                &self.rand,
            )?);
            if let Some(diagnostics) = diagnostics {
                crate::aggregate_diagnostics::bind_expressions(
                    &mut expression,
                    diagnostics,
                    &mut translator.values,
                );
            }
            (format!("SELECT {expression}"), false)
        };
        let mut statement = self.db.prepare(&sql)?;
        let mut batches =
            statement.query_arrow(duckdb::params_from_iter(translator.values.iter()))?;
        if predicate {
            ensure!(
                is_bool_field(batches.get_schema().field(0)),
                "An expression of non-boolean type specified in a context where a condition is expected"
            );
        }
        let batch = batches
            .next()
            .ok_or_else(|| anyhow::anyhow!("scalar assignment returned no value"))?;
        ensure!(
            batch.num_rows() == 1 && batch.num_columns() == if checked { 5 } else { 1 },
            "scalar assignment must return one value"
        );
        if checked {
            let field =
                |index| arrow_value(batch.column(index).as_ref(), 0, batch.schema().field(index));
            match field(1)? {
                Value::Null => {}
                Value::Int(number) => {
                    let (Value::UTinyInt(state), Value::UTinyInt(severity), Value::Text(message)) =
                        (field(2)?, field(3)?, field(4)?)
                    else {
                        bail!("invalid checked scalar diagnostic");
                    };
                    let mut error = SqlError::new(number, state, message);
                    error.severity = severity;
                    return Err(error.into());
                }
                _ => bail!("invalid checked scalar error number"),
            }
        }
        let value = arrow_value(batch.column(0).as_ref(), 0, batch.schema().field(0))?;
        binding_value(value)
    }
}
// Shared validation for NOCOUNT and fixed settings. DATEFIRST has a separate
// typed runtime path so preparation cannot mutate the connection.
fn session_setting(statement: &Statement) -> Result<Option<bool>> {
    let normalized = statement.to_string().to_uppercase().replace(" = ", " ");
    match normalized.as_str() {
        "SET NOCOUNT ON" => return Ok(Some(true)),
        "SET NOCOUNT OFF" => return Ok(Some(false)),
        _ => {}
    }
    ensure!(
        [
            "SET ANSI_NULLS ON",
            "SET ANSI_NULL_DFLT_ON ON",
            "SET ANSI_PADDING ON",
            "SET ANSI_WARNINGS ON",
            "SET ANSI_WARNINGS OFF",
            "SET ARITHABORT ON",
            "SET QUOTED_IDENTIFIER ON",
            "SET CONCAT_NULL_YIELDS_NULL ON",
            "SET NUMERIC_ROUNDABORT OFF",
            "SET IMPLICIT_TRANSACTIONS OFF",
            "SET CURSOR_CLOSE_ON_COMMIT OFF",
            "SET XACT_ABORT OFF",
            "SET XACT_ABORT ON",
            "SET TEXTSIZE 2147483647",
            "SET DATEFORMAT MDY",
            "SET LANGUAGE US_ENGLISH",
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED"
        ]
        .contains(&normalized.as_str()),
        "unsupported session setting: {normalized}"
    );
    Ok(None)
}

// Preserve temporal precision when an evaluated value is rebound later.
fn binding_value(value: Value) -> Result<Value> {
    if let Value::Time64(unit, value) = value {
        let value = ticks(unit, value)?;
        let seconds = value / 10_000_000;
        return Ok(Value::Text(format!(
            "{:02}:{:02}:{:02}.{:07}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60,
            value % 10_000_000
        )));
    }
    Ok(value)
}

fn select_assignments(
    statement: &mut Statement,
    parameters: &HashMap<String, Parameter>,
) -> Result<Vec<(String, SqlType)>> {
    let Statement::Query(query) = statement else {
        return Ok(vec![]);
    };
    let SetExpr::Select(select) = query.body.as_mut() else {
        return Ok(vec![]);
    };
    let is_assignment = |item: &SelectItem| {
        matches!(item,
        SelectItem::ExprWithAlias { alias, .. } if alias.quote_style.is_none() && alias.value.starts_with('@'))
    };
    if !select.projection.iter().any(is_assignment) {
        return Ok(vec![]);
    }
    ensure!(
        select.projection.iter().all(is_assignment),
        "A SELECT statement that assigns a value to a variable must not be combined with data-retrieval operations"
    );
    ensure!(
        select.into.is_none(),
        "unsupported SELECT assignment with INTO"
    );
    let mut assignments = Vec::new();
    for item in &mut select.projection {
        let SelectItem::ExprWithAlias { expr, alias } = item else {
            unreachable!()
        };
        let name = alias.value.to_lowercase();
        let data_type = parameters
            .get(&name)
            .ok_or_else(|| anyhow::anyhow!("Must declare the scalar variable {name}"))?
            .data_type;
        assignments.push((name, data_type));
        *item = SelectItem::UnnamedExpr(Expr::Cast {
            kind: CastKind::Cast,
            expr: Box::new(expr.clone()),
            data_type: crate::sql_type::ast(data_type),
            format: None,
        });
    }
    Ok(assignments)
}

// Preserve the known character family before expression translation erases it.
// Unknown function/column result types conservatively use the Unicode limit.
fn print_is_unicode(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    if let Some(kind) = msduck_sql::expression_metadata::storage::kind(expr, parameters, &|_| None)
        && let Ok(SqlType::Character(kind)) = crate::sql_type::declaration(&kind)
    {
        return matches!(
            kind.family(),
            msduck_core::character::Family::Nchar | msduck_core::character::Family::Nvarchar
        );
    }
    match expr {
        Expr::Value(value) => {
            matches!(value.value, sqlparser::ast::Value::NationalStringLiteral(_))
        }
        Expr::Identifier(id) => parameters
            .get(&id.value.to_lowercase())
            .is_none_or(|p| matches!(p.ast_type(), DataType::Nvarchar(_))),
        Expr::Nested(inner) => print_is_unicode(inner, parameters),
        Expr::Cast { data_type, .. } => matches!(data_type, DataType::Nvarchar(_)),
        Expr::BinaryOp { left, right, .. } => {
            print_is_unicode(left, parameters) || print_is_unicode(right, parameters)
        }
        _ => true,
    }
}
fn print_target(expr: &Expr, parameters: &HashMap<String, Parameter>) -> DataType {
    if print_is_unicode(expr, parameters) {
        DataType::Nvarchar(Some(CharacterLength::Max))
    } else {
        DataType::Varchar(None)
    }
}
pub fn error_number(message: &str) -> i32 {
    if message == msduck_sql::unary_operator::BIT_MINUS {
        return 8117;
    }
    if let Some(error) = runtime_diagnostic(message) {
        return error.number;
    }
    if message.starts_with("Types don't match between the anchor and the recursive part in column ")
        && message.ends_with("\".")
    {
        return 240;
    }
    if let Some(number) = msduck_sql::query_options::error_number(message) {
        return number;
    }
    if let Some(number) = msduck_sql::cte_columns::error_number(message) {
        return number;
    }
    if let Some(number) = msduck_sql::for_json::error_number(message) {
        return number;
    }
    if message == crate::string_escape::ARITY {
        return 174;
    }
    if message == crate::string_escape::NULL_FORMAT {
        return 8116;
    }
    if message
        .strip_prefix("Invalid Input Error: ")
        .unwrap_or(message)
        == crate::character_storage::TRUNCATED
    {
        return 8152;
    }
    if message.starts_with("Sequence Error: nextval: reached ")
        && message.contains("__msduck_identity_")
    {
        return 8115;
    }

    if message == crate::identity::EXPLICIT {
        return 544;
    }
    if message == crate::identity::UPDATE {
        return 8102;
    }
    if message == "Arithmetic overflow error converting IDENTITY seed." {
        return 8115;
    }

    if message.contains("is not supported by date function dateadd for data type")
        || message.contains("is not supported by date function datepart for data type")
        || message.contains("is not supported by date function datename for data type")
    {
        return 9810;
    }
    if message.contains(crate::datetime2_cast::CONVERSION) {
        return 241;
    }
    if message.starts_with("The reference to column ")
        && message.contains("argument to the NTILE function")
    {
        return 4195;
    }
    if message
        .strip_prefix("Invalid Input Error: ")
        .unwrap_or(message)
        == msduck_sql::generate_series::ZERO_STEP
    {
        return 4199;
    }
    if message.contains(crate::ntile::POSITIVE) {
        return 4116;
    }
    if message.contains(crate::ntile::TYPE) {
        return 4110;
    }
    if message.contains(crate::value_window::NEGATIVE_OFFSET) {
        return 8730;
    }
    if message == crate::window_placement::ERROR {
        return 4108;
    }
    if message == "Window element in OVER clause can not also be specified in WINDOW clause." {
        return 4123;
    }
    if let Some(number) = crate::window_frame::error_number(message) {
        return number;
    }
    if let Some(number) = crate::ranking::error_number(message) {
        return number;
    }
    if let Some(number) = crate::grouping::error_number(message) {
        return number;
    }
    if let Some(number) = crate::aggregate::error_number(message) {
        return number;
    }
    if message.starts_with("Operand data type bit is invalid for ")
        || message.starts_with("Operand data type sql_variant is invalid for ")
    {
        return 8117;
    }
    if let Some(number) = crate::merge::error_number(message) {
        return number;
    }
    if message
        == "Invalid Input Error: The offset specified in a OFFSET clause may not be negative."
    {
        return 10742;
    }
    if message
        == "Invalid Input Error: The number of rows provided for a FETCH clause must be greater then zero."
    {
        return 10744;
    }
    if message == "TOP cannot be combined with OFFSET/FETCH in the same query" {
        return 10741;
    }
    if message == "Invalid Input Error: A TOP or FETCH clause contains an invalid value." {
        return 1014;
    }
    if message == "SELECT INTO expressions require column names" {
        return 1038;
    }
    if message == "SELECT INTO column names must be unique" {
        return 2705;
    }
    if message.starts_with("The type of the first argument to NULLIF") {
        return 4151;
    }
    if message.starts_with("At least one of the arguments to COALESCE") {
        return 4127;
    }
    if message.starts_with("At least one of the result expressions in a CASE specification") {
        return 8133;
    }
    if message == "Case expressions may only be nested to level 10." {
        return 125;
    }
    if message.starts_with("Out of Range Error: Overflow in ")
        || message.starts_with("Out of Range Error: Overflow on abs(")
    {
        return 8115;
    }
    if message.starts_with("Constraint Error: Violates foreign key constraint")
        || message.starts_with("Constraint Error: CHECK constraint failed on table ")
    {
        return 547;
    }
    if message.starts_with(
        "Invalid Input Error: Arithmetic overflow error converting expression to data type ",
    ) {
        return 8115;
    }
    if message.contains(
        "Cannot construct data type date, some of the arguments have values which are not valid.",
    ) || message.contains(crate::timefromparts::INVALID)
        || message.contains(crate::datetime2fromparts::INVALID)
        || message.contains(crate::datetimeoffsetfromparts::INVALID)
    {
        return 289;
    }
    if message.contains(crate::datetime2fromparts::INVALID_SCALE)
        || message.contains(crate::timefromparts::INVALID_SCALE)
        || message.contains(crate::datetimeoffsetfromparts::INVALID_SCALE)
    {
        return 10760;
    }
    if message.contains(crate::datediff::OVERFLOW) {
        return 535;
    }
    if message.contains(crate::switchoffset::INVALID)
        || message.contains(crate::switchoffset::ATTACH_INVALID)
    {
        return 9812;
    }
    if message.contains(crate::switchoffset::OVERFLOW)
        || message.contains(crate::switchoffset::ATTACH_OVERFLOW)
    {
        return 9813;
    }
    if message.contains(crate::eomonth::OVERFLOW)
        || message.contains(crate::datetime2_add::OVERFLOW)
        || message.contains(crate::datetime2_add::OFFSET_OVERFLOW)
    {
        return 517;
    }
    if message.starts_with("Cannot truncate table") && message.contains("FOREIGN KEY") {
        return 4712;
    }

    if message.contains("Divide by zero error encountered") {
        return 8134;
    }
    if message.contains("non-boolean type") {
        return 4145;
    }
    if message.contains("must not be combined with data-retrieval") {
        return 141;
    }
    if message == crate::grouping_syntax::CONSTANT_ERROR {
        return 164;
    }
    if message == crate::grouping_syntax::ERROR {
        return 144;
    }
    if message.starts_with("The multi-part identifier ")
        && message.ends_with(" could not be bound.")
    {
        return 4104;
    }
    if message.starts_with("Invalid column name ") {
        return 207;
    }
    if message.contains("Must declare the scalar variable") {
        return 137;
    }
    if message.contains("has already been declared") {
        return 134;
    }
    if message.contains("More than one row returned by a subquery") {
        return 512;
    }
    if message.contains("Could not find prepared statement") {
        return 8179;
    }
    if message.contains("COMMIT has no corresponding") {
        return 3902;
    }
    if message.contains("ROLLBACK has no corresponding") {
        return 3903;
    }
    if message.contains("No transaction or savepoint") {
        return 6401;
    }
    if message.contains("does not exist") {
        208
    } else if message.contains("Duplicate key") || message.contains("PRIMARY KEY or UNIQUE") {
        2627
    } else if message.contains("NOT NULL constraint") {
        515
    } else if message.contains("Conversion Error") {
        245
    } else if message.contains("unsupported") {
        40515
    } else {
        50000
    }
}
/// Parse a batch; `sp_set_session_context` arguments bind by position.
fn parse_batch(sql: &str) -> Result<Vec<Statement>> {
    let mut statements = msduck_sql::batch::parse(sql)?;
    msduck_sql::session_function::positional_arguments(&mut statements);
    msduck_sql::session_function::validate_set_calls(&statements)?;
    msduck_sql::dialect::table_hints::normalize(&mut statements)?;
    Ok(statements)
}
/// A database name is a single identifier; server or other qualifiers are
/// not supported.
fn database_name(name: &ObjectName) -> Result<String> {
    match name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => Ok(ident.value.clone()),
        _ => bail!("unsupported database name {name}"),
    }
}
struct Translator<'a> {
    parameters: &'a HashMap<String, Parameter>,
    values: Vec<Value>,
    parameter_slots: HashMap<String, usize>,
    transactions: u32,
    options_mask: i32,
    transaction_doomed: bool,
    original_login: &'a str,
    /// Read once per translated statement for current-time functions.
    clock: msduck_sql::session_function::Clock,
    rowcount: u64,
    last_error: i32,
    caught_error: Option<&'a SqlError>,
    spid: i16,
}
fn max_length_type(kind: &DataType) -> bool {
    matches!(
        kind,
        DataType::Varchar(Some(CharacterLength::Max))
            | DataType::Nvarchar(Some(CharacterLength::Max))
            | DataType::Varbinary(Some(BinaryLength::Max))
    )
}
fn max_length_expr(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    match expr {
        Expr::Identifier(id) => parameters
            .get(&id.value.to_lowercase())
            .is_some_and(|p| max_length_type(&p.ast_type())),
        Expr::Cast { data_type, .. } => max_length_type(data_type),
        Expr::Nested(expr) => max_length_expr(expr, parameters),
        Expr::BinaryOp { left, right, .. } => {
            max_length_expr(left, parameters) || max_length_expr(right, parameters)
        }
        Expr::Value(value) => match &value.value {
            sqlparser::ast::Value::NationalStringLiteral(s) => s.encode_utf16().count() > 4000,
            sqlparser::ast::Value::SingleQuotedString(s) => s.len() > 8000,
            _ => false,
        },
        _ => false,
    }
}
pub(crate) fn money_expr(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    msduck_sql::expression_metadata::currency::kind(expr, parameters, &|_| None).is_some()
}
pub(crate) fn integral_expr(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    match expr {
        Expr::Identifier(id) => parameters
            .get(&id.value.to_lowercase())
            .is_some_and(|p| integral_type(&p.ast_type())),
        Expr::Value(value) => {
            matches!(&value.value, sqlparser::ast::Value::Number(n, _) if n.parse::<i32>().is_ok())
        }
        Expr::Nested(expr) | Expr::UnaryOp { expr, .. } => integral_expr(expr, parameters),
        Expr::Cast { data_type, .. } => integral_type(data_type),
        Expr::BinaryOp {
            left,
            op:
                BinaryOperator::Plus
                | BinaryOperator::Minus
                | BinaryOperator::Multiply
                | BinaryOperator::Divide
                | BinaryOperator::Modulo,
            right,
        } => integral_expr(left, parameters) && integral_expr(right, parameters),
        _ => crate::case_types::is_integral(expr, parameters),
    }
}
fn string_expr(expr: &Expr, parameters: &HashMap<String, Parameter>) -> bool {
    match expr {
        Expr::Cast { data_type, .. } => matches!(
            data_type,
            DataType::Varchar(_) | DataType::Nvarchar(_) | DataType::Char(_) | DataType::Text
        ),
        Expr::Identifier(id) => parameters
            .get(&id.value.to_lowercase())
            .is_some_and(|p| matches!(p.ast_type(), DataType::Varchar(_) | DataType::Nvarchar(_))),
        Expr::Value(value) => matches!(
            value.value,
            sqlparser::ast::Value::SingleQuotedString(_)
                | sqlparser::ast::Value::NationalStringLiteral(_)
        ),
        Expr::Nested(expr) => string_expr(expr, parameters),
        _ => crate::case_types::is_character(expr, parameters),
    }
}
pub(crate) fn integer_input(argument: Expr, target: &DataType, is_try: bool) -> Expr {
    // A proven integer source cannot be a Unicode carrier or SQL_VARIANT.
    // Dispatching it through a CASE macro repeats nested conditional ASTs.
    // Keep the existing numeric range check, with one source occurrence.
    let integer = crate::case_types::integer_rank(&argument, &Default::default());
    let mut result = if let Some(rank) = integer {
        let kind = match rank {
            0 => "UTINYINT",
            1 => "SMALLINT",
            2 => "INTEGER",
            _ => "BIGINT",
        };
        binary_function(
            "__msduck_integer_text",
            Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(argument),
                data_type: DataType::Varchar(None),
                format: None,
            },
            Expr::Value(sqlparser::ast::Value::SingleQuotedString(kind.into()).into()),
        )
    } else {
        unary_function("__msduck_integer_input", argument)
    };
    if let Expr::Function(f) = &mut result
        && let FunctionArguments::List(args) = &mut f.args
    {
        args.args.extend([
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                sqlparser::ast::Value::SingleQuotedString(target.to_string()).into(),
            ))),
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                sqlparser::ast::Value::Boolean(is_try).into(),
            ))),
        ]);
    }
    result
}
fn translate_type(kind: &mut DataType) -> ControlFlow<String> {
    if crate::variant_pack::is_variant(kind) {
        *kind = crate::variant_pack::storage_type();
        return ControlFlow::Continue(());
    }
    match crate::datetimeoffset_cast::scale(kind) {
        Ok(Some(scale)) => {
            *kind = crate::datetimeoffset_cast::storage_type(scale);
            return ControlFlow::Continue(());
        }
        Err(error) => return ControlFlow::Break(error),
        Ok(None) => {}
    }
    match crate::datetime2_cast::scale(kind) {
        Ok(Some(scale)) => {
            *kind = crate::datetime2_cast::storage_type(scale);
            return ControlFlow::Continue(());
        }
        Err(error) => return ControlFlow::Break(error),
        Ok(None) => {}
    }
    match kind {
        DataType::Float(info) => {
            *kind = match info {
                ExactNumberInfo::None | ExactNumberInfo::Precision(25..=53) => {
                    DataType::Double(ExactNumberInfo::None)
                }
                ExactNumberInfo::Precision(1..=24) => DataType::Real,
                _ => return ControlFlow::Break("FLOAT precision must be between 1 and 53".into()),
            };
        }
        DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("money") => {
            *kind = DataType::Decimal(ExactNumberInfo::PrecisionAndScale(19, 4));
        }
        DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("smallmoney") => {
            *kind = DataType::Decimal(ExactNumberInfo::PrecisionAndScale(10, 4));
        }
        DataType::Nvarchar(_) | DataType::Varchar(_) => *kind = DataType::Varchar(None),
        DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("uniqueidentifier") => {
            *kind = DataType::Uuid;
        }
        DataType::Bit(_) => *kind = DataType::Boolean,
        DataType::Datetime(_) => *kind = DataType::Timestamp(None, TimezoneInfo::None),
        DataType::Custom(name, _) if name.to_string().eq_ignore_ascii_case("smalldatetime") => {
            *kind = DataType::Timestamp(None, TimezoneInfo::None);
        }
        DataType::Varbinary(_) | DataType::Binary(_) => *kind = DataType::Blob(None),
        DataType::TinyInt(_) => *kind = DataType::UTinyInt,
        DataType::Time(scale, TimezoneInfo::None) if scale.is_none_or(|s| s <= 7) => {
            *kind = DataType::Custom(ObjectName::from(vec![Ident::new("TIME_NS")]), vec![]);
        }
        _ => {}
    }
    ControlFlow::Continue(())
}
impl VisitorMut for Translator<'_> {
    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<String> {
        msduck_sql::dialect::table_hints::clear(factor);
        if let Err(error) = msduck_sql::generate_series::lower(factor, self.parameters) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::openjson::lower(factor) {
            return ControlFlow::Break(error);
        }
        ControlFlow::Continue(())
    }
    type Break = String;
    fn pre_visit_ident(&mut self, ident: &mut Ident) -> ControlFlow<String> {
        if ident.quote_style == Some('[') {
            ident.quote_style = Some('"');
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_statement(&mut self, stmt: &mut Statement) -> ControlFlow<String> {
        // Stored definitions are evaluated when used, not when defined.
        if matches!(
            stmt,
            Statement::CreateTable(_)
                | Statement::AlterTable(_)
                | Statement::CreateView(_)
                | Statement::AlterView { .. }
        ) {
            let offset = self.clock.offset_minutes;
            visit_expressions_mut(stmt, |expr| {
                match expr {
                    Expr::Function(function) => {
                        if let Some(kind) = msduck_sql::session_function::current_time(function) {
                            *expr =
                                msduck_sql::session_function::current_time_runtime(kind, offset);
                        } else if msduck_sql::session_function::is_login_name(function) {
                            return ControlFlow::Break(format!(
                                "unsupported {} in a stored definition",
                                function.name
                            ));
                        }
                    }
                    Expr::Identifier(id)
                        if id.quote_style.is_none()
                            && msduck_sql::session_function::is_system_user(&id.value) =>
                    {
                        return ControlFlow::Break(
                            "unsupported SYSTEM_USER in a stored definition".into(),
                        );
                    }
                    _ => {}
                }
                ControlFlow::Continue(())
            })?;
        }
        if let Statement::Insert(insert) = stmt {
            // INTO is optional in T-SQL but required by DuckDB.
            insert.into = true;
        }
        // A MERGE can be nested in a WITH query body, which otherwise passes
        // the top-level Query allowlist. Keep its execution boundary uniform.
        if matches!(stmt, Statement::Merge(_)) {
            return ControlFlow::Break("unsupported MERGE execution".into());
        }
        if let Statement::CreateView(view) = stmt
            && view.or_alter
        {
            view.or_alter = false;
            view.or_replace = true;
        }
        if let Statement::AlterTable(table) = stmt {
            for operation in &mut table.operations {
                if let AlterTableOperation::AlterColumn {
                    op: AlterColumnOperation::SetDataType { data_type, .. },
                    ..
                } = operation
                {
                    if let DataType::Time(Some(scale), _) = data_type
                        && *scale > 7
                    {
                        return ControlFlow::Break("invalid time scale".into());
                    }
                    if let Some(storage) = crate::character_storage::unicode_storage_type(data_type)
                    {
                        *data_type = storage;
                    } else {
                        translate_type(data_type)?;
                    }
                }
                if let AlterTableOperation::AddColumn { column_def, .. } = operation {
                    if let Err(error) = crate::declared_columns::lower_collation(column_def) {
                        return ControlFlow::Break(error);
                    }
                    msduck_sql::money_cast::column(column_def);
                    if let Err(error) = crate::character_storage::column(column_def) {
                        return ControlFlow::Break(error);
                    }
                    crate::variant_pack::column(column_def);
                    if let Err(error) = crate::datetimeoffset_cast::column(column_def) {
                        return ControlFlow::Break(error);
                    }
                    if let Err(error) = crate::datetime2_cast::column(column_def) {
                        return ControlFlow::Break(error);
                    }
                    if let DataType::Time(Some(scale), _) = column_def.data_type
                        && scale > 7
                    {
                        return ControlFlow::Break("invalid time scale".into());
                    }
                    translate_type(&mut column_def.data_type)?;
                }
            }
        }
        if let Statement::CreateTable(table) = stmt {
            for col in &mut table.columns {
                // A computed column takes DuckDB's type for its lowered
                // expression; its declaration is recorded separately.
                if msduck_sql::dialect::computed_column::computed(col).is_some() {
                    col.data_type = DataType::Unspecified;
                    msduck_sql::dialect::computed_column::lower(col);
                    continue;
                }
                if let Err(error) = crate::declared_columns::lower_collation(col) {
                    return ControlFlow::Break(error);
                }
                msduck_sql::money_cast::column(col);
                if let Err(error) = crate::character_storage::column(col) {
                    return ControlFlow::Break(error);
                }
                crate::variant_pack::column(col);
                if let Err(error) = crate::datetimeoffset_cast::column(col) {
                    return ControlFlow::Break(error);
                }
                if let Err(error) = crate::datetime2_cast::column(col) {
                    return ControlFlow::Break(error);
                }
                if let DataType::Time(scale, _) = &col.data_type
                    && scale.is_some_and(|s| s > 7)
                {
                    return ControlFlow::Break("invalid time scale".into());
                }
                translate_type(&mut col.data_type)?;
            }
        }
        ControlFlow::Continue(())
    }
    fn post_visit_statement(&mut self, stmt: &mut Statement) -> ControlFlow<String> {
        // Key columns of PRIMARY KEY and UNIQUE constraints are parsed as
        // ordering expressions, but DuckDB accepts neither ASC/DESC nor the
        // NULLS placement added to ORDER BY. The index key order has no effect
        // on results.
        let constraints: Vec<&mut TableConstraint> = match stmt {
            Statement::CreateTable(table) => table.constraints.iter_mut().collect(),
            Statement::AlterTable(table) => table
                .operations
                .iter_mut()
                .filter_map(|operation| match operation {
                    AlterTableOperation::AddConstraint { constraint, .. } => Some(constraint),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        };
        for constraint in constraints {
            let columns = match constraint {
                TableConstraint::PrimaryKey(key) => &mut key.columns,
                TableConstraint::Unique(key) => &mut key.columns,
                _ => continue,
            };
            for column in columns {
                column.column.options = OrderByOptions {
                    sort: None,
                    nulls_first: None,
                };
            }
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_order_by_expr(&mut self, order: &mut OrderByExpr) -> ControlFlow<String> {
        if order.options.nulls_first.is_none() {
            order.options.nulls_first =
                Some(!matches!(order.options.sort, Some(OrderBySort::Desc)));
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_select(&mut self, select: &mut Select) -> ControlFlow<String> {
        if let Err(error) = crate::named_windows::select(select) {
            return ControlFlow::Break(error);
        }
        for item in &select.projection {
            if let SelectItem::ExprWithAlias { alias, .. } = item
                && alias.quote_style.is_none()
                && alias.value.starts_with('@')
            {
                return ControlFlow::Break("unsupported SELECT variable assignment".into());
            }
            if let SelectItem::UnnamedExpr(Expr::BinaryOp {
                left,
                op: BinaryOperator::Eq,
                ..
            }) = item
                && matches!(left.as_ref(), Expr::Identifier(id) if id.value.starts_with('@') && !id.value.starts_with("@@"))
            {
                return ControlFlow::Break("unsupported SELECT variable assignment".into());
            }
        }
        for table in &mut select.from {
            crate::apply::lower(table);
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<String> {
        if let Err(error) = crate::named_windows::query(query) {
            return ControlFlow::Break(error);
        }
        crate::top::wrap_set_branches(&mut query.body);
        if let Err(error) = crate::top::paging(query) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::top::ranked(query) {
            return ControlFlow::Break(error);
        }
        if let SetExpr::Select(select) = query.body.as_mut()
            && let Some(top) = select.top.take()
        {
            if query.limit_clause.is_some() || query.fetch.is_some() {
                return ControlFlow::Break(
                    "TOP cannot be combined with OFFSET/FETCH in the same query".into(),
                );
            }
            let limit = match top.quantity {
                Some(TopQuantity::Constant(n)) => number(n),
                Some(TopQuantity::Expr(e)) => e,
                None => return ControlFlow::Break("TOP requires a count".into()),
            };
            query.limit_clause = Some(LimitClause::LimitOffset {
                // DuckDB folds NULL scalar inputs without invoking native code.
                // A negative sentinel makes NULL fail the same count validator.
                limit: Some(unary_function(
                    "__msduck_top_count",
                    binary_function("coalesce", limit, number(-1)),
                )),
                offset: None,
                limit_by: vec![],
            });
        }
        ControlFlow::Continue(())
    }
    fn post_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
        crate::grouping::lower(expr);
        crate::variant_pack::canonicalize(expr);
        crate::case_types::lower_literal(expr);
        crate::datetime2_results::lower(expr);
        crate::variant_results::lower(expr);
        crate::datetimeoffset_compare::lower(expr);
        crate::datetime2_compare::lower(expr);
        crate::variant_compare::lower(expr);
        crate::datetime2_date::lower(expr);
        // Parameter replacement can introduce a cast after the pre-visit pass.
        if let Err(error) = crate::datetimeoffset_cast::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datetime2_cast::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Expr::Convert {
            expr: value,
            data_type: Some(kind @ DataType::Time(_, TimezoneInfo::None)),
            is_try,
            styles,
            charset: None,
            ..
        } = expr
            && styles.is_empty()
        {
            *expr = Expr::Cast {
                kind: if *is_try {
                    CastKind::TryCast
                } else {
                    CastKind::Cast
                },
                expr: value.clone(),
                data_type: kind.clone(),
                format: None,
            };
        }
        if let Expr::Cast {
            data_type: DataType::Time(scale, TimezoneInfo::None),
            ..
        } = expr
        {
            let scale = scale.unwrap_or(7);
            if scale > 7 {
                return ControlFlow::Break("invalid time scale".into());
            }
            if let Expr::Cast {
                expr: argument,
                data_type,
                kind,
                ..
            } = expr
            {
                if matches!(argument.as_ref(), Expr::Value(v) if matches!(v.value, sqlparser::ast::Value::Placeholder(_)))
                {
                    // Keep concrete typing on directly bound TIME parameters.
                    translate_type(data_type)?;
                } else {
                    *expr = unary_function(
                        if matches!(kind, CastKind::TryCast | CastKind::SafeCast) {
                            "__msduck_try_time"
                        } else {
                            "__msduck_cast_time"
                        },
                        *argument.clone(),
                    );
                }
            }
            // Parse a fixed function skeleton, then attach the already-visited
            // expression as an AST node: values remain bound exactly once.
            let parsed = Parser::new(&sqlparser::dialect::GenericDialect {})
                .try_with_sql("__msduck_time_round(NULL, NULL)")
                .and_then(|mut parser| parser.parse_expr());
            let Ok(Expr::Function(mut function)) = parsed else {
                return ControlFlow::Break("failed to construct time conversion".into());
            };
            if let FunctionArguments::List(args) = &mut function.args {
                args.args = vec![
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(expr.clone())),
                    FunctionArg::Unnamed(FunctionArgExpr::Expr(number(
                        10u64.pow(9 - scale as u32),
                    ))),
                ];
            }
            *expr = Expr::Function(function);
        }
        if let Expr::Function(function) = expr {
            if function.name.to_string().eq_ignore_ascii_case("COUNT") {
                *expr = Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(expr.clone()),
                    data_type: DataType::Int(None),
                    format: None,
                };
            } else if function.name.to_string().eq_ignore_ascii_case("COUNT_BIG") {
                function.name = ObjectName::from(vec![Ident::new("count")]);
            }
        }
        if let Err(error) = ext::lower_expr(expr) {
            return ControlFlow::Break(error);
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<String> {
        // A typed literal of the statement's clock, lowered below like any cast.
        if let Expr::Function(function) = expr
            && let Some(kind) = msduck_sql::session_function::current_time(function)
        {
            *expr = msduck_sql::session_function::current_time_value(kind, self.clock);
        }
        // Fold proven constant percentile sources before child casts become native adapters.
        rand::seed_conversion(expr, self.parameters);
        if let Err(error) = crate::percentile::lower(expr) {
            return ControlFlow::Break(error);
        }
        msduck_sql::expr::lower_unary_plus(expr);
        if let Err(error) = crate::concat_lower::lower(expr, self.parameters) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::unicode_case::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datalength::lower(expr, self.parameters, &|_| None) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::string_escape::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::json_extract::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::isjson::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datetimeoffsetfromparts::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::timefromparts::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datetime2fromparts::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = msduck_sql::nullif::lower_currency(expr, self.parameters, &|_| None) {
            return ControlFlow::Break(error);
        }
        msduck_sql::money_arithmetic::lower(expr, self.parameters, &|_| None);
        msduck_sql::decimal_division::lower(expr, self.parameters, &|_| None);
        if let Err(error) = msduck_sql::left_right::lower(expr, self.parameters, &|_| None) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = msduck_sql::replicate::lower(expr, self.parameters, &|_| None) {
            return ControlFlow::Break(error);
        }
        msduck_sql::money_compare::lower(expr, self.parameters, &|_| None);
        msduck_sql::money_results::lower(expr, self.parameters, &|_| None);
        if let Err(error) = crate::isnull::lower(expr) {
            return ControlFlow::Break(error);
        }
        crate::result_types::lower_fixed_results(expr);
        if let Err(error) = crate::type_catalog::lower(expr) {
            return ControlFlow::Break(error);
        }
        crate::variant_pack::lower(expr);
        if let Err(error) = crate::variant::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::column_catalog::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::object_catalog::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::schema_catalog::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::identity_metadata::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = msduck_sql::money_format::lower(expr, self.parameters, &|_| None) {
            return ControlFlow::Break(error);
        }
        if matches!(expr, Expr::Cast { expr: source, .. }
            if msduck_sql::projection::character_extrema::bound_result(source))
        {
            crate::concat_lower::annotated_unicode_casts(expr);
        }
        if let Err(error) = crate::varchar::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::nvarchar::lower(expr) {
            return ControlFlow::Break(error);
        }
        // ISNULL(NULL, replacement) can expose a DATETIME2 cast at this node.
        if let Err(error) = crate::datetimeoffset_cast::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datetime2_cast::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::aggregate::mark(expr, self.parameters) {
            return ControlFlow::Break(error);
        }
        if msduck_sql::projection::character_extrema::lower(expr) {
            crate::concat_lower::annotated_unicode_casts(expr);
            let Expr::Function(function) = expr else {
                unreachable!()
            };
            let ansi = function.name.to_string().ends_with("_ansi");
            let FunctionArguments::List(args) = &mut function.args else {
                unreachable!()
            };
            let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = &mut args.args[0] else {
                unreachable!()
            };
            fn operand(value: Expr) -> Expr {
                match value {
                    Expr::Nested(value) | Expr::Collate { expr: value, .. } => operand(*value),
                    value => msduck_sql::expr::unary_function("__msduck_carrier_input", value),
                }
            }
            *value = operand(value.clone());
            if ansi {
                *expr = msduck_sql::expr::binary_function(
                    "__msduck_cast_carrier_varchar",
                    expr.clone(),
                    msduck_sql::expr::number(-1),
                );
            }
        }
        if let Err(error) = crate::percentile::lower(expr) {
            return ControlFlow::Break(error);
        }
        crate::ntile::lower(expr);
        if let Err(error) = crate::value_window::lower(expr, self.parameters) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::switchoffset::lower(expr, self.parameters) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datediff::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::dateadd::lower(expr, self.parameters) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::eomonth::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::datepart::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::calendar_parts::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::ncharacter::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::character::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::space::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::unicode::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::nullif::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Err(error) = crate::choose::lower(expr) {
            return ControlFlow::Break(error);
        }
        if let Expr::Function(function) = expr {
            match crate::predicate::iif_args(function) {
                Ok(Some([test, yes, no])) => {
                    *expr = Expr::Case {
                        case_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                        end_token: sqlparser::ast::helpers::attached_token::AttachedToken::empty(),
                        operand: None,
                        conditions: vec![CaseWhen {
                            condition: test.clone(),
                            result: yes.clone(),
                        }],
                        else_result: Some(Box::new(no.clone())),
                    };
                }
                Err(error) => return ControlFlow::Break(error),
                Ok(None) => {}
            }
        }
        crate::case_types::lower(expr, self.parameters);
        msduck_sql::money_cast::lower(expr);
        if let Expr::UnaryOp {
            op: UnaryOperator::BitwiseNot,
            expr: value,
        } = expr
        {
            *expr = unary_function("__msduck_bitnot", *value.clone());
        }
        if let Expr::Convert {
            is_try,
            expr: argument,
            data_type: Some(kind),
            charset: None,
            styles,
            ..
        } = expr
            && (integral_type(kind)
                || matches!(kind, DataType::Bit(_) | DataType::Datetime(_))
                || matches!(kind, DataType::Custom(name,_) if name.to_string().eq_ignore_ascii_case("smalldatetime")))
            && styles.is_empty()
        {
            *expr = Expr::Cast {
                kind: if *is_try {
                    CastKind::TryCast
                } else {
                    CastKind::Cast
                },
                expr: argument.clone(),
                data_type: kind.clone(),
                format: None,
            };
        }
        if let Expr::BinaryOp { left, op, right } = expr {
            if matches!(
                op,
                BinaryOperator::Plus
                    | BinaryOperator::Minus
                    | BinaryOperator::Multiply
                    | BinaryOperator::Divide
                    | BinaryOperator::Modulo
            ) {
                crate::case_types::integer_comparison(left, right, self.parameters);
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
                crate::case_types::integer_comparison(left, right, self.parameters);
            } else if *op == BinaryOperator::Plus
                && string_expr(left, self.parameters)
                && string_expr(right, self.parameters)
            {
                *op = BinaryOperator::StringConcat;
            } else if matches!(
                op,
                BinaryOperator::BitwiseAnd | BinaryOperator::BitwiseOr | BinaryOperator::BitwiseXor
            ) {
                let name = match op {
                    BinaryOperator::BitwiseAnd => "__msduck_bitand",
                    BinaryOperator::BitwiseOr => "__msduck_bitor",
                    _ => "__msduck_bitxor",
                };
                *expr = binary_function(name, *left.clone(), *right.clone());
            } else if matches!(op, BinaryOperator::Divide | BinaryOperator::Modulo)
                && integral_expr(left, self.parameters)
                && integral_expr(right, self.parameters)
            {
                let name = if *op == BinaryOperator::Divide {
                    "__msduck_int_div"
                } else {
                    "__msduck_int_mod"
                };
                *expr = binary_function(name, *left.clone(), *right.clone());
            }
        }
        match expr {
            Expr::Identifier(id)
                if id.quote_style.is_none()
                    && msduck_sql::session_function::is_system_user(&id.value) =>
            {
                *expr = msduck_sql::session_function::system_user(self.original_login);
            }
            Expr::Identifier(id) if id.quote_style.is_none() && id.value.starts_with("@@") => {
                *expr = match id.value.to_uppercase().as_str() {
                    "@@DATEFIRST" => Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(unary_function(
                            "getvariable",
                            Expr::Value(
                                sqlparser::ast::Value::SingleQuotedString(
                                    "__msduck_datefirst".into(),
                                )
                                .into(),
                            ),
                        )),
                        data_type: DataType::UTinyInt,
                        format: None,
                    },
                    "@@TRANCOUNT" => number(self.transactions),
                    "@@OPTIONS" => Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(number(self.options_mask)),
                        data_type: DataType::Int(None),
                        format: None,
                    },
                    "@@SPID" => Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(number(self.spid)),
                        data_type: DataType::SmallInt(None),
                        format: None,
                    },
                    "@@ROWCOUNT" => number(self.rowcount),
                    "@@ERROR" => number(self.last_error),
                    "@@VERSION" => Expr::Value(
                        sqlparser::ast::Value::SingleQuotedString(
                            "Microsoft SQL Server compatible msduck (DuckDB)".into(),
                        )
                        .into(),
                    ),
                    _ => return ControlFlow::Break(format!("unsupported global {}", id.value)),
                };
            }
            Expr::Identifier(id) if id.quote_style.is_none() && id.value.starts_with('@') => {
                let name = id.value.to_lowercase();
                let Some(value) = self.parameters.get(&name) else {
                    return ControlFlow::Break(format!("Must declare the scalar variable {name}"));
                };
                let slot = if let Some(slot) = self.parameter_slots.get(&name) {
                    *slot
                } else {
                    let bound = match crate::backend_value::to_backend(&value.value) {
                        Ok(bound) => bound,
                        Err(error) => return ControlFlow::Break(error.to_string()),
                    };
                    self.values.push(bound);
                    let slot = self.values.len();
                    self.parameter_slots.insert(name, slot);
                    slot
                };
                if matches!(value.value, ParameterValue::Unicode(_)) {
                    *expr = unary_function(
                        "__msduck_unicode_from_le",
                        Expr::Value(sqlparser::ast::Value::Placeholder(format!("${slot}")).into()),
                    );
                    return ControlFlow::Continue(());
                }
                *expr = Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(Expr::Value(
                        sqlparser::ast::Value::Placeholder(format!("${slot}")).into(),
                    )),
                    data_type: {
                        let mut kind = value.ast_type();
                        if !matches!(
                            value.data_type,
                            SqlType::Time(_) | SqlType::DateTime2(_) | SqlType::DateTimeOffset(_)
                        ) {
                            translate_type(&mut kind)?;
                        }
                        kind
                    },
                    format: None,
                };
            }
            Expr::Value(v) => {
                if let sqlparser::ast::Value::HexStringLiteral(s) = &v.value {
                    *expr = unary_function(
                        "from_hex",
                        Expr::Value(sqlparser::ast::Value::SingleQuotedString(s.clone()).into()),
                    );
                    return ControlFlow::Continue(());
                }
                if let sqlparser::ast::Value::NationalStringLiteral(s) = &v.value {
                    v.value = sqlparser::ast::Value::SingleQuotedString(s.clone());
                }
            }
            Expr::Trim {
                expr: source,
                trim_where,
                trim_what,
                trim_characters,
            } => {
                if trim_characters.is_some() {
                    return ControlFlow::Break("unsupported TRIM argument syntax".into());
                }
                if let Some(characters) = trim_what {
                    if max_length_expr(characters, self.parameters) {
                        return ControlFlow::Break("TRIM characters cannot have a MAX type".into());
                    }
                } else {
                    *trim_what = Some(Box::new(Expr::Value(
                        sqlparser::ast::Value::SingleQuotedString(" ".into()).into(),
                    )));
                }
                let name = match trim_where {
                    Some(TrimWhereField::Leading) => "__msduck_ltrim",
                    Some(TrimWhereField::Trailing) => "__msduck_rtrim",
                    _ => "__msduck_trim",
                };
                *expr = msduck_sql::expr::binary_function(
                    name,
                    crate::concat_lower::trim_input(*source.clone(), self.parameters),
                    *trim_what.clone().unwrap(),
                );
            }
            Expr::Cast {
                expr: argument,
                data_type,
                kind,
                ..
            } if !matches!(data_type, DataType::Time(..)) => {
                let explicit = crate::variant_cast::take(argument);
                if explicit && matches!(data_type, DataType::Bit(_)) {
                    **argument = unary_function(
                        if matches!(kind, CastKind::TryCast | CastKind::SafeCast) {
                            "__msduck_explicit_try_bit"
                        } else {
                            "__msduck_explicit_bit"
                        },
                        *argument.clone(),
                    );
                }
                if integral_type(data_type) && !money_expr(argument, self.parameters) {
                    **argument = integer_input(
                        *argument.clone(),
                        data_type,
                        matches!(kind, CastKind::TryCast | CastKind::SafeCast),
                    );
                    if explicit
                        && let Expr::Function(f) = argument.as_mut()
                        && f.name.to_string() == "__msduck_integer_input"
                    {
                        f.name =
                            ObjectName::from(vec![Ident::new("__msduck_explicit_integer_input")]);
                    }
                }
                translate_type(data_type)?
            }
            Expr::Function(f) => {
                let name = f.name.to_string().to_uppercase();
                if name == "DATEFROMPARTS" {
                    if !matches!(&f.args, FunctionArguments::List(args)
                        if args.args.len() == 3 && args.duplicate_treatment.is_none()
                            && args.clauses.is_empty()
                            && args.args.iter().all(|arg| matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(_)))))
                        || !matches!(f.parameters, FunctionArguments::None)
                        || f.over.is_some()
                        || f.filter.is_some()
                        || !f.within_group.is_empty()
                        || f.null_treatment.is_some()
                    {
                        return ControlFlow::Break(
                            "DATEFROMPARTS requires three scalar arguments".into(),
                        );
                    }
                    f.name = ObjectName::from(vec![Ident::new("__msduck_datefromparts")]);
                    if let FunctionArguments::List(args) = &mut f.args {
                        for argument in &mut args.args {
                            if let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = argument {
                                *value = Expr::Cast {
                                    kind: CastKind::Cast,
                                    expr: Box::new(value.clone()),
                                    data_type: DataType::Int(None),
                                    format: None,
                                };
                            }
                        }
                    }
                    return ControlFlow::Continue(());
                }
                if matches!(name.as_str(), "LTRIM" | "RTRIM") {
                    let FunctionArguments::List(args) = &mut f.args else {
                        return ControlFlow::Break(
                            "trim function requires scalar arguments".into(),
                        );
                    };
                    if !(1..=2).contains(&args.args.len())
                        || args.duplicate_treatment.is_some()
                        || !args.clauses.is_empty()
                        || f.over.is_some()
                        || f.filter.is_some()
                        || !f.within_group.is_empty()
                        || f.null_treatment.is_some()
                        || !matches!(f.parameters, FunctionArguments::None)
                        || !args.args.iter().all(|arg| {
                            matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(_)))
                        })
                    {
                        return ControlFlow::Break(
                            "trim function requires one or two scalar arguments".into(),
                        );
                    }
                    if args.args.len() == 1 {
                        args.args
                            .push(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(
                                sqlparser::ast::Value::SingleQuotedString(" ".into()).into(),
                            ))));
                    } else if let FunctionArg::Unnamed(FunctionArgExpr::Expr(characters)) =
                        &args.args[1]
                        && max_length_expr(characters, self.parameters)
                    {
                        return ControlFlow::Break("trim characters cannot have a MAX type".into());
                    }
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(source)) = &mut args.args[0] {
                        *source = crate::concat_lower::trim_input(source.clone(), self.parameters);
                    }
                    f.name = ObjectName::from(vec![Ident::new(if name == "LTRIM" {
                        "__msduck_ltrim"
                    } else {
                        "__msduck_rtrim"
                    })]);
                    return ControlFlow::Continue(());
                }
                if name == "LEN" {
                    let FunctionArguments::List(args) = &f.args else {
                        return ControlFlow::Break("LEN requires one argument".into());
                    };
                    if args.args.len() != 1
                        || args.duplicate_treatment.is_some()
                        || !args.clauses.is_empty()
                        || f.over.is_some()
                        || f.filter.is_some()
                        || !f.within_group.is_empty()
                        || f.null_treatment.is_some()
                        || !matches!(f.parameters, FunctionArguments::None)
                    {
                        return ControlFlow::Break("LEN requires one scalar argument".into());
                    }
                    let FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)) = &args.args[0] else {
                        return ControlFlow::Break("LEN requires one scalar argument".into());
                    };
                    let large = max_length_expr(arg, self.parameters);
                    f.name = ObjectName::from(vec![Ident::new("__msduck_len")]);
                    *expr = Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(Expr::Function(f.clone())),
                        data_type: if large {
                            DataType::BigInt(None)
                        } else {
                            DataType::Int(None)
                        },
                        format: None,
                    };
                    return ControlFlow::Continue(());
                }
                if name == "NEWID" {
                    if !matches!(&f.args, FunctionArguments::List(args)
                        if args.args.is_empty() && args.duplicate_treatment.is_none() && args.clauses.is_empty())
                        || !matches!(f.parameters, FunctionArguments::None)
                        || f.over.is_some()
                        || f.filter.is_some()
                        || !f.within_group.is_empty()
                        || f.null_treatment.is_some()
                    {
                        return ControlFlow::Break(
                            "NEWID requires no arguments or aggregate clauses".into(),
                        );
                    }
                    // Keep this as a volatile database expression, including
                    // column defaults; never generate a constant at translation.
                    f.name = ObjectName::from(vec![Ident::new("uuid")]);
                    return ControlFlow::Continue(());
                }

                if matches!(
                    name.as_str(),
                    "ERROR_NUMBER"
                        | "ERROR_STATE"
                        | "ERROR_MESSAGE"
                        | "ERROR_SEVERITY"
                        | "ERROR_LINE"
                        | "ERROR_PROCEDURE"
                ) {
                    if !matches!(&f.args, FunctionArguments::List(args) if args.args.is_empty()) {
                        return ControlFlow::Break("error functions take no arguments".into());
                    }
                    let data_type =
                        msduck_sql::session_function::error_type(f).expect("known error function");
                    if name == "ERROR_MESSAGE"
                        && let Some(units) =
                            self.caught_error.and_then(|e| e.message_utf16.as_ref())
                    {
                        self.values.push(duckdb::types::Value::Blob(
                            units.iter().flat_map(|u| u.to_le_bytes()).collect(),
                        ));
                        *expr = binary_function(
                            "__msduck_cast_carrier_nvarchar",
                            unary_function(
                                "__msduck_unicode_from_le",
                                Expr::Value(
                                    sqlparser::ast::Value::Placeholder(format!(
                                        "${}",
                                        self.values.len()
                                    ))
                                    .into(),
                                ),
                            ),
                            number(4000),
                        );
                        return ControlFlow::Continue(());
                    }
                    let value = match (self.caught_error, name.as_str()) {
                        (Some(error), "ERROR_NUMBER") => number(error.number),
                        (Some(error), "ERROR_STATE") => number(error.state),
                        (Some(error), "ERROR_MESSAGE") => Expr::Value(
                            sqlparser::ast::Value::NationalStringLiteral(error.message.clone())
                                .into(),
                        ),
                        (Some(error), "ERROR_SEVERITY") => number(error.severity),
                        (Some(_), "ERROR_LINE") => number(1),
                        _ => Expr::Value(sqlparser::ast::Value::Null.into()),
                    };
                    *expr = Expr::Cast {
                        kind: CastKind::Cast,
                        expr: Box::new(value),
                        data_type,
                        format: None,
                    };
                    return ControlFlow::Continue(());
                }
                if msduck_sql::session_function::is_xact_state(f) {
                    *expr = msduck_sql::session_function::xact_state(self.transactions > 0);
                    if self.transaction_doomed
                        && let Expr::Cast { expr, .. } = expr
                    {
                        **expr = number(-1);
                    }
                } else if msduck_sql::session_function::is_original_login(f) {
                    *expr = msduck_sql::session_function::original_login(self.original_login);
                } else if msduck_sql::session_function::is_login_name(f) {
                    *expr = msduck_sql::session_function::login_name(f, self.original_login);
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}
fn wire_type(kind: &ArrowType) -> Result<Type> {
    if crate::unicode_carrier::is_arrow(kind) {
        return Ok(Type::Text);
    }
    if crate::variant::is_arrow(kind) {
        return Ok(Type::Variant);
    }
    if let Some(scale) = crate::datetimeoffset_cast::arrow_scale(kind) {
        return Ok(Type::DateTimeOffset(scale));
    }
    if let Some(scale) = crate::datetime2_cast::arrow_scale(kind) {
        return Ok(Type::DateTime2(scale));
    }
    Ok(match kind {
        ArrowType::Int8 => Type::Int(2),
        ArrowType::UInt8 => Type::Int(1),
        ArrowType::Int16 => Type::Int(2),
        ArrowType::Int32 => Type::Int(4),
        ArrowType::Int64 => Type::Int(8),
        ArrowType::Boolean => Type::Bit,
        ArrowType::Float32 => Type::Float(4),
        ArrowType::Float64 => Type::Float(8),
        ArrowType::Utf8 | ArrowType::LargeUtf8 => Type::Text,
        ArrowType::Binary | ArrowType::LargeBinary => Type::Binary,
        ArrowType::Decimal128(p, s) if *s >= 0 => Type::Decimal(*p, *s as u8),
        ArrowType::Date32 => Type::Date,
        ArrowType::Time64(_) => Type::Time(7),
        ArrowType::Timestamp(_, None) => Type::DateTime,
        _ => bail!("unsupported result type {kind:?}"),
    })
}
// Read Arrow directly: duckdb-rs Row::get does not support TIME_NS and panics.
fn is_bool_field(field: &duckdb::arrow::datatypes::Field) -> bool {
    field.data_type() == &ArrowType::Boolean
        || (field.data_type() == &ArrowType::Int8
            && field
                .metadata()
                .get("ARROW:extension:name")
                .is_some_and(|name| name == "arrow.bool8"))
}
fn is_uuid_field(field: &duckdb::arrow::datatypes::Field) -> bool {
    field.data_type() == &ArrowType::FixedSizeBinary(16)
        && field
            .metadata()
            .get("ARROW:extension:name")
            .is_some_and(|name| name == "arrow.uuid")
}
fn arrow_value(
    array: &dyn duckdb::arrow::array::Array,
    row: usize,
    field: &duckdb::arrow::datatypes::Field,
) -> Result<Value> {
    use duckdb::arrow::{array::*, datatypes::TimeUnit as Unit};
    if array.is_null(row) {
        return Ok(Value::Null);
    }
    if crate::unicode_carrier::is_arrow(array.data_type()) {
        return crate::unicode_carrier::read(array, row);
    }
    if crate::variant::is_arrow(array.data_type()) {
        return crate::variant::read(array, row);
    }
    if let Some(scale) = crate::datetimeoffset_cast::arrow_scale(array.data_type()) {
        let values = array
            .as_any()
            .downcast_ref::<StructArray>()
            .ok_or_else(|| anyhow::anyhow!("invalid DATETIMEOFFSET array"))?;
        let ticks = values
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| anyhow::anyhow!("invalid DATETIMEOFFSET ticks"))?;
        let offset = values
            .column(1)
            .as_any()
            .downcast_ref::<Int16Array>()
            .ok_or_else(|| anyhow::anyhow!("invalid DATETIMEOFFSET offset"))?;
        ensure!(
            !ticks.is_null(row) && !offset.is_null(row),
            "invalid DATETIMEOFFSET value"
        );
        return Ok(Value::Text(
            crate::datetimeoffset::DateTimeOffset::from_utc(
                crate::datetime2::DateTime2::from_ticks(ticks.value(row))?,
                offset.value(row),
            )?
            .format_iso(scale)?,
        ));
    }
    if let Some(scale) = crate::datetime2_cast::arrow_scale(array.data_type()) {
        let values = array
            .as_any()
            .downcast_ref::<StructArray>()
            .ok_or_else(|| anyhow::anyhow!("invalid DATETIME2 array"))?;
        let ticks = values
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| anyhow::anyhow!("invalid DATETIME2 ticks"))?;
        ensure!(!ticks.is_null(row), "invalid DATETIME2 ticks");
        return Ok(Value::Text(
            crate::datetime2::DateTime2::from_ticks(ticks.value(row))?.format_iso(scale)?,
        ));
    }
    macro_rules! get {
        ($ty:ty) => {
            array
                .as_any()
                .downcast_ref::<$ty>()
                .ok_or_else(|| anyhow::anyhow!("unexpected Arrow array representation"))?
                .value(row)
        };
    }
    Ok(match array.data_type() {
        ArrowType::FixedSizeBinary(16) if is_uuid_field(field) => {
            Value::Text(uuid::Uuid::from_slice(get!(FixedSizeBinaryArray))?.to_string())
        }
        ArrowType::Int8 if is_bool_field(field) => Value::Boolean(get!(Int8Array) != 0),
        ArrowType::Int8 => Value::TinyInt(get!(Int8Array)),
        ArrowType::UInt8 => Value::UTinyInt(get!(UInt8Array)),
        ArrowType::Int16 => Value::SmallInt(get!(Int16Array)),
        ArrowType::Int32 => Value::Int(get!(Int32Array)),
        ArrowType::Int64 => Value::BigInt(get!(Int64Array)),
        ArrowType::Boolean => Value::Boolean(get!(BooleanArray)),
        ArrowType::Float32 => Value::Float(get!(Float32Array)),
        ArrowType::Float64 => Value::Double(get!(Float64Array)),
        ArrowType::Utf8 => Value::Text(get!(StringArray).to_owned()),
        ArrowType::LargeUtf8 => Value::Text(get!(LargeStringArray).to_owned()),
        ArrowType::Binary => Value::Blob(get!(BinaryArray).to_owned()),
        ArrowType::LargeBinary => Value::Blob(get!(LargeBinaryArray).to_owned()),
        ArrowType::Decimal128(p, s) if *s >= 0 => Value::Decimal(duckdb::types::Decimal::new(
            *p,
            *s as u8,
            get!(Decimal128Array),
        )?),
        ArrowType::Date32 => Value::Date32(get!(Date32Array)),
        ArrowType::Time64(Unit::Microsecond) => {
            Value::Time64(TimeUnit::Microsecond, get!(Time64MicrosecondArray))
        }
        ArrowType::Time64(Unit::Nanosecond) => {
            Value::Time64(TimeUnit::Nanosecond, get!(Time64NanosecondArray))
        }
        ArrowType::Timestamp(Unit::Second, None) => {
            Value::Timestamp(TimeUnit::Second, get!(TimestampSecondArray))
        }
        ArrowType::Timestamp(Unit::Millisecond, None) => {
            Value::Timestamp(TimeUnit::Millisecond, get!(TimestampMillisecondArray))
        }
        ArrowType::Timestamp(Unit::Microsecond, None) => {
            Value::Timestamp(TimeUnit::Microsecond, get!(TimestampMicrosecondArray))
        }
        ArrowType::Timestamp(Unit::Nanosecond, None) => {
            Value::Timestamp(TimeUnit::Nanosecond, get!(TimestampNanosecondArray))
        }
        other => bail!("unsupported result type {other:?}"),
    })
}
fn ticks(unit: TimeUnit, value: i64) -> Result<i64> {
    match unit {
        TimeUnit::Second => value.checked_mul(10_000_000),
        TimeUnit::Millisecond => value.checked_mul(10_000),
        TimeUnit::Microsecond => value.checked_mul(10),
        TimeUnit::Nanosecond => Some(value / 100),
    }
    .ok_or_else(|| anyhow::anyhow!("date/time overflow"))
}
pub fn encode_column(out: &mut Vec<u8>, column: &Column, value: &Value) -> Result<()> {
    encode_value_mode(
        out,
        &column.kind,
        value,
        column.fixed_scalar_type().is_some(),
    )
}
pub fn encode_value(out: &mut Vec<u8>, kind: &Type, value: &Value) -> Result<()> {
    encode_value_mode(out, kind, value, false)
}
fn encode_value_mode(out: &mut Vec<u8>, kind: &Type, value: &Value, fixed: bool) -> Result<()> {
    ensure!(
        !fixed || !matches!(value, Value::Null),
        "NULL cannot be encoded as a fixed scalar"
    );
    if matches!(kind, Type::Variant) {
        return crate::variant::encode(out, value);
    }
    if matches!(kind, Type::Text | Type::Nvarchar(_) | Type::Nchar(_)) {
        match value {
            Value::Null => return tds::unicode_value(out, kind, None),
            Value::Struct(_) => return crate::unicode_carrier::encode(out, kind, value),
            Value::Text(value) => {
                let units: Vec<_> = value.encode_utf16().collect();
                return tds::unicode_value(out, kind, Some(&units));
            }
            _ => {}
        }
    }
    if matches!(value, Value::Null) {
        if matches!(kind, Type::Varchar(u16::MAX)) {
            out.extend(u64::MAX.to_le_bytes());
            return Ok(());
        }
        if matches!(
            kind,
            Type::Varchar(_)
                | Type::Char(_)
                | Type::Nvarchar(_)
                | Type::Nchar(_)
                | Type::Varbinary(_)
                | Type::FixedBinary(_)
        ) {
            out.extend(u16::MAX.to_le_bytes());
        } else if matches!(kind, Type::Text | Type::Binary) {
            out.extend(u64::MAX.to_le_bytes());
        } else {
            out.push(0);
        }
        return Ok(());
    }
    match (kind, value) {
        (Type::Varchar(width) | Type::Char(width), Value::Text(value)) => {
            let bytes = tds::encode_cp1252(value)?;
            if matches!(kind, Type::Varchar(u16::MAX)) {
                out.extend((bytes.len() as u64).to_le_bytes());
                if !bytes.is_empty() {
                    out.extend((bytes.len() as u32).to_le_bytes());
                    out.extend(&bytes);
                }
                out.extend(0u32.to_le_bytes());
                return Ok(());
            }
            ensure!(
                *width <= 8000 && bytes.len() <= usize::from(*width),
                "VARCHAR value exceeds declared width"
            );
            ensure!(
                !matches!(kind, Type::Char(_)) || bytes.len() == usize::from(*width),
                "CHAR value does not match declared width"
            );
            out.extend((bytes.len() as u16).to_le_bytes());
            out.extend(bytes);
        }
        (Type::Guid, Value::Text(value)) => {
            out.push(16);
            out.extend(uuid::Uuid::parse_str(value)?.to_bytes_le());
        }
        (Type::Int(n), v) => {
            let int = match v {
                Value::TinyInt(v) => *v as i64,
                Value::UTinyInt(v) => *v as i64,
                Value::SmallInt(v) => *v as i64,
                Value::Int(v) => *v as i64,
                Value::BigInt(v) => *v,
                _ => bail!("integer value/type mismatch"),
            };
            ensure!(matches!(n, 1 | 2 | 4 | 8), "invalid integer wire width");
            ensure!(
                match n {
                    1 => u8::try_from(int).is_ok(),
                    2 => i16::try_from(int).is_ok(),
                    4 => i32::try_from(int).is_ok(),
                    8 => true,
                    _ => unreachable!(),
                },
                "integer result exceeds declared width"
            );
            if !fixed {
                out.push(*n);
            }
            out.extend(&int.to_le_bytes()[..*n as usize]);
        }
        (Type::Bit, Value::Boolean(v)) => {
            if !fixed {
                out.push(1);
            }
            out.push(u8::from(*v));
        }
        (Type::Float(4), Value::Float(v)) => {
            if !fixed {
                out.push(4);
            }
            out.extend(v.to_le_bytes());
        }
        (Type::Float(8), Value::Double(v)) => {
            if !fixed {
                out.push(8);
            }
            out.extend(v.to_le_bytes());
        }
        (Type::Binary, Value::Blob(v)) => plp(out, v),
        (Type::Varbinary(width) | Type::FixedBinary(width), Value::Blob(v)) => {
            ensure!(
                v.len() <= usize::from(*width),
                "binary value exceeds declared width"
            );
            ensure!(
                !matches!(kind, Type::FixedBinary(_)) || v.len() == usize::from(*width),
                "BINARY value does not match declared width"
            );
            out.extend((v.len() as u16).to_le_bytes());
            out.extend(v);
        }
        (Type::Money(width), Value::Decimal(v)) => {
            ensure!(v.scale() == 4, "money scale mismatch");
            if fixed {
                tds::fixed_money(out, *width, v.value())?;
            } else {
                tds::money(out, *width, Some(v.value()))?;
            }
        }
        (Type::Decimal(precision, scale), Value::Decimal(v)) => {
            ensure!(
                (1..=38).contains(precision) && *scale <= *precision,
                "invalid decimal metadata"
            );
            ensure!(v.scale() == *scale, "decimal scale mismatch");
            let magnitude = v.value().unsigned_abs();
            ensure!(
                magnitude < 10u128.pow(*precision as u32),
                "decimal result exceeds declared precision"
            );
            let width = tds::decimal_value_length(magnitude);
            out.extend([width, u8::from(v.value() >= 0)]);
            out.extend(&magnitude.to_le_bytes()[..width as usize - 1]);
        }
        (Type::Date, Value::Date32(v)) => {
            let days = *v as i64 + 719162;
            ensure!(
                (0..=3652058).contains(&days),
                "date outside SQL Server range"
            );
            out.push(3);
            out.extend(&days.to_le_bytes()[..3]);
        }
        (Type::Time(scale), Value::Time64(unit, v)) => {
            let time = ticks(*unit, *v)?;
            ensure!((0..864000000000).contains(&time), "invalid time");
            ensure!(*scale <= 7, "invalid TIME scale");
            let quantum = 10i64.pow(u32::from(7 - *scale));
            let units = ((time + quantum / 2) / quantum) % (864000000000 / quantum);
            let width = match scale {
                0..=2 => 3,
                3..=4 => 4,
                _ => 5,
            };
            out.push(width as u8);
            out.extend(&units.to_le_bytes()[..width]);
        }
        (Type::DateTimeOffset(scale), Value::Text(value)) => {
            let encoded =
                crate::datetimeoffset::DateTimeOffset::parse_iso(value)?.encode(*scale)?;
            out.push(encoded.len() as u8);
            out.extend(encoded);
        }
        (Type::DateTime2(scale), Value::Text(value)) => {
            let encoded = crate::datetime2::DateTime2::parse_iso(value)?.encode(*scale)?;
            out.push(encoded.len() as u8);
            out.extend(encoded);
        }
        (Type::LegacyDateTime(width), Value::Timestamp(unit, v)) => {
            let nanos = i128::from(*v)
                * match unit {
                    TimeUnit::Second => 1_000_000_000,
                    TimeUnit::Millisecond => 1_000_000,
                    TimeUnit::Microsecond => 1_000,
                    TimeUnit::Nanosecond => 1,
                };
            tds::legacy_datetime(out, *width, nanos, fixed)?;
        }
        (Type::DateTime, Value::Timestamp(unit, v)) => {
            let nanos = i128::from(*v)
                * match unit {
                    TimeUnit::Second => 1_000_000_000,
                    TimeUnit::Millisecond => 1_000_000,
                    TimeUnit::Microsecond => 1_000,
                    TimeUnit::Nanosecond => 1,
                };
            let encoded = crate::datetime2::DateTime2::from_unix_nanos(nanos)?.encode(7)?;
            out.push(encoded.len() as u8);
            out.extend(encoded);
        }
        _ => bail!("unsupported result value for {kind:?}"),
    }
    Ok(())
}
fn plp(out: &mut Vec<u8>, data: &[u8]) {
    out.extend((data.len() as u64).to_le_bytes());
    if !data.is_empty() {
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
    }
    out.extend(0u32.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rpc_settings_restore_after_native_transaction_failure() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(session.batch_response(
            "CREATE TABLE dbo.scope_tx(n INT PRIMARY KEY); INSERT INTO dbo.scope_tx VALUES(1)",
            &HashMap::new(), false, None,
        ).1);
        assert!(!session.batch_response(
            "SET DATEFIRST 3; SET ANSI_WARNINGS OFF; BEGIN TRAN; INSERT INTO dbo.scope_tx VALUES(1)",
            &HashMap::new(), true, None,
        ).1);
        assert_eq!(session.datefirst, 7);
        assert!(session.ansi_warnings);
        assert!(
            session
                .batch_response("ROLLBACK", &HashMap::new(), true, None)
                .1
        );
        let first: i32 = session
            .db
            .query_row("SELECT getvariable('__msduck_datefirst')", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(first, 7);
    }

    fn alignment_fields(count: usize) -> Vec<crate::query_catalog::Field> {
        (0..count)
            .map(|_| crate::query_catalog::Field {
                name: "unrelated".into(),
                info: None,
                json_fragment: false,
                properties: msduck_core::result::Properties::expression(false),
                collation: Some(Ok(msduck_core::collation::Label::Explicit(
                    "Latin1_General_100_BIN2".into(),
                ))),
            })
            .collect()
    }

    fn alignment_result(
        db: &Connection,
        sql: &str,
        fields: &[crate::query_catalog::Field],
        declared: &[Option<Type>],
    ) -> Result<(Vec<u8>, u64)> {
        let mut statement = db.prepare(sql)?;
        let batches = statement.query_arrow([])?;
        let schema = batches.get_schema();
        Session::encode_batches(batches, &schema, fields, declared)
    }

    #[test]
    fn descriptor_alignment_success_rejects_partial_facts() {
        let db = Connection::open_in_memory().unwrap();
        for sql in [
            "SELECT CAST(NULL AS INTEGER) AS n, 'long value' AS s",
            "SELECT 'long value' AS s, CAST(NULL AS INTEGER) AS n",
        ] {
            let baseline = alignment_result(&db, sql, &[], &[]).unwrap();
            for count in [1, 3] {
                assert_eq!(
                    alignment_result(&db, sql, &alignment_fields(count), &[]).unwrap(),
                    baseline,
                    "mismatched field count {count}: {sql}"
                );
                assert_eq!(
                    alignment_result(&db, sql, &[], &vec![Some(Type::Nvarchar(1)); count]).unwrap(),
                    baseline,
                    "mismatched override count {count}: {sql}"
                );
            }
        }
    }

    #[test]
    fn descriptor_alignment_errors_reject_partial_facts() {
        let db = Connection::open_in_memory().unwrap();
        for sql in [
            "SELECT CAST(NULL AS INTEGER) AS n, 'long value' AS s",
            "SELECT 'long value' AS s, CAST(NULL AS INTEGER) AS n",
        ] {
            let statement = db.prepare(sql).unwrap();
            let baseline = crate::query_error::describe(&statement, &[], &[]).unwrap();
            for count in [1, 3] {
                assert_eq!(
                    crate::query_error::describe(&statement, &alignment_fields(count), &[])
                        .unwrap(),
                    baseline,
                    "mismatched field count {count}: {sql}"
                );
                assert_eq!(
                    crate::query_error::describe(
                        &statement,
                        &[],
                        &vec![Some(Type::Nvarchar(1)); count],
                    )
                    .unwrap(),
                    baseline,
                    "mismatched override count {count}: {sql}"
                );
            }
        }
    }

    #[test]
    fn descriptor_alignment_preserves_complete_facts_on_success_and_error() {
        let db = Connection::open_in_memory().unwrap();
        let sql = "SELECT CAST(NULL AS INTEGER) AS n, 'text' AS s";
        let mut fields = alignment_fields(2);
        for field in &mut fields {
            field.name = "duplicate".into();
            field.properties = msduck_core::result::Properties::expression(true);
        }
        let declared = [None, Some(Type::Nvarchar(8))];
        let expected_columns = vec![
            Column {
                name: "duplicate".into(),
                kind: Type::Int(4),
                properties: fields[0].properties,
                collation: tds::collation::Collation::for_name("Latin1_General_100_BIN2"),
            },
            Column {
                name: "duplicate".into(),
                kind: Type::Nvarchar(8),
                properties: fields[1].properties,
                collation: tds::collation::Collation::for_name("Latin1_General_100_BIN2"),
            },
        ];
        let mut metadata = Vec::new();
        tds::metadata(&mut metadata, &expected_columns).unwrap();
        let statement = db.prepare(sql).unwrap();
        assert_eq!(
            crate::query_error::describe(&statement, &fields, &declared),
            Some(metadata.clone())
        );
        let (out, count) = alignment_result(&db, sql, &fields, &declared).unwrap();
        assert_eq!(count, 1);
        assert!(out.starts_with(&metadata));
        let empty = format!("{sql} WHERE false");
        assert_eq!(
            alignment_result(&db, &empty, &fields, &declared).unwrap(),
            (metadata, 0)
        );
    }

    #[test]
    fn public_bin2_comparisons_match_live_rows_with_nullable_case_metadata() {
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../reference/bin2-comparisons.json")).unwrap();
        let ansi: serde_json::Value =
            serde_json::from_str(include_str!("../reference/bin2-ansi-comparisons.json")).unwrap();
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let mut checked = 0;
        for case in reference["results"]
            .as_array()
            .unwrap()
            .iter()
            .chain(ansi["results"].as_array().unwrap())
            .filter(|case| case["reference"]["errors"].as_array().unwrap().is_empty())
        {
            let sql = case["query"].as_str().unwrap();
            session.validate_prepared_sql(sql, &[]).unwrap();
            let (out, success) = session.batch_response(sql, &Default::default(), false, None);
            assert!(success, "{}: {out:?}", case["name"]);
            let set = &case["reference"]["sets"][0];
            let columns = set["columns"]
                .as_array()
                .unwrap()
                .iter()
                .map(|col| Column {
                    name: col["name"].as_str().unwrap().into(),
                    kind: Type::Int(4),
                    collation: None,
                    // SQL Server folds the constant CASE conditions to prove
                    // non-null results. Our declaration pass remains nullable;
                    // client differential coverage preserves that metadata gap.
                    properties: msduck_core::result::Properties::expression(true),
                })
                .collect::<Vec<_>>();
            let mut expected = Vec::new();
            tds::metadata(&mut expected, &columns).unwrap();
            expected.push(0xd1);
            for value in set["rows"][0].as_array().unwrap() {
                if let Some(value) = value.as_i64() {
                    expected.push(4);
                    expected.extend((value as i32).to_le_bytes());
                } else {
                    expected.push(0);
                }
            }
            assert!(
                out.starts_with(&expected),
                "{}: expected {expected:?}, got {out:?}",
                case["name"]
            );
            checked += 1;
        }
        assert_eq!(checked, 13);
    }

    #[test]
    fn sensitive_collation_errors_are_compilation_errors_in_execution_and_prepare() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        for (expression, operation) in [
            (
                "REPLACE(N'a' COLLATE Latin1_General_100_CI_AS,N'b' COLLATE Latin1_General_100_CS_AS,N'z')",
                "replace",
            ),
            (
                "STUFF(N'a' COLLATE Latin1_General_100_CI_AS,1,1,N'b' COLLATE Latin1_General_100_CS_AS)",
                "stuff",
            ),
            (
                "NULLIF(N'a' COLLATE Latin1_General_100_CI_AS,N'b' COLLATE Latin1_General_100_CS_AS)",
                "equal to",
            ),
        ] {
            for sql in [
                format!("SELECT ({expression}) COLLATE Latin1_General_100_BIN2 AS s"),
                format!("SELECT LEN({expression}) AS n"),
                format!("SELECT 1 AS n WHERE ({expression}) IS NULL"),
                format!("SELECT 1 AS n ORDER BY {expression}"),
            ] {
                let error = session.validate_prepared_sql(&sql, &[]).unwrap_err();
                let error = error.downcast_ref::<SqlError>().unwrap();
                assert_eq!((error.number, error.state, error.severity), (468, 9, 16));
                assert!(
                    error
                        .message
                        .ends_with(&format!("in the {operation} operation."))
                );
                let (out, success) = session.batch_response(&sql, &Default::default(), false, None);
                assert!(!success);
                assert_eq!(out[0], 0xaa, "compilation must not emit result metadata");
                assert_eq!(i32::from_le_bytes(out[3..7].try_into().unwrap()), 468);
                assert_eq!(&out[7..9], &[9, 16]);
            }
        }
        assert!(
            session
                .batch_response("SELECT 1 AS recovered", &Default::default(), false, None)
                .1
        );
    }

    #[test]
    fn result_collations_reach_rows_empty_results_and_error_descriptors() {
        use duckdb::arrow::{array::StringArray, datatypes::Field, record_batch::RecordBatch};
        use msduck_core::collation::Label;
        let schema = std::sync::Arc::new(duckdb::arrow::datatypes::Schema::new(vec![
            Field::new("a", ArrowType::Utf8, true),
            Field::new("b", ArrowType::Utf8, true),
        ]));
        let labels = ["Latin1_General_100_CS_AS", "Latin1_General_100_BIN2"];
        let fields = labels
            .iter()
            .zip(["a", "b"])
            .map(|(label, name)| crate::query_catalog::Field {
                name: name.into(),
                info: None,
                json_fragment: false,
                properties: Default::default(),
                collation: Some(Ok(Label::Explicit((*label).into()))),
            })
            .collect::<Vec<_>>();
        let mut expected = Vec::new();
        tds::metadata(
            &mut expected,
            &fields
                .iter()
                .zip(labels)
                .map(|(field, name)| Column {
                    name: field.name.clone(),
                    kind: Type::Text,
                    properties: field.properties,
                    collation: tds::collation::Collation::for_name(name),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for values in [vec![], vec![Some("a"), None, Some("🦆")]] {
            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    std::sync::Arc::new(StringArray::from(values.clone())),
                    std::sync::Arc::new(StringArray::from(values.clone())),
                ],
            )
            .unwrap();
            let (out, count) =
                Session::encode_batches(std::iter::once(batch), &schema, &fields, &[]).unwrap();
            assert_eq!(count, values.len() as u64);
            assert!(out.starts_with(&expected));
            if values.is_empty() {
                assert_eq!(out, expected);
            }
        }
        let db = duckdb::Connection::open_in_memory().unwrap();
        let prepared = db
            .prepare("SELECT CAST(NULL AS VARCHAR) AS a, CAST(NULL AS VARCHAR) AS b")
            .unwrap();
        assert_eq!(
            crate::query_error::describe(&prepared, &fields, &[]).unwrap(),
            expected
        );
        assert!(crate::query_catalog::wire_collation(&fields, 1, 0).is_none());
        let mut unresolved = fields;
        unresolved[0].collation = Some(Ok(Label::NoCollation {
            left: labels[0].into(),
            right: labels[1].into(),
        }));
        assert!(crate::query_catalog::wire_collation(&unresolved, 2, 0).is_none());
    }

    #[test]
    fn typed_diagnostics_survive_context_without_reclassification() {
        fn wire(error: anyhow::Error) -> (i32, u8, u8, String) {
            let mut bytes = Vec::new();
            let number = emit_error(&mut bytes, &error);
            assert_eq!(bytes[0], 0xaa);
            assert_eq!(i32::from_le_bytes(bytes[3..7].try_into().unwrap()), number);
            let len = u16::from_le_bytes(bytes[9..11].try_into().unwrap()) as usize;
            let units = bytes[11..11 + len * 2]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>();
            (
                number,
                bytes[7],
                bytes[8],
                String::from_utf16(&units).unwrap(),
            )
        }
        let message = msduck_core::json_path::SCALAR;
        let explicit =
            anyhow::Error::new(SqlError::new(51000, 7, message)).context("outer execution context");
        assert_eq!(wire(explicit), (51000, 7, 16, message.into()));
        let wrapped = format!("Invalid Input Error: {message}");
        assert!(msduck_core::json_path::diagnostic(&wrapped).is_none());
        assert_eq!(
            wire(anyhow::anyhow!(wrapped)),
            (13623, 2, 16, message.into())
        );
        let syntax = "Window element in OVER clause can not also be specified in WINDOW clause.";
        assert_eq!(wire(anyhow::anyhow!(syntax)), (4123, 1, 15, syntax.into()));
        assert!(crate::json_extract::diagnostic(&format!("user text: {message}")).is_none());
        let numeric = "Arithmetic overflow error converting expression to data type money.";
        let decimal = "Arithmetic overflow error converting expression to data type numeric.";
        let decimal_wrapped = format!("Invalid Input Error: {decimal}");
        assert_eq!(
            wire(anyhow::anyhow!(decimal_wrapped.clone())),
            (8115, 2, 16, decimal.into())
        );
        assert_eq!(
            sql_error_from_message(&decimal_wrapped),
            SqlError::new(8115, 2, decimal)
        );
        assert_eq!(
            wire(anyhow::Error::new(SqlError::new(51002, 8, &decimal_wrapped)).context("outer")),
            (51002, 8, 16, decimal_wrapped)
        );
        let wrapped = format!("Invalid Input Error: {numeric}");
        assert_eq!(
            wire(anyhow::anyhow!(wrapped.clone())),
            (8115, 1, 16, numeric.into())
        );
        assert_eq!(
            sql_error_from_message(&wrapped),
            SqlError::new(8115, 1, numeric)
        );
        assert_eq!(
            wire(anyhow::Error::new(SqlError::new(51001, 9, &wrapped)).context("outer")),
            (51001, 9, 16, wrapped)
        );
        assert!(
            runtime_diagnostic(&format!("Invalid Input Error: {numeric} trailing text")).is_none()
        );
    }
    #[test]
    fn translation_keeps_literal_contents_and_nested_top_scopes() {
        let mut statements =
            parse_batch("SELECT TOP (2) N'[x] TOP 100' AS [text], (SELECT TOP (1) 9) AS n")
                .unwrap();
        let parameters = HashMap::new();
        let mut translator = Translator {
            parameters: &parameters,
            values: vec![],
            parameter_slots: HashMap::new(),
            transactions: 0,
            options_mask: 5496,
            transaction_doomed: false,
            original_login: "sa",
            clock: crate::current_time::now(),
            rowcount: 0,
            last_error: 0,
            caught_error: None,
            spid: 51,
        };
        assert!(matches!(
            VisitMut::visit(&mut statements[0], &mut translator),
            ControlFlow::Continue(())
        ));
        let sql = statements[0].to_string();
        assert!(sql.contains("'[x] TOP 100'"));
        assert!(sql.contains("SELECT 9 LIMIT __msduck_top_count("));
        assert_eq!(sql.matches("LIMIT __msduck_top_count(").count(), 2);
    }
    #[test]
    fn time_tokens_use_declared_scale_width_and_midnight_rounding() {
        for scale in 0..=7 {
            let width = match scale {
                0..=2 => 3,
                3..=4 => 4,
                _ => 5,
            };
            for nanos in [0, 45_296_123_456_700, 86_399_999_999_900] {
                let mut out = vec![];
                encode_value(
                    &mut out,
                    &Type::Time(scale),
                    &Value::Time64(TimeUnit::Nanosecond, nanos),
                )
                .unwrap();
                assert_eq!(out.len(), width + 1);
                assert_eq!(out[0] as usize, width);
                let mut bytes = [0u8; 8];
                bytes[..width].copy_from_slice(&out[1..]);
                let quantum = 10i64.pow(u32::from(7 - scale));
                assert_eq!(
                    i64::from_le_bytes(bytes),
                    ((nanos / 100 + quantum / 2) / quantum) % (864000000000 / quantum)
                );
            }
            let mut out = vec![];
            encode_value(&mut out, &Type::Time(scale), &Value::Null).unwrap();
            assert_eq!(out, [0]);
        }
        assert!(
            encode_value(
                &mut vec![],
                &Type::Time(8),
                &Value::Time64(TimeUnit::Nanosecond, 0)
            )
            .is_err()
        );
    }

    #[test]
    fn decimal_and_date_tokens_match_wire_layout() {
        let mut out = vec![];
        encode_value(
            &mut out,
            &Type::Decimal(38, 2),
            &Value::Decimal(duckdb::types::Decimal::new(38, 2, -12345).unwrap()),
        )
        .unwrap();
        assert_eq!(out[0..4], [5, 0, 0x39, 0x30]);
        assert_eq!(out.len(), 6);
        out.clear();
        encode_value(&mut out, &Type::Date, &Value::Date32(-719162)).unwrap();
        assert_eq!(out, [3, 0, 0, 0]);
    }
}

#[cfg(test)]
mod ddl_completion_tests {
    use super::*;

    #[test]
    fn ddl_batch_and_rpc_tokens_match_captured_commands_and_rowcounts() {
        // Captured twice in fresh SQL Server databases, both batch and RPC.
        let cases = [
            ("CREATE SCHEMA ddl_schema", 253, None),
            ("CREATE TABLE dbo.ddl_plain(v INT)", 198, None),
            ("CREATE TABLE dbo.ddl_identity(id INT IDENTITY)", 198, None),
            ("INSERT INTO dbo.ddl_plain VALUES(1),(2)", 195, Some(2u64)),
            (
                "CREATE VIEW dbo.ddl_view AS SELECT v FROM dbo.ddl_plain",
                207,
                None,
            ),
            (
                "ALTER VIEW dbo.ddl_view AS SELECT v+1 AS v FROM dbo.ddl_plain",
                207,
                None,
            ),
            ("DROP VIEW dbo.ddl_view", 208, None),
            ("ALTER TABLE dbo.ddl_plain ADD extra INT", 216, None),
            ("ALTER TABLE dbo.ddl_plain DROP COLUMN extra", 216, None),
            ("TRUNCATE TABLE dbo.ddl_plain", 234, None),
            ("CREATE INDEX ddl_index ON dbo.ddl_plain(v)", 200, None),
            ("DROP INDEX ddl_index ON dbo.ddl_plain", 201, None),
            ("DROP TABLE dbo.ddl_plain", 199, None),
            ("DROP TABLE dbo.ddl_identity", 199, None),
            ("DROP SCHEMA ddl_schema", 253, None),
        ];
        for rpc in [false, true] {
            let server = crate::server::Server::open(":memory:").unwrap();
            let mut session = Session::new(server.connection().unwrap()).unwrap();
            for (sql, command, count) in cases {
                let (actual, ok) = session.batch_response(sql, &Default::default(), rpc, None);
                assert!(ok, "rpc={rpc}, {sql}: {actual:?}");
                let mut expected = Vec::new();
                if !rpc || command != 253 {
                    expected.extend([
                        if rpc { 0xff } else { 0xfd },
                        u8::from(rpc) | if count.is_some() { 16 } else { 0 },
                        0,
                    ]);
                    expected.extend((command as u16).to_le_bytes());
                    expected.extend(count.unwrap_or(0).to_le_bytes());
                }
                if rpc {
                    expected.extend([0x79, 0, 0, 0, 0, 0xfe, 0, 0, 0xe0, 0]);
                    expected.extend(0u64.to_le_bytes());
                }
                assert_eq!(actual, expected, "rpc={rpc}, {sql}");
                assert_eq!(session.rowcount, count.unwrap_or(0), "{sql}");
                assert_eq!(session.last_error, 0, "{sql}");
            }
        }
    }
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    use crate::{
        server::Server,
        tds::{BeginTransaction, TransactionRequest},
    };
    #[test]
    fn restart_commits_or_rolls_back_then_allocates_a_new_descriptor() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE TABLE items (id INT)")
            .unwrap();
        session.begin_transaction(2, "first").unwrap();
        let first = session.transaction_descriptor;
        session
            .db
            .execute_batch("INSERT INTO items VALUES (1)")
            .unwrap();
        let response = session
            .transaction_request(TransactionRequest::Commit {
                restart: Some(BeginTransaction {
                    isolation: 5,
                    name: "second".into(),
                }),
            })
            .unwrap();
        assert_eq!(response[3], 9);
        assert_eq!(response[17], 8);
        let second = session.transaction_descriptor;
        assert_ne!(first, second);
        assert_eq!(session.transactions, 1);
        session
            .db
            .execute_batch("INSERT INTO items VALUES (2)")
            .unwrap();
        session
            .transaction_request(TransactionRequest::Rollback {
                name: "second".into(),
                restart: Some(BeginTransaction {
                    isolation: 0,
                    name: String::new(),
                }),
            })
            .unwrap();
        assert_ne!(second, session.transaction_descriptor);
        assert_eq!(
            session
                .db
                .query_row("SELECT COUNT(*) FROM items", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        session.rollback_transaction("").unwrap();
        assert_eq!(session.transaction_descriptor, 0);
    }
    #[test]
    fn invalid_restart_and_unknown_names_preserve_the_active_transaction() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session.begin_transaction(0, "outer").unwrap();
        let descriptor = session.transaction_descriptor;
        // Levels 1-5 are SQL Server's; 6 is not a level.
        assert!(
            session
                .transaction_request(TransactionRequest::Commit {
                    restart: Some(BeginTransaction {
                        isolation: 6,
                        name: String::new()
                    })
                })
                .is_err()
        );
        assert!(session.rollback_transaction("missing").is_err());
        // A savepoint request keeps the transaction (see the transactions
        // extension, docs/gaps-transactions.md).
        session
            .transaction_request(TransactionRequest::Save {
                name: "point".into(),
            })
            .unwrap();
        assert_eq!(session.transaction_descriptor, descriptor);
        assert_eq!(session.transactions, 1);
        session.rollback_transaction("outer").unwrap();
    }
    #[test]
    fn disconnect_rolls_back_uncommitted_writes() {
        let server = Server::open(":memory:").unwrap();
        server
            .connection()
            .unwrap()
            .execute_batch("CREATE TABLE dbo.items (id INT)")
            .unwrap();
        {
            let mut session = Session::new(server.connection().unwrap()).unwrap();
            session.begin_transaction(0, "").unwrap();
            session
                .db
                .execute_batch("INSERT INTO items VALUES (1)")
                .unwrap();
        }
        assert_eq!(
            server
                .connection()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM dbo.items", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[cfg(test)]
mod decimal_encoding_tests {
    use super::*;
    #[test]
    fn magnitude_selects_wire_width_and_precision_rejects_overflow() {
        for precision in [9, 10, 19, 20, 28, 29, 38] {
            let mut out = Vec::new();
            let value = Value::Decimal(duckdb::types::Decimal::new(precision, 2, -12345).unwrap());
            encode_value(&mut out, &Type::Decimal(precision, 2), &value).unwrap();
            assert_eq!(out, [5, 0, 0x39, 0x30, 0, 0]);
        }
        for (magnitude, width) in [
            (0, 5),
            (u32::MAX as i128, 5),
            (1i128 << 32, 9),
            (u64::MAX as i128, 9),
            (1i128 << 64, 13),
            ((1i128 << 96) - 1, 13),
            (1i128 << 96, 17),
            (10i128.pow(38) - 1, 17),
        ] {
            for coefficient in [magnitude, -magnitude] {
                let mut out = Vec::new();
                let value =
                    Value::Decimal(duckdb::types::Decimal::new(38, 0, coefficient).unwrap());
                encode_value(&mut out, &Type::Decimal(38, 0), &value).unwrap();
                assert_eq!(out[0], width);
                assert_eq!(out[1], u8::from(coefficient >= 0));
                assert_eq!(out.len(), width as usize + 1);
                let mut bytes = [0u8; 16];
                bytes[..out.len() - 2].copy_from_slice(&out[2..]);
                assert_eq!(u128::from_le_bytes(bytes), magnitude as u128);
            }
        }
        let value = Value::Decimal(duckdb::types::Decimal::new(5, 0, 10000).unwrap());
        assert!(encode_value(&mut vec![], &Type::Decimal(4, 0), &value).is_err());
        assert!(encode_value(&mut vec![], &Type::Decimal(5, 1), &value).is_err());
    }
}

#[cfg(test)]
mod loop_tests {
    use super::*;
    use crate::server::Server;

    #[test]
    fn repeated_results_obey_whole_batch_response_limit() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let (response, success) = session.batch_response(
            "SELECT REPEAT('x', 4500000); SELECT REPEAT('y', 4500000)",
            &HashMap::new(),
            false,
            None,
        );
        assert!(!success);
        assert!(response.len() <= tds::MAX_MESSAGE);
        let message: Vec<u8> = "batch result exceeds"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(
            response
                .windows(message.len())
                .any(|window| window == message)
        );
        assert!(
            session
                .batch_response("SELECT 1", &HashMap::new(), false, None)
                .1
        );
    }

    #[test]
    fn runaway_loop_hits_execution_limit_and_session_recovers() {
        let server = Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let (response, success) =
            session.batch_response("WHILE 1=1 BEGIN END", &HashMap::new(), false, None);
        assert!(!success);
        let message: Vec<u8> = "10000-step limit"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(
            response
                .windows(message.len())
                .any(|window| window == message)
        );
        assert!(
            session
                .batch_response("SELECT 1", &HashMap::new(), false, None)
                .1
        );
        assert_eq!(session.transactions, 0);
    }
}

#[cfg(test)]
mod fixed_scalar_encoding_tests {
    use super::*;
    #[test]
    fn fixed_scalar_rows_have_no_prefix_and_reject_null_before_writing() {
        for (kind, value, expected) in [
            (Type::Int(1), Value::UTinyInt(255), vec![255]),
            (Type::Int(2), Value::SmallInt(i16::MIN), vec![0, 128]),
            (Type::Int(4), Value::Int(i32::MIN), vec![0, 0, 0, 128]),
            (
                Type::Int(8),
                Value::BigInt(i64::MIN),
                vec![0, 0, 0, 0, 0, 0, 0, 128],
            ),
            (Type::Bit, Value::Boolean(true), vec![1]),
            (Type::Float(4), Value::Float(1.25), vec![0, 0, 160, 63]),
            (
                Type::Float(8),
                Value::Double(-2.5),
                vec![0, 0, 0, 0, 0, 0, 4, 192],
            ),
        ] {
            let mut column = Column {
                collation: None,
                name: "x".into(),
                kind,
                properties: msduck_core::result::Properties::expression(false),
            };
            let mut out = vec![];
            encode_column(&mut out, &column, &value).unwrap();
            assert_eq!(out, expected);
            out.clear();
            out.push(0xaa);
            assert!(encode_column(&mut out, &column, &Value::Null).is_err());
            assert_eq!(out, [0xaa]);
            column.properties.null_extend();
            out.clear();
            encode_column(&mut out, &column, &value).unwrap();
            assert_eq!(out[0] as usize, expected.len());
            assert_eq!(&out[1..], expected);
            out.clear();
            encode_column(&mut out, &column, &Value::Null).unwrap();
            assert_eq!(out, [0]);
        }
        for (width, value) in [(1, -1), (1, 256), (2, 32768), (4, i64::MAX), (9, 0)] {
            let mut out = vec![0xaa];
            assert!(encode_value(&mut out, &Type::Int(width), &Value::BigInt(value)).is_err());
            assert_eq!(out, [0xaa]);
        }
    }
}

#[cfg(test)]
mod unary_plus_tests {
    use super::*;

    #[test]
    fn unary_plus_preserves_native_values_and_evaluates_volatile_input_once() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let parameters = HashMap::new();
        for sql in [
            "N'abc'",
            "'abc'",
            "CAST(1 AS BIT)",
            "CAST(NULL AS INT)",
            "CAST('2024-01-01' AS DATE)",
            "CAST('2024-01-01' AS DATETIME2(3))",
            "CAST(12.34 AS DECIMAL(5,2))",
            "CAST(12.34 AS MONEY)",
        ] {
            let expr = Parser::new(&crate::dialect::ServerDialect)
                .try_with_sql(sql)
                .unwrap()
                .parse_expr()
                .unwrap();
            let expected = session
                .evaluate_expression(expr.clone(), &parameters, false)
                .unwrap();
            let actual = session
                .evaluate_expression(
                    Expr::UnaryOp {
                        op: UnaryOperator::Plus,
                        expr: Box::new(expr),
                    },
                    &parameters,
                    false,
                )
                .unwrap();
            assert_eq!(actual, expected, "{sql}");
        }
        for source in ["(VALUES(1),(2))", "(VALUES(1),(CAST(NULL AS INT)))"] {
            let plain = session.batch(
                &format!("SELECT n AS p FROM {source} s(n) ORDER BY n"),
                &parameters,
                false,
            );
            let plus = session.batch(
                &format!("SELECT +n AS p FROM {source} s(n) ORDER BY n"),
                &parameters,
                false,
            );
            assert_eq!(plain, plus, "{source}");
        }
        session
            .db
            .execute_batch("CREATE SEQUENCE unary_plus_sequence")
            .unwrap();
        let expr = Parser::new(&crate::dialect::ServerDialect)
            .try_with_sql("+nextval('unary_plus_sequence')")
            .unwrap()
            .parse_expr()
            .unwrap();
        assert_eq!(
            session
                .evaluate_expression(expr, &parameters, false)
                .unwrap(),
            Value::BigInt(1)
        );
        let current: i64 = session
            .db
            .query_row("SELECT currval('unary_plus_sequence')", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(current, 1);
    }
}

#[cfg(test)]
mod unary_bit_tests {
    use super::*;

    #[test]
    fn bit_negation_preflight_prevents_writes_and_binding_preserves_diagnostic() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE TABLE unary_guard(n INTEGER)")
            .unwrap();
        let parameters = HashMap::new();
        let mut expected = Vec::new();
        tds::sql_error(
            &mut expected,
            &SqlError::new(8117, 1, msduck_sql::unary_operator::BIT_MINUS),
        );
        tds::done(&mut expected, 0xfd, 2, 0, 0);
        for sql in [
            "INSERT INTO unary_guard VALUES(1); SELECT -CAST(1 AS BIT)",
            "IF 1=0 SELECT -CAST(1 AS BIT)",
            "BEGIN TRY SELECT -CAST(1 AS BIT) END TRY BEGIN CATCH SELECT 99 END CATCH",
            "SELECT -b FROM (VALUES(CAST(1 AS BIT))) s(b)",
            "BEGIN TRY SELECT -b FROM (VALUES(CAST(1 AS BIT))) s(b) END TRY BEGIN CATCH SELECT 99 END CATCH",
        ] {
            let (tokens, success) = session.batch_response(sql, &parameters, false, None);
            assert!(!success, "{sql}");
            assert_eq!(tokens, expected, "{sql}");
        }
        let written: i64 = session
            .db
            .query_row("SELECT COUNT(*) FROM unary_guard", [], |row| row.get(0))
            .unwrap();
        assert_eq!(written, 0);
        let error = session
            .validate_prepared_sql("SELECT -@b", &[("@b".into(), SqlType::Bit)])
            .unwrap_err();
        assert_eq!(error.downcast_ref::<SqlError>().unwrap().number, 8117);
    }
}

#[cfg(test)]
mod output_materialization_tests {
    use super::*;
    #[test]
    fn paired_output_prepares_without_writes_and_keeps_old_new_images() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let mut parameters = HashMap::new();
        for statement in parse_batch("CREATE TABLE paired_source(id INT PRIMARY KEY,n INT); INSERT INTO paired_source VALUES(10,1),(20,2); CREATE TABLE paired_sink(old_id INT,new_id INT)").unwrap() {
            session.execute(statement, &mut parameters).unwrap();
        }
        let sql = "UPDATE paired_source SET id=id+100,n=n+10 OUTPUT deleted.id,inserted.id,deleted.n,inserted.n";
        session.validate_prepared_sql(sql, &[]).unwrap();
        let values = |session: &Session| {
            session
                .db
                .prepare("SELECT id,n FROM paired_source ORDER BY id")
                .unwrap()
                .query_map([], |r| Ok((r.get::<_, i32>(0)?, r.get::<_, i32>(1)?)))
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(values(&session), vec![(10, 1), (20, 2)]);
        let result = session
            .execute(parse_batch(sql).unwrap().remove(0), &mut parameters)
            .unwrap();
        assert_eq!(result.count, Some(2));
        assert_eq!(values(&session), vec![(110, 11), (120, 12)]);
        let sql =
            "UPDATE paired_source SET id=id+100 OUTPUT deleted.id,inserted.id INTO paired_sink";
        session
            .execute(parse_batch(sql).unwrap().remove(0), &mut parameters)
            .unwrap();
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT COUNT(*) FROM paired_sink WHERE new_id=old_id+100",
                    [],
                    |r| r.get::<_, i32>(0)
                )
                .unwrap(),
            2
        );
        let before_failure = values(&session);
        session
            .execute(
                parse_batch("CREATE TABLE paired_guard(old_id INT,new_n INT NOT NULL)")
                    .unwrap()
                    .remove(0),
                &mut parameters,
            )
            .unwrap();
        for sql in [
            "UPDATE paired_source SET id=1 OUTPUT deleted.id,inserted.id",
            "UPDATE paired_source SET n=NULL OUTPUT deleted.id,inserted.n INTO paired_guard",
        ] {
            assert!(
                session
                    .execute(parse_batch(sql).unwrap().remove(0), &mut parameters)
                    .is_err()
            );
            assert_eq!(values(&session), before_failure);
            assert_eq!(
                session
                    .db
                    .query_row("SELECT COUNT(*) FROM paired_guard", [], |r| r
                        .get::<_, i32>(0))
                    .unwrap(),
                0
            );
        }
        assert_eq!(session.db.query_row("SELECT COUNT(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_output_image_%'", [], |r| r.get::<_,i64>(0)).unwrap(),0);
    }

    #[test]
    fn parameterized_output_binds_without_writes_and_cleans_images_on_every_outcome() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        let mut parameters = HashMap::from([(
            "@p".into(),
            Parameter {
                data_type: SqlType::Int,
                value: ParameterValue::Int(7),
            },
        )]);
        for statement in parse_batch("CREATE TABLE image_source(id INT PRIMARY KEY); CREATE TABLE image_sink(id INT,n INT NOT NULL)").unwrap() {
            session.execute(statement, &mut parameters).unwrap();
        }
        let sql = "INSERT INTO image_source(id) OUTPUT inserted.id,@p INTO image_sink(id,n) VALUES(1),(2)";
        session
            .validate_prepared_sql(sql, &[("@p".into(), SqlType::Int)])
            .unwrap();
        let count = |session: &Session, table: &str| {
            session
                .db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap()
        };
        let images = |session: &Session| {
            session.db.query_row("SELECT COUNT(*) FROM duckdb_tables() WHERE database_name='temp' AND table_name LIKE '__msduck_output_image_%'", [], |r| r.get::<_,i64>(0)).unwrap()
        };
        assert_eq!(count(&session, "image_source"), 0);
        assert_eq!(images(&session), 0);
        let result = session
            .execute(parse_batch(sql).unwrap().remove(0), &mut parameters)
            .unwrap();
        assert_eq!(result.count, Some(2));
        assert_eq!(result.command, 0xc3);
        assert!(result.tokens.is_empty());
        assert_eq!(
            session
                .db
                .query_row("SELECT CAST(SUM(n) AS BIGINT) FROM image_sink", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            14
        );
        parameters.get_mut("@p").unwrap().value = ParameterValue::Int(9);
        session.execute(parse_batch("INSERT INTO image_source(id) OUTPUT inserted.id,@p INTO image_sink(id,n) VALUES(3)").unwrap().remove(0), &mut parameters).unwrap();
        assert_eq!(
            session
                .db
                .query_row("SELECT n FROM image_sink WHERE id=3", [], |r| r
                    .get::<_, i32>(0))
                .unwrap(),
            9
        );
        assert_eq!(images(&session), 0);
        assert!(
            session
                .validate_prepared_sql(
                    "INSERT INTO image_source(id) OUTPUT SUM(inserted.id)+@p VALUES(4)",
                    &[("@p".into(), SqlType::Int)]
                )
                .is_err()
        );
        let empty = session
            .execute(
                parse_batch(
                    "INSERT INTO image_source(id) OUTPUT inserted.id,@p AS p SELECT 4 WHERE 1=0",
                )
                .unwrap()
                .remove(0),
                &mut parameters,
            )
            .unwrap();
        assert_eq!(empty.count, Some(0));
        assert_eq!(empty.tokens.first(), Some(&0x81));
        assert!(
            session
                .execute(
                    parse_batch(
                        "INSERT INTO image_source(id) OUTPUT inserted.id,@p AS p VALUES(1)"
                    )
                    .unwrap()
                    .remove(0),
                    &mut parameters
                )
                .is_err()
        );
        parameters.get_mut("@p").unwrap().value = ParameterValue::Null;
        let error = session.execute(parse_batch("INSERT INTO image_source(id) OUTPUT inserted.id,@p INTO image_sink(id,n) VALUES(4)").unwrap().remove(0), &mut parameters).err().unwrap();
        assert_eq!(error_number(&error.to_string()), 515);
        assert_eq!(count(&session, "image_source"), 3);
        assert_eq!(count(&session, "image_sink"), 3);
        assert_eq!(images(&session), 0);
    }
}

#[cfg(test)]
mod index_catalog_integration_tests {
    use super::*;

    #[test]
    fn index_catalog_startup_and_empty_result_descriptors_match_reference() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../reference/index-catalog.json")).unwrap();
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        for view in ["indexes", "index_columns"] {
            let wire = &fixture["declarations"][view]["wire"]["sets"][0]["columns"];
            let columns = wire
                .as_array()
                .unwrap()
                .iter()
                .map(|c| {
                    let kind = match c["type"].as_str().unwrap() {
                        "Int" => Type::Int(4),
                        "TinyInt" => Type::Int(1),
                        "IntN" => Type::Int(c["length"].as_u64().unwrap() as u8),
                        "BitN" => Type::Bit,
                        "NVarChar" => {
                            if c["length"] == 65535 {
                                Type::Text
                            } else {
                                Type::Nvarchar(c["length"].as_u64().unwrap() as u16 / 2)
                            }
                        }
                        other => panic!("unhandled reference type {other}"),
                    };
                    let flags = c["flags"].as_u64().unwrap();
                    let collation = c["collation"].as_object().map(|_| {
                        tds::collation::Collation::new(
                            c["collation"]["lcid"].as_u64().unwrap() as u32,
                            c["collation"]["flags"].as_u64().unwrap() as u8,
                            c["collation"]["version"].as_u64().unwrap() as u8,
                            c["collation"]["sortId"].as_u64().unwrap() as u8,
                        )
                        .unwrap()
                    });
                    Column {
                        name: c["name"].as_str().unwrap().into(),
                        kind,
                        properties: msduck_core::result::Properties {
                            nullable: Some(flags & 1 != 0),
                            origin: if flags & 32 != 0 {
                                msduck_core::result::Origin::Expression
                            } else {
                                msduck_core::result::Origin::Stored
                            },
                        },
                        collation,
                    }
                })
                .collect::<Vec<_>>();
            let mut expected = vec![];
            tds::metadata(&mut expected, &columns).unwrap();
            for name in [format!("sys.{view}"), format!("[sys].[{view}]")] {
                let sql = format!("SELECT * FROM {name} WHERE 1=0");
                let (actual, ok) = session.batch_response(&sql, &Default::default(), false, None);
                assert!(ok, "{sql}: {actual:?}");
                assert!(
                    actual.starts_with(&expected),
                    "{sql}: expected {expected:?}, actual {actual:?}"
                );
            }
        }
    }

    #[test]
    fn index_creation_uses_table_owned_identity_and_transactional_catalog() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        for sql in [
            "CREATE TABLE dbo.a(id INT)",
            "CREATE TABLE dbo.b(id INT)",
            "CREATE INDEX ix ON dbo.a(id)",
            "CREATE UNIQUE INDEX ix ON dbo.b(id)",
        ] {
            let (out, ok) = session.batch_response(sql, &Default::default(), false, None);
            assert!(ok, "{sql}: {out:?}");
        }
        let indexes = crate::index_catalog::acquire_complete(&session.db).unwrap();
        assert_eq!(indexes.len(), 2);
        assert!(indexes.iter().all(|i| i.name == "ix" && i.index_id == 2));
        assert_ne!(indexes[0].backend_name, indexes[1].backend_name);
        for sql in [
            "BEGIN TRAN; CREATE INDEX transient ON dbo.a(id); ROLLBACK",
            "BEGIN TRAN; DROP TABLE dbo.a; ROLLBACK",
        ] {
            assert!(
                session
                    .batch_response(sql, &Default::default(), false, None)
                    .1,
                "{sql}"
            );
            assert_eq!(
                crate::index_catalog::acquire_complete(&session.db).unwrap(),
                indexes
            );
        }
        assert!(
            session
                .batch_response("DROP TABLE dbo.a", &Default::default(), false, None)
                .1
        );
        assert_eq!(
            crate::index_catalog::acquire_complete(&session.db)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT count(*) FROM main.__msduck_index_catalog",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
}

#[cfg(test)]
mod drop_index_runtime_tests {
    use super::*;
    #[test]
    fn captured_drop_requests_preserve_diagnostics_completion_and_remaining_indexes() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../reference/drop-index.json")).unwrap();
        for rpc in [false, true] {
            for case in fixture["results"].as_array().unwrap() {
                let server = crate::server::Server::open(":memory:").unwrap();
                let mut session = Session::new(server.connection().unwrap()).unwrap();
                for sql in fixture["setup"].as_array().unwrap() {
                    let (out, ok) = session.batch_response(
                        sql.as_str().unwrap(),
                        &Default::default(),
                        false,
                        None,
                    );
                    assert!(ok, "setup {sql}: {out:?}");
                }
                let sql = case["sql"].as_str().unwrap();
                let (actual, ok) = session.batch_response(sql, &Default::default(), rpc, None);
                let errors = case["result"]["errors"].as_array().unwrap();
                assert_eq!(ok, errors.is_empty(), "{sql}: {actual:?}");
                let mut expected = vec![];
                for e in errors {
                    tds::sql_error(
                        &mut expected,
                        &SqlError::from_utf16(
                            e["number"].as_i64().unwrap() as i32,
                            e["state"].as_u64().unwrap() as u8,
                            e["class"].as_u64().unwrap() as u8,
                            e["message"].as_str().unwrap().encode_utf16().collect(),
                        ),
                    );
                }
                let number = errors.first().map(|e| e["number"].as_i64().unwrap() as i32);
                if rpc {
                    // SQL Server RPC captures distinguish statement errors from
                    // syntax/option failures that terminate the entire request.
                    if number.is_none() || number == Some(3701) {
                        tds::done(
                            &mut expected,
                            0xff,
                            if number.is_some() { 3 } else { 1 },
                            201,
                            0,
                        );
                    }
                    if number != Some(3748) {
                        expected.push(0x79);
                        expected.extend(number.unwrap_or(0).to_le_bytes());
                    }
                    tds::done(
                        &mut expected,
                        0xfe,
                        if matches!(number, Some(156 | 159 | 3748)) {
                            2
                        } else {
                            0
                        },
                        224,
                        0,
                    );
                } else {
                    let done = &case["completion"][0];
                    tds::done(
                        &mut expected,
                        0xfd,
                        if errors.is_empty() { 0 } else { 2 },
                        done["curCmd"].as_u64().unwrap() as u16,
                        0,
                    );
                }
                assert_eq!(actual, expected, "{sql}");
                assert_eq!(
                    serde_json::json!([[session.rowcount, session.last_error]]),
                    case["state"]["sets"][0]["rows"],
                    "{sql}"
                );
                let rows = session
                    .db
                    // Direct DuckDB observation needs explicit trailing-space
                    // equality; public T-SQL predicates are covered over TDS.
                    .prepare(
                        &fixture["inventory"]
                            .as_str()
                            .unwrap()
                            .replace("o.type='U'", "rtrim(o.type)='U'"),
                    )
                    .unwrap()
                    .query_map([], |r| {
                        Ok(vec![
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ])
                    })
                    .unwrap()
                    .collect::<duckdb::Result<Vec<_>>>()
                    .unwrap();
                assert_eq!(
                    serde_json::json!(rows),
                    case["remaining"]["sets"][0]["rows"],
                    "{sql}"
                );
            }
        }
    }
    #[test]
    fn caller_transaction_can_rollback_a_drop_before_a_later_missing_target() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        assert!(
            session
                .batch_response(
                    "CREATE TABLE dbo.a(id INT); CREATE INDEX ix ON dbo.a(id); BEGIN TRAN",
                    &Default::default(),
                    false,
                    None
                )
                .1
        );
        assert!(
            !session
                .batch_response(
                    "DROP INDEX ix ON dbo.a, absent ON dbo.a",
                    &Default::default(),
                    false,
                    None
                )
                .1
        );
        assert_eq!(session.transactions, 1);
        assert!(
            crate::index_catalog::acquire_complete(&session.db)
                .unwrap()
                .is_empty()
        );
        assert!(
            session
                .batch_response("ROLLBACK", &Default::default(), false, None)
                .1
        );
        assert_eq!(
            crate::index_catalog::acquire_complete(&session.db)
                .unwrap()
                .len(),
            1
        );
    }
}

#[cfg(test)]
mod duplicate_index_runtime_tests {
    use super::*;
    #[test]
    fn duplicate_index_errors_match_captured_tokens_and_preserve_the_session() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../reference/index-catalog.json")).unwrap();
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = Session::new(server.connection().unwrap()).unwrap();
        for sql in [
            "CREATE TABLE dbo.a(id INT,value INT)",
            "CREATE INDEX ix ON dbo.a(id)",
            "CREATE SCHEMA alt",
            "CREATE TABLE alt.[odd.table]([odd.column] INT,other INT)",
            "CREATE INDEX [odd.index] ON alt.[odd.table]([odd.column])",
        ] {
            let (out, ok) = session.batch_response(sql, &Default::default(), false, None);
            assert!(ok, "{sql}: {out:?}");
        }
        let before = crate::index_catalog::acquire_complete(&session.db).unwrap();
        let mut checked = 0;
        for case in fixture["results"].as_array().unwrap() {
            if !case["id"].as_str().unwrap().starts_with("duplicate-") {
                continue;
            }
            let error = &case["result"]["errors"][0];
            let mut expected = vec![];
            tds::sql_error(
                &mut expected,
                &SqlError::from_utf16(
                    error["number"].as_i64().unwrap() as i32,
                    error["state"].as_u64().unwrap() as u8,
                    error["class"].as_u64().unwrap() as u8,
                    error["message"].as_str().unwrap().encode_utf16().collect(),
                ),
            );
            tds::done(
                &mut expected,
                0xfd,
                2,
                case["completion"][0]["curCmd"].as_u64().unwrap() as u16,
                0,
            );
            let (actual, ok) = session.batch_response(
                case["sql"].as_str().unwrap(),
                &Default::default(),
                false,
                None,
            );
            assert!(!ok);
            assert_eq!(actual, expected, "{}", case["id"]);
            assert_eq!(
                serde_json::json!([[session.rowcount, session.last_error, session.transactions]]),
                case["state"]["sets"][0]["rows"]
            );
            assert_eq!(
                crate::index_catalog::acquire_complete(&session.db).unwrap(),
                before
            );
            checked += 1;
        }
        assert_eq!(checked, 4);
        assert!(
            session
                .batch_response(
                    "CREATE INDEX usable ON dbo.a(value)",
                    &Default::default(),
                    false,
                    None
                )
                .1
        );
    }
}

/// The table a one-part name means when it is the alias of one of `tables`.
fn resolve_alias(name: &ObjectName, tables: &[&TableWithJoins]) -> Option<ObjectName> {
    let [ObjectNamePart::Identifier(single)] = name.0.as_slice() else {
        return None;
    };
    tables.iter().find_map(|table| {
        std::iter::once(&table.relation)
            .chain(table.joins.iter().map(|join| &join.relation))
            .find_map(|factor| match factor {
                TableFactor::Table {
                    name: table,
                    alias: Some(alias),
                    ..
                } if alias.name.value.eq_ignore_ascii_case(&single.value) => Some(table.clone()),
                _ => None,
            })
    })
}

/// The one-part UPDATE or DELETE target nodes that name a FROM alias
/// (`UPDATE i ... FROM t AS i`) rather than a table. They are identified by
/// address, so other relations spelled like the alias stay tables.
fn alias_targets(statement: &Statement) -> Vec<*const ObjectName> {
    let statement = match statement {
        Statement::Query(query) => match query.body.as_ref() {
            SetExpr::Update(statement) | SetExpr::Delete(statement) => statement,
            _ => return vec![],
        },
        statement => statement,
    };
    let (written, tables): (Vec<&ObjectName>, Vec<&TableWithJoins>) = match statement {
        Statement::Update(update) => {
            let TableFactor::Table { name, .. } = &update.table.relation else {
                return vec![];
            };
            let mut tables = vec![&update.table];
            if let Some(
                UpdateTableFromKind::BeforeSet(from) | UpdateTableFromKind::AfterSet(from),
            ) = &update.from
            {
                tables.extend(from);
            }
            (vec![name], tables)
        }
        Statement::Delete(delete) => {
            let (FromTable::WithFromKeyword(from) | FromTable::WithoutKeyword(from)) = &delete.from;
            (delete.tables.iter().collect(), from.iter().collect())
        }
        _ => return vec![],
    };
    written
        .into_iter()
        .filter(|name| resolve_alias(name, &tables).is_some())
        .map(|name| name as *const ObjectName)
        .collect()
}

/// The relation an INSERT, UPDATE or DELETE writes, with a FROM alias
/// resolved to its table.
fn dml_target(statement: &Statement) -> Option<ObjectName> {
    let resolve = |name: &ObjectName, tables: &[&TableWithJoins]| {
        resolve_alias(name, tables).unwrap_or_else(|| name.clone())
    };
    match statement {
        Statement::Insert(insert) => match &insert.table {
            TableObject::TableName(name) => Some(name.clone()),
            _ => None,
        },
        Statement::Update(update) => {
            let TableFactor::Table { name, .. } = &update.table.relation else {
                return None;
            };
            let mut tables = vec![&update.table];
            if let Some(
                UpdateTableFromKind::BeforeSet(from) | UpdateTableFromKind::AfterSet(from),
            ) = &update.from
            {
                tables.extend(from);
            }
            Some(resolve(name, &tables))
        }
        Statement::Delete(delete) => {
            let (FromTable::WithFromKeyword(from) | FromTable::WithoutKeyword(from)) = &delete.from;
            let tables = from.iter().collect::<Vec<_>>();
            match delete.tables.first() {
                Some(name) => Some(resolve(name, &tables)),
                None => match from.first().map(|table| &table.relation) {
                    Some(TableFactor::Table { name, .. }) => Some(name.clone()),
                    _ => None,
                },
            }
        }
        _ => None,
    }
}

/// The relations of a statement, by database (`Session::cross_database`).
/// Nodes are identified by address, so names that only share a spelling
/// with a CTE in another scope, a table function or an alias target stay
/// tables.
#[derive(Default)]
struct Relations {
    current: String,
    foreign: std::collections::BTreeMap<String, String>,
    /// Relation nodes of the current database.
    local: Vec<*const ObjectName>,
    /// Whether a SELECT INTO creates a table in the current database.
    into: bool,
    /// Whether a temporary table or table variable is among the relations.
    temporary: bool,
    /// The CTE names in scope, innermost query last.
    ctes: Vec<Vec<String>>,
    /// Table function call nodes, such as `GENERATE_SERIES(1, 2)`.
    functions: Vec<*const ObjectName>,
    /// UPDATE and DELETE target nodes that name a FROM alias
    /// (`alias_targets`).
    targets: Vec<*const ObjectName>,
}
impl Relations {
    fn collect<T: Visit>(node: &T, current: String, targets: &[*const ObjectName]) -> Self {
        let mut relations = Relations {
            current,
            targets: targets.to_vec(),
            ..Default::default()
        };
        let _ = node.visit(&mut relations);
        relations
    }

    /// Whether `name` is a relation node of the current database.
    fn local(&self, name: &ObjectName) -> bool {
        self.local.iter().any(|local| std::ptr::eq(*local, name))
    }

    fn add(&mut self, name: &ObjectName) {
        let node = name as *const ObjectName;
        if self.targets.contains(&node) || self.functions.contains(&node) {
            return;
        }
        match name.0.as_slice() {
            [ObjectNamePart::Identifier(database), _, _]
                if !database.value.eq_ignore_ascii_case(&self.current) =>
            {
                self.foreign
                    .entry(database.value.clone())
                    .or_insert_with(|| name.to_string());
            }
            [ObjectNamePart::Identifier(single)]
                if self
                    .ctes
                    .iter()
                    .flatten()
                    .any(|cte| cte.eq_ignore_ascii_case(&single.value)) => {}
            [ObjectNamePart::Identifier(single)] if single.value.starts_with(['#', '@']) => {
                self.temporary = true;
                self.local.push(node);
            }
            _ => self.local.push(node),
        }
    }
}
impl Visitor for Relations {
    type Break = ();
    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        self.ctes.push(
            query
                .with
                .iter()
                .flat_map(|with| &with.cte_tables)
                .map(|cte| cte.alias.name.value.clone())
                .collect(),
        );
        let mut body = query.body.as_ref();
        while let SetExpr::SetOperation { left, .. } = body {
            body = left;
        }
        // SELECT INTO creates its one- or two-part target in the
        // current database.
        if let SetExpr::Select(select) = body
            && select.into.is_some()
        {
            self.into = true;
        }
        ControlFlow::Continue(())
    }
    fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
        self.ctes.pop();
        ControlFlow::Continue(())
    }
    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::Table {
            name,
            args: Some(_),
            ..
        } = factor
        {
            self.functions.push(name);
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<()> {
        self.add(relation);
        ControlFlow::Continue(())
    }
}

/// A statement running in another database (`Session::enter_home`).
struct Home {
    previous: (String, String),
    /// The other database, in use while the statement runs.
    guard: crate::database_catalog::Use,
    /// The session's database, swapped out for `guard`, stays its own use.
    _counted: Counted,
}

/// Another database that a running statement uses (`Session::use_other`).
struct OtherUse {
    guard: crate::database_catalog::Use,
    _counted: Counted,
}

/// Databases running statements use, by catalog alias.
#[derive(Default)]
struct OtherUses(std::sync::Arc<std::sync::Mutex<HashMap<String, usize>>>);

impl OtherUses {
    fn add(&self, alias: &str) -> Counted {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(alias.to_string())
            .or_default() += 1;
        Counted {
            alias: alias.to_string(),
            uses: self.0.clone(),
        }
    }

    fn count(&self, alias: &str) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(alias)
            .copied()
            .unwrap_or(0)
    }
}

/// One entry of `OtherUses`, removed when dropped.
struct Counted {
    alias: String,
    uses: std::sync::Arc<std::sync::Mutex<HashMap<String, usize>>>,
}

impl Drop for Counted {
    fn drop(&mut self) {
        let mut uses = self
            .uses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = uses.get_mut(&self.alias) {
            *count -= 1;
            if *count == 0 {
                uses.remove(&self.alias);
            }
        }
    }
}

/// Whether `Session::execute` routes a statement (`execute_routed`): a query
/// or DML statement with a three-part relation name.
fn names_other_databases(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Query(_) | Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_)
    ) && visit_relations(statement, |relation| {
        if relation.0.len() >= 3 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}

/// Where `Session::execute` runs a statement.
enum Routed {
    Here(Box<Statement>),
    Home(Box<Statement>, String, Vec<String>),
    Mixed(Box<Statement>, Vec<String>),
}

/// How a statement refers to databases other than the current one.
enum CrossDatabase {
    /// Only the current database.
    Local,
    /// `0`, another database, in which the statement runs, because it
    /// references nothing else or writes it. It reads the current database
    /// and the others listed in `1`.
    Home(String, Vec<String>),
    /// The current database and the others listed, or several others,
    /// which it reads.
    Mixed(Vec<String>),
}

/// Name relations as seen from `home`, the database a statement runs in:
/// drop `home`'s catalog and give relations of `current`, the session's
/// database, its catalog.
fn rehome<T: Visit + VisitMut>(
    node: &mut T,
    home: &str,
    current: &str,
    targets: &[*const ObjectName],
) {
    let relations = Relations::collect(&*node, current.to_string(), targets);
    let _ = visit_relations_mut(node, |relation| {
        if let [ObjectNamePart::Identifier(database), _, _] = relation.0.as_slice()
            && database.value == home
        {
            relation.0.remove(0);
        } else if relations.local(relation) {
            if relation.0.len() == 1 {
                relation
                    .0
                    .insert(0, ObjectNamePart::Identifier(Ident::new("dbo")));
            }
            relation.0.insert(
                0,
                ObjectNamePart::Identifier(Ident::with_quote('"', current)),
            );
        }
        ControlFlow::<()>::Continue(())
    });
}
