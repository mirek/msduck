//! Native casing with explicit SQL Server table selection and lossless UTF-16.
use crate::unicode_carrier::{CELL_LIMIT, CHUNK_LIMIT, kind, vector_units};
use duckdb::{
    core::{DataChunkHandle, Inserter},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::case_mapping::{Direction, Family};

struct Case<const UPPER: bool>;
impl<const UPPER: bool> VScalar for Case<UPPER> {
    type State = Family;
    fn invoke(
        family: &Family,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let structure = input.struct_vector(0);
        let data = structure.child(0, len);
        let direction = if UPPER {
            Direction::Upper
        } else {
            Direction::Lower
        };
        let mut remaining = CHUNK_LIMIT;
        let mut rows = Vec::with_capacity(len);
        for row in 0..len {
            let Some(mut units) = vector_units(&source, &data, row, len)? else {
                rows.push(None);
                continue;
            };
            let size = units
                .len()
                .checked_mul(2)
                .ok_or("Unicode casing length overflow")?;
            if size > CELL_LIMIT || size > remaining {
                return Err("Unicode casing output exceeds the configured limit".into());
            }
            remaining -= size;
            family.map_in_place(direction, &mut units);
            rows.push(Some(
                units
                    .into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            ));
        }
        // Validate and bound the whole chunk before publishing output values.
        let mut result = output.struct_vector();
        let mut data = result.child(0, len);
        for (row, value) in rows.iter().enumerate() {
            if let Some(value) = value {
                data.insert(row, value.as_slice());
            } else {
                result.set_null(row);
                data.set_null(row);
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(vec![kind()], kind())]
    }
}

/// Family selection is immutable registration state, never a nullable SQL value.
/// Callers resolve a supported collation before selecting this closed set.
pub fn function(family: Family, direction: Direction) -> &'static str {
    match (family, direction) {
        (Family::SqlLatin1, Direction::Lower) => "__msduck_lower_unicode_legacy",
        (Family::SqlLatin1, Direction::Upper) => "__msduck_upper_unicode_legacy",
        (Family::Latin1General100, Direction::Lower) => "__msduck_lower_unicode_100",
        (Family::Latin1General100, Direction::Upper) => "__msduck_upper_unicode_100",
    }
}
/// Consume only the scoped operand annotation emitted by the pure binder.
pub fn lower(expr: &mut sqlparser::ast::Expr) -> Result<(), String> {
    use sqlparser::ast::*;
    let Expr::Function(f) = expr else {
        return Ok(());
    };
    let name = f.name.to_string().to_ascii_uppercase();
    let direction = match name.as_str() {
        "LOWER" => Direction::Lower,
        "UPPER" => Direction::Upper,
        _ => return Ok(()),
    };
    let Some(source) = msduck_sql::function_args::unary(f, &name)? else {
        return Ok(());
    };
    let Expr::Collate {
        expr: cast,
        collation,
    } = source
    else {
        return Ok(());
    };
    let Expr::Cast {
        expr: marker,
        data_type,
        kind: CastKind::Cast,
        format: None,
    } = cast.as_ref()
    else {
        return Ok(());
    };
    let Expr::Function(marker) = marker.as_ref() else {
        return Ok(());
    };
    let marker_name = marker.name.to_string();
    if !matches!(
        marker_name.as_str(),
        "__msduck_case_input_legacy" | "__msduck_case_input_100"
    ) {
        return Ok(());
    }
    let family = Family::for_collation(&collation.to_string())
        .ok_or("unsupported bound casing collation")?;
    let expected = match family {
        Family::SqlLatin1 => "__msduck_case_input_legacy",
        Family::Latin1General100 => "__msduck_case_input_100",
    };
    let unicode_type = match data_type {
        DataType::Nvarchar(Some(CharacterLength::Max)) => true,
        DataType::Nvarchar(Some(CharacterLength::IntegerLength { length, unit: None })) => {
            *length <= 4000
        }
        _ => {
            matches!(msduck_sql::sql_type::declaration(data_type), Ok(msduck_core::types::Type::Character(t)) if t.family()==msduck_core::character::Family::Nchar)
        }
    };
    if marker_name != expected || !unicode_type {
        return Err("invalid bound Unicode casing annotation".into());
    }
    let original = msduck_sql::function_args::unary(marker, expected)?
        .ok_or("invalid bound Unicode casing operand")?;
    // The binder already selected the operand's collation. COLLATE does not
    // change its value and must not reach DuckDB as a collation on a carrier.
    fn operand(expr: Expr) -> Expr {
        match expr {
            Expr::Nested(value) | Expr::Collate { expr: value, .. } => operand(*value),
            value => value,
        }
    }
    *expr = msduck_sql::expr::unary_function(
        function(family, direction),
        msduck_sql::expr::unary_function("__msduck_carrier_input", operand(original.clone())),
    );
    Ok(())
}

pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    for family in [Family::SqlLatin1, Family::Latin1General100] {
        db.register_scalar_function_with_state::<Case<false>>(
            function(family, Direction::Lower),
            &family,
        )?;
        db.register_scalar_function_with_state::<Case<true>>(
            function(family, Direction::Upper),
            &family,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> duckdb::Connection {
        let db = duckdb::Connection::open_in_memory().unwrap();
        crate::unicode_carrier::register(&db).unwrap();
        register(&db).unwrap();
        db
    }
    #[test]
    fn native_casing_preserves_units_and_selects_reference_family() {
        let db = database();
        let raw: Vec<u8>=db.query_row("SELECT (__msduck_lower_unicode_legacy(__msduck_unicode_from_le(from_hex('41003ED84200')))).__msduck_utf16le",[],|r|r.get(0)).unwrap();
        assert_eq!(raw, [0x61, 0, 0x3e, 0xd8, 0x62, 0]);
        for (family, expected) in [
            (Family::SqlLatin1, [0x80, 1]),
            (Family::Latin1General100, [0x43, 2]),
        ] {
            let name = function(family, Direction::Upper);
            let raw: Vec<u8> = db
                .query_row(
                    &format!("SELECT ({name}(__msduck_pack_unicode('ƀ'))).__msduck_utf16le"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(raw, expected);
        }
        let null: duckdb::types::Value = db
            .query_row(
                "SELECT __msduck_lower_unicode_100(__msduck_pack_unicode(NULL))",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(null, duckdb::types::Value::Null);
        let empty: Vec<u8>=db.query_row("SELECT (__msduck_upper_unicode_legacy(__msduck_pack_unicode(''))).__msduck_utf16le",[],|r|r.get(0)).unwrap();
        assert!(empty.is_empty());
    }
    #[test]
    fn native_casing_rejects_payloads_and_resource_sizes() {
        let db = database();
        for (sql, expected) in [
            (
                "SELECT __msduck_upper_unicode_legacy(struct_pack(__msduck_utf16le := from_hex('01')))",
                "odd byte length",
            ),
            (
                "SELECT __msduck_upper_unicode_legacy(struct_pack(__msduck_utf16le := NULL::BLOB))",
                "NULL payload",
            ),
            (
                "SELECT __msduck_upper_unicode_legacy(struct_pack(__msduck_utf16le := CAST(repeat('a',16777218) AS BLOB)))",
                "Unicode input exceeds the configured cell limit",
            ),
            (
                "SELECT count(__msduck_upper_unicode_legacy(struct_pack(__msduck_utf16le := CAST(repeat('a',80000)||repeat('b',CAST(i%2 AS INTEGER)*2) AS BLOB)))) FROM range(1000) t(i)",
                "Unicode casing output exceeds the configured limit",
            ),
        ] {
            let error = db
                .query_row(sql, [], |r| r.get::<_, duckdb::types::Value>(0))
                .expect_err(sql);
            assert!(error.to_string().contains(expected), "{sql}: {error}");
        }
        assert!(Family::for_collation("unsupported").is_none());
    }
    #[test]
    fn native_casing_evaluates_input_once_across_filtered_chunks() {
        for family in [Family::SqlLatin1, Family::Latin1General100] {
            let db = database();
            let lower = function(family, Direction::Lower);
            let upper = function(family, Direction::Upper);
            db.execute_batch(&format!("CREATE SEQUENCE case_source; CREATE TABLE case_values AS SELECT i,{lower}(__msduck_pack_unicode(CASE WHEN i%17=0 THEN NULL ELSE 'A'||CAST(nextval('case_source') AS VARCHAR)||'Z' END)) AS u FROM range(6000) t(i)")).unwrap();
            let count: i64 = db
                .query_row("SELECT count(u) FROM case_values", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 5647);
            let calls: i64 = db
                .query_row("SELECT currval('case_source')", [], |r| r.get(0))
                .unwrap();
            assert_eq!(calls, 5647);
            let count: i64 = db
                .query_row(
                    &format!("SELECT count({upper}(u)) FROM case_values WHERE i%3=0"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1882);
        }
    }
}
