//! Root GUID conversion adapter. Registration is explicit and connection-owned.
use duckdb::{
    Connection,
    core::{DataChunkHandle, Inserter, LogicalTypeId as Id},
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::arrow::WritableVector,
};
use msduck_core::types::uniqueidentifier::{parse_nvarchar, parse_varchar};

const ERROR_PREFIX: &str = "__msduck_guid_character_conversion:";
const CONVERSION: &str =
    "Conversion failed when converting from a character string to uniqueidentifier.";

/// Recognize only this adapter's canonical native error envelope. Ordinary
/// backend failures and application diagnostics retain their own identities.
pub fn diagnostic(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    let message = message
        .strip_prefix("Invalid Input Error: ")?
        .strip_prefix(ERROR_PREFIX)?;
    (message == CONVERSION).then(|| msduck_core::diagnostic::SqlError::new(8169, 2, CONVERSION))
}

/// Register the storage conversion without changing existing assignment plans.
/// The caller also owns diagnostic translation and transaction error handling.
pub fn register(db: &Connection) -> duckdb::Result<()> {
    db.register_scalar_function::<CharacterGuid>("__msduck_guid_character_text")?;
    // typeof is a binding-time property: only the selected branch evaluates v.
    // DuckDB owns native UUID representation; the adapter emits canonical text
    // from the core's mixed-endian bytes instead of assuming native slot layout.
    db.execute_batch(
        "CREATE OR REPLACE MACRO main.__msduck_guid_assignment(v) AS CASE
         WHEN typeof(v)='UUID' THEN CAST(v AS UUID)
         ELSE CAST(__msduck_guid_character_text(v) AS UUID) END",
    )
}

struct CharacterGuid;
impl VScalar for CharacterGuid {
    type State = ();
    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![Id::Any.into()],
            Id::Varchar.into(),
        )]
    }
    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let len = input.len();
        let source = input.flat_vector(0);
        let logical = source.logical_type();
        let unicode = crate::unicode_carrier::is_logical(&logical);
        if !unicode && !matches!(logical.id(), Id::Varchar | Id::SqlNull) {
            return Err("unsupported source type for GUID storage assignment".into());
        }
        let structure = unicode.then(|| input.struct_vector(0));
        let payload = structure.as_ref().map(|vector| vector.child(0, len));
        let mut target = output.flat_vector();
        for row in 0..len {
            if source.row_is_null(row as u64) {
                target.set_null(row);
                continue;
            }
            let bytes = if let Some(payload) = &payload {
                let units = crate::unicode_carrier::vector_units(&source, payload, row, len)?
                    .ok_or("missing non-NULL GUID Unicode source")?;
                parse_nvarchar(&units).map_err(|error| format!("{ERROR_PREFIX}{error}"))?
            } else {
                // GUID syntax is ASCII. UTF-8 and CP1252 agree in the accepted
                // prefix; any non-ASCII prefix fails and suffixes are ignored.
                let text = crate::unicode_carrier::bytes(&source, row, len)?;
                parse_varchar(&text).map_err(|error| format!("{ERROR_PREFIX}{error}"))?
            };
            target.insert(row, uuid::Uuid::from_bytes_le(bytes).to_string().as_str());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const GUID: &str = "00112233-4455-6677-8899-aabbccddeeff";

    #[test]
    fn native_error_envelope_preserves_core_identity_without_reclassifying_other_errors() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let error = db
            .query_row::<String, _, _>(
                "SELECT CAST(__msduck_guid_assignment(?) AS VARCHAR)",
                ["bad"],
                |row| row.get(0),
            )
            .unwrap_err();
        assert_eq!(
            diagnostic(&error.to_string()),
            Some(parse_varchar(b"bad").unwrap_err())
        );
        for message in [
            CONVERSION.to_owned(),
            format!("Invalid Input Error: {CONVERSION}"),
            format!("Invalid Input Error: {ERROR_PREFIX}{CONVERSION} extra"),
            format!("other error: {ERROR_PREFIX}{CONVERSION}"),
        ] {
            assert_eq!(diagnostic(&message), None);
        }
        let invalid_carrier = db
            .query_row::<String, _, _>(
                "SELECT CAST(__msduck_guid_assignment(struct_pack(__msduck_utf16le:=?)) AS VARCHAR)",
                [vec![0_u8]],
                |row| row.get(0),
            )
            .unwrap_err();
        assert_eq!(diagnostic(&invalid_carrier.to_string()), None);
    }

    #[test]
    fn characters_null_and_native_guid_have_exact_uuid_layout() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        for text in [
            GUID.to_owned(),
            format!("{{{GUID}}}EXTRA"),
            format!("{GUID}EXTRA"),
        ] {
            let actual: String = db
                .query_row(
                    "SELECT CAST(__msduck_guid_assignment(?) AS VARCHAR)",
                    [text],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(actual, GUID);
        }
        let actual: String = db
            .query_row(
                "SELECT CAST(__msduck_guid_assignment(CAST(? AS UUID)) AS VARCHAR)",
                [GUID],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(actual, GUID);
        let absent: Option<String> = db
            .query_row(
                "SELECT CAST(__msduck_guid_assignment(CAST(NULL AS VARCHAR)) AS VARCHAR)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(absent, None);
    }

    #[test]
    fn raw_unicode_suffix_surrogates_are_preserved_until_core_conversion() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        let encoded: Vec<u8> = format!("{{{GUID}}}")
            .encode_utf16()
            .chain([0xd800])
            .flat_map(u16::to_le_bytes)
            .collect();
        let actual: String = db.query_row(
            "SELECT CAST(__msduck_guid_assignment(struct_pack(__msduck_utf16le:=?)) AS VARCHAR)",
            [encoded], |row| row.get(0),
        ).unwrap();
        assert_eq!(actual, GUID);
        let invalid = db.query_row::<String, _, _>(
            "SELECT CAST(__msduck_guid_assignment(struct_pack(__msduck_utf16le:=?)) AS VARCHAR)",
            [vec![0, 0xd8]], |row| row.get(0),
        ).unwrap_err();
        assert!(invalid.to_string().contains(
            "Conversion failed when converting from a character string to uniqueidentifier."
        ));
    }

    #[test]
    fn native_conversion_evaluates_each_operand_once_across_chunks() {
        let db = Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE SEQUENCE guid_calls START 1")
            .unwrap();
        let count: i64 = db.query_row(
            "SELECT count(__msduck_guid_assignment('00112233-4455-6677-8899-aabbccddeeff'||CAST(nextval('guid_calls') AS VARCHAR))) FROM range(6000)",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 6000);
        let calls: i64 = db
            .query_row("SELECT currval('guid_calls')", [], |row| row.get(0))
            .unwrap();
        assert_eq!(calls, 6000);
    }
}
