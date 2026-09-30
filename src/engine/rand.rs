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
pub(super) fn seed_conversion(expression: &mut Expr) {
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
        db.register_scalar_function_with_state::<Draw<false>>("__msduck_rand", self)?;
        db.register_scalar_function_with_state::<Draw<true>>("__msduck_rand_seed", self)
    }
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
pub(super) fn declarations<T: VisitMut>(node: &mut T) {
    struct Declare;
    impl VisitorMut for Declare {
        type Break = ();
        fn post_visit_expr(&mut self, expression: &mut Expr) -> ControlFlow<()> {
            if matches!(expression, Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("rand"))
            {
                *expression = Expr::Cast {
                    kind: CastKind::Cast,
                    expr: Box::new(Expr::Value(sqlparser::ast::Value::Null.into())),
                    data_type: DataType::Double(ExactNumberInfo::None),
                    format: None,
                };
            }
            ControlFlow::Continue(())
        }
    }
    let _ = node.visit(&mut Declare);
}

#[cfg(test)]
mod tests {
    use super::*;
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
