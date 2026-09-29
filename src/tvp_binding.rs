//! Catalog-backed TVP binding. This adapter does not write rows or execute RPCs.
use duckdb::Connection;
use msduck_core::diagnostic::SqlError;
use msduck_tds::tvp::{Cell, ColumnType, Limits, Tvp, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultProfile {
    /// MS-TDS requires the TVP default-status bit to be zero.
    Specification,
    /// The retained SQL Server 2025 build accepts a default-bit NULL as empty.
    CapturedSqlServer2025,
}

/// Procedure declaration and session lookup context, supplied by the caller.
pub struct Context<'a> {
    pub schema: &'a str,
    pub type_name: &'a str,
    pub default_schema: &'a str,
    pub parameter_name: &'a str,
    pub parameter_ordinal: u16,
    pub default_profile: DefaultProfile,
}

pub enum Input<'a, 'wire> {
    Omitted,
    Parameter { status: u8, value: &'a Tvp<'wire> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Supply {
    Omitted,
    DefaultBit,
    Explicit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnKind {
    Int,
    Nvarchar { max_units: u16, collation: String },
    Varbinary { max_bytes: u16 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub nullable: bool,
    pub kind: ColumnKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableType {
    pub schema: String,
    pub name: String,
    pub user_type_id: i32,
    pub object_id: i32,
    pub columns: Vec<Column>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnedCell {
    Null,
    Int(i32),
    /// Raw UTF-16, including isolated surrogate code units.
    Nvarchar(Vec<u16>),
    Varbinary(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundTvp {
    pub table_type: TableType,
    pub supply: Supply,
    pub rows: Vec<Vec<OwnedCell>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub error: SqlError,
    pub line_number: u32,
}

#[derive(Debug)]
pub enum Error {
    Catalog(duckdb::Error),
    Diagnostics(Vec<Diagnostic>),
    Limit(&'static str),
    Unsupported(&'static str),
    Malformed(&'static str),
}
impl From<duckdb::Error> for Error {
    fn from(value: duckdb::Error) -> Self {
        Self::Catalog(value)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Catalog(error) => error.fmt(f),
            Self::Diagnostics(errors) => write!(f, "TVP binding failed: {errors:?}"),
            Self::Limit(message) | Self::Unsupported(message) | Self::Malformed(message) => {
                f.write_str(message)
            }
        }
    }
}
impl std::error::Error for Error {}

fn diagnostic(number: i32, state: u8, message: String) -> Error {
    Error::Diagnostics(vec![Diagnostic {
        error: SqlError::new(number, state, message),
        line_number: 1,
    }])
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.encode_utf16().count() <= 128 && !name.contains('\0')
}
fn wire_name(units: &[u16]) -> Result<String, Error> {
    if units.len() > 128 || units.contains(&0) {
        return Err(Error::Malformed("invalid TVP identifier length or NUL"));
    }
    String::from_utf16(units).map_err(|_| Error::Unsupported("unpaired TVP identifier surrogate"))
}
fn protocol(context: &Context<'_>) -> String {
    format!(
        "The incoming tabular data stream (TDS) remote procedure call (RPC) protocol stream is incorrect. Table-valued parameter {} (\"{}\"), row 0, column 0: Data type 0xF3 (user-defined table type)",
        context.parameter_ordinal, context.parameter_name
    )
}

/// Resolve one registered type and its ordered columns with a bounded read.
/// Names are values in bound queries, never interpolated SQL identifiers.
fn catalog(
    db: &Connection,
    schema: &str,
    name: &str,
    limits: Limits,
    ordinal: u16,
) -> Result<TableType, Error> {
    let maximum = limits.max_columns.min(1024);
    let mut statement = db.prepare(
        "SELECT s.name,t.name,t.user_type_id,t.type_table_object_id,c.column_id,c.name,\
         c.system_type_id,c.user_type_id,c.max_length,c.is_nullable,c.collation_name \
         FROM sys.table_types t JOIN main.__msduck_schemas s ON s.schema_id=t.schema_id \
         JOIN main.__msduck_table_type_columns c ON c.object_id=t.type_table_object_id \
         WHERE lower(s.name)=lower(?) AND lower(t.name)=lower(?) ORDER BY c.column_id LIMIT ?",
    )?;
    let mut query = statement.query(duckdb::params![schema, name, (maximum + 1) as i64])?;
    let mut result: Option<TableType> = None;
    while let Some(row) = query.next()? {
        let canonical_schema: String = row.get(0)?;
        let canonical_name: String = row.get(1)?;
        let user_type_id: i32 = row.get(2)?;
        let object_id: i32 = row.get(3)?;
        let ordinal: i32 = row.get(4)?;
        let column_name: String = row.get(5)?;
        let system_id: u8 = row.get(6)?;
        let column_user_id: i32 = row.get(7)?;
        let length: i16 = row.get(8)?;
        let nullable: bool = row.get(9)?;
        let collation: Option<String> = row.get(10)?;
        if !valid_name(&canonical_schema)
            || !valid_name(&canonical_name)
            || !valid_name(&column_name)
        {
            return Err(Error::Malformed("invalid catalog TVP identifier"));
        }
        if column_user_id != i32::from(system_id) {
            return Err(Error::Unsupported("TVP alias column declaration"));
        }
        let kind = match (system_id, length, collation) {
            (56, 4, None) => ColumnKind::Int,
            (231, 2..=8000, Some(collation)) if length % 2 == 0 => ColumnKind::Nvarchar {
                max_units: length as u16 / 2,
                collation,
            },
            (165, 1..=8000, None) => ColumnKind::Varbinary {
                max_bytes: length as u16,
            },
            _ => return Err(Error::Unsupported("TVP catalog column declaration")),
        };
        let table = result.get_or_insert_with(|| TableType {
            schema: canonical_schema.clone(),
            name: canonical_name.clone(),
            user_type_id,
            object_id,
            columns: Vec::new(),
        });
        if table.columns.len() >= maximum {
            return Err(Error::Limit("TVP catalog column limit"));
        }
        if ordinal != table.columns.len() as i32 + 1
            || table.user_type_id != user_type_id
            || table.object_id != object_id
            || table.schema != canonical_schema
            || table.name != canonical_name
        {
            return Err(Error::Malformed(
                "inconsistent TVP catalog identity or column order",
            ));
        }
        table.columns.push(Column {
            name: column_name,
            nullable,
            kind,
        });
    }
    result.ok_or_else(|| {
        diagnostic(
            2715,
            3,
            format!(
                "Column, parameter, or variable #{ordinal}: Cannot find data type {schema}.{name}."
            ),
        )
    })
}

/// Bind owned values without writing or materializing a SQL table. Callers must
/// hold their catalog/execution transaction across binding and future use.
/// `limits` are rechecked even for manually constructed decoded values.
pub fn bind(
    db: &Connection,
    context: &Context<'_>,
    input: Input<'_, '_>,
    limits: Limits,
) -> Result<BoundTvp, Error> {
    if !valid_name(context.schema)
        || !valid_name(context.type_name)
        || !valid_name(context.default_schema)
        || !valid_name(context.parameter_name)
        || context.parameter_ordinal == 0
    {
        return Err(Error::Malformed("invalid explicit TVP binding context"));
    }
    let table_type = catalog(
        db,
        context.schema,
        context.type_name,
        limits,
        context.parameter_ordinal,
    )?;
    let Input::Parameter { status, value } = input else {
        return Ok(BoundTvp {
            table_type,
            supply: Supply::Omitted,
            rows: Vec::new(),
        });
    };
    if !matches!(status, 0 | 2) {
        return Err(Error::Unsupported("TVP RPC parameter status"));
    }
    let schema = wire_name(&value.type_name.schema)?;
    let name = wire_name(&value.type_name.name)?;
    if status == 2 {
        if context.default_profile != DefaultProfile::CapturedSqlServer2025 {
            return Err(Error::Unsupported("TVP default bit conflicts with MS-TDS"));
        }
        if !matches!(value.value, Value::Null) || !schema.is_empty() || !name.is_empty() {
            return Err(Error::Unsupported("uncaptured TVP default-bit shape"));
        }
        return Ok(BoundTvp {
            table_type,
            supply: Supply::DefaultBit,
            rows: Vec::new(),
        });
    }
    if matches!(value.value, Value::Null) {
        return Err(diagnostic(
            8060,
            1,
            format!(
                "{} is null and not set to default.  A null table-valued parameter is required to be sent as a default parameter.",
                protocol(context)
            ),
        ));
    }
    if name.is_empty() && !schema.is_empty() {
        return Err(diagnostic(
            8049,
            2,
            format!("{} has an invalid type name specified.", protocol(context)),
        ));
    }
    if !name.is_empty() {
        let supplied = catalog(
            db,
            if schema.is_empty() {
                context.default_schema
            } else {
                &schema
            },
            &name,
            limits,
            context.parameter_ordinal,
        )?;
        if supplied.user_type_id != table_type.user_type_id
            || supplied.object_id != table_type.object_id
        {
            return Err(diagnostic(
                206,
                3,
                format!(
                    "Operand type clash: {}.{} is incompatible with {}.{}",
                    table_type.schema, table_type.name, supplied.schema, supplied.name
                ),
            ));
        }
    }
    let Value::Table { columns, rows } = &value.value else {
        unreachable!()
    };
    if columns.len() != table_type.columns.len() {
        return Err(diagnostic(
            500,
            1,
            format!(
                "Trying to pass a table-valued parameter with {} column(s) where the corresponding user-defined table type requires {} column(s).",
                columns.len(),
                table_type.columns.len()
            ),
        ));
    }
    if rows.len() > limits.max_rows
        || rows
            .len()
            .checked_mul(columns.len())
            .is_none_or(|n| n > limits.max_cells)
    {
        return Err(Error::Limit("TVP row or cell limit"));
    }
    for (wire, declared) in columns.iter().zip(&table_type.columns) {
        if wire.user_type != 0 || wire.flags & !1 != 0 {
            return Err(Error::Unsupported("TVP column flags or user type"));
        }
        match (&wire.column_type, &declared.kind) {
            (ColumnType::IntN { width: 4 | 8 }, ColumnKind::Int) => {}
            (
                ColumnType::NVarChar {
                    max_bytes,
                    collation,
                },
                ColumnKind::Nvarchar { .. },
            ) if *max_bytes != 0
                && *max_bytes != u16::MAX
                && max_bytes % 2 == 0
                && *collation == [0; 5] => {}
            (
                ColumnType::VarBinary {
                    max_bytes: 1..=65534,
                },
                ColumnKind::Varbinary { .. },
            ) => {}
            _ => return Err(Error::Unsupported("TVP wire/declaration conversion")),
        }
    }
    let mut owned = Vec::with_capacity(rows.len());
    let mut remaining = limits.max_input_bytes;
    for row in rows {
        if row.len() != columns.len() {
            return Err(Error::Malformed("TVP row width"));
        }
        let mut converted = Vec::with_capacity(row.len());
        for ((cell, wire), declared) in row.iter().zip(columns).zip(&table_type.columns) {
            let bytes = match cell {
                Cell::Null if declared.nullable => {
                    converted.push(OwnedCell::Null);
                    continue;
                }
                Cell::Null => {
                    return Err(Error::Unsupported(
                        "TVP NULL for NOT NULL column diagnostic",
                    ));
                }
                Cell::Default => return Err(Error::Unsupported("TVP defaulted column")),
                Cell::Bytes(bytes) => *bytes,
            };
            if bytes.len() > limits.max_cell_bytes || bytes.len() > remaining {
                return Err(Error::Limit("TVP owned byte or cell limit"));
            }
            remaining -= bytes.len();
            let cell = match (&wire.column_type, &declared.kind) {
                (ColumnType::IntN { width }, ColumnKind::Int) => {
                    if bytes.len() != usize::from(*width) {
                        return Err(Error::Malformed("TVP integer cell width"));
                    }
                    let integer = if *width == 4 {
                        i64::from(i32::from_le_bytes(bytes.try_into().unwrap()))
                    } else {
                        i64::from_le_bytes(bytes.try_into().unwrap())
                    };
                    let integer = i32::try_from(integer).map_err(|_| Error::Diagnostics(vec![
                        Diagnostic { error: SqlError::new(8115, 2, "Arithmetic overflow error converting expression to data type int."), line_number: 0 },
                        Diagnostic { error: SqlError::new(8061, 1, format!("The data for table-valued parameter \"{}\" doesn't conform to the table type of the parameter. SQL Server error is: 8115, state: 2",context.parameter_name)), line_number: 1 },
                    ]))?;
                    OwnedCell::Int(integer)
                }
                (
                    ColumnType::NVarChar { max_bytes, .. },
                    ColumnKind::Nvarchar { max_units, .. },
                ) => {
                    if bytes.len() % 2 != 0 || bytes.len() > usize::from(*max_bytes) {
                        return Err(Error::Malformed("TVP Unicode cell length"));
                    }
                    if bytes.len() / 2 > usize::from(*max_units) {
                        return Err(Error::Unsupported(
                            "TVP Unicode assignment overflow diagnostic",
                        ));
                    }
                    OwnedCell::Nvarchar(
                        bytes
                            .chunks_exact(2)
                            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                            .collect(),
                    )
                }
                (
                    ColumnType::VarBinary { max_bytes },
                    ColumnKind::Varbinary { max_bytes: target },
                ) => {
                    if bytes.len() > usize::from(*max_bytes) {
                        return Err(Error::Malformed("TVP binary cell length"));
                    }
                    if bytes.len() > usize::from(*target) {
                        return Err(Error::Unsupported(
                            "TVP binary assignment overflow diagnostic",
                        ));
                    }
                    OwnedCell::Varbinary(bytes.to_vec())
                }
                _ => unreachable!("metadata checked before row conversion"),
            };
            converted.push(cell);
        }
        owned.push(converted);
    }
    Ok(BoundTvp {
        table_type,
        supply: Supply::Explicit,
        rows: owned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value as Json;

    fn context() -> Context<'static> {
        Context {
            schema: "dbo",
            type_name: "TvpProbe",
            default_schema: "dbo",
            parameter_name: "@rows",
            parameter_ordinal: 1,
            default_profile: DefaultProfile::CapturedSqlServer2025,
        }
    }
    fn create(db: &Connection, schema: &str, name: &str, autocommit: bool) {
        use sqlparser::{ast::Statement, parser::Parser};
        let Statement::CreateTable(table) = Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "CREATE TABLE probe(id INT NULL,label NVARCHAR(10) NULL,payload VARBINARY(10) NULL)",
        )
        .unwrap()
        .remove(0) else {
            panic!()
        };
        let columns: Vec<_> = table
            .columns
            .iter()
            .map(|c| crate::type_catalog::TableColumn {
                name: &c.name.value,
                data_type: &c.data_type,
                nullable: true,
                collation_name: None,
            })
            .collect();
        crate::type_catalog::create_table_type(db, schema, name, &columns, autocommit).unwrap();
    }
    fn setup() -> (crate::server::Server, crate::server::Connection) {
        let server = crate::server::Server::open(":memory:").unwrap();
        let db = server.connection().unwrap();
        create(&db, "dbo", "TvpProbe", true);
        create(&db, "dbo", "TvpOther", true);
        let schema = sqlparser::parser::Parser::parse_sql(
            &crate::dialect::ServerDialect,
            "CREATE SCHEMA app",
        )
        .unwrap()
        .remove(0);
        crate::schema_catalog::execute(&db, &schema, true).unwrap();
        create(&db, "app", "TvpProbe", true);
        (server, db)
    }
    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }
    fn raw_input(bytes: &[u8]) -> Option<(u8, Tvp<'_>)> {
        let mut cursor = msduck_tds::Cursor::new(bytes);
        let header_size = cursor.u32().unwrap() as usize;
        cursor.take(header_size - 4).unwrap();
        let name = cursor.u16().unwrap() as usize;
        assert_ne!(name, 65535);
        cursor.take(name * 2).unwrap();
        cursor.u16().unwrap();
        if cursor.remaining() == 0 {
            return None;
        }
        let name = cursor.u8().unwrap() as usize;
        cursor.take(name * 2).unwrap();
        let status = cursor.u8().unwrap();
        let offset = bytes.len() - cursor.remaining();
        Some((
            status,
            msduck_tds::tvp::decode_exact(&bytes[offset..], Limits::default()).unwrap(),
        ))
    }
    fn apply(db: &Connection, bytes: &[u8], limits: Limits) -> Result<BoundTvp, Error> {
        match raw_input(bytes) {
            None => bind(db, &context(), Input::Omitted, limits),
            Some((status, value)) => bind(
                db,
                &context(),
                Input::Parameter {
                    status,
                    value: &value,
                },
                limits,
            ),
        }
    }
    fn compare_errors(error: Error, reference: &Json) {
        let Error::Diagnostics(actual) = error else {
            panic!("unexpected {error:?}")
        };
        let expected = reference.as_array().unwrap();
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!(
                actual.error.number,
                expected["number"].as_i64().unwrap() as i32
            );
            assert_eq!(
                actual.error.state,
                expected["state"].as_u64().unwrap() as u8
            );
            assert_eq!(
                actual.error.severity,
                expected["class"].as_u64().unwrap() as u8
            );
            assert_eq!(actual.error.message, expected["message"].as_str().unwrap());
            if let Some(line) = expected.get("lineNumber") {
                assert_eq!(actual.line_number, line.as_u64().unwrap() as u32);
            }
        }
    }
    #[test]
    fn all_retained_rpc_bytes_bind_to_catalog_without_writes() {
        let (_server, db) = setup();
        let mut cases = 0;
        for bytes in [
            include_str!("../reference/tvp-wire.json"),
            include_str!("../reference/tvp-default.json"),
            include_str!("../reference/tvp-binding.json"),
        ] {
            let fixture: Json = serde_json::from_str(bytes).unwrap();
            for run in fixture["runs"].as_array().unwrap() {
                for observation in run["observations"].as_array().unwrap() {
                    let name = observation["name"].as_str().unwrap();
                    let bytes = hex(observation["request"]["payloadHex"].as_str().unwrap());
                    let actual = apply(&db, &bytes, Limits::default());
                    let errors = observation
                        .get("response")
                        .or_else(|| observation.get("callback"))
                        .expect("missing captured response")
                        .get("errors")
                        .expect("missing captured errors");
                    if !errors.as_array().unwrap().is_empty() {
                        compare_errors(actual.unwrap_err(), errors);
                    } else {
                        let actual = actual.unwrap();
                        assert_eq!(actual.table_type.schema, "dbo");
                        assert_eq!(actual.table_type.name, "TvpProbe");
                        assert_eq!(actual.table_type.columns.len(), 3);
                        assert_eq!(actual.table_type.columns[0].kind, ColumnKind::Int);
                        assert_eq!(
                            actual.table_type.columns[1].kind,
                            ColumnKind::Nvarchar {
                                max_units: 10,
                                collation: "SQL_Latin1_General_CP1_CI_AS".into()
                            }
                        );
                        assert_eq!(
                            actual.table_type.columns[2].kind,
                            ColumnKind::Varbinary { max_bytes: 10 }
                        );
                        let expected_rows = observation["request"]["tvp"]["rows"]
                            .as_array()
                            .or_else(|| {
                                observation["request"]["parameter"]["tvp"]["rows"].as_array()
                            });
                        if let Some(rows) = expected_rows {
                            assert_eq!(actual.rows.len(), rows.len(), "{name}");
                            for (actual, expected) in actual.rows.iter().zip(rows) {
                                for (actual, expected) in
                                    actual.iter().zip(expected.as_array().unwrap())
                                {
                                    if expected["null"] == true {
                                        assert_eq!(actual, &OwnedCell::Null);
                                        continue;
                                    }
                                    let bytes = hex(expected["valueHex"].as_str().unwrap());
                                    match actual {
                                        OwnedCell::Int(value) => {
                                            let reference = if bytes.len() == 4 {
                                                i64::from(i32::from_le_bytes(
                                                    bytes.try_into().unwrap(),
                                                ))
                                            } else {
                                                i64::from_le_bytes(bytes.try_into().unwrap())
                                            };
                                            assert_eq!(i64::from(*value), reference);
                                        }
                                        OwnedCell::Nvarchar(units) => assert_eq!(
                                            units
                                                .iter()
                                                .flat_map(|u| u.to_le_bytes())
                                                .collect::<Vec<_>>(),
                                            bytes
                                        ),
                                        OwnedCell::Varbinary(value) => assert_eq!(value, &bytes),
                                        OwnedCell::Null => panic!("fabricated NULL"),
                                    }
                                }
                            }
                        } else {
                            assert!(actual.rows.is_empty());
                        }
                        let supply = match name {
                            "omitted TVP" => Supply::Omitted,
                            "default-bit null TVP" => Supply::DefaultBit,
                            _ => Supply::Explicit,
                        };
                        assert_eq!(actual.supply, supply);
                    }
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 80);
        let count: i64 = db
            .query_row("SELECT count(*) FROM sys.table_types", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn malformed_metadata_limits_raw_utf16_and_empty_cells_are_preserved() {
        let (_server, db) = setup();
        let fixture: Json =
            serde_json::from_str(include_str!("../reference/tvp-wire.json")).unwrap();
        let observation = &fixture["runs"][0]["observations"][3];
        let bytes = hex(observation["request"]["payloadHex"].as_str().unwrap());
        let (_, mut value) = raw_input(&bytes).unwrap();
        let Value::Table { rows, .. } = &mut value.value else {
            panic!()
        };
        let raw_surrogates = [0x00, 0xd8, 0x01, 0xdc, 0xff, 0xdb];
        rows[0][1] = Cell::Bytes(&raw_surrogates);
        let expected = vec![0xd800, 0xdc01, 0xdbff];
        let result = bind(
            &db,
            &context(),
            Input::Parameter {
                status: 0,
                value: &value,
            },
            Limits::default(),
        )
        .unwrap();
        assert_eq!(result.rows[0][1], OwnedCell::Nvarchar(expected));
        assert_eq!(result.rows[1][1], OwnedCell::Null);
        assert_eq!(result.rows[2][2], OwnedCell::Varbinary(Vec::new()));
        let limited = Limits {
            max_rows: 2,
            ..Limits::default()
        };
        assert!(matches!(
            bind(
                &db,
                &context(),
                Input::Parameter {
                    status: 0,
                    value: &value
                },
                limited
            ),
            Err(Error::Limit(_))
        ));
        let limited = Limits {
            max_input_bytes: 1,
            ..Limits::default()
        };
        assert!(matches!(
            bind(
                &db,
                &context(),
                Input::Parameter {
                    status: 0,
                    value: &value
                },
                limited
            ),
            Err(Error::Limit(_))
        ));
        let Value::Table { rows, .. } = &mut value.value else {
            panic!()
        };
        rows[0].pop();
        assert!(matches!(
            bind(
                &db,
                &context(),
                Input::Parameter {
                    status: 0,
                    value: &value
                },
                Limits::default()
            ),
            Err(Error::Malformed(_))
        ));
        let Value::Table { columns, .. } = &mut value.value else {
            panic!()
        };
        columns[0].flags = 0x200;
        assert!(matches!(
            bind(
                &db,
                &context(),
                Input::Parameter {
                    status: 0,
                    value: &value
                },
                Limits::default()
            ),
            Err(Error::Unsupported(_))
        ));
        let null = Tvp {
            type_name: msduck_tds::tvp::TypeName {
                schema: Vec::new(),
                name: Vec::new(),
            },
            value: Value::Null,
        };
        let strict = Context {
            default_profile: DefaultProfile::Specification,
            ..context()
        };
        assert!(matches!(
            bind(
                &db,
                &strict,
                Input::Parameter {
                    status: 2,
                    value: &null
                },
                Limits::default()
            ),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn catalog_rollbacks_reopen_and_bound_names_keep_identity() {
        let directory = std::env::temp_dir().join(format!(
            "msduck-tvp-binding-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("server.duckdb");
        {
            let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
            let db = server.connection().unwrap();
            db.execute_batch("BEGIN TRANSACTION").unwrap();
            create(&db, "dbo", "TvpProbe", false);
            let within = bind(&db, &context(), Input::Omitted, Limits::default()).unwrap();
            db.execute_batch("ROLLBACK").unwrap();
            assert!(matches!(
                bind(&db, &context(), Input::Omitted, Limits::default()),
                Err(Error::Diagnostics(_))
            ));
            create(&db, "dbo", "TvpProbe", true);
            let restored = bind(&db, &context(), Input::Omitted, Limits::default()).unwrap();
            assert_ne!(
                within.table_type.user_type_id,
                restored.table_type.user_type_id
            );
            let malicious = Context {
                type_name: "TvpProbe' OR 1=1 --",
                ..context()
            };
            assert!(matches!(
                bind(&db, &malicious, Input::Omitted, Limits::default()),
                Err(Error::Diagnostics(_))
            ));
        }
        {
            let server = crate::server::Server::open(path.to_str().unwrap()).unwrap();
            let db = server.connection().unwrap();
            let result = bind(&db, &context(), Input::Omitted, Limits::default()).unwrap();
            assert_eq!(result.table_type.columns.len(), 3);
            assert_eq!(result.table_type.columns[0].name, "id");
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
