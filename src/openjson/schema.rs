//! Explicit-schema source rows and scalar/fragment column extraction.
use super::*;
use msduck_core::openjson::schema::{column, sources_with_limit};
const FORBIDDEN_TYPE: &str = "TEXT, NTEXT, SQL_VARIANT and IMAGE types cannot be used as column types in OPENJSON function with explicit schema. These types are not supported in WITH clause.";
const AS_JSON_TYPE: &str =
    "AS JSON option can be specified only for column of nvarchar(max) type in WITH clause.";
pub(super) fn diagnostic(message: &str) -> Option<SqlError> {
    match message {
        FORBIDDEN_TYPE => Some(SqlError::new(13614, 1, FORBIDDEN_TYPE)),
        AS_JSON_TYPE => Some(SqlError::new(13618, 1, AS_JSON_TYPE)),
        _ => None,
    }
}
pub(super) fn texts(
    input: &DataChunkHandle,
    row: usize,
) -> Result<Option<[String; 2]>, Box<dyn std::error::Error>> {
    let mut texts = [String::new(), String::new()];
    for (i, text) in texts.iter_mut().enumerate() {
        let vector = input.flat_vector(i);
        if vector.row_is_null(row as u64) {
            return Ok(None);
        };
        // Both arguments have exact VARCHAR signatures; keep copied inline bytes alive.
        let mut value =
            unsafe { vector.as_slice_with_len::<duckdb::ffi::duckdb_string_t>(input.len())[row] };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                duckdb::ffi::duckdb_string_t_data(&mut value).cast::<u8>(),
                duckdb::ffi::duckdb_string_t_length(value) as usize,
            )
        };
        *text = checked_varchar(bytes, crate::unicode_carrier::CELL_LIMIT)?.to_owned();
    }
    Ok(Some(texts))
}
fn varchar_sources(
    source: &str,
    path: &str,
    remaining: &mut usize,
) -> Result<Vec<String>, &'static str> {
    let rows = sources_with_limit(source, path, remaining)?;
    // The list vector copies these bytes while the decoded rows remain alive.
    let mut output_bytes = 0usize;
    for value in &rows {
        if value.len() > crate::unicode_carrier::CELL_LIMIT {
            return Err(OUTPUT_LIMIT);
        }
        output_bytes = output_bytes.checked_add(value.len()).ok_or(OUTPUT_LIMIT)?;
    }
    *remaining = remaining.checked_sub(output_bytes).ok_or(OUTPUT_LIMIT)?;
    Ok(rows)
}
fn varchar_column(
    source: &str,
    path: &str,
    fragment: bool,
    remaining: &mut usize,
) -> Result<Option<String>, &'static str> {
    let Some(value) = column(source, path, fragment)? else {
        return Ok(None);
    };
    if value.len() > crate::unicode_carrier::CELL_LIMIT {
        return Err(OUTPUT_LIMIT);
    }
    // DuckDB retains one copy. This String is per-row scratch and is dropped
    // immediately after insertion; the cell limit bounds that transient copy.
    *remaining = remaining.checked_sub(value.len()).ok_or(OUTPUT_LIMIT)?;
    Ok(Some(value))
}
struct Sources;
impl VScalar for Sources {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if crate::unicode_carrier::is_logical(&input.flat_vector(0).logical_type()) {
            return unicode_sources(input, output);
        }
        let mut result = output.list_vector();
        let mut all = Vec::new();
        let mut remaining = crate::unicode_carrier::CHUNK_LIMIT;
        for row in 0..input.len() {
            let offset = all.len();
            if let Some([source, path]) = texts(input, row)? {
                all.extend(varchar_sources(&source, &path, &mut remaining)?);
            }
            result.set_entry(row, offset, all.len() - offset);
        }
        let child = result.child(all.len());
        result.try_set_len(all.len())?;
        for (i, value) in all.iter().enumerate() {
            child.insert(i, value.as_str());
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        [false, true]
            .into_iter()
            .map(|unicode| {
                let text = || {
                    if unicode {
                        crate::unicode_carrier::kind()
                    } else {
                        Id::Varchar.into()
                    }
                };
                ScalarFunctionSignature::exact(vec![text(), text()], Type::list(&text()))
            })
            .collect()
    }
}
struct Column<const FRAGMENT: bool, const TEXT: bool = false>;
impl<const FRAGMENT: bool, const TEXT: bool> VScalar for Column<FRAGMENT, TEXT> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if crate::unicode_carrier::is_logical(&input.flat_vector(0).logical_type()) {
            if TEXT {
                let mut result = output.flat_vector();
                let mut remaining = crate::unicode_carrier::CHUNK_LIMIT;
                for row in 0..input.len() {
                    let value = if let Some([source, path]) = unicode_texts(input, row)? {
                        msduck_core::openjson::schema::column_utf16(&source, &path, FRAGMENT)?
                    } else {
                        None
                    };
                    if let Some(value) = value {
                        // Non-character converters consume text, never DuckDB's
                        // printable STRUCT representation. Reject invalid UTF16;
                        // replacement characters would change conversion input.
                        let text = String::from_utf16(&value)?;
                        remaining = remaining
                            .checked_sub(text.len())
                            .ok_or("OPENJSON result exceeds the configured output limit")?;
                        result.insert(row, text.as_str());
                    } else {
                        result.set_null(row);
                    }
                }
                return Ok(());
            }
            return unicode_column::<FRAGMENT>(input, output);
        }
        let mut result = output.flat_vector();
        let mut remaining = crate::unicode_carrier::CHUNK_LIMIT;
        for row in 0..input.len() {
            if let Some([source, path]) = texts(input, row)?
                && let Some(value) = varchar_column(&source, &path, FRAGMENT, &mut remaining)?
            {
                result.insert(row, value.as_str());
            } else {
                result.set_null(row);
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        [false, true]
            .into_iter()
            .map(|unicode| {
                let text = || {
                    if unicode {
                        crate::unicode_carrier::kind()
                    } else {
                        Id::Varchar.into()
                    }
                };
                ScalarFunctionSignature::exact(
                    vec![text(), text()],
                    if TEXT { Id::Varchar.into() } else { text() },
                )
            })
            .collect()
    }
}
pub(super) fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Sources>("__msduck_openjson_sources")?;
    db.register_scalar_function::<Column<false>>("__msduck_openjson_scalar")?;
    db.register_scalar_function::<Column<false, true>>("__msduck_openjson_scalar_text")?;
    db.register_scalar_function::<Column<true>>("__msduck_openjson_fragment")
}

fn unicode_sources(
    input: &DataChunkHandle,
    output: &mut dyn WritableVector,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut result = output.list_vector();
    let mut all = Vec::new();
    let mut remaining = crate::unicode_carrier::CHUNK_LIMIT;
    for row in 0..input.len() {
        let offset = all.len();
        if let Some([source, path]) = unicode_texts(input, row)? {
            for value in
                msduck_core::openjson::schema::sources_utf16_with_limit(&source, &path, remaining)?
            {
                remaining = remaining
                    .checked_sub(std::mem::size_of::<Vec<u8>>())
                    .ok_or("OPENJSON result exceeds the configured output limit")?;
                all.push(encode_units(&value, &mut remaining)?);
            }
        }
        result.set_entry(row, offset, all.len() - offset);
    }
    let child = result.struct_child(all.len());
    result.try_set_len(all.len())?;
    let bytes = child.child(0, all.len());
    for (row, value) in all.iter().enumerate() {
        bytes.insert(row, value.as_slice());
    }
    Ok(())
}

fn unicode_column<const FRAGMENT: bool>(
    input: &DataChunkHandle,
    output: &mut dyn WritableVector,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut all = Vec::with_capacity(input.len());
    let mut remaining = crate::unicode_carrier::CHUNK_LIMIT;
    for row in 0..input.len() {
        let value = if let Some([source, path]) = unicode_texts(input, row)? {
            msduck_core::openjson::schema::column_utf16(&source, &path, FRAGMENT)?
                .as_deref()
                .map(|value| encode_units(value, &mut remaining))
                .transpose()?
        } else {
            None
        };
        all.push(value);
    }
    let mut result = output.struct_vector();
    let mut bytes = result.child(0, input.len());
    for (row, value) in all.iter().enumerate() {
        if let Some(value) = value {
            bytes.insert(row, value.as_slice());
        } else {
            result.set_null(row);
            bytes.set_null(row);
        }
    }
    Ok(())
}
/// WITH is a column declaration, so omitted lengths mean 1, not CAST's 30.
pub use msduck_sql::expression_metadata::openjson::declared_type;
pub(super) fn projection(columns: &[OpenJsonTableColumn]) -> Result<Vec<SelectItem>, String> {
    columns
        .iter()
        .map(|column| {
            if column.as_json && column.r#type != DataType::Nvarchar(Some(CharacterLength::Max)) {
                return Err(AS_JSON_TYPE.into());
            }
            if matches!(column.r#type, DataType::Text)
                || matches!(&column.r#type, DataType::Custom(name, _) if ["ntext","image","sql_variant"].iter().any(|kind| name.to_string().eq_ignore_ascii_case(kind))) {
                return Err(FORBIDDEN_TYPE.into());
            }
            let path = column.path.clone().unwrap_or_else(|| {
                format!(
                    "$.{}",
                    serde_json::to_string(&column.name.value)
                        .expect("identifier string serializes")
                )
            });
            if let Some(expr) = binary::expression(Expr::CompoundIdentifier(vec![Ident::new("x"),Ident::new("r")]),crate::engine::unary_function("__msduck_carrier_input", Expr::Value(Value::SingleQuotedString(path.clone()).into())),&column.r#type)? {
                return Ok(SelectItem::ExprWithAlias {expr,alias:column.name.clone()});
            }
            let kind = declared_type(&column.r#type);
            let character = matches!(msduck_sql::sql_type::declaration(&kind), Ok(msduck_core::types::Type::Character(_)));
            let value = crate::engine::binary_function(
                if column.as_json {
                    "__msduck_openjson_fragment"
                } else if !character {
                    "__msduck_openjson_scalar_text"
                } else {
                    "__msduck_openjson_scalar"
                },
                Expr::CompoundIdentifier(vec![Ident::new("x"), Ident::new("r")]),
                crate::engine::unary_function("__msduck_carrier_input", Expr::Value(Value::SingleQuotedString(path).into())),
            );
            let expr = if let Ok(msduck_core::types::Type::Character(target)) = msduck_sql::sql_type::declaration(&kind) {
                use msduck_core::character::{Family, Length};
                crate::engine::binary_function(
                    match target.family() {
                        Family::Nvarchar => "__msduck_cast_carrier_nvarchar",
                        Family::Nchar => "__msduck_cast_carrier_nchar",
                        Family::Varchar => "__msduck_cast_carrier_varchar",
                        Family::Char => "__msduck_cast_carrier_char",
                    },
                    value,
                    msduck_sql::expr::number(match target.length() {
                        Length::Max => -1,
                        Length::Bounded(n) => i32::from(n),
                    }),
                )
            } else { Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(value),
                data_type: kind,
                format: None,
            }};
            Ok(SelectItem::ExprWithAlias {
                expr,
                alias: column.name.clone(),
            })
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn varchar_schema_budgets_source_rows_and_selected_columns() {
        let source = r#"["x"]"#;
        let size = std::mem::size_of::<String>() + 3 + 3;
        let mut remaining = size;
        assert_eq!(
            varchar_sources(source, "$", &mut remaining).unwrap(),
            vec![r#""x""#]
        );
        assert_eq!(remaining, 0);
        assert_eq!(
            varchar_sources(source, "$", &mut remaining),
            Err(OUTPUT_LIMIT)
        );
        assert_eq!(
            varchar_sources(source, "$", &mut (size - 1)),
            Err(OUTPUT_LIMIT)
        );
        assert_eq!(
            varchar_sources("[]", "$", &mut 0).unwrap(),
            Vec::<String>::new()
        );
        let mut remaining = 3;
        for _ in 0..3 {
            assert_eq!(
                varchar_column(r#""x""#, "$", false, &mut remaining).unwrap(),
                Some("x".into())
            );
        }
        assert_eq!(remaining, 0);
        assert_eq!(
            varchar_column(r#""x""#, "$", false, &mut remaining),
            Err(OUTPUT_LIMIT)
        );
        assert_eq!(
            varchar_column("null", "$", false, &mut remaining).unwrap(),
            None
        );
        assert_eq!(
            varchar_column("{}", "strict $.missing", false, &mut remaining),
            Err("OPENJSON: missing column path")
        );
    }
    #[test]
    fn source_vectors_and_column_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE source_calls; CREATE SEQUENCE column_calls")
            .unwrap();
        let n:i64=db.query_row("SELECT count(*) FROM (SELECT unnest(__msduck_openjson_sources(printf('[%d,null]',nextval('source_calls')),'$')) s FROM range(6000))",[],|r|r.get(0)).unwrap();
        assert_eq!(n, 12000);
        let n: i64 = db.query_row("SELECT count(*) FROM (SELECT unnest(__msduck_openjson_sources(CASE WHEN i%17=0 THEN NULL ELSE '[1]' END, CASE WHEN i%19=0 THEN NULL ELSE '$' END)) FROM range(6000) t(i))", [], |r| r.get(0)).unwrap();
        assert_eq!(
            n,
            (0..6000).filter(|i| i % 17 != 0 && i % 19 != 0).count() as i64
        );
        let n:i64=db.query_row("SELECT count(*) FROM (SELECT __msduck_openjson_scalar(printf('%d',nextval('column_calls')),CASE WHEN i%17=0 THEN NULL ELSE '$' END) v FROM range(6000) t(i)) WHERE v IS NOT NULL",[],|r|r.get(0)).unwrap();
        assert_eq!(n, 6000 - (5999 / 17 + 1));
        for name in ["source_calls", "column_calls"] {
            let n: i64 = db
                .query_row(&format!("SELECT currval('{name}')"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 6000);
        }
    }
}
