//! Preserve a described result prefix when native query execution fails.
use crate::{
    query_catalog::Field,
    tds::{self, Type},
};
use duckdb::{
    Statement,
    core::{LogicalTypeHandle, LogicalTypeId as Id},
};

#[derive(Debug)]
pub struct CompilationFailure(String);
impl std::fmt::Display for CompilationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
pub fn compilation(error: anyhow::Error) -> anyhow::Error {
    let message = error.to_string();
    error.context(CompilationFailure(message))
}

#[derive(Debug)]
pub struct FailedQuery {
    pub metadata: Vec<u8>,
    pub command: u16,
    message: String,
}
impl std::fmt::Display for FailedQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
pub fn attach(error: duckdb::Error, metadata: Vec<u8>, command: u16) -> anyhow::Error {
    attach_context(anyhow::Error::new(error), metadata, command)
}
pub fn attach_context(error: anyhow::Error, metadata: Vec<u8>, command: u16) -> anyhow::Error {
    let message = error.to_string();
    error.context(FailedQuery {
        metadata,
        command,
        message,
    })
}

/// Recognize native signed arithmetic overflow, not arbitrary application text.
/// SQL conversion overflows have different states and use their own adapters.
pub fn integer_overflow(message: &str) -> Option<msduck_core::diagnostic::SqlError> {
    let message = message.strip_prefix("Out of Range Error: Overflow in ")?;
    let (operation, operands) = message.split_once(" of ")?;
    let operator = match operation {
        "addition" => " + ",
        "subtraction" => " - ",
        "multiplication" => " * ",
        _ => return None,
    };
    let (kind, operands) = operands.split_once(" (")?;
    let (left, right) = operands.strip_suffix(")!")?.split_once(operator)?;
    let (left, right) = (left.parse::<i64>().ok()?, right.parse::<i64>().ok()?);
    let overflow = match kind {
        "INT32" => {
            let (a, b) = (i32::try_from(left).ok()?, i32::try_from(right).ok()?);
            match operation {
                "addition" => a.checked_add(b),
                "subtraction" => a.checked_sub(b),
                _ => a.checked_mul(b),
            }
            .is_none()
        }
        "INT64" => match operation {
            "addition" => left.checked_add(right),
            "subtraction" => left.checked_sub(right),
            _ => left.checked_mul(right),
        }
        .is_none(),
        _ => return None,
    };
    overflow.then(|| {
        msduck_core::diagnostic::SqlError::new(
            8115,
            2,
            format!(
                "Arithmetic overflow error converting expression to data type {}.",
                if kind == "INT32" { "int" } else { "bigint" }
            ),
        )
    })
}

/// A preparation-time runtime failure can precede the physical descriptor.
/// Use only complete logical fields; do not execute a query to recover metadata.
pub fn describe_fields(fields: &[Field], declared: &[Option<Type>]) -> Option<Vec<u8>> {
    if fields.is_empty() || (!declared.is_empty() && declared.len() != fields.len()) {
        return None;
    }
    let aligned = crate::result_metadata::Aligned::new(fields.len(), fields, declared);
    let columns = fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let kind = aligned.declared(index).cloned().or_else(|| {
                let info = field.info.as_ref()?;
                Some(match info.system_type_id? {
                    48 => Type::Int(1),
                    52 => Type::Int(2),
                    56 => Type::Int(4),
                    127 => Type::Int(8),
                    104 => Type::Bit,
                    59 => Type::Float(4),
                    62 => Type::Float(8),
                    106 | 108 => {
                        let declaration =
                            msduck_core::types::DecimalType::new(info.precision?, info.scale?)
                                .ok()?;
                        Type::Decimal(declaration.precision(), declaration.scale())
                    }
                    36 => Type::Guid,
                    40 => Type::Date,
                    61 => Type::DateTime,
                    42 => Type::DateTime2(msduck_core::types::Scale::new(info.scale?).ok()?.get()),
                    43 => Type::DateTimeOffset(
                        msduck_core::types::Scale::new(info.scale?).ok()?.get(),
                    ),
                    98 => Type::Variant,
                    _ => return None,
                })
            })?;
            Some(aligned.column(index, field.name.clone(), kind))
        })
        .collect::<Option<Vec<_>>>()?;
    let mut metadata = Vec::new();
    tds::metadata(&mut metadata, &columns).ok()?;
    (metadata.len() <= tds::MAX_MESSAGE - 1024).then_some(metadata)
}
fn wire(kind: &LogicalTypeHandle, declared: Option<&Type>) -> Option<Type> {
    let id = kind.id();
    if crate::unicode_carrier::is_logical(kind) {
        return Some(match declared {
            Some(kind @ (Type::Text | Type::Nvarchar(_) | Type::Nchar(_))) => kind.clone(),
            _ => Type::Text,
        });
    }

    if let Some(declared) = declared
        && (id == Id::Varchar
            || matches!(
                (declared, id),
                (Type::Time(_), Id::Time | Id::TimeNs)
                    | (Type::Varbinary(_) | Type::FixedBinary(_), Id::Blob)
            )
            || matches!(declared, Type::Money(_)) && id == Id::Decimal && kind.decimal_scale() == 4)
    {
        return Some(declared.clone());
    }
    Some(match id {
        Id::Boolean => Type::Bit,
        Id::UTinyint => Type::Int(1),
        Id::Tinyint | Id::Smallint => Type::Int(2),
        Id::Integer => Type::Int(4),
        Id::Bigint => Type::Int(8),
        Id::Float => Type::Float(4),
        Id::Double => Type::Float(8),
        Id::Decimal => Type::Decimal(kind.decimal_width(), kind.decimal_scale()),
        Id::Varchar => Type::Text,
        Id::Blob => Type::Binary,
        Id::Date => Type::Date,
        Id::Time | Id::TimeNs => Type::Time(7),
        Id::Timestamp | Id::TimestampS | Id::TimestampMs | Id::TimestampNs => Type::DateTime,
        Id::Uuid => Type::Guid,
        _ => return None,
    })
}
/// Unknown shapes remain unavailable; never synthesize a partial column list.
pub fn describe(
    statement: &Statement<'_>,
    fields: &[Field],
    declared: &[Option<Type>],
) -> Option<Vec<u8>> {
    let prepared = statement.prepared_columns().ok()?;
    let aligned = crate::result_metadata::Aligned::new(prepared.len(), fields, declared);
    let columns = prepared
        .into_iter()
        .enumerate()
        .map(|(index, (name, kind))| {
            Some(aligned.column(index, name, wire(&kind, aligned.declared(index))?))
        })
        .collect::<Option<Vec<_>>>()?;
    let mut metadata = Vec::new();
    tds::metadata(&mut metadata, &columns).ok()?;
    (metadata.len() <= tds::MAX_MESSAGE - 1024).then_some(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tds::Column;
    #[test]
    fn logical_error_descriptors_require_complete_aligned_fields() {
        let field = Field {
            name: "n".into(),
            info: Some(msduck_core::catalog::TypeMetadata {
                system_type_id: Some(56),
                ..Default::default()
            }),
            properties: msduck_core::result::Properties::expression(true),
            collation: None,
            json_fragment: false,
        };
        let mut expected = Vec::new();
        tds::metadata(
            &mut expected,
            &[Column {
                name: "n".into(),
                kind: Type::Int(4),
                properties: field.properties,
                collation: None,
            }],
        )
        .unwrap();
        assert_eq!(
            describe_fields(std::slice::from_ref(&field), &[]),
            Some(expected)
        );
        assert!(describe_fields(&[], &[]).is_none());
        assert!(describe_fields(std::slice::from_ref(&field), &[None, None]).is_none());
        let mut unknown = field.clone();
        unknown.info = None;
        assert!(describe_fields(&[field, unknown], &[]).is_none());
    }
    #[test]
    fn native_constant_overflow_is_recognized_before_physical_description() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        let sql = "SELECT CASE WHEN 2147483647+1=0 THEN 7 ELSE NULL END AS n";
        let error = match db.prepare(sql) {
            Ok(_) => panic!("expected preparation-time overflow"),
            Err(error) => error,
        };
        let error = integer_overflow(&error.to_string()).unwrap();
        assert_eq!((error.number, error.state), (8115, 2));
        for message in [
            "Out of Range Error: Overflow in addition of INT32 (1 + 1)!",
            "Out of Range Error: Overflow in addition of INT32 (2147483647 + 1)! trailing",
            "application: Out of Range Error: Overflow in addition of INT32 (2147483647 + 1)!",
        ] {
            assert!(integer_overflow(message).is_none());
        }
    }
    #[test]
    fn runtime_error_preserves_described_columns_before_error_or_catch() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        let select = "SELECT CAST(1 AS DECIMAL(5,2))/CAST(0 AS DECIMAL(5,2)) AS d";
        let mut expected = Vec::new();
        tds::metadata(
            &mut expected,
            &[Column {
                collation: None,
                name: "d".into(),
                kind: Type::Decimal(13, 8),
                properties: msduck_core::result::Properties::expression(true),
            }],
        )
        .unwrap();
        let (response, success) = session.batch_response(select, &Default::default(), false, None);
        assert!(!success);
        assert!(response.starts_with(&expected));
        assert_eq!(response[expected.len()], 0xaa);
        let (response, success) = session.batch_response(
            &format!(
                "BEGIN TRY {select}; END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n; END CATCH"
            ),
            &Default::default(),
            false,
            None,
        );
        assert!(success);
        let mut entered = Vec::new();
        tds::done(&mut entered, 0xfd, 1, 349, 0);
        entered.extend(expected);
        assert!(response.starts_with(&entered));
    }
}

#[cfg(test)]
mod continuation_tests {
    #[test]
    fn arithmetic_query_errors_preserve_later_writes_and_failure_status() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        assert!(
            session
                .batch_response(
                    "CREATE TABLE dbo.error_guard(n INT)",
                    &Default::default(),
                    false,
                    None
                )
                .1
        );
        let (_,success)=session.batch_response("INSERT INTO dbo.error_guard VALUES(1); SELECT CAST(1 AS DECIMAL(5,2))/CAST(0 AS DECIMAL(5,2)); INSERT INTO dbo.error_guard VALUES(2)",&Default::default(),false,None);
        assert!(!success);
        let sum: i32 = session
            .db
            .query_row("SELECT SUM(n)::INT FROM dbo.error_guard", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sum, 3);
        let (_, success) = session.batch_response(
            "SELECT 1/0; SELECT 2/0; INSERT INTO dbo.error_guard VALUES(@@ERROR)",
            &Default::default(),
            false,
            None,
        );
        assert!(!success);
        assert_eq!(
            session
                .db
                .query_row("SELECT MAX(n) FROM dbo.error_guard", [], |r| r
                    .get::<_, i32>(0))
                .unwrap(),
            8134
        );
        let (_, success) = session.batch_response(
            "SELECT CAST('bad' AS INT); INSERT INTO dbo.error_guard VALUES(99999)",
            &Default::default(),
            false,
            None,
        );
        assert!(!success);
        assert_eq!(
            session
                .db
                .query_row("SELECT MAX(n) FROM dbo.error_guard", [], |r| r
                    .get::<_, i32>(0))
                .unwrap(),
            8134
        );
    }
}
