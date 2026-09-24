//! Conversion between owned scalar bindings and DuckDB values.
use anyhow::{Result, bail};
use duckdb::types::{TimeUnit as BackendUnit, Value as BackendValue};
use msduck_core::value::{Decimal, TimeUnit, Value};

pub(crate) fn to_backend(value: &Value) -> Result<BackendValue> {
    Ok(match value {
        Value::Null => BackendValue::Null,
        Value::Boolean(v) => BackendValue::Boolean(*v),
        Value::TinyInt(v) => BackendValue::TinyInt(*v),
        Value::UTinyInt(v) => BackendValue::UTinyInt(*v),
        Value::SmallInt(v) => BackendValue::SmallInt(*v),
        Value::Int(v) => BackendValue::Int(*v),
        Value::BigInt(v) => BackendValue::BigInt(*v),
        Value::Float(v) => BackendValue::Float(*v),
        Value::Double(v) => BackendValue::Double(*v),
        Value::Decimal(v) => BackendValue::Decimal(duckdb::types::Decimal::new(
            v.precision(),
            v.scale(),
            v.coefficient(),
        )?),
        Value::Timestamp(unit, v) => BackendValue::Timestamp(
            match unit {
                TimeUnit::Second => BackendUnit::Second,
                TimeUnit::Millisecond => BackendUnit::Millisecond,
                TimeUnit::Microsecond => BackendUnit::Microsecond,
                TimeUnit::Nanosecond => BackendUnit::Nanosecond,
            },
            *v,
        ),
        Value::Text(v) => BackendValue::Text(v.clone()),
        Value::Unicode(units) => {
            anyhow::ensure!(
                units.len() <= 8 * 1024 * 1024,
                "Unicode binding exceeds the configured limit"
            );
            BackendValue::Blob(units.iter().flat_map(|u| u.to_le_bytes()).collect())
        }
        Value::Blob(v) => BackendValue::Blob(v.clone()),
        Value::Date32(v) => BackendValue::Date32(*v),
    })
}

pub(crate) fn from_backend(value: BackendValue) -> Result<Value> {
    Ok(match value {
        BackendValue::Null => Value::Null,
        BackendValue::Boolean(v) => Value::Boolean(v),
        BackendValue::TinyInt(v) => Value::TinyInt(v),
        BackendValue::UTinyInt(v) => Value::UTinyInt(v),
        BackendValue::SmallInt(v) => Value::SmallInt(v),
        BackendValue::Int(v) => Value::Int(v),
        BackendValue::BigInt(v) => Value::BigInt(v),
        BackendValue::Float(v) => Value::Float(v),
        BackendValue::Double(v) => Value::Double(v),
        BackendValue::Decimal(v) => Value::Decimal(Decimal::new(v.width(), v.scale(), v.value())?),
        BackendValue::Timestamp(unit, v) => Value::Timestamp(
            match unit {
                BackendUnit::Second => TimeUnit::Second,
                BackendUnit::Millisecond => TimeUnit::Millisecond,
                BackendUnit::Microsecond => TimeUnit::Microsecond,
                BackendUnit::Nanosecond => TimeUnit::Nanosecond,
            },
            v,
        ),
        BackendValue::Text(v) => Value::Text(v),
        value @ BackendValue::Struct(_) => {
            Value::from_utf16(crate::unicode_carrier::units(&value)?)
        }
        BackendValue::Blob(v) => Value::Blob(v),
        BackendValue::Date32(v) => Value::Date32(v),
        // TIME is normalized to exact text by the engine before rebinding.
        _ => bail!("unsupported scalar parameter representation"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_bindings_keep_raw_units_distinct_from_binary_values() {
        let value = Value::Unicode(vec![0xd83e, 32]);
        let bytes = vec![0x3e, 0xd8, 32, 0];
        assert_eq!(
            to_backend(&value).unwrap(),
            BackendValue::Blob(bytes.clone())
        );
        assert_eq!(
            from_backend(BackendValue::Blob(bytes.clone())).unwrap(),
            Value::Blob(bytes.clone())
        );
        let carrier = BackendValue::Struct(
            vec![("__msduck_utf16le".into(), BackendValue::Blob(bytes))].into(),
        );
        assert_eq!(from_backend(carrier).unwrap(), value);
        let carrier = BackendValue::Struct(
            vec![("__msduck_utf16le".into(), BackendValue::Blob(vec![65, 0]))].into(),
        );
        assert_eq!(from_backend(carrier).unwrap(), Value::Text("A".into()));
        let malformed = BackendValue::Struct(
            vec![("__msduck_utf16le".into(), BackendValue::Blob(vec![65]))].into(),
        );
        assert!(from_backend(malformed).is_err());
    }

    #[test]
    fn bindings_preserve_scalar_representations() {
        let mut values = vec![
            Value::Null,
            Value::Boolean(true),
            Value::TinyInt(i8::MIN),
            Value::UTinyInt(u8::MAX),
            Value::SmallInt(i16::MIN),
            Value::Int(i32::MAX),
            Value::BigInt(i64::MIN),
            Value::Float(-0.0),
            Value::Double(f64::MIN_POSITIVE),
            Value::Text("雪🦆\0".into()),
            Value::Blob(vec![0, 128, 255]),
            Value::Date32(-719162),
        ];
        for precision in 1..=38 {
            for scale in [0, precision] {
                values.push(Value::Decimal(
                    Decimal::new(precision, scale, 1 - 10i128.pow(u32::from(precision))).unwrap(),
                ));
            }
        }
        for unit in [
            TimeUnit::Second,
            TimeUnit::Millisecond,
            TimeUnit::Microsecond,
            TimeUnit::Nanosecond,
        ] {
            for count in [i64::MIN, -1, 0, i64::MAX] {
                values.push(Value::Timestamp(unit, count));
            }
        }
        for value in values {
            let restored = from_backend(to_backend(&value).unwrap()).unwrap();
            assert_eq!(restored, value);
            if let Value::Float(v) = restored {
                assert_eq!(v.to_bits(), (-0.0f32).to_bits());
            }
        }
        assert!(from_backend(BackendValue::List(vec![])).is_err());
        assert!(from_backend(BackendValue::Time64(BackendUnit::Nanosecond, 1)).is_err());
    }

    #[test]
    fn exact_decimal_bindings_survive_database_execution() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        for (precision, scale, coefficient, expected) in [
            (
                38,
                0,
                10i128.pow(38) - 1,
                "99999999999999999999999999999999999999",
            ),
            (38, 38, -1, "-0.00000000000000000000000000000000000001"),
            (19, 4, i64::MIN as i128, "-922337203685477.5808"),
            (5, 2, 0, "0.00"),
        ] {
            let original = Value::Decimal(Decimal::new(precision, scale, coefficient).unwrap());
            let bound = to_backend(&original).unwrap();
            let returned: BackendValue = db
                .query_row("SELECT ?", [&bound], |row| row.get(0))
                .unwrap();
            let restored = from_backend(returned).unwrap();
            assert_eq!(restored, original);
            let Value::Decimal(decimal) = restored else {
                panic!("expected decimal")
            };
            assert_eq!(decimal.to_string(), expected);
        }
    }
}
