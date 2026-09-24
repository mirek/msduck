//! Native vectors adapt exact currency coefficients to deterministic formatting.
use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::character::{CastInput, CharacterType, Family, Length};

struct Format<const TRY: bool>;
impl<const TRY: bool> VScalar for Format<TRY> {
    type State = ();
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let styles = input.flat_vector(1);
        let widths = input.flat_vector(2);
        let families = input.flat_vector(3);
        let mut result = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64)
                || styles.row_is_null(row as u64)
                || widths.row_is_null(row as u64)
                || families.row_is_null(row as u64)
            {
                result.set_null(row);
                continue;
            }
            // Exact DECIMAL(19,4)/INTEGER signatures and flattened chunks bound
            // these physical reads; decimal precision 19 uses HUGEINT storage.
            let raw = unsafe { source.as_slice_with_len::<duckdb::ffi::duckdb_hugeint>(len)[row] };
            let scaled = (i128::from(raw.upper) << 64) | i128::from(raw.lower);
            msduck_core::money::MoneyType::Money.check_scaled(scaled)?;
            let style = unsafe { styles.as_slice_with_len::<i32>(len)[row] };
            let width = unsafe { widths.as_slice_with_len::<i32>(len)[row] };
            let family = match unsafe { families.as_slice_with_len::<i32>(len)[row] } {
                0 => Family::Varchar,
                1 => Family::Char,
                2 => Family::Nvarchar,
                3 => Family::Nchar,
                _ => return Err("invalid currency format character family".into()),
            };
            let target = CharacterType::new(
                family,
                if width == -1 {
                    Length::Max
                } else {
                    Length::Bounded(width.try_into()?)
                },
            )?;
            let text = msduck_core::money::format(scaled as i64, style);
            match target.cast(&text, CastInput::Currency) {
                Ok(value) => result.insert(row, value.as_ref()),
                Err(_) if TRY => result.set_null(row),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                LogicalTypeHandle::decimal(19, 4),
                Id::Integer.into(),
                Id::Integer.into(),
                Id::Integer.into(),
            ],
            Id::Varchar.into(),
        )]
    }
}
pub fn register(db: &duckdb::Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<Format<false>>("__msduck_money_format")?;
    db.register_scalar_function::<Format<true>>("__msduck_try_money_format")
}

#[cfg(test)]
mod tests {
    #[test]
    fn format_evaluates_value_and_style_once_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        session
            .db
            .execute_batch(
                "CREATE SEQUENCE money_format_values; CREATE SEQUENCE money_format_styles",
            )
            .unwrap();
        let (_, ok) = session.batch_response(
            "SELECT CONVERT(VARCHAR(30),CAST(nextval('money_format_values') AS MONEY),CAST(nextval('money_format_styles')%3 AS INT)) AS s INTO money_format_rows FROM range(6000)",
            &Default::default(), false, None,
        );
        assert!(ok);
        assert_eq!(
            session
                .db
                .query_row(
                    "SELECT COUNT(*) FROM money_format_rows WHERE s IS NOT NULL",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            6000
        );
        for name in ["money_format_values", "money_format_styles"] {
            assert_eq!(
                session
                    .db
                    .query_row(&format!("SELECT currval('{name}')"), [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                6000
            );
        }
    }
}
