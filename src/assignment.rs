//! Target conversions shared by INSERT, UPDATE and ALTER COLUMN.
use sqlparser::ast::*;

pub fn storage_kind(name: &str) -> Option<DataType> {
    if let Some(scale) = name
        .strip_prefix("TIME(")
        .and_then(|s| s.strip_suffix(')'))
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s <= 7)
    {
        return Some(DataType::Time(Some(scale), TimezoneInfo::None));
    }
    crate::character_storage::storage_kind(name)
        .or_else(|| crate::integer_conversion::storage_integer_type(name))
        .or_else(|| crate::datetime2_cast::storage_kind(name))
        .or_else(|| crate::datetimeoffset_cast::storage_kind(name))
        .or_else(|| crate::variant_pack::storage_kind(name))
        .or_else(|| match name.to_ascii_uppercase().as_str() {
            "DATE" => Some(DataType::Date),
            "TIME_NS" => Some(DataType::Time(None, TimezoneInfo::None)),
            "MONEY" | "SMALLMONEY" => Some(DataType::Custom(
                ObjectName::from(vec![Ident::new(name.to_ascii_lowercase())]),
                vec![],
            )),
            _ => None,
        })
}

pub fn convert(value: Expr, kind: &DataType, money: bool) -> Expr {
    if let Some(kind) = msduck_sql::money_cast::money_type(kind) {
        return msduck_sql::money_cast::convert(value, kind, false);
    }
    if crate::character_storage::is_character(kind) {
        let value = if money {
            msduck_sql::money_format::storage(value)
        } else {
            value
        };
        return crate::character_storage::convert(value, kind).unwrap();
    }
    if matches!(kind, DataType::Date) {
        return crate::engine::unary_function("__msduck_cast_date", value);
    }
    if let DataType::Time(scale, _) = kind {
        let value = crate::engine::unary_function("__msduck_cast_time", value);
        return crate::engine::binary_function(
            "__msduck_time_round",
            value,
            Expr::Value(
                Value::Number(10u64.pow(9 - scale.unwrap_or(7) as u32).to_string(), false).into(),
            ),
        );
    }
    if crate::variant_pack::is_storage(kind) {
        return crate::variant_pack::convert(value);
    }
    if let Some(scale) = crate::datetimeoffset_cast::storage_scale(kind) {
        return crate::datetimeoffset_cast::convert(value, scale);
    }
    if let Some(scale) = crate::datetime2_cast::storage_scale(kind) {
        return crate::datetime2_cast::convert(value, scale);
    }
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(if money {
            value
        } else {
            crate::engine::integer_input(value, kind, false)
        }),
        data_type: kind.clone(),
        format: None,
    }
}

pub fn convert_for_storage(value: Expr, kind: &DataType, money: bool, utf16: bool) -> Expr {
    if utf16 && crate::character_storage::is_character(kind) {
        let value = if money {
            msduck_sql::money_format::storage(value)
        } else {
            value
        };
        return crate::character_storage::convert_with_layout(
            value,
            kind,
            crate::character_storage::Layout::Utf16,
        )
        .expect("Unicode carrier target has a Unicode declaration");
    }
    convert(value, kind, money)
}

/// Preserve physical layout before restoring declarations erased by DuckDB.
pub fn utf16_targets(
    columns: &[(String, String, Option<String>)],
) -> std::collections::HashSet<String> {
    columns
        .iter()
        .filter(|c| crate::unicode_carrier::is_storage_name(&c.1))
        .map(|c| c.0.to_lowercase())
        .collect()
}

/// Restore declared types whose bounds are erased by DuckDB storage.
pub fn declared_targets(
    db: &duckdb::Connection,
    schema: &str,
    table: &str,
    columns: &mut [(String, String, Option<String>)],
) -> duckdb::Result<()> {
    let name = format!(
        "{}.{}",
        Ident::with_quote('"', schema),
        Ident::with_quote('"', table)
    );
    let mut statement=db.prepare("SELECT name,CAST(scale AS INTEGER),CAST(system_type_id AS INTEGER),CAST(max_length AS INTEGER) FROM sys.columns WHERE object_id=__msduck_object_id(?,NULL) AND system_type_id IN (41,60,122,167,175,231,239)")?;
    for row in statement.query_map([name], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i32>(1)?,
            r.get::<_, i32>(2)?,
            r.get::<_, i32>(3)?,
        ))
    })? {
        let (name, scale, id, bytes) = row?;
        if let Some(column) = columns.iter_mut().find(|c| c.0.eq_ignore_ascii_case(&name)) {
            column.1 = if id == 60 {
                "MONEY".into()
            } else if id == 122 {
                "SMALLMONEY".into()
            } else if id == 41 {
                format!("TIME({scale})")
            } else {
                let kind = match id {
                    167 => "VARCHAR",
                    175 => "CHAR",
                    231 => "NVARCHAR",
                    239 => "NCHAR",
                    _ => unreachable!(),
                };
                let width = if bytes == -1 {
                    -1
                } else if id == 231 || id == 239 {
                    bytes / 2
                } else {
                    bytes
                };
                format!("__MSDUCK_{kind}({width})")
            };
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn time_declarations_defaults_and_rollback_survive_restart() {
        let path = std::env::temp_dir().join(format!(
            "msduck-time-restart-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let run = |session: &mut crate::engine::Session, sql: &str| {
            assert!(
                session
                    .batch_response(sql, &Default::default(), false, None)
                    .1,
                "{sql}"
            );
        };
        {
            let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
            let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
            run(
                &mut session,
                "CREATE TABLE dbo.saved_time(id INT,t TIME(3) DEFAULT '12:00:00.1249'); INSERT INTO dbo.saved_time(id) VALUES(1); BEGIN TRAN; ALTER TABLE dbo.saved_time ALTER COLUMN t TIME(2); UPDATE dbo.saved_time SET t='01:02:03.456'; ROLLBACK; INSERT INTO dbo.saved_time(id) VALUES(2)",
            );
            assert_eq!(
                session
                    .db
                    .query_row(
                        "SELECT min(epoch_ns(t)),max(epoch_ns(t)) FROM dbo.saved_time",
                        [],
                        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                    )
                    .unwrap(),
                (43_200_125_000_000, 43_200_125_000_000)
            );
        }
        {
            let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
            let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
            run(
                &mut session,
                "INSERT INTO dbo.saved_time(id) VALUES(3); ALTER TABLE dbo.saved_time ALTER COLUMN t TIME(2); INSERT INTO dbo.saved_time(id) VALUES(4)",
            );
            let mut statement = session
                .db
                .prepare("SELECT epoch_ns(t) FROM dbo.saved_time ORDER BY id")
                .unwrap();
            let values = statement
                .query_map([], |r| r.get::<_, i64>(0))
                .unwrap()
                .collect::<duckdb::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(
                values,
                vec![
                    43_200_130_000_000,
                    43_200_130_000_000,
                    43_200_130_000_000,
                    43_200_120_000_000
                ]
            );
        }
        {
            let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
            let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
            run(&mut session, "INSERT INTO dbo.saved_time(id) VALUES(5)");
            assert_eq!(
                session
                    .db
                    .query_row(
                        "SELECT epoch_ns(t) FROM dbo.saved_time WHERE id=5",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                43_200_120_000_000
            );
            assert_eq!(session.db.query_row("SELECT CAST(scale AS INTEGER) FROM sys.columns WHERE object_id=__msduck_object_id('dbo.saved_time',NULL) AND name='t'",[],|r|r.get::<_,i32>(0)).unwrap(),2);
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn time_writes_round_each_scale_across_chunks() {
        let server = crate::server::Server::open(":memory:").unwrap();
        let mut session = crate::engine::Session::new(server.connection().unwrap()).unwrap();
        for scale in 0..=7 {
            let sql = format!(
                "CREATE TABLE dbo.time_write_{scale}(t TIME({scale})); INSERT INTO dbo.time_write_{scale} SELECT CASE WHEN i%3=0 THEN NULL ELSE CAST('12:34:56.1234567' AS TIME(7)) END FROM range(6000) r(i)"
            );
            let (_, ok) = session.batch_response(&sql, &Default::default(), false, None);
            assert!(ok, "scale {scale}");
            let quantum = 10i64.pow(9 - scale);
            let expected = ((45_296_123_456_700i64 + quantum / 2) / quantum) * quantum;
            let sql = format!(
                "SELECT count(*),count(t),min(epoch_ns(t)),max(epoch_ns(t)) FROM dbo.time_write_{scale}"
            );
            assert_eq!(
                session
                    .db
                    .query_row(&sql, [], |r| Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?
                    )))
                    .unwrap(),
                (6000, 4000, expected, expected)
            );
        }
    }
}
