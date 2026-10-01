//! BACKUP DATABASE, RESTORE HEADERONLY/FILELISTONLY/VERIFYONLY/DATABASE and
//! msdb backup history. See docs/gaps-backup.md.
//!
//! A backup set's payload is a complete DuckDB database made with
//! `COPY FROM DATABASE`, so it is one consistent snapshot of the database,
//! for in-memory and file-backed servers alike (see `media`). RESTORE
//! DATABASE stages that file in the catalog directory and lets the database
//! catalog attach it as a new database or in place of an existing one.
use super::Feature;
use crate::database_catalog::Diagnostics;
use crate::engine::{Execution, Parameter, Session, StatementErrors, sql_error_from_message};
use crate::tds;
use anyhow::{Context, Result, bail};
use msduck_core::diagnostic::SqlError;
use msduck_core::value::Value as ParameterValue;
use msduck_sql::dialect::ext::backup::{self as syntax, Operand, Operation, Request};
use sqlparser::ast::{Expr, Statement, UnaryOperator};
use std::collections::HashMap;
use std::path::Path;

mod media;
mod msdb;
mod rows;

use media::{FileEntry, Header, Media, Set};
use rows::{Cell, Kind};

#[derive(Default)]
pub(crate) struct State {
    /// Whether the current request is an RPC, for intermediate DONE tokens.
    rpc: bool,
}

pub(super) struct Hooks;

impl Feature for Hooks {
    fn name(&self) -> &'static str {
        "backup"
    }

    fn batch(
        &self,
        session: &mut Session,
        _sql: &str,
        _parameters: &HashMap<String, Parameter>,
        rpc: bool,
    ) -> Option<(Vec<u8>, bool)> {
        session.ext.backup.rpc = rpc;
        None
    }

    fn statement(
        &self,
        session: &mut Session,
        statement: &mut Statement,
        parameters: &mut HashMap<String, Parameter>,
    ) -> Result<Option<Execution>> {
        if let Some((request, args)) = syntax::request(statement) {
            let args: Vec<Expr> = args.into_iter().cloned().collect();
            let operation = request.operation;
            return run(session, &request, &args, parameters)
                .map(Some)
                .map_err(|error| terminate(operation, error));
        }
        msdb::route(session, statement, parameters)
    }
}

/// SQL Server ends every failed BACKUP or RESTORE with 3013, except for
/// errors raised while compiling the statement (an unknown option).
fn terminate(operation: Operation, error: anyhow::Error) -> anyhow::Error {
    let mut errors = match error.downcast::<Diagnostics>() {
        Ok(Diagnostics(errors)) => errors,
        Err(error) => match error.downcast::<SqlError>() {
            Ok(error) if error.number == 155 => return error.into(),
            Ok(error) => vec![error],
            Err(error) => vec![sql_error_from_message(&format!("{error:#}"))],
        },
    };
    errors.push(SqlError::new(
        3013,
        1,
        format!("{} is terminating abnormally.", operation.statement()),
    ));
    StatementErrors(errors).into()
}

/// Options SQL Server accepts for each statement; those that do not change
/// what msduck stores are accepted and ignored.
const BACKUP_OPTIONS: &[&str] = &[
    "FORMAT",
    "NOFORMAT",
    "INIT",
    "NOINIT",
    "NAME",
    "DESCRIPTION",
    "COMPRESSION",
    "NO_COMPRESSION",
    "COPY_ONLY",
    "STATS",
    "CHECKSUM",
    "NO_CHECKSUM",
    "SKIP",
    "NOSKIP",
    "REWIND",
    "NOREWIND",
    "UNLOAD",
    "NOUNLOAD",
    "MEDIANAME",
    "MEDIADESCRIPTION",
    "BUFFERCOUNT",
    "MAXTRANSFERSIZE",
    "BLOCKSIZE",
    "CONTINUE_AFTER_ERROR",
    "STOP_ON_ERROR",
    "EXPIREDATE",
    "RETAINDAYS",
    "DIFFERENTIAL",
    "ENCRYPTION",
    "NORECOVERY",
    "NO_TRUNCATE",
    "STANDBY",
    "PASSWORD",
    "MEDIAPASSWORD",
];
const RESTORE_OPTIONS: &[&str] = &[
    "FILE",
    "REPLACE",
    "RECOVERY",
    "NORECOVERY",
    "STANDBY",
    "STATS",
    "CHECKSUM",
    "NO_CHECKSUM",
    "REWIND",
    "NOREWIND",
    "UNLOAD",
    "NOUNLOAD",
    "BUFFERCOUNT",
    "MAXTRANSFERSIZE",
    "BLOCKSIZE",
    "CONTINUE_AFTER_ERROR",
    "STOP_ON_ERROR",
    "KEEP_REPLICATION",
    "KEEP_CDC",
    "ENABLE_BROKER",
    "ERROR_BROKER_CONVERSATIONS",
    "NEW_BROKER",
    "MEDIANAME",
    "MEDIAPASSWORD",
    "PASSWORD",
    "PARTIAL",
    "RESTRICTED_USER",
    "RESTART",
    "STOPAT",
    "STOPATMARK",
    "STOPBEFOREMARK",
    "LOADHISTORY",
    "FILESTREAM",
    "CREDENTIAL",
];
/// Accepted options that would change the result; msduck refuses them.
const UNSUPPORTED: &[&str] = &[
    "DIFFERENTIAL",
    "ENCRYPTION",
    "NORECOVERY",
    "NO_TRUNCATE",
    "STANDBY",
    "PASSWORD",
    "MEDIAPASSWORD",
    "PARTIAL",
    "RESTRICTED_USER",
    "RESTART",
    "STOPAT",
    "STOPATMARK",
    "STOPBEFOREMARK",
    "FILESTREAM",
    "CREDENTIAL",
];

fn unsupported(what: &str) -> anyhow::Error {
    anyhow::anyhow!("{what} is not supported by msduck")
}

fn run(
    session: &mut Session,
    request: &Request,
    args: &[Expr],
    parameters: &HashMap<String, Parameter>,
) -> Result<Execution> {
    let known = if request.operation.is_backup() {
        BACKUP_OPTIONS
    } else {
        RESTORE_OPTIONS
    };
    let statement = if request.operation.is_backup() {
        "BACKUP"
    } else {
        "RESTORE"
    };
    for option in &request.options {
        if !known.contains(&option.name.as_str()) {
            let mut error = SqlError::new(
                155,
                1,
                format!("'{}' is not a recognized {statement} option.", option.name),
            );
            error.severity = 15;
            bail!(error);
        }
    }
    let values = Values { args, parameters };
    let database = request
        .database
        .as_ref()
        .map(|operand| values.text(operand))
        .transpose()?;
    if let Some(option) = request
        .options
        .iter()
        .find(|option| UNSUPPORTED.contains(&option.name.as_str()))
    {
        // BACKUP LOG of a SIMPLE database fails before its options matter.
        if request.operation != Operation::BackupLog {
            return Err(unsupported(&format!(
                "{} WITH {}",
                request.operation.statement(),
                option.name
            )));
        }
    }
    if !request.file_clauses.is_empty() {
        return Err(unsupported(&format!(
            "{} of individual files or filegroups",
            request.operation.statement()
        )));
    }
    if request.mirror {
        return Err(unsupported("MIRROR TO"));
    }
    let device = match request.devices.as_slice() {
        [] => None,
        [device] if device.kind == "DISK" => Some(values.text(&device.value)?),
        [device] if device.kind.is_empty() => return Err(unsupported("A logical backup device")),
        [device] => return Err(unsupported(&format!("A {} backup device", device.kind))),
        _ => return Err(unsupported("A striped backup to several devices")),
    };
    let stats = match request.option("STATS") {
        None => None,
        Some(None) => Some(10),
        Some(Some(value)) => Some(values.int(value)?.clamp(1, 100)),
    };
    let file = request
        .option("FILE")
        .flatten()
        .map(|value| values.int(value))
        .transpose()?
        .unwrap_or(1);
    let name = request
        .option("NAME")
        .flatten()
        .map(|value| values.optional_text(value))
        .transpose()?
        .flatten();
    let description = request
        .option("DESCRIPTION")
        .flatten()
        .map(|value| values.optional_text(value))
        .transpose()?
        .flatten();
    let media_name = request
        .option("MEDIANAME")
        .flatten()
        .map(|value| values.optional_text(value))
        .transpose()?
        .flatten();
    let media_description = request
        .option("MEDIADESCRIPTION")
        .flatten()
        .map(|value| values.optional_text(value))
        .transpose()?
        .flatten();
    let moves = request
        .moves
        .iter()
        .map(|(logical, physical)| Ok((values.text(logical)?, values.text(physical)?)))
        .collect::<Result<Vec<_>>>()?;
    let messages = Messages::new(session, request.operation);
    match request.operation {
        Operation::BackupDatabase | Operation::BackupLog => backup(
            session,
            request,
            Backup {
                database: database.unwrap_or_default(),
                device: device.unwrap_or_default(),
                stats,
                name,
                description,
                media_name,
                media_description,
            },
            messages,
        ),
        Operation::RestoreHeaderOnly => {
            let (path, sets) = open_media(session, request.operation, device)?;
            headeronly(session, &path, &sets)
        }
        Operation::RestoreFileListOnly => {
            let (path, sets) = open_media(session, request.operation, device)?;
            let set = position(&sets, file, &path)?;
            filelistonly(session, set)
        }
        Operation::RestoreVerifyOnly => {
            let (path, sets) = open_media(session, request.operation, device)?;
            let set = position(&sets, file, &path)?;
            if !media::verify(Path::new(&path), set)? {
                bail!(damaged(set.position));
            }
            let mut messages = messages;
            messages.info(
                3262,
                format!("The backup set on file {} is valid.", set.position),
            );
            Ok(messages.finish(None))
        }
        Operation::RestoreDatabase => {
            let Some(device) = device else {
                return Err(unsupported("RESTORE DATABASE without FROM"));
            };
            restore(
                session,
                request,
                Restore {
                    database: database.unwrap_or_default(),
                    device,
                    file,
                    stats,
                    moves,
                },
                messages,
            )
        }
        Operation::RestoreLog => Err(unsupported("RESTORE LOG")),
        Operation::RestoreLabelOnly => Err(unsupported("RESTORE LABELONLY")),
    }
}

/// Values of the carrier's arguments: literals and variables.
struct Values<'a> {
    args: &'a [Expr],
    parameters: &'a HashMap<String, Parameter>,
}

impl Values<'_> {
    fn value(&self, operand: &Operand) -> Result<ParameterValue> {
        let expr = match operand {
            Operand::Name(name) => return Ok(ParameterValue::Text(name.clone())),
            Operand::Arg(index) => self
                .args
                .get(*index)
                .context("missing BACKUP or RESTORE argument")?,
        };
        evaluate(expr, self.parameters)
    }

    fn optional_text(&self, operand: &Operand) -> Result<Option<String>> {
        Ok(match self.value(operand)? {
            ParameterValue::Null => None,
            ParameterValue::Text(text) => Some(text),
            ParameterValue::Unicode(units) => Some(String::from_utf16_lossy(&units)),
            ParameterValue::Blob(_) => bail!("a backup name or path cannot be binary"),
            other => Some(scalar_text(&other)),
        })
    }

    fn text(&self, operand: &Operand) -> Result<String> {
        self.optional_text(operand)?
            .context("a backup database name, device or file name cannot be NULL")
    }

    fn int(&self, operand: &Operand) -> Result<i64> {
        let value = self.value(operand)?;
        Ok(match value {
            ParameterValue::TinyInt(v) => v.into(),
            ParameterValue::UTinyInt(v) => v.into(),
            ParameterValue::SmallInt(v) => v.into(),
            ParameterValue::Int(v) => v.into(),
            ParameterValue::BigInt(v) => v,
            ParameterValue::Text(text) => text.trim().parse()?,
            other => scalar_text(&other).parse()?,
        })
    }
}

fn scalar_text(value: &ParameterValue) -> String {
    match value {
        ParameterValue::Null => String::new(),
        ParameterValue::Boolean(v) => u8::from(*v).to_string(),
        ParameterValue::TinyInt(v) => v.to_string(),
        ParameterValue::UTinyInt(v) => v.to_string(),
        ParameterValue::SmallInt(v) => v.to_string(),
        ParameterValue::Int(v) => v.to_string(),
        ParameterValue::BigInt(v) => v.to_string(),
        ParameterValue::Float(v) => v.to_string(),
        ParameterValue::Double(v) => v.to_string(),
        ParameterValue::Text(text) => text.clone(),
        ParameterValue::Unicode(units) => String::from_utf16_lossy(units),
        other => format!("{other:?}"),
    }
}

/// A literal, a variable, or a negated number.
fn evaluate(expr: &Expr, parameters: &HashMap<String, Parameter>) -> Result<ParameterValue> {
    use sqlparser::ast::Value;
    Ok(match expr {
        Expr::Value(value) => match &value.value {
            Value::SingleQuotedString(text) | Value::NationalStringLiteral(text) => {
                ParameterValue::Text(text.clone())
            }
            Value::Number(number, _) => ParameterValue::BigInt(number.parse()?),
            Value::Null => ParameterValue::Null,
            other => bail!("unsupported BACKUP or RESTORE value {other}"),
        },
        Expr::Identifier(ident) if ident.value.starts_with('@') => {
            let name = ident.value.to_lowercase();
            parameters
                .get(&name)
                .map(|parameter| parameter.value.clone())
                .ok_or_else(|| anyhow::anyhow!("Must declare the scalar variable {name}"))?
        }
        Expr::Nested(inner) => evaluate(inner, parameters)?,
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => match evaluate(expr, parameters)? {
            ParameterValue::BigInt(value) => ParameterValue::BigInt(-value),
            other => bail!("cannot negate {other:?}"),
        },
        other => bail!("unsupported BACKUP or RESTORE value {other}; use a literal or a variable"),
    })
}

/// INFO messages, each followed by an intermediate DONE as SQL Server sends
/// them, and the statement's completion command.
struct Messages {
    tokens: Vec<u8>,
    pending: Option<Vec<u8>>,
    rpc: bool,
    nocount: bool,
    command: u16,
}

impl Messages {
    fn new(session: &Session, operation: Operation) -> Self {
        // Completion commands captured in reference/gaps-backup.json.
        let command = match operation {
            Operation::BackupDatabase => 228,
            Operation::BackupLog => 235,
            Operation::RestoreDatabase | Operation::RestoreLog => 229,
            Operation::RestoreHeaderOnly | Operation::RestoreLabelOnly => 250,
            Operation::RestoreFileListOnly => 376,
            Operation::RestoreVerifyOnly => 377,
        };
        Self {
            tokens: Vec::new(),
            pending: None,
            rpc: session.ext.backup.rpc,
            nocount: session.nocount,
            command,
        }
    }

    fn flush(&mut self) {
        if let Some(message) = self.pending.take() {
            self.tokens.extend(message);
            if !(self.rpc && self.nocount) {
                tds::done(
                    &mut self.tokens,
                    if self.rpc { 0xff } else { 0xfd },
                    1,
                    self.command,
                    0,
                );
            }
        }
    }

    fn info(&mut self, number: i32, message: String) {
        self.flush();
        let mut token = Vec::new();
        let units: Vec<u16> = message.encode_utf16().collect();
        tds::diagnostic_utf16(
            &mut token,
            tds::DiagnosticKind::Information,
            0,
            1,
            number,
            &units,
        );
        self.pending = Some(token);
    }

    fn stats(&mut self, stats: Option<i64>) {
        if let Some(step) = stats {
            for percent in (1..=100 / step).map(|n| n * step) {
                self.info(3211, format!("{percent} percent processed."));
            }
        }
    }

    /// The tokens and completion; `result` is a result set to send before
    /// the final DONE, with its row count.
    fn finish(mut self, result: Option<(Vec<u8>, u64)>) -> Execution {
        self.flush_last();
        if let Some((tokens, rows)) = result {
            self.tokens.extend(tokens);
            tds::done(
                &mut self.tokens,
                if self.rpc { 0xff } else { 0xfd },
                if self.nocount { 1 } else { 17 },
                230,
                if self.nocount { 0 } else { rows },
            );
        }
        Execution::statement(self.tokens, None, self.command)
    }

    /// The last message is followed by the statement's own DONE.
    fn flush_last(&mut self) {
        if let Some(message) = self.pending.take() {
            self.tokens.extend(message);
        }
    }
}

/// Microseconds since the Unix epoch in local time, as GETDATE reports it.
fn local_now() -> i64 {
    let clock = crate::current_time::now();
    clock.utc_ticks / 10 + i64::from(clock.offset_minutes) * 60_000_000
}

fn server_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "msduck".into())
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// SQL Server's operating system error text for a failed device open.
fn device_error(path: &str, state: u8, error: &std::io::Error) -> SqlError {
    let (code, text) = match error.kind() {
        std::io::ErrorKind::NotFound if state == 2 => {
            (2, "The system cannot find the file specified.")
        }
        _ => (5, "Access is denied."),
    };
    SqlError::new(
        3201,
        state,
        format!("Cannot open backup device '{path}'. Operating system error {code}({text})."),
    )
}

fn malformed(path: &str) -> SqlError {
    SqlError::new(
        3241,
        0,
        format!(
            "The media family on device '{path}' is incorrectly formed. SQL Server cannot process this media family."
        ),
    )
}

fn damaged(position: u32) -> anyhow::Error {
    anyhow::anyhow!(
        "the backup set on file {position} is damaged: its contents do not match the checksum recorded when it was written"
    )
}

/// Read a media file for RESTORE.
fn open_media(
    session: &Session,
    operation: Operation,
    device: Option<String>,
) -> Result<(String, Vec<Set>)> {
    if session.transactions > 0 {
        bail!(in_transaction());
    }
    let Some(path) = device else {
        return Err(unsupported(&format!(
            "{} without FROM",
            operation.statement()
        )));
    };
    match media::read(Path::new(&path)) {
        Ok(Media::Sets(sets)) => Ok((path, sets)),
        Ok(Media::Missing) => bail!(device_error(
            &path,
            2,
            &std::io::Error::from(std::io::ErrorKind::NotFound)
        )),
        Ok(Media::Empty) => bail!(SqlError::new(
            3254,
            1,
            format!("The volume on device '{path}' is empty.")
        )),
        Ok(Media::Malformed) => bail!(malformed(&path)),
        Err(error) => {
            let io = error
                .downcast_ref::<std::io::Error>()
                .map(|error| std::io::Error::from(error.kind()))
                .unwrap_or_else(|| std::io::Error::from(std::io::ErrorKind::PermissionDenied));
            bail!(device_error(&path, 2, &io))
        }
    }
}

fn position<'a>(sets: &'a [Set], file: i64, path: &str) -> Result<&'a Set> {
    sets.iter()
        .find(|set| i64::from(set.position) == file)
        .ok_or_else(|| {
            SqlError::new(
                3287,
                1,
                format!("The file ID {file} on device '{path}' is incorrectly formed and can not be read."),
            )
            .into()
        })
}

fn in_transaction() -> SqlError {
    SqlError::new(
        3021,
        0,
        "Cannot perform a backup or restore operation within a transaction.",
    )
}

/// Pages SQL Server would report for a number of bytes.
fn pages(bytes: u64) -> u64 {
    bytes.div_ceil(8192)
}

fn summary(operation: Operation, pages: u64, bytes: u64, started: std::time::Instant) -> String {
    let seconds = started.elapsed().as_secs_f64().max(0.001);
    format!(
        "{} successfully processed {pages} pages in {seconds:.3} seconds ({:.3} MB/sec).",
        operation.statement(),
        bytes as f64 / 1_048_576.0 / seconds
    )
}

struct Backup {
    database: String,
    device: String,
    stats: Option<i64>,
    name: Option<String>,
    description: Option<String>,
    media_name: Option<String>,
    media_description: Option<String>,
}

fn backup(
    session: &mut Session,
    request: &Request,
    backup: Backup,
    mut messages: Messages,
) -> Result<Execution> {
    let started = std::time::Instant::now();
    let catalog = session.database.catalog().clone();
    let missing = || {
        SqlError::new(
            911,
            11,
            format!(
                "Database '{}' does not exist. Make sure that the name is entered correctly.",
                backup.database
            ),
        )
    };
    // msdb exists from its first use, a backup of it included.
    if backup
        .database
        .eq_ignore_ascii_case(crate::database_catalog::MSDB)
        && session.transactions == 0
    {
        msdb::ensure(session)?;
    }
    if catalog.resolve(&session.db, &backup.database)?.is_none() {
        bail!(missing());
    }
    if request.operation == Operation::BackupLog {
        // Every msduck database uses the SIMPLE recovery model.
        bail!(SqlError::new(
            4208,
            1,
            "The statement BACKUP LOG is not allowed while the recovery model is SIMPLE. Use BACKUP DATABASE or change the recovery model using ALTER DATABASE."
        ));
    }
    if session.transactions > 0 {
        bail!(in_transaction());
    }
    // Reading the files may record the database's recovery family, so it
    // runs only outside a transaction.
    let files = catalog
        .files(&session.db, &backup.database)?
        .ok_or_else(missing)?;
    let path = backup.device.clone();
    let device = Path::new(&path);
    // The device's directory must exist; SQL Server on Linux reports a
    // missing directory as access denied.
    if !device
        .parent()
        .map(|parent| parent.as_os_str().is_empty() || parent.is_dir())
        .unwrap_or(false)
        || device.is_dir()
    {
        bail!(device_error(
            &path,
            1,
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied)
        ));
    }
    if writes_database_file(session, device)? {
        bail!(SqlError::new(
            3201,
            1,
            format!(
                "Cannot open backup device '{path}'. Operating system error 32(The process cannot access the file because it is being used by another process.)."
            )
        ));
    }
    // One BACKUP writes media at a time, so a concurrent append never
    // copies stale sets or loses its own.
    static DEVICES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _device = DEVICES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let format = request.has("FORMAT");
    let init = request.has("INIT");
    let existing = match media::read(device) {
        Ok(Media::Sets(sets)) => Some(sets),
        Ok(Media::Missing | Media::Empty) => None,
        Ok(Media::Malformed) if format => None,
        Ok(Media::Malformed) => bail!(malformed(&path)),
        Err(error) => {
            let io = error
                .downcast_ref::<std::io::Error>()
                .map(|error| std::io::Error::from(error.kind()))
                .unwrap_or_else(|| std::io::Error::from(std::io::ErrorKind::PermissionDenied));
            bail!(device_error(&path, 1, &io))
        }
    };
    msdb::ensure(session)?;
    // The media header: FORMAT writes a new one; INIT keeps it but not its
    // sets; otherwise the new set is appended.
    let previous = existing.as_ref().filter(|_| !format);
    let media_header = previous
        .and_then(|sets| sets.first())
        .map(|set| &set.header);
    let kept: &[Set] = match (previous, init) {
        (Some(sets), false) => sets,
        _ => &[],
    };
    let media_compressed =
        media_header.map_or(request.has("COMPRESSION"), |header| header.media_compressed);
    let compressed = request.has("COMPRESSION") || media_compressed;
    let staging = catalog.staging_path("backup")?;
    let snapshot = snapshot(session, &files.alias, &staging);
    let result = snapshot.and_then(|()| {
        let payload_size = std::fs::metadata(&staging)?.len();
        let digest = media::sha256(&mut std::fs::File::open(&staging)?)?;
        let finish = local_now();
        let header = Header {
            media_guid: media_header.map_or_else(crate::database_catalog::new_guid, |header| {
                header.media_guid.clone()
            }),
            media_name: media_header.map_or(backup.media_name.clone(), |header| {
                backup.media_name.clone().or(header.media_name.clone())
            }),
            media_description: media_header.map_or(backup.media_description.clone(), |header| {
                backup
                    .media_description
                    .clone()
                    .or(header.media_description.clone())
            }),
            media_compressed,
            name: backup.name.clone(),
            description: backup.description.clone(),
            compressed,
            copy_only: request.has("COPY_ONLY"),
            checksum: request.has("CHECKSUM"),
            user_name: session.original_login.clone(),
            server_name: server_name(),
            database_name: files.name.clone(),
            // The registry records UTC; SQL Server reports local time.
            database_creation_date: if files.database_id == crate::database_catalog::MASTER_ID {
                files.create_date
            } else {
                files.create_date
                    + i64::from(crate::current_time::now().offset_minutes) * 60_000_000
            },
            backup_start: finish,
            backup_finish: finish,
            family_guid: files.family_guid.to_uppercase(),
            database_guid: files.database_guid.to_uppercase(),
            backup_set_guid: crate::database_catalog::new_guid(),
            files: vec![
                FileEntry {
                    logical: files.data_name.clone(),
                    physical: files.data_path.clone(),
                    kind: 'D',
                    size: payload_size,
                },
                FileEntry {
                    logical: files.log_name.clone(),
                    physical: files.log_path.clone(),
                    kind: 'L',
                    size: 0,
                },
            ],
            payload_sha256: digest,
        };
        media::write(
            device,
            Some((device, kept)).filter(|(_, kept)| !kept.is_empty()),
            &header,
            &staging,
        )
        .map_err(|error| {
            let io = error
                .downcast_ref::<std::io::Error>()
                .map(|error| std::io::Error::from(error.kind()));
            match io {
                Some(io) => device_error(&path, 1, &io).into(),
                None => error,
            }
        })?;
        Ok((header, payload_size))
    });
    let _ = std::fs::remove_file(&staging);
    let _ = std::fs::remove_file(staging.with_extension("duckdb.wal"));
    let (header, payload_size) = result?;
    let position = kept.len() as u32 + 1;
    history::backup(session, &header, position, &path, payload_size)?;
    messages.stats(backup.stats);
    let data_pages = pages(payload_size);
    messages.info(
        4035,
        format!(
            "Processed {data_pages} pages for database '{}', file '{}' on file {position}.",
            files.name, files.data_name
        ),
    );
    messages.info(
        4035,
        format!(
            "Processed 0 pages for database '{}', file '{}' on file {position}.",
            files.name, files.log_name
        ),
    );
    messages.info(
        3014,
        summary(request.operation, data_pages, payload_size, started),
    );
    Ok(messages.finish(None))
}

/// Whether a device names an attached database's file or its WAL, which a
/// backup must never replace.
fn writes_database_file(session: &Session, device: &Path) -> Result<bool> {
    let resolve = |path: &Path| -> Option<std::path::PathBuf> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        let parent = std::fs::canonicalize(parent.unwrap_or(Path::new("."))).ok()?;
        Some(parent.join(path.file_name()?))
    };
    let Some(device) = resolve(device) else {
        return Ok(false);
    };
    let mut query = session
        .db
        .prepare("SELECT path FROM duckdb_databases() WHERE path IS NOT NULL")?;
    let paths = query
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    Ok(paths.iter().any(|path| {
        resolve(Path::new(path)).is_some_and(|file| {
            let mut wal = file.clone().into_os_string();
            wal.push(".wal");
            device == file || device.as_os_str() == wal
        })
    }))
}

/// Copy a database into a new DuckDB file in one statement, which reads a
/// single consistent snapshot.
fn snapshot(session: &Session, alias: &str, staging: &Path) -> Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let target = format!(
        "__msduck_backup_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let staging = staging
        .to_str()
        .context("backup staging path is not UTF-8")?;
    session.db.execute_batch(&format!(
        "ATTACH {} AS {}",
        literal(staging),
        quote(&target)
    ))?;
    let copied = session.db.execute_batch(&format!(
        "COPY FROM DATABASE {} TO {}",
        quote(alias),
        quote(&target)
    ));
    let detached = session
        .db
        .execute_batch(&format!("DETACH DATABASE {}", quote(&target)));
    copied?;
    detached?;
    Ok(())
}

const HEADER_COLUMNS: &[(&str, Kind)] = &[
    ("BackupName", Kind::Nvarchar(128)),
    ("BackupDescription", Kind::Nvarchar(255)),
    ("BackupType", Kind::TinyInt),
    ("ExpirationDate", Kind::DateTime),
    ("Compressed", Kind::TinyInt),
    ("Position", Kind::SmallInt),
    ("DeviceType", Kind::TinyInt),
    ("UserName", Kind::Nvarchar(128)),
    ("ServerName", Kind::Nvarchar(128)),
    ("DatabaseName", Kind::Nvarchar(128)),
    ("DatabaseVersion", Kind::Int),
    ("DatabaseCreationDate", Kind::DateTime),
    ("BackupSize", Kind::BigInt),
    ("FirstLSN", Kind::Numeric(25)),
    ("LastLSN", Kind::Numeric(25)),
    ("CheckpointLSN", Kind::Numeric(25)),
    ("DatabaseBackupLSN", Kind::Numeric(25)),
    ("BackupStartDate", Kind::DateTime),
    ("BackupFinishDate", Kind::DateTime),
    ("SortOrder", Kind::SmallInt),
    ("CodePage", Kind::SmallInt),
    ("UnicodeLocaleId", Kind::Int),
    ("UnicodeComparisonStyle", Kind::Int),
    ("CompatibilityLevel", Kind::TinyInt),
    ("SoftwareVendorId", Kind::Int),
    ("SoftwareVersionMajor", Kind::Int),
    ("SoftwareVersionMinor", Kind::Int),
    ("SoftwareVersionBuild", Kind::Int),
    ("MachineName", Kind::Nvarchar(128)),
    ("Flags", Kind::Int),
    ("BindingID", Kind::Guid),
    ("RecoveryForkID", Kind::Guid),
    ("Collation", Kind::Nvarchar(128)),
    ("FamilyGUID", Kind::Guid),
    ("HasBulkLoggedData", Kind::Bit),
    ("IsSnapshot", Kind::Bit),
    ("IsReadOnly", Kind::Bit),
    ("IsSingleUser", Kind::Bit),
    ("HasBackupChecksums", Kind::Bit),
    ("IsDamaged", Kind::Bit),
    ("BeginsLogChain", Kind::Bit),
    ("HasIncompleteMetaData", Kind::Bit),
    ("IsForceOffline", Kind::Bit),
    ("IsCopyOnly", Kind::Bit),
    ("FirstRecoveryForkID", Kind::Guid),
    ("ForkPointLSN", Kind::Numeric(25)),
    ("RecoveryModel", Kind::Nvarchar(60)),
    ("DifferentialBaseLSN", Kind::Numeric(25)),
    ("DifferentialBaseGUID", Kind::Guid),
    ("BackupTypeDescription", Kind::Nvarchar(128)),
    ("BackupSetGUID", Kind::Guid),
    ("CompressedBackupSize", Kind::BigInt),
    ("Containment", Kind::TinyInt),
    ("KeyAlgorithm", Kind::Nvarchar(32)),
    ("EncryptorThumbprint", Kind::Varbinary(20)),
    ("EncryptorType", Kind::Nvarchar(32)),
    ("LastValidRestoreTime", Kind::DateTime),
    ("TimeZone", Kind::SmallInt),
    ("CompressionAlgorithm", Kind::Nvarchar(32)),
];

/// The SQL Server version a backup reports: msduck answers as SQL Server
/// 2022 (compatibility level 160), as captured in reference/gaps-backup.json.
const DATABASE_VERSION: i64 = 957;
const SOFTWARE_VERSION: (i64, i64, i64) = (16, 0, 4236);
const SOFTWARE_VENDOR: i64 = 4608;

fn flags(header: &Header) -> i64 {
    512 + if header.checksum { 16 } else { 0 } + if header.copy_only { 1024 } else { 0 }
}

fn header_row(set: &Set) -> Vec<Cell> {
    let header = &set.header;
    let size = i64::try_from(set.payload_len).unwrap_or(i64::MAX);
    vec![
        Cell::optional(header.name.as_deref()),
        Cell::optional(header.description.as_deref()),
        Cell::Int(1),
        Cell::Null,
        Cell::Int(i64::from(header.compressed)),
        Cell::Int(i64::from(set.position)),
        Cell::Int(2),
        Cell::text(&header.user_name),
        Cell::text(&header.server_name),
        Cell::text(&header.database_name),
        Cell::Int(DATABASE_VERSION),
        Cell::DateTime(header.database_creation_date),
        Cell::Int(size),
        Cell::Null,
        Cell::Null,
        Cell::Null,
        Cell::Null,
        Cell::DateTime(header.backup_start),
        Cell::DateTime(header.backup_finish),
        Cell::Int(52),
        Cell::Int(0),
        Cell::Int(1033),
        Cell::Int(196609),
        Cell::Int(160),
        Cell::Int(SOFTWARE_VENDOR),
        Cell::Int(SOFTWARE_VERSION.0),
        Cell::Int(SOFTWARE_VERSION.1),
        Cell::Int(SOFTWARE_VERSION.2),
        Cell::text(&header.server_name),
        Cell::Int(flags(header)),
        Cell::Guid(header.database_guid.clone()),
        Cell::Guid(header.family_guid.clone()),
        Cell::text("SQL_Latin1_General_CP1_CI_AS"),
        Cell::Guid(header.family_guid.clone()),
        Cell::Bit(false),
        Cell::Bit(false),
        Cell::Bit(false),
        Cell::Bit(false),
        Cell::Bit(header.checksum),
        Cell::Bit(false),
        Cell::Bit(false),
        Cell::Bit(false),
        Cell::Bit(false),
        Cell::Bit(header.copy_only),
        Cell::Guid(header.family_guid.clone()),
        Cell::Null,
        Cell::text("SIMPLE"),
        Cell::Null,
        Cell::Null,
        Cell::text("Database"),
        Cell::Guid(header.backup_set_guid.clone()),
        Cell::Int(size),
        Cell::Int(0),
        Cell::Null,
        Cell::Null,
        Cell::Null,
        Cell::Null,
        Cell::Int(0),
        if header.compressed {
            Cell::text("MS_XPRESS")
        } else {
            Cell::Null
        },
    ]
}

fn headeronly(session: &Session, _path: &str, sets: &[Set]) -> Result<Execution> {
    let rows: Vec<Vec<Cell>> = sets.iter().map(header_row).collect();
    let mut tokens = Vec::new();
    rows::result_set(&mut tokens, HEADER_COLUMNS, &rows)?;
    Ok(Messages::new(session, Operation::RestoreHeaderOnly)
        .finish(Some((tokens, rows.len() as u64))))
}

const FILELIST_COLUMNS: &[(&str, Kind)] = &[
    ("LogicalName", Kind::Nvarchar(128)),
    ("PhysicalName", Kind::Nvarchar(260)),
    ("Type", Kind::Nchar(1)),
    ("FileGroupName", Kind::Nvarchar(128)),
    ("Size", Kind::BigInt),
    ("MaxSize", Kind::BigInt),
    ("FileId", Kind::BigInt),
    ("CreateLSN", Kind::Numeric(25)),
    ("DropLSN", Kind::Numeric(25)),
    ("UniqueId", Kind::Guid),
    ("ReadOnlyLSN", Kind::Numeric(25)),
    ("ReadWriteLSN", Kind::Numeric(25)),
    ("BackupSizeInBytes", Kind::BigInt),
    ("SourceBlockSize", Kind::Int),
    ("FileGroupId", Kind::Int),
    ("LogGroupGUID", Kind::Guid),
    ("DifferentialBaseLSN", Kind::Numeric(25)),
    ("DifferentialBaseGUID", Kind::Guid),
    ("IsReadOnly", Kind::Bit),
    ("IsPresent", Kind::Bit),
    ("TDEThumbprint", Kind::Varbinary(20)),
    ("SnapshotUrl", Kind::Nvarchar(336)),
];

fn filelistonly(session: &Session, set: &Set) -> Result<Execution> {
    const ZERO: &str = "00000000-0000-0000-0000-000000000000";
    let rows: Vec<Vec<Cell>> = set
        .header
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let data = file.kind == 'D';
            vec![
                Cell::text(&file.logical),
                Cell::text(&file.physical),
                Cell::text(file.kind.to_string()),
                if data {
                    Cell::text("PRIMARY")
                } else {
                    Cell::Null
                },
                Cell::Int(i64::try_from(file.size).unwrap_or(i64::MAX)),
                Cell::Int(if data {
                    35_184_372_080_640
                } else {
                    2_199_023_255_552
                }),
                Cell::Int(index as i64 + 1),
                Cell::Decimal(0),
                Cell::Decimal(0),
                Cell::Null,
                Cell::Decimal(0),
                Cell::Decimal(0),
                Cell::Int(i64::try_from(file.size).unwrap_or(i64::MAX)),
                Cell::Int(4096),
                Cell::Int(i64::from(data)),
                Cell::Null,
                Cell::Decimal(0),
                Cell::Guid(ZERO.into()),
                Cell::Bit(false),
                Cell::Bit(true),
                Cell::Null,
                Cell::Null,
            ]
        })
        .collect();
    let mut tokens = Vec::new();
    rows::result_set(&mut tokens, FILELIST_COLUMNS, &rows)?;
    Ok(Messages::new(session, Operation::RestoreFileListOnly)
        .finish(Some((tokens, rows.len() as u64))))
}

struct Restore {
    database: String,
    device: String,
    file: i64,
    stats: Option<i64>,
    moves: Vec<(String, String)>,
}

fn restore(
    session: &mut Session,
    request: &Request,
    restore: Restore,
    mut messages: Messages,
) -> Result<Execution> {
    let started = std::time::Instant::now();
    let (path, sets) = open_media(session, request.operation, Some(restore.device.clone()))?;
    let set = position(&sets, restore.file, &path)?;
    let header = &set.header;
    if header
        .database_name
        .eq_ignore_ascii_case(crate::database_catalog::MASTER)
    {
        return Err(unsupported("Restoring a backup of master"));
    }
    let data = header.file('D').context("backup set has no data file")?;
    let log = header.file('L').context("backup set has no log file")?;
    // SQL Server compares logical names case-insensitively.
    for (logical, _) in &restore.moves {
        if !header
            .files
            .iter()
            .any(|file| file.logical.eq_ignore_ascii_case(logical))
        {
            bail!(SqlError::new(
                3234,
                2,
                format!(
                    "Logical file '{logical}' is not part of database '{}'. Use RESTORE FILELISTONLY to list the logical file names.",
                    restore.database
                )
            ));
        }
    }
    let target = |file: &FileEntry| {
        restore
            .moves
            .iter()
            .rev()
            .find(|(logical, _)| file.logical.eq_ignore_ascii_case(logical))
            .map_or_else(|| file.physical.clone(), |(_, physical)| physical.clone())
    };
    let (data_path, log_path) = (target(data), target(log));
    msdb::ensure(session)?;
    let catalog = session.database.catalog().clone();
    let staging = catalog.staging_path("restore")?;
    let restored = (|| -> Result<crate::database_catalog::Database> {
        if !media::extract(Path::new(&path), set, &staging)? {
            return Err(damaged(set.position));
        }
        let current = session.database.alias().to_owned();
        catalog.restore(
            &session.db,
            &crate::database_catalog::Restore {
                name: &restore.database,
                staging: &staging,
                data_name: &data.logical,
                log_name: &log.logical,
                data_path: &data_path,
                log_path: &log_path,
                family_guid: &header.family_guid,
                database_guid: &header.database_guid,
                replace: request.has("REPLACE"),
            },
            &|alias| session.held(alias),
            &current,
        )
    })();
    let _ = std::fs::remove_file(&staging);
    let _ = std::fs::remove_file(staging.with_extension("duckdb.wal"));
    let database = restored?;
    history::restore(
        session,
        &history::Restored {
            set,
            device: &path,
            database: &database.name,
            replace: request.has("REPLACE"),
            data_path: &data_path,
            log_path: &log_path,
        },
    )?;
    messages.stats(restore.stats);
    let data_pages = pages(set.payload_len);
    messages.info(
        4035,
        format!(
            "Processed {data_pages} pages for database '{}', file '{}' on file {}.",
            database.name, data.logical, set.position
        ),
    );
    messages.info(
        4035,
        format!(
            "Processed 0 pages for database '{}', file '{}' on file {}.",
            database.name, log.logical, set.position
        ),
    );
    messages.info(
        3014,
        summary(request.operation, data_pages, set.payload_len, started),
    );
    Ok(messages.finish(None))
}

/// msdb history rows, written with T-SQL in msdb's context.
mod history {
    use super::msdb::{self, literal};
    use super::{Header, SOFTWARE_VENDOR, SOFTWARE_VERSION, Session, Set, flags};
    use anyhow::Result;

    /// The media set row for a media GUID, created on first use.
    fn media_set(session: &mut Session, header: &Header, device: &str) -> Result<i32> {
        let found: Option<i32> = session
            .db
            .query_row(
                "SELECT media_set_id FROM \"msdb\".dbo.backupmediaset
                 WHERE upper(CAST(media_uuid AS VARCHAR))=upper(?)",
                [&header.media_guid],
                |row| row.get(0),
            )
            .ok();
        if let Some(id) = found {
            return Ok(id);
        }
        let id = msdb::insert(
            session,
            "backupmediaset",
            &format!(
                "{},1,{},{},N'Microsoft SQL Server',{SOFTWARE_VENDOR},1,1,0,{},0",
                literal::guid(&header.media_guid),
                literal::optional(header.media_name.as_deref()),
                literal::optional(header.media_description.as_deref()),
                literal::bit(header.media_compressed),
            ),
        )?;
        msdb::insert(
            session,
            "backupmediafamily",
            &format!(
                "{id},1,{},1,NULL,{},2,4096,0",
                literal::guid(&crate::database_catalog::new_guid()),
                literal::text(device),
            ),
        )?;
        Ok(id)
    }

    /// The backupset row for a backup set GUID, created on first use (a
    /// restore of a backup taken elsewhere records it, as SQL Server does).
    fn backup_set(
        session: &mut Session,
        header: &Header,
        position: u32,
        device: &str,
        size: u64,
    ) -> Result<i32> {
        let found: Option<i32> = session
            .db
            .query_row(
                "SELECT backup_set_id FROM \"msdb\".dbo.backupset
                 WHERE upper(CAST(backup_set_uuid AS VARCHAR))=upper(?)",
                [&header.backup_set_guid],
                |row| row.get(0),
            )
            .ok();
        if let Some(id) = found {
            return Ok(id);
        }
        let media_set = media_set(session, header, device)?;
        let compressed_size = size;
        let id = msdb::insert(
            session,
            "backupset",
            &format!(
                "{set_guid},{media_set},1,1,1,1,1,1,{position},NULL,
                 {SOFTWARE_VENDOR},{name},{description},{user},{major},{minor},{build},0,0,NULL,NULL,NULL,NULL,
                 {created},{started},{finished},'D',52,0,160,{database_version},{size},{database},{server},{server},
                 {flags},1033,196609,N'SQL_Latin1_General_CP1_CI_AS',0,N'SIMPLE',0,0,0,0,{checksum},0,0,0,0,
                 {copy_only},{family},{family},NULL,{database_guid},{family},NULL,NULL,{compressed_size},NULL,NULL,
                 NULL,NULL,{algorithm}",
                set_guid = literal::guid(&header.backup_set_guid),
                name = literal::optional(header.name.as_deref()),
                description = literal::optional(header.description.as_deref()),
                user = literal::text(&header.user_name),
                major = SOFTWARE_VERSION.0,
                minor = SOFTWARE_VERSION.1,
                build = SOFTWARE_VERSION.2,
                created = literal::datetime(header.database_creation_date),
                started = literal::datetime(header.backup_start),
                finished = literal::datetime(header.backup_finish),
                database_version = super::DATABASE_VERSION,
                database = literal::text(&header.database_name),
                server = literal::text(&header.server_name),
                flags = flags(header),
                checksum = literal::bit(header.checksum),
                copy_only = literal::bit(header.copy_only),
                family = literal::guid(&header.family_guid),
                database_guid = literal::guid(&header.database_guid),
                algorithm = if header.compressed {
                    "N'MS_XPRESS'"
                } else {
                    "NULL"
                },
            ),
        )?;
        for (index, file) in header.files.iter().enumerate() {
            let data = file.kind == 'D';
            msdb::insert(
                session,
                "backupfile",
                &format!(
                    "{id},1,1,{filegroup},{page_size},{number},{pages},'{kind}',
                     4096,{size},{logical},NULL,{physical},0,N'ONLINE',NULL,NULL,NULL,NULL,NULL,NULL,NULL,{size},
                     NULL,0,1",
                    filegroup = if data { "N'PRIMARY'" } else { "NULL" },
                    page_size = if data { "8192" } else { "NULL" },
                    number = index + 1,
                    pages = super::pages(file.size),
                    kind = file.kind,
                    size = file.size,
                    logical = literal::text(&file.logical),
                    physical = literal::text(&file.physical),
                ),
            )?;
        }
        Ok(id)
    }

    pub fn backup(
        session: &mut Session,
        header: &Header,
        position: u32,
        device: &str,
        size: u64,
    ) -> Result<()> {
        let _history = msdb::history_lock();
        msdb::in_msdb(session, |session| {
            backup_set(session, header, position, device, size).map(|_| ())
        })
    }

    pub struct Restored<'a> {
        pub set: &'a Set,
        pub device: &'a str,
        pub database: &'a str,
        pub replace: bool,
        pub data_path: &'a str,
        pub log_path: &'a str,
    }

    pub fn restore(session: &mut Session, restored: &Restored<'_>) -> Result<()> {
        let _history = msdb::history_lock();
        msdb::in_msdb(session, |session| {
            let set = restored.set;
            let backup_set = backup_set(
                session,
                &set.header,
                set.position,
                restored.device,
                set.payload_len,
            )?;
            let user = literal::text(&session.original_login);
            let id = msdb::insert(
                session,
                "restorehistory",
                &format!(
                    "{date},{database},{user},{backup_set},'D',{replace},1,0,NULL,1,NULL,NULL",
                    date = literal::datetime(super::local_now()),
                    database = literal::text(restored.database),
                    replace = literal::bit(restored.replace),
                ),
            )?;
            for (number, path) in [(1, restored.data_path), (2, restored.log_path)] {
                msdb::insert(
                    session,
                    "restorefile",
                    &format!("{id},{number},NULL,{}", literal::text(path)),
                )?;
            }
            Ok(())
        })
    }
}
