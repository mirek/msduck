//! Session-owned RAND state. Native functions belong to the shared catalog;
//! their explicit, bounded call contexts belong to one execution and session.
use anyhow::{Result, bail};
use duckdb::{
    Connection,
    core::{DataChunkHandle, LogicalTypeId},
    types::Value,
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use ring::rand::{SecureRandom, SystemRandom};
use sqlparser::ast::*;
use std::{
    collections::HashMap,
    ops::ControlFlow,
    sync::{Arc, Mutex},
};

type Ticket = [u8; 16];
const LIMIT: usize = 4096;

/// SQL Server converts RAND seeds to INT. Route the explicit conversion through
/// the existing source-aware translator instead of DuckDB overload coercion
/// (which rounds floating values and cannot bind DECIMAL literal INT_MIN).
pub(super) fn seed_conversion(
    expression: &mut Expr,
    parameters: &HashMap<String, super::Parameter>,
) {
    let Expr::Function(function) = expression else {
        return;
    };
    if !function.name.to_string().eq_ignore_ascii_case("rand") {
        return;
    }
    let FunctionArguments::List(arguments) = &mut function.args else {
        return;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(seed))] = arguments.args.as_mut_slice() else {
        return;
    };
    fn float_source(seed: &Expr, parameters: &HashMap<String, super::Parameter>) -> bool {
        match seed {
            Expr::Nested(seed) => float_source(seed, parameters),
            Expr::Identifier(name) => parameters.iter().any(|(key, parameter)| {
                key.eq_ignore_ascii_case(&name.value)
                    && parameter.data_type == super::SqlType::Float
            }),
            Expr::Cast { data_type, .. } => {
                matches!(
                    data_type,
                    DataType::Double(_)
                        | DataType::DoublePrecision
                        | DataType::Float(ExactNumberInfo::None)
                ) || matches!(data_type, DataType::Float(ExactNumberInfo::Precision(bits)) if *bits > 24)
            }
            _ => false,
        }
    }
    if float_source(seed, parameters) {
        *seed = super::unary_function("__msduck_rand_float_seed", seed.clone());
        return;
    }
    fn source(seed: &Expr, parameters: &HashMap<String, super::Parameter>) -> Option<DataType> {
        match seed {
            Expr::Nested(seed) => source(seed, parameters),
            Expr::Identifier(name) => parameters.iter().find_map(|(key, parameter)| {
                key.eq_ignore_ascii_case(&name.value)
                    .then(|| parameter.ast_type())
            }),
            Expr::Cast { data_type, .. } => Some(data_type.clone()),
            Expr::Value(value) => match value.value {
                sqlparser::ast::Value::SingleQuotedString(_) => Some(DataType::Varchar(None)),
                sqlparser::ast::Value::NationalStringLiteral(_) => Some(DataType::Nvarchar(None)),
                _ => None,
            },
            _ => None,
        }
    }
    match source(seed, parameters) {
        Some(DataType::BigInt(_)) => {
            *seed = super::unary_function("__msduck_rand_bigint_seed", seed.clone());
            return;
        }
        Some(
            kind @ (DataType::Varchar(_)
            | DataType::Char(_)
            | DataType::Text
            | DataType::Nvarchar(_)),
        ) => {
            let unicode = matches!(kind, DataType::Nvarchar(_));
            *seed = super::binary_function(
                "__msduck_rand_character_seed",
                seed.clone(),
                Expr::Value(sqlparser::ast::Value::Boolean(unicode).into()),
            );
            return;
        }
        _ => {}
    }
    *seed = Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(seed.clone()),
        data_type: DataType::Int(None),
        format: None,
    };
}

#[derive(Debug)]
pub(super) struct Generator {
    x: u64,
    y: u64,
}
impl Generator {
    fn seeded(seed: i32) -> Self {
        let seed = u64::from(seed.unsigned_abs());
        Self {
            x: if (1..2147483563).contains(&seed) {
                seed
            } else {
                12345
            },
            y: 67890,
        }
    }
    pub(super) fn new() -> Result<Arc<Mutex<Self>>> {
        let mut bytes = [0; 4];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("RAND entropy unavailable"))?;
        Ok(Arc::new(Mutex::new(Self::seeded(i32::from_ne_bytes(
            bytes,
        )))))
    }
    fn draw(&mut self) -> f64 {
        self.x = self.x * 40014 % 2147483563;
        self.y = self.y * 40692 % 2147483399;
        let mut difference = self.x as i64 - self.y as i64;
        if difference < 1 {
            difference += 2147483562;
        }
        // SQL Server uses this rounded constant, not the exact reciprocal.
        difference as f64 * 4.656613e-10
    }
}

struct Call {
    generator: Arc<Mutex<Generator>>,
    value: Option<f64>,
}
type Entries = HashMap<Ticket, Arc<Mutex<Call>>>;
#[derive(Clone, Default)]
pub(crate) struct Registry(Arc<Mutex<Entries>>);
pub(super) struct Scope {
    registry: Registry,
    ticket: Ticket,
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.registry
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.ticket);
    }
}
impl Registry {
    fn begin(&self, generator: &Arc<Mutex<Generator>>) -> Result<Scope> {
        let mut entries = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("RAND registry poisoned"))?;
        if entries.len() >= LIMIT {
            bail!("too many active RAND call contexts");
        }
        let mut ticket = [0; 16];
        SystemRandom::new()
            .fill(&mut ticket)
            .map_err(|_| anyhow::anyhow!("RAND ticket generation failed"))?;
        if entries.contains_key(&ticket) {
            bail!("RAND ticket collision");
        }
        entries.insert(
            ticket,
            Arc::new(Mutex::new(Call {
                generator: generator.clone(),
                value: None,
            })),
        );
        Ok(Scope {
            registry: self.clone(),
            ticket,
        })
    }
    pub(crate) fn register(&self, db: &Connection) -> duckdb::Result<()> {
        db.register_scalar_function::<FloatSeed>("__msduck_rand_float_seed")?;
        db.register_scalar_function::<BigIntSeed>("__msduck_rand_bigint_seed")?;
        db.register_scalar_function::<CharacterSeed>("__msduck_rand_character_seed")?;
        db.register_scalar_function_with_state::<Draw<false>>("__msduck_rand", self)?;
        db.register_scalar_function_with_state::<Draw<true>>("__msduck_rand_seed", self)
    }
}

const FLOAT_OVERFLOW: &str = "__msduck_rand_float_seed_overflow:";
const BIGINT_OVERFLOW: &str = "__msduck_rand_bigint_seed_overflow";
const CHARACTER_FAILURE: &str = "__msduck_rand_character_seed_failure:";

struct BigIntSeed;
impl VScalar for BigIntSeed {
    type State = ();
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Bigint.into()],
            LogicalTypeId::Integer.into(),
        )]
    }
    fn volatile() -> bool {
        true
    }
    fn special_null_handling() -> bool {
        true
    }
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let values = input.flat_vector(0);
        let mut result = output.flat_vector();
        for row in 0..input.len() {
            if values.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            let value = unsafe { values.as_slice_with_len::<i64>(input.len())[row] };
            let value = i32::try_from(value).map_err(|_| BIGINT_OVERFLOW)?;
            unsafe {
                result.as_mut_slice::<i32>()[row] = value;
            }
        }
        Ok(())
    }
}

struct CharacterSeed;
impl VScalar for CharacterSeed {
    type State = ();
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Any.into(), LogicalTypeId::Boolean.into()],
            LogicalTypeId::Integer.into(),
        )]
    }
    fn volatile() -> bool {
        true
    }
    fn special_null_handling() -> bool {
        true
    }
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = input.flat_vector(0);
        let labels = input.flat_vector(1);
        let text = source.logical_type().id() == LogicalTypeId::Varchar;
        let carrier = crate::unicode_carrier::is_logical(&source.logical_type());
        if !text && !carrier {
            return Err("unsupported RAND character seed carrier".into());
        }
        let structure = carrier.then(|| input.struct_vector(0));
        let payload = structure
            .as_ref()
            .map(|structure| structure.child(0, input.len()));
        let mut result = output.flat_vector();
        for row in 0..input.len() {
            if labels.row_is_null(row as u64) {
                return Err("NULL RAND source family".into());
            }
            if source.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            let unicode = unsafe { labels.as_slice_with_len::<bool>(input.len())[row] };
            let units = if let Some(payload) = &payload {
                if !unicode {
                    return Err("unsupported ANSI RAND Unicode carrier".into());
                }
                if payload.row_is_null(row as u64) {
                    return Err("NULL RAND Unicode payload".into());
                }
                let bytes = crate::unicode_carrier::bytes(payload, row, input.len())?;
                if bytes.len() % 2 != 0 {
                    return Err("invalid RAND Unicode payload length".into());
                }
                if bytes.len() > 8000 {
                    return Err(format!("{CHARACTER_FAILURE}u:oversize").into());
                }
                bytes
                    .chunks_exact(2)
                    .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                    .collect::<Vec<_>>()
            } else {
                let mut value = unsafe {
                    source.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(input.len())[row]
                };
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
                        duckdb::ffi::duckdb_string_t_length(value) as usize,
                    )
                };
                if bytes.len() > 12000 {
                    return Err(format!(
                        "{CHARACTER_FAILURE}{}:oversize",
                        if unicode { 'u' } else { 'a' }
                    )
                    .into());
                }
                std::str::from_utf8(bytes)?
                    .encode_utf16()
                    .take(4001)
                    .collect::<Vec<_>>()
            };
            match crate::integer_conversion::unicode_integer(&units, "INT") {
                Ok(value) => {
                    let value = value.parse::<i32>()?;
                    unsafe {
                        result.as_mut_slice::<i32>()[row] = value;
                    }
                }
                Err(_) => {
                    use std::fmt::Write;
                    let mut message =
                        format!("{CHARACTER_FAILURE}{}:", if unicode { 'u' } else { 'a' });
                    if units.len() > 4000 {
                        message.push_str("oversize");
                    } else {
                        for unit in units {
                            write!(message, "{unit:04x}")?;
                        }
                    }
                    return Err(message.into());
                }
            }
        }
        Ok(())
    }
}
struct FloatSeed;
impl VScalar for FloatSeed {
    type State = ();
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![LogicalTypeId::Double.into()],
            LogicalTypeId::Integer.into(),
        )]
    }
    fn volatile() -> bool {
        true
    }
    fn special_null_handling() -> bool {
        true
    }
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let values = input.flat_vector(0);
        let mut result = output.flat_vector();
        for row in 0..input.len() {
            if values.row_is_null(row as u64) {
                result.set_null(row);
                continue;
            }
            let value = unsafe { values.as_slice_with_len::<f64>(input.len())[row] };
            let integer = value.trunc();
            if !integer.is_finite()
                || integer < f64::from(i32::MIN)
                || integer > f64::from(i32::MAX)
            {
                return Err(format!("{FLOAT_OVERFLOW}{:016x}", value.to_bits()).into());
            }
            unsafe {
                result.as_mut_slice::<i32>()[row] = integer as i32;
            }
        }
        Ok(())
    }
}

pub(super) fn diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    use msduck_core::diagnostic::SqlError;
    let message = message.strip_prefix("Invalid Input Error: ")?;
    if message == BIGINT_OVERFLOW {
        return Some(SqlError::new(
            8115,
            2,
            "Arithmetic overflow error converting expression to data type int.",
        ));
    }
    if let Some(encoded) = message.strip_prefix(CHARACTER_FAILURE) {
        let (family, encoded) = encoded.split_once(':')?;
        if !matches!(family, "u" | "a") {
            return None;
        }
        let units = if encoded == "oversize" {
            vec![65; 4001]
        } else {
            if encoded.len() > 16000
                || encoded.len() % 4 != 0
                || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return None;
            }
            encoded
                .as_bytes()
                .chunks_exact(4)
                .map(|bytes| u16::from_str_radix(std::str::from_utf8(bytes).ok()?, 16).ok())
                .collect::<Option<Vec<_>>>()?
        };
        let error = crate::integer_conversion::unicode_integer(&units, "INT").err()?;
        if family == "u" {
            return Some(error);
        }
        // Replace only the generated prefix. The original value (possibly
        // containing the word nvarchar or unpaired units) remains untouched.
        let (old, new) = match error.number {
            245 => (
                "Conversion failed when converting the nvarchar value '",
                "Conversion failed when converting the varchar value '",
            ),
            248 => (
                "The conversion of the nvarchar value '",
                "The conversion of the varchar value '",
            ),
            _ => return Some(error),
        };
        let original = error
            .message_utf16
            .clone()
            .unwrap_or_else(|| error.message.encode_utf16().collect());
        let old = old.encode_utf16().collect::<Vec<_>>();
        let remainder = original.strip_prefix(old.as_slice())?;
        return Some(SqlError::from_utf16(
            error.number,
            error.state,
            error.severity,
            new.encode_utf16()
                .chain(remainder.iter().copied())
                .collect(),
        ));
    }
    let bits = message.strip_prefix(FLOAT_OVERFLOW)?;
    if bits.len() != 16 || !bits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let value = f64::from_bits(u64::from_str_radix(bits, 16).ok()?);
    if !value.is_finite() || (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&value.trunc()) {
        return None;
    }
    Some(msduck_core::diagnostic::SqlError::new(
        232,
        3,
        format!("Arithmetic overflow error for type int, value = {value:.6}."),
    ))
}
struct Draw<const SEEDED: bool>;
impl<const SEEDED: bool> VScalar for Draw<SEEDED> {
    type State = Registry;
    fn signatures() -> Vec<ScalarFunctionSignature> {
        let mut inputs = vec![LogicalTypeId::Blob.into()];
        if SEEDED {
            inputs.push(LogicalTypeId::Integer.into());
        }
        vec![ScalarFunctionSignature::exact(
            inputs,
            LogicalTypeId::Double.into(),
        )]
    }
    fn volatile() -> bool {
        true
    }
    fn special_null_handling() -> bool {
        true
    }
    fn invoke(
        registry: &Registry,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let tickets = input.flat_vector(0);
        let seeds = SEEDED.then(|| input.flat_vector(1));
        let mut result = output.flat_vector();
        for row in 0..input.len() {
            if tickets.row_is_null(row as u64) {
                return Err("NULL RAND ticket".into());
            }
            let mut value = unsafe {
                tickets.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(input.len())[row]
            };
            if unsafe { duckdb::ffi::duckdb_string_t_length(value) } != 16 {
                return Err("invalid RAND ticket length".into());
            }
            let ticket: Ticket = unsafe {
                std::slice::from_raw_parts(
                    duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
                    16,
                )
            }
            .try_into()
            .expect("checked ticket length");
            let call = registry
                .0
                .lock()
                .map_err(|_| "RAND registry poisoned")?
                .get(&ticket)
                .cloned()
                .ok_or("unknown RAND call context")?;
            // NULL seed neither draws nor resets the generator.
            if seeds
                .as_ref()
                .is_some_and(|seeds| seeds.row_is_null(row as u64))
            {
                result.set_null(row);
                continue;
            }
            let mut call = call.lock().map_err(|_| "RAND call context poisoned")?;
            let value = match call.value {
                Some(value) => value,
                None => {
                    let value = {
                        let mut generator =
                            call.generator.lock().map_err(|_| "RAND session poisoned")?;
                        if let Some(seeds) = &seeds {
                            *generator = Generator::seeded(unsafe {
                                seeds.as_slice_with_len::<i32>(input.len())[row]
                            });
                        }
                        generator.draw()
                    };
                    call.value = Some(value);
                    value
                }
            };
            unsafe {
                result.as_mut_slice::<f64>()[row] = value;
            }
        }
        Ok(())
    }
}

/// Run after parameter translation; append BLOB bindings instead of SQL literals.
/// Retain every scope until all result batches have been consumed, including errors.
pub(super) fn lower<T: VisitMut>(
    node: &mut T,
    values: &mut Vec<Value>,
    registry: &Registry,
    generator: &Arc<Mutex<Generator>>,
) -> Result<Vec<Scope>> {
    struct Lower<'a> {
        values: &'a mut Vec<Value>,
        registry: &'a Registry,
        generator: &'a Arc<Mutex<Generator>>,
        scopes: Vec<Scope>,
    }
    impl VisitorMut for Lower<'_> {
        type Break = anyhow::Error;
        fn post_visit_expr(&mut self, expression: &mut Expr) -> ControlFlow<Self::Break> {
            let Expr::Function(function) = expression else {
                return ControlFlow::Continue(());
            };
            if !function.name.to_string().eq_ignore_ascii_case("rand") {
                return ControlFlow::Continue(());
            }
            if !supported_shape(function) {
                return ControlFlow::Break(anyhow::anyhow!("unsupported RAND shape"));
            }
            let FunctionArguments::List(arguments) = &mut function.args else {
                return ControlFlow::Break(anyhow::anyhow!("unsupported RAND arguments"));
            };
            if arguments.args.len() > 1
                || !arguments.clauses.is_empty()
                || function.over.is_some()
                || function.filter.is_some()
                || !function.within_group.is_empty()
                || arguments.duplicate_treatment.is_some()
            {
                return ControlFlow::Break(anyhow::anyhow!("unsupported RAND shape"));
            }
            let scope = match self.registry.begin(self.generator) {
                Ok(scope) => scope,
                Err(error) => return ControlFlow::Break(error),
            };
            self.values.push(Value::Blob(scope.ticket.to_vec()));
            let ticket = Expr::Value(
                sqlparser::ast::Value::Placeholder(format!("${}", self.values.len())).into(),
            );
            let seeded = !arguments.args.is_empty();
            arguments
                .args
                .insert(0, FunctionArg::Unnamed(FunctionArgExpr::Expr(ticket)));
            function.name = ObjectName::from(vec![Ident::new(if seeded {
                "__msduck_rand_seed"
            } else {
                "__msduck_rand"
            })]);
            self.scopes.push(scope);
            ControlFlow::Continue(())
        }
    }
    let mut lower = Lower {
        values,
        registry,
        generator,
        scopes: vec![],
    };
    if let ControlFlow::Break(error) = node.visit(&mut lower) {
        return Err(error);
    }
    Ok(lower.scopes)
}

/// Fraction declaration acquisition uses FLOAT syntax without running RAND.
fn supported_shape(function: &Function) -> bool {
    let FunctionArguments::List(arguments) = &function.args else {
        return false;
    };
    matches!(
        arguments.args.as_slice(),
        [] | [FunctionArg::Unnamed(FunctionArgExpr::Expr(_))]
    ) && arguments.clauses.is_empty()
        && arguments.duplicate_treatment.is_none()
        && function.over.is_none()
        && function.filter.is_none()
        && function.within_group.is_empty()
        && function.null_treatment.is_none()
        && matches!(function.parameters, FunctionArguments::None)
        && !function.uses_odbc_syntax
}

pub(super) fn declarations<T: VisitMut>(node: &mut T) -> bool {
    struct Declare(bool);
    impl VisitorMut for Declare {
        type Break = ();
        fn post_visit_expr(&mut self, expression: &mut Expr) -> ControlFlow<()> {
            if matches!(expression, Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("rand") && supported_shape(f))
            {
                // As in result_types::bound_statement, CONVERT is a declaration
                // surrogate, not a literal-NULL proof for COALESCE/CASE. This
                // profile never reaches the executor or inspects RNG/seed values.
                *expression = Expr::Convert {
                    is_try: false,
                    expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
                    data_type: Some(DataType::Double(ExactNumberInfo::None)),
                    charset: None,
                    target_before_value: true,
                    styles: vec![],
                };
                self.0 = true;
            }
            ControlFlow::Continue(())
        }
    }
    let mut declare = Declare(false);
    let _ = node.visit(&mut declare);
    declare.0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declaration_profiles_preserve_unknown_values_and_supported_shapes() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let session = super::super::Session::new(server.connection().unwrap()).unwrap();
        let state = {
            let state = session.rand.lock().unwrap();
            (state.x, state.y)
        };
        for (expression, nullable) in [
            ("RAND(42)", true),
            ("RAND(NULL)", true),
            ("COALESCE(RAND(),1)", true),
            ("ISNULL(RAND(),1)", false),
            ("CASE WHEN 1=1 THEN .5 ELSE RAND() END", true),
        ] {
            let mut statement = sqlparser::parser::Parser::parse_sql(
                &crate::dialect::ServerDialect,
                &format!("SELECT {expression} AS r"),
            )
            .unwrap()
            .remove(0);
            let original = statement.clone();
            assert!(declarations(&mut statement));
            assert!(original.to_string().contains("RAND"));
            assert!(!statement.to_string().contains("RAND"));
            let Statement::Query(query) = statement else {
                panic!("query");
            };
            let fields = crate::query_catalog::projection_with_parameters(
                &session.db,
                &query,
                &HashMap::new(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                fields[0].info.as_ref().unwrap().system_type_id,
                Some(62),
                "{expression}"
            );
            assert_eq!(
                fields[0].properties.nullable,
                Some(nullable),
                "{expression}"
            );
            assert_eq!(
                fields[0].properties.origin,
                msduck_core::result::Origin::Expression
            );
        }
        for sql in ["SELECT RAND(1,2)", "SELECT RAND() OVER()"] {
            let mut statement =
                sqlparser::parser::Parser::parse_sql(&crate::dialect::ServerDialect, sql)
                    .unwrap()
                    .remove(0);
            let original = statement.clone();
            assert!(!declarations(&mut statement));
            assert_eq!(statement, original);
        }
        let after = session.rand.lock().unwrap();
        assert_eq!((after.x, after.y), state);
    }
    #[test]
    fn character_seed_preserves_source_and_raw_unicode_diagnostics() {
        let db = Connection::open_in_memory().unwrap();
        Registry::default().register(&db).unwrap();
        for unicode in [false, true] {
            for text in ["abc", "1.9", "nvarchar"] {
                let error = db
                    .query_row(
                        "SELECT __msduck_rand_character_seed(?,?)",
                        duckdb::params![text, unicode],
                        |row| row.get::<_, i32>(0),
                    )
                    .unwrap_err();
                let error = diagnostic(&error.to_string()).unwrap();
                assert_eq!((error.number, error.state, error.severity), (245, 1, 16));
                assert_eq!(
                    error.message,
                    format!(
                        "Conversion failed when converting the {} value '{text}' to data type int.",
                        if unicode { "nvarchar" } else { "varchar" }
                    )
                );
            }
            let value: i32 = db
                .query_row(
                    "SELECT __msduck_rand_character_seed('42',?)",
                    [unicode],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(value, 42);
            let value: Option<i32> = db
                .query_row(
                    "SELECT __msduck_rand_character_seed(CAST(NULL AS VARCHAR),?)",
                    [unicode],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(value, None);
        }
        let error = db.query_row("SELECT __msduck_rand_character_seed(struct_pack(__msduck_utf16le:=from_hex('00d8')),true)",[],|row| row.get::<_,i32>(0)).unwrap_err();
        let error = diagnostic(&error.to_string()).unwrap();
        assert_eq!(error.number, 245);
        assert!(error.message_utf16.unwrap().contains(&0xd800));
        let error = db
            .query_row(
                "SELECT __msduck_rand_character_seed(repeat('x',4001),true)",
                [],
                |row| row.get::<_, i32>(0),
            )
            .unwrap_err();
        let error = diagnostic(&error.to_string()).unwrap();
        assert_eq!((error.number, error.state), (8152, 10));
        assert!(
            diagnostic("Invalid Input Error: __msduck_rand_character_seed_failure:u:0034")
                .is_none()
        );
        assert!(
            diagnostic("Invalid Input Error: __msduck_rand_character_seed_failure:u:00d8 extra")
                .is_none()
        );
    }
    #[test]
    fn bigint_seed_keeps_checked_bounds_and_overflow_state() {
        let db = Connection::open_in_memory().unwrap();
        Registry::default().register(&db).unwrap();
        for value in [i64::from(i32::MIN), 42, i64::from(i32::MAX)] {
            let result: i32 = db
                .query_row("SELECT __msduck_rand_bigint_seed(?)", [value], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(i64::from(result), value);
        }
        for value in [
            i64::from(i32::MIN) - 1,
            i64::from(i32::MAX) + 1,
            i64::MIN,
            i64::MAX,
        ] {
            let error = db
                .query_row("SELECT __msduck_rand_bigint_seed(?)", [value], |row| {
                    row.get::<_, i32>(0)
                })
                .unwrap_err();
            let error = diagnostic(&error.to_string()).unwrap();
            assert_eq!((error.number, error.state), (8115, 2));
        }
    }
    #[test]
    fn float_seed_truncates_and_retains_captured_overflow_identity() {
        let db = Connection::open_in_memory().unwrap();
        Registry::default().register(&db).unwrap();
        for value in [0.9_f64, 1.9, -1.9, 42.9] {
            let integer: i32 = db
                .query_row("SELECT __msduck_rand_float_seed(?)", [value], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(integer, value.trunc() as i32);
        }
        let null: Option<i32> = db
            .query_row("SELECT __msduck_rand_float_seed(NULL)", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(null, None);
        let error = db
            .query_row(
                "SELECT __msduck_rand_float_seed(?)",
                [2147483648.0_f64],
                |row| row.get::<_, i32>(0),
            )
            .unwrap_err();
        let error = diagnostic(&error.to_string()).unwrap();
        assert_eq!((error.number, error.state, error.severity), (232, 3, 16));
        assert_eq!(
            error.message,
            "Arithmetic overflow error for type int, value = 2147483648.000000."
        );
        for invalid in [
            "__msduck_rand_float_seed_overflow:41e0000000000000",
            "Invalid Input Error: __msduck_rand_float_seed_overflow:41e0000000000000 extra",
            "Invalid Input Error: __msduck_rand_float_seed_overflow:3ff0000000000000",
        ] {
            assert!(diagnostic(invalid).is_none());
        }
    }
    #[test]
    fn seeded_reference_stream_and_boundaries() {
        let mut generator = Generator::seeded(42);
        for expected in [
            0.7143559450345097_f64,
            0.041009986028273604,
            0.6493841884705345,
            0.40716252980396633,
        ] {
            assert_eq!(generator.draw().to_bits(), expected.to_bits());
        }
        for seed in [0, i32::MIN, 2147483563, i32::MAX] {
            assert_eq!(
                Generator::seeded(seed).draw().to_bits(),
                Generator::seeded(12345).draw().to_bits()
            );
        }
        assert_eq!(
            Generator::seeded(-42).draw().to_bits(),
            Generator::seeded(42).draw().to_bits()
        );
    }
    #[test]
    fn native_calls_are_lazy_memoized_and_scoped() {
        let db = Connection::open_in_memory().unwrap();
        let registry = Registry::default();
        registry.register(&db).unwrap();
        let generator = Arc::new(Mutex::new(Generator::seeded(42)));
        let scope = registry.begin(&generator).unwrap();
        let values: Vec<f64> = db
            .prepare("SELECT __msduck_rand(?) FROM range(6000)")
            .unwrap()
            .query_map([scope.ticket.as_slice()], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(values.len(), 6000);
        assert!(
            values
                .iter()
                .all(|v| v.to_bits() == 0.7143559450345097_f64.to_bits())
        );
        drop(scope);
        let scope = registry.begin(&generator).unwrap();
        db.execute(
            "SELECT __msduck_rand(?) WHERE false",
            [scope.ticket.as_slice()],
        )
        .unwrap();
        db.execute(
            "SELECT CASE WHEN true THEN 0.5 ELSE __msduck_rand(?) END",
            [scope.ticket.as_slice()],
        )
        .unwrap();
        let value: f64 = db
            .query_row(
                "SELECT __msduck_rand(?)",
                [scope.ticket.as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value.to_bits(), 0.041009986028273604_f64.to_bits());
        let stale = scope.ticket;
        drop(scope);
        assert!(registry.0.lock().unwrap().is_empty());
        assert!(
            db.query_row("SELECT __msduck_rand(?)", [stale.as_slice()], |row| row
                .get::<_, f64>(0))
                .is_err()
        );
    }
}
