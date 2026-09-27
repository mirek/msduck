//! SQL Server JSON syntax validation, adapted from upstream json.ts lexical rules.
//! An explicit grammar stack avoids recursion and numeric conversion limits.
use duckdb::{Connection, core::LogicalTypeId as Id, ffi::*};
use sqlparser::ast::*;
use std::{
    ffi::{CStr, CString},
    panic::{AssertUnwindSafe, catch_unwind},
};

use msduck_core::json::{isjson, isjson_utf16};
use msduck_core::types::Type as SqlType;

pub const DEPTH_ERROR: &CStr =
    c"JSON text/path that has more than 128 nesting levels cannot be parsed.";

pub fn lower(expr: &mut Expr) -> Result<(), String> {
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    if !f.name.to_string().eq_ignore_ascii_case("ISJSON") {
        return Ok(());
    }
    let FunctionArguments::List(args) = &f.args else {
        return Err("ISJSON requires one or two scalar arguments".into());
    };
    if !(1..=2).contains(&args.args.len())
        || args.duplicate_treatment.is_some()
        || !args.clauses.is_empty()
        || !matches!(f.parameters, FunctionArguments::None)
        || f.over.is_some()
        || f.filter.is_some()
        || f.null_treatment.is_some()
        || !f.within_group.is_empty()
    {
        return Err("unsupported ISJSON arguments or modifiers".into());
    }
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(value)) = &args.args[0] else {
        return Err("ISJSON requires a scalar input".into());
    };
    let mode = if let Some(arg) = args.args.get(1) {
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Identifier(kind))) = arg else {
            return Err("ISJSON requires a JSON type keyword".into());
        };
        match kind.value.to_ascii_uppercase().as_str() {
            "VALUE" => 1,
            "ARRAY" => 2,
            "OBJECT" => 3,
            "SCALAR" => 4,
            _ => return Err("invalid ISJSON type constraint".into()),
        }
    } else {
        0
    };
    let mut declaration = value;
    while let Expr::Nested(inner) = declaration {
        declaration = inner;
    }
    let declared = match declaration {
        Expr::Cast { data_type, .. } => Some(data_type),
        Expr::Convert {
            data_type: Some(data_type),
            ..
        } => Some(data_type),
        _ => None,
    };
    if let Some(kind) = declared.and_then(|kind| msduck_sql::sql_type::declaration(kind).ok()) {
        let rejected = match kind {
            SqlType::Text => Some(("__msduck_isjson_reject_text", false)),
            SqlType::Xml => Some(("__msduck_isjson_reject_xml", false)),
            SqlType::Money => Some(("__msduck_isjson_reject_money", true)),
            SqlType::SmallMoney => Some(("__msduck_isjson_reject_smallmoney", true)),
            SqlType::DateTime => Some(("__msduck_isjson_reject_datetime", true)),
            SqlType::SmallDateTime => Some(("__msduck_isjson_reject_smalldatetime", true)),
            _ => None,
        };
        if let Some((name, bind_operand)) = rejected {
            // SQL Server resolves source names before rejecting these types.
            // Keep supported CASTs as children for binding, while the bind
            // callback prevents their runtime conversion from executing.
            // DuckDB cannot bind XML or TEXT here; those retain the earlier
            // placeholder path until their declarations are modeled.
            *expr = crate::engine::unary_function(
                name,
                if bind_operand {
                    value.clone()
                } else {
                    Expr::Value(Value::Number("0".into(), false).into())
                },
            );
            return Ok(());
        }
    }
    // Preserve the source type until the native bind callback checks it.
    // The carrier-input macro casts numerics and binary to text, which would
    // turn SQL Server's compile-time 8116 into an ordinary JSON result.
    *expr = crate::engine::unary_function(&format!("__msduck_isjson_{mode}"), value.clone());
    Ok(())
}

unsafe fn carrier(kind: duckdb_logical_type) -> bool {
    unsafe {
        if duckdb_get_type_id(kind) != Id::Struct as u32
            || duckdb_struct_type_child_count(kind) != 1
        {
            return false;
        }
        let name = duckdb_struct_type_child_name(kind, 0);
        if name.is_null() {
            return false;
        }
        let named = CStr::from_ptr(name).to_bytes() == b"__msduck_utf16le";
        duckdb_free(name.cast());
        let mut child = duckdb_struct_type_child_type(kind, 0);
        let blob = duckdb_get_type_id(child) == Id::Blob as u32;
        duckdb_destroy_logical_type(&mut child);
        named && blob
    }
}

unsafe fn type_name(kind: duckdb_logical_type) -> Option<&'static str> {
    Some(match Id::from(unsafe { duckdb_get_type_id(kind) }) {
        Id::Boolean => "bit",
        Id::Tinyint | Id::UTinyint => "tinyint",
        Id::Smallint | Id::USmallint => "smallint",
        Id::Integer | Id::UInteger | Id::IntegerLiteral => "int",
        Id::Bigint | Id::UBigint | Id::Hugeint | Id::UHugeint => "bigint",
        Id::Decimal => "decimal",
        Id::Float => "real",
        Id::Double => "float",
        Id::Date => "date",
        Id::Time | Id::TimeNs => "time",
        Id::Timestamp | Id::TimestampS | Id::TimestampMs | Id::TimestampNs => "datetime2",
        Id::TimestampTZ => "datetimeoffset",
        Id::Uuid => "uniqueidentifier",
        Id::Blob => "varbinary",
        Id::Struct => unsafe {
            if duckdb_struct_type_child_count(kind) == 0 {
                return None;
            }
            let child = duckdb_struct_type_child_name(kind, 0);
            if child.is_null() {
                return None;
            }
            let name = CStr::from_ptr(child).to_bytes();
            let result = if name.starts_with(b"__msduck_datetime2_") {
                Some("datetime2")
            } else if name.starts_with(b"__msduck_datetimeoffset_") {
                Some("datetimeoffset")
            } else {
                None
            };
            duckdb_free(child.cast());
            return result;
        },
        _ => return None,
    })
}

unsafe extern "C" fn bind(info: duckdb_bind_info) {
    let outcome = catch_unwind(AssertUnwindSafe(|| unsafe {
        let mut expression = duckdb_scalar_function_bind_get_argument(info, 0);
        let mut kind = duckdb_expression_return_type(expression);
        let id = duckdb_get_type_id(kind);
        let accepted =
            matches!(Id::from(id), Id::Varchar | Id::StringLiteral | Id::SqlNull) || carrier(kind);
        let name = (!accepted).then(|| type_name(kind));
        duckdb_destroy_logical_type(&mut kind);
        duckdb_destroy_expression(&mut expression);
        match name {
            Some(Some(name)) => {
                let message = CString::new(format!(
                    "Argument data type {} is invalid for argument 1 of isjson function.",
                    name
                ))
                .expect("static message has no NUL");
                duckdb_scalar_function_bind_set_error(info, message.as_ptr());
            }
            Some(None) => duckdb_scalar_function_bind_set_error(
                info,
                c"ISJSON input has an unsupported native type.".as_ptr(),
            ),
            None => {}
        }
    }));
    if outcome.is_err() {
        unsafe { duckdb_scalar_function_bind_set_error(info, c"ISJSON binding failed.".as_ptr()) }
    }
}

unsafe extern "C" fn reject_text(info: duckdb_bind_info) {
    unsafe {
        duckdb_scalar_function_bind_set_error(
            info,
            c"Argument data type text is invalid for argument 1 of isjson function.".as_ptr(),
        );
    }
}

unsafe extern "C" fn reject_xml(info: duckdb_bind_info) {
    unsafe {
        duckdb_scalar_function_bind_set_error(
            info,
            c"Argument data type xml is invalid for argument 1 of isjson function.".as_ptr(),
        );
    }
}

unsafe extern "C" fn reject_declared<const KIND: u8>(info: duckdb_bind_info) {
    let message = match KIND {
        0 => c"Argument data type money is invalid for argument 1 of isjson function.",
        1 => c"Argument data type smallmoney is invalid for argument 1 of isjson function.",
        2 => c"Argument data type datetime is invalid for argument 1 of isjson function.",
        3 => c"Argument data type smalldatetime is invalid for argument 1 of isjson function.",
        _ => c"ISJSON has an unsupported declared type.",
    };
    unsafe { duckdb_scalar_function_bind_set_error(info, message.as_ptr()) }
}

unsafe fn present(vector: duckdb_vector, row: idx_t) -> bool {
    unsafe {
        let validity = duckdb_vector_get_validity(vector);
        validity.is_null() || duckdb_validity_row_is_valid(validity, row)
    }
}

unsafe fn string_bytes(vector: duckdb_vector, row: usize) -> Result<Vec<u8>, &'static CStr> {
    unsafe {
        let mut value = duckdb_vector_get_data(vector)
            .cast::<duckdb_string_t>()
            .add(row)
            .read_unaligned();
        let size = duckdb_string_t_length(value) as usize;
        if size > crate::unicode_carrier::CELL_LIMIT {
            return Err(c"ISJSON input exceeds the configured cell limit.");
        }
        if size == 0 {
            return Ok(Vec::new());
        }
        Ok(std::slice::from_raw_parts(duckdb_string_t_data(&mut value).cast(), size).to_vec())
    }
}

unsafe extern "C" fn invoke<const MODE: u8>(
    info: duckdb_function_info,
    input: duckdb_data_chunk,
    output: duckdb_vector,
) {
    let outcome = catch_unwind(AssertUnwindSafe(|| unsafe {
        let len = duckdb_data_chunk_get_size(input) as usize;
        let source = duckdb_data_chunk_get_vector(input, 0);
        let mut kind = duckdb_vector_get_column_type(source);
        let id = duckdb_get_type_id(kind);
        let unicode = carrier(kind);
        duckdb_destroy_logical_type(&mut kind);
        if !matches!(Id::from(id), Id::Varchar | Id::StringLiteral | Id::SqlNull) && !unicode {
            return Err(c"ISJSON received a noncharacter input after binding.");
        }
        let child = unicode.then(|| duckdb_struct_vector_get_child(source, 0));
        duckdb_vector_ensure_validity_writable(output);
        let validity = duckdb_vector_get_validity(output);
        let destination = duckdb_vector_get_data(output).cast::<i32>();
        let mut remaining = crate::unicode_carrier::CHUNK_LIMIT;
        for row in 0..len {
            if id == Id::SqlNull as u32 || !present(source, row as idx_t) {
                duckdb_validity_set_row_validity(validity, row as idx_t, false);
                continue;
            }
            let value = if let Some(child) = child {
                if !present(child, row as idx_t) {
                    return Err(c"invalid Unicode carrier: NULL payload");
                }
                let bytes = string_bytes(child, row)?;
                if bytes.len() % 2 != 0 || bytes.len() > remaining {
                    return Err(c"invalid Unicode carrier byte length or chunk limit");
                }
                remaining -= bytes.len();
                let units = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>();
                i32::from(isjson_utf16(&units, MODE).map_err(|_| DEPTH_ERROR)?)
            } else {
                let bytes = string_bytes(source, row)?;
                if bytes.len() > remaining {
                    return Err(c"ISJSON input exceeds the configured chunk limit.");
                }
                remaining -= bytes.len();
                i32::from(isjson(&bytes, MODE).map_err(|_| DEPTH_ERROR)?)
            };
            destination.add(row).write_unaligned(value);
            duckdb_validity_set_row_validity(validity, row as idx_t, true);
        }
        Ok(())
    }));
    let error = match outcome {
        Ok(Ok(())) => return,
        Ok(Err(error)) => error,
        Err(_) => c"ISJSON execution failed.",
    };
    unsafe { duckdb_scalar_function_set_error(info, error.as_ptr()) }
}

pub fn register(db: &Connection) -> duckdb::Result<()> {
    unsafe fn one<const MODE: u8>(
        db: &Connection,
        name: &CStr,
        bind_callback: unsafe extern "C" fn(duckdb_bind_info),
    ) -> duckdb::Result<()> {
        unsafe {
            let mut function = duckdb_create_scalar_function();
            let mut any = duckdb_create_logical_type(Id::Any as u32);
            let mut integer = duckdb_create_logical_type(Id::Integer as u32);
            duckdb_scalar_function_set_name(function, name.as_ptr());
            duckdb_scalar_function_add_parameter(function, any);
            duckdb_scalar_function_set_return_type(function, integer);
            duckdb_scalar_function_set_special_handling(function);
            duckdb_scalar_function_set_bind(function, Some(bind_callback));
            duckdb_scalar_function_set_function(function, Some(invoke::<MODE>));
            let result = db.register_scalar_function_raw(function);
            duckdb_destroy_scalar_function(&mut function);
            duckdb_destroy_logical_type(&mut any);
            duckdb_destroy_logical_type(&mut integer);
            result
        }
    }
    unsafe {
        one::<0>(db, c"__msduck_isjson_0", bind)?;
        one::<1>(db, c"__msduck_isjson_1", bind)?;
        one::<2>(db, c"__msduck_isjson_2", bind)?;
        one::<3>(db, c"__msduck_isjson_3", bind)?;
        one::<4>(db, c"__msduck_isjson_4", bind)?;
        one::<0>(db, c"__msduck_isjson_reject_text", reject_text)?;
        one::<0>(db, c"__msduck_isjson_reject_xml", reject_xml)?;
        one::<0>(db, c"__msduck_isjson_reject_money", reject_declared::<0>)?;
        one::<0>(
            db,
            c"__msduck_isjson_reject_smallmoney",
            reject_declared::<1>,
        )?;
        one::<0>(db, c"__msduck_isjson_reject_datetime", reject_declared::<2>)?;
        one::<0>(
            db,
            c"__msduck_isjson_reject_smalldatetime",
            reject_declared::<3>,
        )?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_error_retains_native_text_for_sql_diagnostic() {
        let db = Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let within_limit = "[".repeat(128) + "0" + &"]".repeat(128);
        let value: i32 = db
            .query_row(
                &format!("SELECT __msduck_isjson_0('{within_limit}')"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, 1);
        let deep = "[".repeat(129) + "0" + &"]".repeat(129);
        let error = db
            .query_row(&format!("SELECT __msduck_isjson_0('{deep}')"), [], |row| {
                row.get::<_, i32>(0)
            })
            .unwrap_err();
        assert_eq!(
            crate::json_extract::diagnostic(&error.to_string()),
            Some(msduck_core::diagnostic::SqlError::new(
                13606,
                1,
                DEPTH_ERROR.to_str().unwrap(),
            ))
        );
    }

    #[test]
    fn binding_rejects_noncharacter_inputs_including_typed_nulls() {
        let db = Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        for (expression, kind) in [
            ("1", "int"),
            ("CAST(NULL AS INTEGER)", "int"),
            ("CAST(1 AS BOOLEAN)", "bit"),
            ("CAST(1.25 AS DECIMAL(5,2))", "decimal"),
            ("from_hex('7B7D')", "varbinary"),
        ] {
            let error = db
                .prepare(&format!("SELECT __msduck_isjson_0({expression})"))
                .err()
                .unwrap();
            assert!(
                error.to_string().contains(&format!(
                    "Argument data type {kind} is invalid for argument 1 of isjson function."
                )),
                "{expression}: {error}"
            );
        }
        let valid: i32 = db
            .query_row("SELECT __msduck_isjson_0('{}')", [], |row| row.get(0))
            .unwrap();
        assert_eq!(valid, 1);
        let null: Option<i32> = db
            .query_row(
                "SELECT __msduck_isjson_0(CAST(NULL AS VARCHAR))",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(null, None);
    }

    #[test]
    fn chunks_nulls_and_single_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let wrong:i64=db.query_row("SELECT count(*) FROM range(6000) r(i) WHERE __msduck_isjson_0(CASE i%4 WHEN 0 THEN NULL WHEN 1 THEN '{\"a\":1}' WHEN 2 THEN '[1,]' ELSE '42' END) IS DISTINCT FROM CASE i%4 WHEN 0 THEN NULL WHEN 1 THEN 1 ELSE 0 END",[],|r|r.get(0)).unwrap();
        assert_eq!(wrong, 0);
        db.execute_batch("CREATE SEQUENCE json_calls").unwrap();
        let count:i64=db.query_row("SELECT count(*) FROM range(6000) WHERE __msduck_isjson_0(printf('[%d]',nextval('json_calls')))=1",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 6000);
        assert_eq!(
            db.query_row("SELECT currval('json_calls')", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6000
        );
    }

    #[test]
    fn ansi_rows_share_the_chunk_budget() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::scalar::register(&db).unwrap();
        let error = db
            .query_row(
                "SELECT sum(__msduck_isjson_0(repeat(' ', 1048576 + i % 2))) FROM range(70) r(i)",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_err();
        assert!(error.to_string().contains("configured chunk limit"));
    }
}
