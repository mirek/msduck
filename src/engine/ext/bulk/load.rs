//! Receiving one BulkLoadBCP message and loading its rows.
//!
//! Rows are decoded as packets arrive. A load that fits in one INSERT
//! statement runs as `INSERT INTO target (columns) VALUES (...)` with typed
//! parameters. Larger loads first fill a private staging table (`#...`, with
//! the INSERT BULK declarations), statement by statement, and then copy it
//! with one `INSERT INTO target (columns) SELECT ... ORDER BY row`. Either
//! way the target sees a single INSERT, so SQL Server's semantics follow
//! from the engine's INSERT: conversions and their errors, keys, NOT NULL,
//! identity, triggers (once per load), atomicity and @@ROWCOUNT.
use super::{
    NAME,
    plan::Plan,
    wire::{self, codec},
};
use crate::{
    engine::{Parameter, Session, emit_error, ext},
    tds,
};
use msduck_core::{diagnostic::SqlError, types::Type, value::Value};
use std::collections::HashMap;

/// SQL Server's text for 4804.
const PREMATURE_END: &str = "While reading current row from host, a premature end-of-message was encountered--an incoming data stream was interrupted when the server expected to see more data. The host program may have terminated. Ensure that you are using a supported client application programming interface (API).";

/// The CurCmd of a completed or failed load (captured).
const BULK_COMMAND: u16 = 240;
/// The CurCmd of a load that failed before any row was written.
const ABORT_COMMAND: u16 = 253;

/// T-SQL limits a VALUES list to 1000 rows; parameters per statement stay
/// near 2000 as for a client request.
fn rows_per_statement(columns: usize) -> usize {
    (2000 / (columns + 2)).clamp(1, 1000)
}

struct Row {
    values: Vec<Parameter>,
    /// Index into [`Load::patterns`].
    pattern: usize,
    /// Bytes of its wire values.
    size: usize,
}

/// The decoder re-parses an unfinished token on every push, so packets are
/// handed to it in pieces of at least this size: a large PLP value in small
/// packets would otherwise cost quadratic time.
const DECODE_CHUNK: usize = 1024 * 1024;

/// Buffered rows beyond this many value bytes are staged even when they
/// would fit in one statement.
const BUFFER_LIMIT: usize = 8 * 1024 * 1024;

/// A NULL pattern and how many rows have it.
struct Pattern {
    columns: Vec<usize>,
    rows: usize,
}

pub(super) struct Load {
    plan: Plan,
    decoder: codec::Decoder,
    /// Encoded diagnostics of the first failure and its @@ERROR. The rest of
    /// the message is read and discarded.
    failure: Option<(Vec<u8>, i32)>,
    checked: bool,
    /// Decoded rows not yet written.
    rows: Vec<Row>,
    received: u64,
    /// The staging table, once a load needs more than one statement.
    staging: Option<String>,
    /// Sets of bulk columns whose NULLs take the column default (without
    /// KEEP_NULLS), in order of first appearance. The empty set is always
    /// first.
    patterns: Vec<Pattern>,
    /// Rows the load's INSERT statements wrote so far.
    written: u64,
    /// Packet bytes not yet handed to the decoder.
    unread: Vec<u8>,
    /// Bytes of the buffered rows' values.
    buffered: usize,
}

impl Load {
    pub fn new(plan: Plan) -> Self {
        Self {
            plan,
            decoder: codec::Decoder::new(codec::EomMode::RequireDone),
            failure: None,
            checked: false,
            rows: Vec::new(),
            received: 0,
            staging: None,
            patterns: vec![Pattern {
                columns: Vec::new(),
                rows: 0,
            }],
            written: 0,
            unread: Vec::new(),
            buffered: 0,
        }
    }

    fn fail(&mut self, errors: &[SqlError]) {
        if self.failure.is_none() {
            let mut tokens = Vec::new();
            for error in errors {
                tds::sql_error(&mut tokens, error);
            }
            let number = errors.last().map_or(0, |error| error.number);
            self.failure = Some((tokens, number));
        }
        self.discard();
    }

    fn fail_with(&mut self, error: &anyhow::Error) {
        if self.failure.is_none() {
            let mut tokens = Vec::new();
            let number = emit_error(&mut tokens, error);
            self.failure = Some((tokens, number));
        }
        self.discard();
    }

    /// Forget the rows a failed load still holds.
    fn discard(&mut self) {
        self.rows.clear();
        self.unread.clear();
        self.buffered = 0;
    }

    /// One packet of the message.
    pub fn push(&mut self, session: &mut Session, bytes: &[u8], eom: bool) {
        if self.failure.is_some() {
            return;
        }
        self.unread.extend_from_slice(bytes);
        if self.unread.len() < DECODE_CHUNK && !eom {
            return;
        }
        let input = std::mem::take(&mut self.unread);
        let chunk = match self.decoder.push(&input, eom) {
            Ok(chunk) => chunk,
            Err(_) => {
                // Captured: a stream without COLMETADATA (tedious sends only
                // DONE for zero rows) fails with state 2; a row that does
                // not match its metadata (tedious sends a short binary(n)
                // value with length n) with state 1 and severity 17, after
                // the metadata checks.
                let error = match self.decoder.columns() {
                    None => SqlError::new(4804, 2, PREMATURE_END),
                    Some(columns) => {
                        let mismatch = (!self.checked).then(|| self.check(columns)).flatten();
                        mismatch.unwrap_or_else(|| {
                            let mut error = SqlError::new(4804, 1, PREMATURE_END);
                            error.severity = 17;
                            error
                        })
                    }
                };
                self.fail(&[error]);
                return;
            }
        };
        if !self.checked
            && let Some(columns) = self.decoder.columns()
        {
            self.checked = true;
            if let Some(error) = self.check(columns) {
                self.fail(&[error]);
                return;
            }
        }
        for row in chunk.rows {
            if let Err(error) = self.add(row) {
                self.fail_with(&error);
                return;
            }
        }
        // Keep at most one statement's rows (and BUFFER_LIMIT bytes)
        // buffered: a load that fits in one statement never needs the
        // staging table.
        let per_statement = rows_per_statement(self.plan.columns.len());
        while self.rows.len() > per_statement
            || (self.buffered > BUFFER_LIMIT && !self.rows.is_empty())
        {
            let count = self.rows.len().min(per_statement);
            if let Err(failure) = self.stage(session, count) {
                self.failure = Some(failure);
                self.discard();
            }
        }
    }

    /// SQL Server's checks of the COLMETADATA token.
    fn check(&self, columns: &[wire::WireColumn]) -> Option<SqlError> {
        if columns.len() != self.plan.columns.len() {
            return Some(SqlError::new(4804, 3, PREMATURE_END));
        }
        for (index, (wire_column, bound)) in columns.iter().zip(&self.plan.columns).enumerate() {
            let target = self.plan.target(bound);
            if let Some(state) =
                wire::incompatible(wire_column, &bound.declared, target.nullable, target.max)
            {
                return Some(SqlError::new(
                    4816,
                    state,
                    format!(
                        "Invalid column type from bcp client for colid {}.",
                        index + 1
                    ),
                ));
            }
        }
        None
    }

    fn add(&mut self, row: Vec<codec::Value>) -> anyhow::Result<()> {
        let columns = self.decoder.columns().unwrap_or_default();
        let mut values = Vec::with_capacity(row.len());
        let mut pattern = Vec::new();
        let size = row
            .iter()
            .map(|value| value.bytes.as_ref().map_or(0, Vec::len))
            .sum::<usize>();
        for (index, (value, bound)) in row.iter().zip(&self.plan.columns).enumerate() {
            if let msduck_core::types::Type::Character(character) = bound.declared
                && msduck_core::bulk_character_admission::row(
                    character,
                    value.bytes.as_ref().map(Vec::len),
                ) == msduck_core::bulk_character_admission::Row::DeclaredLengthExceeded
            {
                let mut error = SqlError::new(
                    4815,
                    1,
                    format!(
                        "Received an invalid column length from the bcp client for colid {}.",
                        index + 1,
                    ),
                );
                error.severity = 17;
                return Err(error.into());
            }
            let value = wire::value(&columns[index].type_info, value.bytes.as_deref())?;
            if matches!(value, Value::Null)
                && !self.plan.options.keep_nulls
                && self.plan.target(bound).default
            {
                pattern.push(index);
            }
            values.push(Parameter {
                value,
                data_type: bound.declared,
            });
        }
        let pattern = match self
            .patterns
            .iter()
            .position(|known| known.columns == pattern)
        {
            Some(index) => index,
            None => {
                self.patterns.push(Pattern {
                    columns: pattern,
                    rows: 0,
                });
                self.patterns.len() - 1
            }
        };
        self.patterns[pattern].rows += 1;
        self.rows.push(Row {
            values,
            pattern,
            size,
        });
        self.buffered += size;
        self.received += 1;
        Ok(())
    }

    /// Write the first `count` buffered rows to the staging table.
    fn stage(&mut self, session: &mut Session, count: usize) -> Result<(), (Vec<u8>, i32)> {
        let name = match &self.staging {
            Some(name) => name.clone(),
            None => {
                let name = format!("#__msduck_bulk_{}", session.ext.token);
                let columns = self
                    .plan
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| format!("[c{index}] {} NULL", column.declared_text))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "CREATE TABLE {name} ([__row] bigint NOT NULL, [__pattern] int NOT NULL, {columns})"
                );
                checked(session, &sql, &HashMap::new())?;
                self.staging = Some(name.clone());
                name
            }
        };
        let per_statement = rows_per_statement(self.plan.columns.len());
        let mut first = self.received - self.rows.len() as u64;
        let rows: Vec<Row> = self.rows.drain(..count).collect();
        self.buffered -= rows.iter().map(|row| row.size).sum::<usize>();
        for chunk in rows.chunks(per_statement) {
            let mut parameters = HashMap::new();
            let mut tuples = Vec::with_capacity(chunk.len());
            for (offset, row) in chunk.iter().enumerate() {
                let mut items = vec![format!("@r{offset}"), format!("@p{offset}")];
                parameters.insert(
                    format!("@r{offset}"),
                    Parameter {
                        value: Value::BigInt(first as i64 + offset as i64),
                        data_type: Type::BigInt,
                    },
                );
                parameters.insert(
                    format!("@p{offset}"),
                    Parameter {
                        value: Value::Int(row.pattern as i32),
                        data_type: Type::Int,
                    },
                );
                for (index, value) in row.values.iter().enumerate() {
                    let name = format!("@b{offset}_{index}");
                    items.push(name.clone());
                    parameters.insert(name, value.clone());
                }
                tuples.push(format!("({})", items.join(", ")));
            }
            let columns = (0..self.plan.columns.len())
                .map(|index| format!("[c{index}]"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "INSERT INTO {name} ([__row], [__pattern], {columns}) VALUES {}",
                tuples.join(", ")
            );
            checked(session, &sql, &parameters)?;
            first += chunk.len() as u64;
        }
        Ok(())
    }

    /// The end of the message: load the rows and return the response.
    pub fn finish(mut self, session: &mut Session, ignored: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if ignored {
            // The client abandoned the message (IGNORE): nothing is loaded
            // (captured: a failed DONE without a diagnostic).
            self.drop_staging(session);
            tds::done(&mut out, 0xfd, 2, ABORT_COMMAND, 0);
            return out;
        }
        if self.failure.is_none() && self.staging.is_some() && !self.rows.is_empty() {
            let count = self.rows.len();
            if let Err(failure) = self.stage(session, count) {
                self.failure = Some(failure);
            }
        }
        if let Some((tokens, number)) = self.failure.take() {
            self.drop_staging(session);
            out.extend(tokens);
            session.last_error = number;
            session.rowcount = 0;
            tds::done(&mut out, 0xfd, 2, ABORT_COMMAND, 0);
            return out;
        }
        let (tokens, outcome) = self.load(session);
        self.drop_staging(session);
        out.extend(tokens);
        match outcome {
            Ok(count) => {
                session.rowcount = count;
                session.last_error = 0;
                let (status, count) = if session.nocount {
                    (0, 0)
                } else {
                    (0x10, count)
                };
                tds::done(&mut out, 0xfd, status, BULK_COMMAND, count);
            }
            Err((number, command)) => {
                session.rowcount = 0;
                session.last_error = number;
                tds::done(&mut out, 0xfd, 2, command, 0);
            }
        }
        out
    }

    fn drop_staging(&mut self, session: &mut Session) {
        if let Some(name) = self.staging.take() {
            let (rowcount, error) = (session.rowcount, session.last_error);
            let _ = run(session, &format!("DROP TABLE {name}"), &HashMap::new());
            session.rowcount = rowcount;
            session.last_error = error;
        }
    }

    /// Run the load's INSERT statements in one transaction (the caller's,
    /// or one owned here) with the options applied. Returns the tokens to
    /// send before the final DONE, and the row count or the failure's
    /// @@ERROR and DONE command.
    fn load(&mut self, session: &mut Session) -> (Vec<u8>, Result<u64, (i32, u16)>) {
        if self.received == 0 {
            return (Vec::new(), Ok(0));
        }
        let xact_abort = session.xact_abort;
        let transaction = match super::super::constraints::Transaction::begin(session) {
            Ok(transaction) => transaction,
            Err(error) => {
                let mut tokens = Vec::new();
                let number = emit_error(&mut tokens, &error);
                return (tokens, Err((number, ABORT_COMMAND)));
            }
        };
        let owned = transaction.owned;
        // An owned transaction is this statement's own: XACT_ABORT must not
        // make the engine roll it back as if it were the client's.
        if owned {
            session.xact_abort = false;
        }
        let mut tokens = Vec::new();
        self.written = 0;
        let mut result = self.apply_options(session, &mut tokens);
        if owned {
            session.xact_abort = xact_abort;
            // The client never began this transaction: it must not see it
            // begin or end (a trigger error rolls it back in the engine).
            tokens = without_transaction_changes(&tokens);
        }
        let error = session.last_error;
        if owned && session.transactions == 0 {
            // The engine already ended the owned transaction (a failed or
            // rolled-back trigger); nothing it wrote remains.
            if result.is_ok() {
                result = Err(false);
            }
        } else {
            let wrote = self.written > 0;
            let finished = transaction.finish_or_abort(
                session,
                result
                    .as_ref()
                    .map(|_| ())
                    .map_err(|_| anyhow::anyhow!("bulk load failed")),
                wrote,
            );
            if let (Ok(_), Err(commit)) = (&result, finished) {
                // The owned transaction did not commit: nothing was loaded.
                let number = emit_error(&mut tokens, &commit);
                return (tokens, Err((number, ABORT_COMMAND)));
            }
        }
        match result {
            Ok(count) => (tokens, Ok(count)),
            Err(terminated) => {
                // With XACT_ABORT, SQL Server ends the batch without 3621.
                let command = if terminated && !xact_abort {
                    BULK_COMMAND
                } else {
                    if xact_abort {
                        strip_terminated(&mut tokens);
                    }
                    ABORT_COMMAND
                };
                (tokens, Err((error, command)))
            }
        }
    }

    /// Identity, constraint and trigger options around the INSERT
    /// statements. The error flag tells whether the statement was
    /// terminated (3621) rather than the batch aborted.
    fn apply_options(&mut self, session: &mut Session, tokens: &mut Vec<u8>) -> Result<u64, bool> {
        // Without CHECK_CONSTRAINTS the constraints feature, which enforces
        // CHECK and FOREIGN KEY constraints, is suspended for the INSERT;
        // without FIRE_TRIGGERS, the triggers feature.
        let mut skipped = Vec::new();
        if !self.plan.options.check_constraints {
            skipped.push("constraints");
        }
        if !self.plan.options.fire_triggers {
            skipped.push("triggers");
        }
        let identity = self.plan.keeps_identity();
        if identity {
            // An RPC-like frame restores the session's IDENTITY_INSERT
            // setting when it ends.
            ext::batch_begin(session, true);
        }
        let result = (|| {
            if identity && let Err(failure) = options::identity_on(session, &self.plan.table) {
                tokens.extend(without_done(&failure));
                return Err(false);
            }
            suspended(session, &skipped, |session| self.insert(session, tokens))
        })();
        if identity {
            let (rowcount, error) = (session.rowcount, session.last_error);
            ext::batch_end(session);
            session.rowcount = rowcount;
            session.last_error = error;
        }
        if result.is_ok()
            && !self.plan.options.check_constraints
            && let Err(error) = options::distrust_constraints(session, &self.plan)
        {
            let number = emit_error(tokens, &error);
            session.last_error = number;
            return Err(false);
        }
        result
    }

    /// The INSERT statements themselves.
    fn insert(&mut self, session: &mut Session, tokens: &mut Vec<u8>) -> Result<u64, bool> {
        let targets: Vec<String> = self
            .plan
            .columns
            .iter()
            .map(|column| bracket(&self.plan.target(column).name))
            .collect();
        let statements: Vec<(String, HashMap<String, Parameter>)> = match &self.staging {
            None => {
                // One statement: DEFAULT stands in for each NULL that takes
                // the column default.
                let mut parameters = HashMap::new();
                let mut tuples = Vec::with_capacity(self.rows.len());
                for (offset, row) in self.rows.iter().enumerate() {
                    let pattern = &self.patterns[row.pattern].columns;
                    let items = row
                        .values
                        .iter()
                        .enumerate()
                        .map(|(index, value)| {
                            if pattern.contains(&index) {
                                "DEFAULT".to_owned()
                            } else {
                                let name = format!("@b{offset}_{index}");
                                parameters.insert(name.clone(), value.clone());
                                name
                            }
                        })
                        .collect::<Vec<_>>();
                    tuples.push(format!("({})", items.join(", ")));
                }
                vec![(
                    format!(
                        "INSERT INTO {} ({}) VALUES {}",
                        self.plan.table,
                        targets.join(", "),
                        tuples.join(", ")
                    ),
                    parameters,
                )]
            }
            Some(staging) => {
                // One statement per set of defaulted NULL columns, which
                // that statement omits so the target default applies.
                let used: Vec<usize> = (0..self.patterns.len())
                    .filter(|pattern| self.patterns[*pattern].rows > 0)
                    .collect();
                let distinct = used.len();
                let mut statements = Vec::new();
                for pattern in used {
                    let omitted = &self.patterns[pattern].columns;
                    let (columns, sources): (Vec<_>, Vec<_>) = targets
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| !omitted.contains(index))
                        .map(|(index, target)| (target.clone(), format!("[c{index}]")))
                        .unzip();
                    if columns.is_empty() {
                        // Every column takes its default: DEFAULT rows.
                        let rows = self.patterns[pattern].rows;
                        for start in (0..rows).step_by(1000) {
                            let values = vec!["(DEFAULT)"; (rows - start).min(1000)];
                            statements.push((
                                format!(
                                    "INSERT INTO {} ({}) VALUES {}",
                                    self.plan.table,
                                    targets[0],
                                    values.join(", ")
                                ),
                                HashMap::new(),
                            ));
                        }
                        continue;
                    }
                    let filter = if distinct > 1 {
                        format!(" WHERE [__pattern] = {pattern}")
                    } else {
                        String::new()
                    };
                    statements.push((
                        format!(
                            "INSERT INTO {} ({}) SELECT {} FROM {staging}{filter} ORDER BY [__row]",
                            self.plan.table,
                            columns.join(", "),
                            sources.join(", ")
                        ),
                        HashMap::new(),
                    ));
                }
                statements
            }
        };
        let mut total = 0;
        for (sql, parameters) in statements {
            let (response, ok) = run(session, &sql, &parameters);
            let body = without_done(&response);
            if !ok {
                let terminated = contains_terminated(&body);
                tokens.extend(body);
                return Err(terminated);
            }
            tokens.extend(body);
            total += session.rowcount;
            self.written = total;
        }
        Ok(total)
    }
}

mod options {
    //! Identity and constraint handling around the INSERT.
    use super::{bracket, run, split_done};
    use crate::engine::Session;
    use std::collections::HashMap;

    /// Bulk loads keep the values of a listed identity column, like SET
    /// IDENTITY_INSERT ON for the load's INSERT. The caller runs this inside
    /// an RPC-like batch frame, so the session's own setting (for this or
    /// another table) is restored when the load ends. A session can have
    /// only one such table, so another table's setting is suspended here.
    pub(super) fn identity_on(session: &mut Session, table: &str) -> Result<(), Vec<u8>> {
        let on = format!("SET IDENTITY_INSERT {table} ON");
        let (response, ok) = run(session, &on, &HashMap::new());
        if ok {
            return Ok(());
        }
        if session.last_error == 8107
            && let Some(previous) = active_identity_table(session, &response)
        {
            let (_, off) = run(
                session,
                &format!("SET IDENTITY_INSERT {previous} OFF"),
                &HashMap::new(),
            );
            let (response, ok) = run(session, &on, &HashMap::new());
            if off && ok {
                return Ok(());
            }
            return Err(split_done(&response).0.to_vec());
        }
        Err(split_done(&response).0.to_vec())
    }

    /// The table that 8107 names ('database.schema.table'), as a two-part
    /// name when it is in the current database.
    fn active_identity_table(session: &Session, response: &[u8]) -> Option<String> {
        let text = utf16_text(response);
        let start = text.find("IDENTITY_INSERT is already ON for table '")?
            + "IDENTITY_INSERT is already ON for table '".len();
        let rest = &text[start..];
        let name = &rest[..rest.find("'. ")?];
        let mut parts = name.splitn(3, '.');
        let (database, schema, table) = (parts.next()?, parts.next()?, parts.next()?);
        database
            .eq_ignore_ascii_case(&session.database().name)
            .then(|| format!("{}.{}", bracket(schema), bracket(table)))
    }

    /// The UTF-16 message text in a token stream, decoded loosely.
    fn utf16_text(bytes: &[u8]) -> String {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let even = String::from_utf16_lossy(&units);
        let units: Vec<u16> = bytes
            .get(1..)
            .unwrap_or_default()
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        even + &String::from_utf16_lossy(&units)
    }

    /// Without CHECK_CONSTRAINTS, SQL Server marks the target's enabled
    /// CHECK and FOREIGN KEY constraints not trusted after the load. Only
    /// constraints still trusted are written, so concurrent loads into the
    /// same table rarely touch the same catalog rows.
    pub(super) fn distrust_constraints(
        session: &mut Session,
        plan: &super::Plan,
    ) -> anyhow::Result<()> {
        let name = format!("{}.{}", bracket(&plan.schema), bracket(&plan.backend));
        session.db.execute(
            "UPDATE main.__msduck_constraints SET is_not_trusted = true
             WHERE parent_object_id = __msduck_object_id(?, 'U')
               AND type_code IN ('C', 'F') AND NOT is_disabled AND NOT is_not_trusted",
            [&name],
        )?;
        Ok(())
    }
}

fn bracket(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

/// Run `work` with the named features' statement hooks suspended.
fn suspended<T>(
    session: &mut Session,
    features: &[&'static str],
    work: impl FnOnce(&mut Session) -> T,
) -> T {
    match features.split_first() {
        None => work(session),
        Some((first, rest)) => {
            ext::reenter(session, first, |session| suspended(session, rest, work))
        }
    }
}

/// Run a statement through the engine with this feature's hooks suspended.
fn run(
    session: &mut Session,
    sql: &str,
    parameters: &HashMap<String, Parameter>,
) -> (Vec<u8>, bool) {
    ext::reenter(session, NAME, |session| {
        session.batch_response_inner(sql, parameters, None, None)
    })
}

/// Run an auxiliary statement; its failure is the load's failure.
fn checked(
    session: &mut Session,
    sql: &str,
    parameters: &HashMap<String, Parameter>,
) -> Result<(), (Vec<u8>, i32)> {
    let (response, ok) = run(session, sql, parameters);
    if ok {
        return Ok(());
    }
    let (body, _) = split_done(&response);
    Err((body.to_vec(), session.last_error))
}

/// Split the final DONE token off a response.
fn split_done(tokens: &[u8]) -> (&[u8], Option<&[u8]>) {
    if tokens.len() >= 13 && matches!(tokens[tokens.len() - 13], 0xfd..=0xff) {
        let (body, done) = tokens.split_at(tokens.len() - 13);
        (body, Some(done))
    } else {
        (tokens, None)
    }
}

/// A statement's tokens without its completions: SQL Server's response to a
/// bulk load has no DONE tokens of its own statements or of the triggers it
/// fires, only the load's DONE. Tokens that cannot be sized here (result
/// sets a trigger returns) end the filtering and are kept as they are.
fn without_done(tokens: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut at = 0;
    while at < tokens.len() {
        let length = match tokens[at] {
            0xfd..=0xff => {
                at += 13;
                continue;
            }
            0xaa | 0xab | 0xe3 if at + 3 <= tokens.len() => {
                3 + usize::from(u16::from_le_bytes([tokens[at + 1], tokens[at + 2]]))
            }
            0x79 => 5,
            _ => {
                out.extend_from_slice(&tokens[at..]);
                break;
            }
        };
        let end = (at + length).min(tokens.len());
        out.extend_from_slice(&tokens[at..end]);
        at = end;
    }
    out
}

/// Tokens without the ENVCHANGE tokens of transaction changes (types 8, 9,
/// 10 and 17), for a transaction the client did not begin.
fn without_transaction_changes(tokens: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut at = 0;
    while at < tokens.len() {
        let length = match tokens[at] {
            0xaa | 0xab | 0xe3 if at + 3 <= tokens.len() => {
                3 + usize::from(u16::from_le_bytes([tokens[at + 1], tokens[at + 2]]))
            }
            _ => {
                out.extend_from_slice(&tokens[at..]);
                break;
            }
        };
        let end = (at + length).min(tokens.len());
        let transaction = tokens[at] == 0xe3 && matches!(tokens.get(at + 3), Some(8 | 9 | 10 | 17));
        if !transaction {
            out.extend_from_slice(&tokens[at..end]);
        }
        at = end;
    }
    out
}

/// The INFO token SQL Server sends after a terminated statement.
fn terminated() -> Vec<u8> {
    let mut token = Vec::new();
    tds::diagnostic_utf16(
        &mut token,
        tds::DiagnosticKind::Information,
        0,
        0,
        3621,
        &"The statement has been terminated."
            .encode_utf16()
            .collect::<Vec<_>>(),
    );
    token
}

fn contains_terminated(tokens: &[u8]) -> bool {
    let token = terminated();
    tokens.windows(token.len()).any(|window| window == token)
}

fn strip_terminated(tokens: &mut Vec<u8>) {
    let token = terminated();
    if let Some(start) = tokens
        .windows(token.len())
        .position(|window| window == token)
    {
        tokens.drain(start..start + token.len());
    }
}
