//! Native checked-value arithmetic. Expected SQL errors remain ordinary data.
use duckdb::{
    Connection,
    core::{DataChunkHandle, FlatVector, Inserter, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::checked_integer::{Integer, Operation, calculate};

type Error = Box<dyn std::error::Error>;
struct Arithmetic<const OP: u8>;

fn operand(vector: &FlatVector<'_>, row: usize, kind: Id) -> Integer {
    let null = vector.row_is_null(row as u64);
    // The caller checks type and capacity. Only valid cells are read; NULL
    // payloads need not be initialized. The C API flattens input vectors.
    match kind {
        Id::Integer => Integer::Int(if null {
            None
        } else {
            Some(unsafe { vector.as_mut_ptr::<i32>().add(row).read() })
        }),
        Id::Bigint => Integer::BigInt(if null {
            None
        } else {
            Some(unsafe { vector.as_mut_ptr::<i64>().add(row).read() })
        }),
        _ => unreachable!("validated integer input"),
    }
}

impl<const OP: u8> VScalar for Arithmetic<OP> {
    type State = ();
    fn special_null_handling() -> bool {
        true
    }
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Error> {
        if input.num_columns() != 2 {
            return Err("checked integer arity mismatch".into());
        }
        let operation = match OP {
            0 => Operation::Add,
            1 => Operation::Subtract,
            2 => Operation::Multiply,
            3 => Operation::Divide,
            4 => Operation::Modulo,
            _ => return Err("invalid checked integer operation".into()),
        };
        let len = input.len();
        let left = input.flat_vector(0);
        let right = input.flat_vector(1);
        let left_kind = left.logical_type().id();
        let right_kind = right.logical_type().id();
        if !matches!(left_kind, Id::Integer | Id::Bigint)
            || !matches!(right_kind, Id::Integer | Id::Bigint)
            || len > left.capacity()
            || len > right.capacity()
        {
            return Err("invalid checked integer input vector".into());
        }
        let big = left_kind == Id::Bigint || right_kind == Id::Bigint;
        let result = output.struct_vector();
        let mut values = result.child(0, len);
        let mut numbers = result.child(1, len);
        let mut states = result.child(2, len);
        let mut severities = result.child(3, len);
        let mut messages = result.child(4, len);
        if values.logical_type().id() != if big { Id::Bigint } else { Id::Integer }
            || numbers.logical_type().id() != Id::Integer
            || states.logical_type().id() != Id::UTinyint
            || severities.logical_type().id() != Id::UTinyint
            || messages.logical_type().id() != Id::Varchar
        {
            return Err("invalid checked integer output vector".into());
        }
        for row in 0..len {
            let outcome = calculate(
                operation,
                operand(&left, row, left_kind),
                operand(&right, row, right_kind),
            );
            // Each wrapper refers to a distinct child allocation. Row bounds
            // and exact physical types are checked above before pointer writes.
            let value = match &outcome {
                Ok(Integer::Int(v)) => v.map(i64::from),
                Ok(Integer::BigInt(v)) => *v,
                Err(_) => None,
            };
            unsafe {
                if big {
                    values
                        .as_mut_ptr::<i64>()
                        .add(row)
                        .write(value.unwrap_or(0));
                } else {
                    values
                        .as_mut_ptr::<i32>()
                        .add(row)
                        .write(value.unwrap_or(0) as i32);
                }
            }
            if value.is_none() {
                values.set_null(row);
            }
            if let Err(error) = outcome {
                // Messages come only from the bounded deterministic integer
                // rules (fixed text plus a fixed type name), never user input.
                if error.message.len() > 128 {
                    return Err("checked integer diagnostic exceeds bound".into());
                }
                unsafe {
                    numbers.as_mut_ptr::<i32>().add(row).write(error.number);
                    states.as_mut_ptr::<u8>().add(row).write(error.state);
                    severities.as_mut_ptr::<u8>().add(row).write(error.severity);
                }
                messages.insert(row, &error.message);
            } else {
                unsafe {
                    numbers.as_mut_ptr::<i32>().add(row).write(0);
                    states.as_mut_ptr::<u8>().add(row).write(0);
                    severities.as_mut_ptr::<u8>().add(row).write(0);
                }
                numbers.set_null(row);
                states.set_null(row);
                severities.set_null(row);
                messages.set_null(row);
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        [Id::Integer, Id::Bigint]
            .into_iter()
            .flat_map(|left| {
                [Id::Integer, Id::Bigint].into_iter().map(move |right| {
                    let value = if left == Id::Bigint || right == Id::Bigint {
                        Id::Bigint
                    } else {
                        Id::Integer
                    };
                    ScalarFunctionSignature::exact(
                        vec![left.into(), right.into()],
                        LogicalTypeHandle::struct_type(&[
                            ("value", value.into()),
                            ("error_number", Id::Integer.into()),
                            ("error_state", Id::UTinyint.into()),
                            ("error_severity", Id::UTinyint.into()),
                            ("error_message", Id::Varchar.into()),
                        ]),
                    )
                })
            })
            .collect()
    }
}

pub fn register(db: &Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Arithmetic<0>>("__msduck_checked_add")?;
    db.register_scalar_function::<Arithmetic<1>>("__msduck_checked_subtract")?;
    db.register_scalar_function::<Arithmetic<2>>("__msduck_checked_multiply")?;
    db.register_scalar_function::<Arithmetic<3>>("__msduck_checked_divide")?;
    db.register_scalar_function::<Arithmetic<4>>("__msduck_checked_modulo")
}
