//! Integer sql_variant values used by identity catalog projections.
use anyhow::{Result, bail, ensure};
use duckdb::{
    arrow::{
        array::{Array, Int64Array, LargeStringArray, StringArray, StructArray, UInt8Array},
        datatypes::DataType,
    },
    types::Value,
};

struct Tag;
impl duckdb::vscalar::VScalar for Tag {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut duckdb::core::DataChunkHandle,
        output: &mut dyn duckdb::vtab::arrow::WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use duckdb::core::LogicalTypeId as Id;
        let len = input.len();
        let source = input.flat_vector(0);
        let logical = source.logical_type();
        let kind = logical.id();
        let tagged = kind == Id::Struct
            && matches!(logical.num_children(), 2 | 3)
            && logical.child_name(0) == "__msduck_variant_type"
            && logical.child(0).id() == Id::UTinyint
            && logical.child_name(1) == "__msduck_variant_integer"
            && logical.child(1).id() == Id::Bigint;
        let tag = match kind {
            Id::Boolean => 104,
            Id::UTinyint => 48,
            Id::Smallint => 52,
            Id::Integer => 56,
            Id::Bigint => 127,
            Id::SqlNull => 0,
            _ if tagged => 0,
            _ => return Err("unsupported SQL_VARIANT_PROPERTY input type".into()),
        };
        let structure = tagged.then(|| input.struct_vector(0));
        let tags = structure.as_ref().map(|s| s.child(0, len));
        let mut out = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) {
                out.set_null(row);
                continue;
            }
            let value = if let Some(tags) = &tags {
                if tags.row_is_null(row as u64) {
                    return Err("invalid sql_variant tag".into());
                }
                unsafe { tags.as_slice_with_len::<u8>(len)[row] }
            } else {
                tag
            };
            unsafe {
                out.as_mut_slice::<u8>()[row] = value;
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<duckdb::vscalar::ScalarFunctionSignature> {
        use duckdb::core::LogicalTypeId as Id;
        vec![duckdb::vscalar::ScalarFunctionSignature::exact(
            vec![Id::Any.into()],
            Id::UTinyint.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Tag>("__msduck_variant_tag")
}

pub fn is_arrow(kind: &DataType) -> bool {
    let DataType::Struct(fields) = kind else {
        return false;
    };
    (fields.len() == 2
        || (fields.len() == 3
            && fields[2].name() == "__msduck_variant_sysname"
            && matches!(fields[2].data_type(), DataType::Utf8 | DataType::LargeUtf8)))
        && fields[0].name() == "__msduck_variant_type"
        && fields[0].data_type() == &DataType::UInt8
        && fields[1].name() == "__msduck_variant_integer"
        && fields[1].data_type() == &DataType::Int64
}

pub fn read(array: &dyn Array, row: usize) -> Result<Value> {
    let values = array
        .as_any()
        .downcast_ref::<StructArray>()
        .ok_or_else(|| anyhow::anyhow!("invalid sql_variant array"))?;
    let tags = values
        .column(0)
        .as_any()
        .downcast_ref::<UInt8Array>()
        .ok_or_else(|| anyhow::anyhow!("invalid sql_variant tag"))?;
    ensure!(!tags.is_null(row), "invalid sql_variant tag");
    if tags.value(row) == 231 {
        ensure!(values.num_columns() == 3, "missing sql_variant sysname");
        let text = values.column(2);
        ensure!(!text.is_null(row), "invalid sql_variant sysname");
        let text = if let Some(text) = text.as_any().downcast_ref::<StringArray>() {
            text.value(row)
        } else if let Some(text) = text.as_any().downcast_ref::<LargeStringArray>() {
            text.value(row)
        } else {
            bail!("invalid sql_variant sysname array")
        };
        return Ok(Value::Text(text.to_owned()));
    }
    let integers = values
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| anyhow::anyhow!("invalid sql_variant integer"))?;
    ensure!(
        !tags.is_null(row) && !integers.is_null(row),
        "invalid sql_variant payload"
    );
    let value = integers.value(row);
    Ok(match tags.value(row) {
        104 if matches!(value, 0 | 1) => Value::Boolean(value != 0),
        104 => bail!("invalid BIT sql_variant value"),
        48 => Value::UTinyInt(u8::try_from(value)?),
        52 => Value::SmallInt(i16::try_from(value)?),
        56 => Value::Int(i32::try_from(value)?),
        127 => Value::BigInt(value),
        _ => bail!("unsupported sql_variant base type"),
    })
}

pub fn encode(out: &mut Vec<u8>, value: &Value) -> Result<()> {
    if let Value::Text(text) = value {
        let bytes = crate::tds::text(text);
        ensure!(
            bytes.len() <= 256,
            "sql_variant sysname exceeds 128 characters"
        );
        out.extend((bytes.len() as u32 + 9).to_le_bytes());
        out.extend([0xe7, 7]);
        out.extend(crate::tds::COLLATION);
        out.extend(256u16.to_le_bytes());
        out.extend(bytes);
        return Ok(());
    }
    let (tag, bytes) = match value {
        Value::Null => {
            out.extend(0u32.to_le_bytes());
            return Ok(());
        }
        Value::Boolean(v) => (0x32, vec![u8::from(*v)]),
        Value::UTinyInt(v) => (0x30, vec![*v]),
        Value::SmallInt(v) => (0x34, v.to_le_bytes().to_vec()),
        Value::Int(v) => (0x38, v.to_le_bytes().to_vec()),
        Value::BigInt(v) => (0x7f, v.to_le_bytes().to_vec()),
        _ => bail!("unsupported sql_variant payload"),
    };
    out.extend((bytes.len() as u32 + 2).to_le_bytes());
    out.extend([tag, 0]);
    out.extend(bytes);
    Ok(())
}

pub fn lower(expr: &mut sqlparser::ast::Expr) -> Result<(), String> {
    use sqlparser::ast::*;
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    if !f
        .name
        .to_string()
        .eq_ignore_ascii_case("SQL_VARIANT_PROPERTY")
    {
        return Ok(());
    }
    let FunctionArguments::List(args) = &f.args else {
        return Err("SQL_VARIANT_PROPERTY requires two arguments".into());
    };
    if args.args.len() != 2
        || !args
            .args
            .iter()
            .all(|arg| matches!(arg, FunctionArg::Unnamed(FunctionArgExpr::Expr(_))))
    {
        return Err("SQL_VARIANT_PROPERTY requires two scalar arguments".into());
    }
    let mut check = f.clone();
    if let FunctionArguments::List(args) = &mut check.args {
        args.args.truncate(1);
    }
    crate::function_args::unary(&check, "SQL_VARIANT_PROPERTY")?;
    f.name = ObjectName::from(vec![Ident::new("__msduck_variant_property")]);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn properties_evaluate_volatile_arguments_once_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        db.execute_batch("CREATE SEQUENCE value_calls; CREATE SEQUENCE property_calls")
            .unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) WHERE (__msduck_variant_property(CASE WHEN nextval('value_calls')%2=0 THEN 1 ELSE NULL END,'precision')).__msduck_variant_integer=10",[],|r|r.get::<_,i64>(0)).unwrap(),3000);
        assert_eq!(
            db.query_row("SELECT currval('value_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
        assert_eq!(db.query_row("SELECT count(*) FROM range(6000) WHERE (__msduck_variant_property(1,CASE WHEN nextval('property_calls')%2=0 THEN 'precision' ELSE NULL END)).__msduck_variant_integer=10",[],|r|r.get::<_,i64>(0)).unwrap(),3000);
        assert_eq!(
            db.query_row("SELECT currval('property_calls')", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }
    #[test]
    fn catalog_state_survives_reopen_and_streams_across_chunks() {
        let path = std::env::temp_dir().join(format!(
            "msduck-variant-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for first in [true, false] {
            let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
            let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
            if first {
                assert!(session.batch_response("CREATE TABLE dbo.variant_ids(id BIGINT IDENTITY(9223372036854775807,-1)); INSERT INTO dbo.variant_ids DEFAULT VALUES",&Default::default(),false,None).1);
            }
            assert_eq!(session.db.query_row("SELECT last_value.__msduck_variant_integer FROM sys.identity_columns WHERE object_id=__msduck_object_id('variant_ids',NULL)",[],|r|r.get::<_,i64>(0)).unwrap(),i64::MAX);
            assert!(session.batch_response("SELECT seed_value,increment_value,last_value FROM sys.identity_columns CROSS JOIN range(6000) WHERE object_id=OBJECT_ID('variant_ids')",&Default::default(),false,None).1);
        }
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn integer_wire_vectors() {
        use duckdb::types::Value;
        for (value, expected) in [
            (Value::Null, vec![0, 0, 0, 0]),
            (Value::Boolean(false), vec![3, 0, 0, 0, 0x32, 0, 0]),
            (Value::Boolean(true), vec![3, 0, 0, 0, 0x32, 0, 1]),
            (
                Value::Text("int".into()),
                vec![
                    15, 0, 0, 0, 0xe7, 7, 9, 4, 0xd0, 0, 0x34, 0, 1, 105, 0, 110, 0, 116, 0,
                ],
            ),
            (Value::UTinyInt(255), vec![3, 0, 0, 0, 0x30, 0, 255]),
            (Value::SmallInt(-2), vec![4, 0, 0, 0, 0x34, 0, 254, 255]),
            (
                Value::Int(i32::MIN),
                vec![6, 0, 0, 0, 0x38, 0, 0, 0, 0, 128],
            ),
            (
                Value::BigInt(i64::MAX),
                vec![10, 0, 0, 0, 0x7f, 0, 255, 255, 255, 255, 255, 255, 255, 127],
            ),
        ] {
            let mut bytes = vec![];
            crate::engine::encode_value(&mut bytes, &crate::tds::Type::Variant, &value).unwrap();
            assert_eq!(bytes, expected);
        }
    }
}
