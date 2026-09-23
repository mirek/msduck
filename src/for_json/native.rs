//! Native vector boundary for JSON row serialization inside scalar subqueries.
use super::*;
use duckdb::{
    core::{DataChunkHandle, FlatVector, Inserter, LogicalTypeId as Id, StructVector},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
type Spec = (Vec<String>, Vec<bool>, bool, Vec<Option<u8>>);

fn text(vector: &FlatVector<'_>, row: usize, len: usize) -> Result<String> {
    ensure!(
        vector.logical_type().id() == Id::Varchar,
        "invalid JSON adapter string input"
    );
    ensure!(
        !vector.row_is_null(row as u64),
        "invalid NULL JSON adapter string"
    );
    // Exact VARCHAR slots; copy inline strings before borrowing their data.
    let mut value = unsafe { vector.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(len)[row] };
    let bytes = unsafe {
        std::slice::from_raw_parts(
            duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
            duckdb::ffi::duckdb_string_t_length(value) as usize,
        )
    };
    Ok(std::str::from_utf8(bytes)?.to_owned())
}

fn value(
    structure: &StructVector<'_>,
    column: usize,
    row: usize,
    len: usize,
) -> Result<(Value, Option<Type>)> {
    let vector = structure.child(column, len);
    if vector.row_is_null(row as u64) {
        return Ok((Value::Null, None));
    }
    let logical = vector.logical_type();
    let kind = logical.id();
    // Every read below is selected by the bound physical type and a live row.
    macro_rules! read {
        ($ty:ty) => {
            unsafe { vector.as_slice_with_len::<$ty>(len)[row] }
        };
    }
    let value = match kind {
        Id::Boolean => Value::Boolean(read!(u8) != 0),
        Id::Tinyint => Value::TinyInt(read!(i8)),
        Id::Smallint => Value::SmallInt(read!(i16)),
        Id::Integer => Value::Int(read!(i32)),
        Id::Bigint => Value::BigInt(read!(i64)),
        Id::UTinyint => Value::UTinyInt(read!(u8)),
        Id::USmallint => Value::USmallInt(read!(u16)),
        Id::UInteger => Value::UInt(read!(u32)),
        Id::UBigint => Value::UBigInt(read!(u64)),
        Id::Float => Value::Float(read!(f32)),
        Id::Double => Value::Double(read!(f64)),
        Id::Hugeint => {
            let v = read!(duckdb::ffi::duckdb_hugeint);
            Value::HugeInt((i128::from(v.upper) << 64) | i128::from(v.lower))
        }
        Id::Decimal => {
            let width = logical.decimal_width();
            let coefficient = match width {
                1..=4 => i128::from(read!(i16)),
                5..=9 => i128::from(read!(i32)),
                10..=18 => i128::from(read!(i64)),
                19..=38 => {
                    let v = read!(duckdb::ffi::duckdb_hugeint);
                    (i128::from(v.upper) << 64) | i128::from(v.lower)
                }
                _ => bail!("invalid native decimal width"),
            };
            Value::Decimal(duckdb::types::Decimal::new(
                width,
                logical.decimal_scale(),
                coefficient,
            )?)
        }
        Id::Varchar => Value::Text(text(&vector, row, len)?),
        Id::Blob => {
            let mut v = read!(duckdb::ffi::duckdb_string_t);
            Value::Blob(
                unsafe {
                    std::slice::from_raw_parts(
                        duckdb::ffi::duckdb_string_t_data(&mut v).cast::<u8>(),
                        duckdb::ffi::duckdb_string_t_length(v) as usize,
                    )
                }
                .to_vec(),
            )
        }
        Id::Uuid => {
            let v = read!(duckdb::ffi::duckdb_hugeint);
            let bits = ((v.upper as u64 as u128) << 64 | u128::from(v.lower)) ^ (1_u128 << 127);
            Value::Text(uuid::Uuid::from_u128(bits).to_string())
        }
        Id::Date => Value::Date32(read!(i32)),
        Id::Time => Value::Time64(TimeUnit::Microsecond, read!(i64)),
        Id::TimeNs => Value::Time64(TimeUnit::Nanosecond, read!(i64)),
        Id::Timestamp => Value::Timestamp(TimeUnit::Microsecond, read!(i64)),
        Id::TimestampS => Value::Timestamp(TimeUnit::Second, read!(i64)),
        Id::TimestampMs => Value::Timestamp(TimeUnit::Millisecond, read!(i64)),
        Id::TimestampNs => Value::Timestamp(TimeUnit::Nanosecond, read!(i64)),
        Id::Struct => {
            ensure!(
                logical.num_children() > 0,
                "unsupported empty JSON structure"
            );
            let nested = structure.struct_vector_child(column);
            let field = logical.child_name(0);
            if field == "__msduck_variant_type" {
                ensure!(
                    matches!(logical.num_children(), 2 | 3)
                        && logical.child(0).id() == Id::UTinyint
                        && logical.child(1).id() == Id::Bigint
                        && logical.child_name(1) == "__msduck_variant_integer",
                    "invalid native JSON variant shape"
                );
                let tags = nested.child(0, len);
                ensure!(
                    !tags.row_is_null(row as u64),
                    "invalid native JSON variant tag"
                );
                let tag = unsafe { tags.as_slice_with_len::<u8>(len)[row] };
                if tag == 231 {
                    ensure!(
                        logical.num_children() == 3
                            && logical.child_name(2) == "__msduck_variant_sysname",
                        "invalid native JSON variant sysname shape"
                    );
                    return Ok((Value::Text(text(&nested.child(2, len), row, len)?), None));
                }
                let integers = nested.child(1, len);
                ensure!(
                    !integers.row_is_null(row as u64),
                    "invalid native JSON variant payload"
                );
                let integer = unsafe { integers.as_slice_with_len::<i64>(len)[row] };
                let value = match tag {
                    104 if matches!(integer, 0 | 1) => Value::Boolean(integer != 0),
                    48 => Value::UTinyInt(u8::try_from(integer)?),
                    52 => Value::SmallInt(i16::try_from(integer)?),
                    56 => Value::Int(i32::try_from(integer)?),
                    127 => Value::BigInt(integer),
                    _ => bail!("unsupported nested FOR JSON variant base type"),
                };
                return Ok((value, None));
            }
            let scale = |prefix: &str| {
                field
                    .strip_prefix(prefix)
                    .and_then(|s| s.parse::<u8>().ok())
                    .filter(|s| *s <= 7)
            };
            let dt2 = scale("__msduck_datetime2_");
            let dto = scale("__msduck_datetimeoffset_");
            if let Some(scale) = dt2.or(dto) {
                ensure!(
                    logical.num_children() == if dto.is_some() { 2 } else { 1 }
                        && logical.child(0).id() == Id::Bigint,
                    "invalid native JSON temporal shape"
                );
                let ticks = nested.child(0, len);
                ensure!(
                    !ticks.row_is_null(row as u64),
                    "invalid native JSON temporal ticks"
                );
                let ticks = crate::datetime2::DateTime2::from_ticks(unsafe {
                    ticks.as_slice_with_len::<i64>(len)[row]
                })?;
                if dto.is_some() {
                    ensure!(
                        logical.child(1).id() == Id::Smallint
                            && logical.child_name(1) == "__msduck_offset_minutes",
                        "invalid native JSON offset shape"
                    );
                    let offset = nested.child(1, len);
                    ensure!(
                        !offset.row_is_null(row as u64),
                        "invalid native JSON offset"
                    );
                    let offset = unsafe { offset.as_slice_with_len::<i16>(len)[row] };
                    return Ok((
                        Value::Text(
                            crate::datetimeoffset::DateTimeOffset::from_utc(ticks, offset)?
                                .format_iso(scale)?,
                        ),
                        Some(Type::DateTimeOffset(scale)),
                    ));
                }
                return Ok((
                    Value::Text(ticks.format_iso(scale)?),
                    Some(Type::DateTime2(scale)),
                ));
            }
            bail!("unsupported nested FOR JSON native structure")
        }
        _ => bail!("unsupported nested FOR JSON value type {kind:?}"),
    };
    Ok((value, None))
}
struct Row;
impl VScalar for Row {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        (|| -> Result<()> {
            let len = input.len();
            ensure!(
                input.flat_vector(0).logical_type().id() == Id::Struct,
                "invalid JSON row input"
            );
            let source = input.struct_vector(0);
            let specs = input.flat_vector(1);
            let result = output.flat_vector();
            let mut cached = None;
            for row in 0..len {
                let encoded = text(&specs, row, len)?;
                if cached
                    .as_ref()
                    .is_none_or(|(key, _, _, _, _)| key != &encoded)
                {
                    let (names, flags, include_null_values, scales): Spec =
                        serde_json::from_str(&encoded)?;
                    ensure!(
                        names.len() == source.num_children()
                            && flags.len() == names.len()
                            && scales.len() == names.len(),
                        "invalid JSON row descriptor width"
                    );
                    let options = Options {
                        include_null_values,
                        without_array_wrapper: true,
                        root: None,
                    };
                    let output = Output {
                        options,
                        fragments: names
                            .iter()
                            .zip(&flags)
                            .filter(|(_, f)| **f)
                            .map(|(name, _)| name.clone())
                            .collect(),
                    };
                    let plan = output.plan(&names)?;
                    cached = Some((encoded, names, output, plan, scales));
                }
                let (_, names, descriptor, plan, scales) = cached.as_ref().unwrap();
                let (values, mut types): (Vec<_>, Vec<_>) = (0..source.num_children())
                    .map(|column| value(&source, column, row, len))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .unzip();
                for (kind, scale) in types.iter_mut().zip(scales) {
                    if let Some(scale) = scale {
                        *kind = Some(Type::Time(*scale));
                    }
                }
                let mut writer = descriptor.writer(plan)?;
                descriptor.row(&mut writer, names, &values, &types)?;
                result.insert(row, writer.finish().as_str());
            }
            Ok(())
        })()
        .map_err(Into::into)
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into(), Id::Varchar.into()],
            Id::Varchar.into(),
        )]
    }
}
struct Wrap<const ARRAY: bool>;
impl<const ARRAY: bool> VScalar for Wrap<ARRAY> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        (|| -> Result<()> {
            let len = input.len();
            let source = input.flat_vector(0);
            let root = input.flat_vector(1);
            let result = output.flat_vector();
            for row in 0..len {
                let rows = if source.row_is_null(row as u64) {
                    String::new()
                } else {
                    text(&source, row, len)?
                };
                let root = text(&root, row, len)?;
                let value = json::wrap_rows(&rows, root.strip_prefix('1'), !ARRAY)?;
                ensure!(
                    value.len() <= tds::MAX_MESSAGE,
                    "result exceeds current 16 MiB response limit"
                );
                result.insert(row, value.as_str());
            }
            Ok(())
        })()
        .map_err(Into::into)
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Varchar.into(), Id::Varchar.into()],
            Id::Varchar.into(),
        )]
    }
}
pub fn register(db: &Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Row>("__msduck_json_row")?;
    db.register_scalar_function::<Wrap<true>>("__msduck_json_array")?;
    db.register_scalar_function::<Wrap<false>>("__msduck_json_unwrapped")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_queries_evaluate_source_once_and_preparation_never_steps_it() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch("CREATE SEQUENCE nested_json_counter START 1")
            .unwrap();
        let sql = "DECLARE @json NVARCHAR(MAX)=(SELECT nextval('nested_json_counter') AS n FROM range(6000) FOR JSON PATH); SELECT @json";
        session.validate_prepared_sql(sql, &[]).unwrap();
        let first: i64 = session
            .db
            .query_row("SELECT nextval('nested_json_counter')", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(first, 1);
        let response = session.batch(sql, &HashMap::new(), false);
        assert_eq!(session.last_error, 0, "{response:?}");
        let next: i64 = session
            .db
            .query_row("SELECT nextval('nested_json_counter')", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(next, 6002);
    }
}
