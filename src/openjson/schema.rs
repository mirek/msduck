//! Explicit-schema source rows and scalar/fragment column extraction.
use super::*;
use msduck_core::openjson::schema::{column, sources};
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
        *text = std::str::from_utf8(bytes)?.to_owned();
    }
    Ok(Some(texts))
}
struct Sources;
impl VScalar for Sources {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut result = output.list_vector();
        let mut all = Vec::new();
        for row in 0..input.len() {
            let offset = all.len();
            if let Some([source, path]) = texts(input, row)? {
                all.extend(sources(&source, &path)?);
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
        vec![ScalarFunctionSignature::exact(
            vec![Id::Varchar.into(), Id::Varchar.into()],
            Type::list(&Id::Varchar.into()),
        )]
    }
}
struct Column<const FRAGMENT: bool>;
impl<const FRAGMENT: bool> VScalar for Column<FRAGMENT> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut result = output.flat_vector();
        for row in 0..input.len() {
            if let Some([source, path]) = texts(input, row)?
                && let Some(value) = column(&source, &path, FRAGMENT)?
            {
                result.insert(row, value.as_str());
            } else {
                result.set_null(row);
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Varchar.into(), Id::Varchar.into()],
            Id::Varchar.into(),
        )]
    }
}
pub(super) fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Sources>("__msduck_openjson_sources")?;
    db.register_scalar_function::<Column<false>>("__msduck_openjson_scalar")?;
    db.register_scalar_function::<Column<true>>("__msduck_openjson_fragment")
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
            if let Some(expr) = binary::expression(Expr::CompoundIdentifier(vec![Ident::new("x"),Ident::new("r")]),Expr::Value(Value::SingleQuotedString(path.clone()).into()),&column.r#type)? {
                return Ok(SelectItem::ExprWithAlias {expr,alias:column.name.clone()});
            }
            let value = crate::engine::binary_function(
                if column.as_json {
                    "__msduck_openjson_fragment"
                } else {
                    "__msduck_openjson_scalar"
                },
                Expr::CompoundIdentifier(vec![Ident::new("x"), Ident::new("r")]),
                Expr::Value(Value::SingleQuotedString(path).into()),
            );
            let expr = Expr::Cast {
                kind: CastKind::Cast,
                expr: Box::new(value),
                data_type: declared_type(&column.r#type),
                format: None,
            };
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
    fn source_vectors_and_column_evaluation() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE source_calls; CREATE SEQUENCE column_calls")
            .unwrap();
        let n:i64=db.query_row("SELECT count(*) FROM (SELECT unnest(__msduck_openjson_sources(printf('[%d,null]',nextval('source_calls')),'$')) s FROM range(6000))",[],|r|r.get(0)).unwrap();
        assert_eq!(n, 12000);
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
