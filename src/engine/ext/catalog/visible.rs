//! What user queries see of the catalog.
//!
//! - Temporary objects live in backend tables of the current database
//!   (`__msduck_temp_*`, `__msduck_tv_*`, `__msduck_global_*`), which other
//!   features find through `sys.objects` and `OBJECT_ID`. SQL Server shows
//!   them only in tempdb, so a user query's `sys.objects`, `sys.all_objects`
//!   and `sys.tables` read a derived table without them. The derived table
//!   still selects from the system view, so result metadata is unchanged.
//! - `INFORMATION_SCHEMA` views are SQL Server's, not DuckDB's: a reference
//!   becomes a derived table over msduck's catalog with SQL Server's column
//!   names and declared types, and the current database's name.
use crate::engine::Session;
use anyhow::Result;
use sqlparser::ast::*;
use std::ops::ControlFlow;

/// The alias that marks a system view this rewrite already filtered.
const FILTERED: &str = "__msduck_visible";

/// Base rows of the INFORMATION_SCHEMA views, without catalog names.
const BASE: &str = "
CREATE OR REPLACE MACRO main.__msduck_is_type(name,max_length,prec,scale) AS {
  'character_maximum_length': CAST(CASE WHEN name IN ('char','varchar','binary','varbinary') THEN max_length
      WHEN name IN ('nchar','nvarchar') THEN CASE WHEN max_length=-1 THEN -1 ELSE max_length//2 END
      WHEN name IN ('text','image') THEN 2147483647 WHEN name='ntext' THEN 1073741823 WHEN name='xml' THEN -1 END AS INTEGER),
  'character_octet_length': CAST(CASE WHEN name IN ('char','varchar','binary','varbinary','nchar','nvarchar') THEN max_length
      WHEN name IN ('text','image') THEN 2147483647 WHEN name='ntext' THEN 2147483646 WHEN name='xml' THEN -1 END AS INTEGER),
  'numeric_precision': CAST(CASE WHEN name IN ('tinyint','smallint','int','bigint','decimal','numeric','float','real','money','smallmoney') THEN prec END AS UTINYINT),
  'numeric_precision_radix': CAST(CASE WHEN name IN ('float','real') THEN 2
      WHEN name IN ('tinyint','smallint','int','bigint','decimal','numeric','money','smallmoney') THEN 10 END AS SMALLINT),
  'numeric_scale': CAST(CASE WHEN name IN ('tinyint','smallint','int','bigint','decimal','numeric','money','smallmoney') THEN scale END AS INTEGER),
  'datetime_precision': CAST(CASE name WHEN 'datetime' THEN 3 WHEN 'smalldatetime' THEN 0 WHEN 'date' THEN 0
      WHEN 'time' THEN scale WHEN 'datetime2' THEN scale WHEN 'datetimeoffset' THEN scale END AS SMALLINT),
  'character_set_name': CASE WHEN name IN ('char','varchar','text') THEN 'iso_1'
      WHEN name IN ('nchar','nvarchar','ntext') THEN 'UNICODE' END,
  'character': name IN ('char','varchar','text','nchar','nvarchar','ntext')};
CREATE OR REPLACE VIEW main.__msduck_is_objects AS
  SELECT o.object_id,s.name AS schema_name,o.name,o.type_code FROM main.__msduck_objects o
  JOIN main.__msduck_schemas s USING(schema_id) WHERE NOT main.__msduck_temporary(o.name);
CREATE OR REPLACE VIEW main.__msduck_is_tables AS
  SELECT schema_name AS TABLE_SCHEMA,name AS TABLE_NAME,
    CASE type_code WHEN 'U' THEN 'BASE TABLE' ELSE 'VIEW' END AS TABLE_TYPE
  FROM main.__msduck_is_objects;
CREATE OR REPLACE VIEW main.__msduck_is_columns AS
  SELECT o.schema_name AS TABLE_SCHEMA,o.name AS TABLE_NAME,c.name AS COLUMN_NAME,c.column_id AS ORDINAL_POSITION,
    d.definition AS COLUMN_DEFAULT,CASE WHEN c.is_nullable THEN 'YES' ELSE 'NO' END AS IS_NULLABLE,
    b.name AS DATA_TYPE,f.character_maximum_length AS CHARACTER_MAXIMUM_LENGTH,
    f.character_octet_length AS CHARACTER_OCTET_LENGTH,f.numeric_precision AS NUMERIC_PRECISION,
    f.numeric_precision_radix AS NUMERIC_PRECISION_RADIX,f.numeric_scale AS NUMERIC_SCALE,
    f.datetime_precision AS DATETIME_PRECISION,f.character_set_name AS CHARACTER_SET_NAME,
    CASE WHEN f.character THEN c.collation_name END AS COLLATION_NAME,
    CASE WHEN t.is_user_defined THEN ts.name END AS DOMAIN_SCHEMA,
    CASE WHEN t.is_user_defined THEN t.name END AS DOMAIN_NAME
  FROM sys.columns c JOIN main.__msduck_is_objects o ON o.object_id=c.object_id
  LEFT JOIN sys.types t ON t.user_type_id=c.user_type_id
  LEFT JOIN sys.schemas ts ON ts.schema_id=t.schema_id
  LEFT JOIN sys.types b ON b.user_type_id=c.system_type_id
  LEFT JOIN sys.default_constraints d ON d.parent_object_id=c.object_id AND d.parent_column_id=c.column_id
  CROSS JOIN LATERAL (SELECT main.__msduck_is_type(b.name,c.max_length,c.precision,c.scale) AS f);
CREATE OR REPLACE VIEW main.__msduck_is_views AS
  SELECT o.schema_name AS TABLE_SCHEMA,o.name AS TABLE_NAME,left(m.definition,4000) AS VIEW_DEFINITION,
    'NONE' AS CHECK_OPTION,'NO' AS IS_UPDATABLE
  FROM main.__msduck_is_objects o LEFT JOIN sys.sql_modules m ON m.object_id=o.object_id WHERE o.type_code='V';
CREATE OR REPLACE VIEW main.__msduck_is_schemata AS
  SELECT name AS SCHEMA_NAME,name AS SCHEMA_OWNER,'iso_1' AS DEFAULT_CHARACTER_SET_NAME FROM sys.schemas;
CREATE OR REPLACE VIEW main.__msduck_is_constraints AS
  SELECT c.object_id,c.parent_object_id,s.name AS schema_name,c.name,o.schema_name AS table_schema,o.name AS table_name,c.type_code,
    c.columns,c.referenced_object_id,c.referenced_columns,c.delete_action,c.update_action,c.definition
  FROM main.__msduck_live_constraints c JOIN main.__msduck_is_objects o ON o.object_id=c.parent_object_id
  JOIN main.__msduck_schemas s ON s.schema_id=c.schema_id;
CREATE OR REPLACE VIEW main.__msduck_is_key_columns AS
  SELECT c.schema_name,c.name,c.table_schema,c.table_name,k.name AS column_name,CAST(k.ordinal AS INTEGER) AS ordinal
  FROM main.__msduck_is_constraints c
  JOIN main.__msduck_keys mk ON c.type_code IN ('PK','UQ') AND mk.tag=CAST(c.object_id AS BIGINT)-2000000000
  CROSS JOIN LATERAL (SELECT unnest(from_json(mk.key_columns,'[\"VARCHAR\"]')) AS name,
    unnest(generate_series(1,len(from_json(mk.key_columns,'[\"VARCHAR\"]')))) AS ordinal) k
  UNION ALL
  SELECT c.schema_name,c.name,c.table_schema,c.table_name,k.name,CAST(k.ordinal AS INTEGER)
  FROM main.__msduck_is_constraints c
  CROSS JOIN LATERAL (SELECT unnest(c.columns) AS name,unnest(generate_series(1,len(c.columns))) AS ordinal) k
  WHERE c.type_code='F';
CREATE OR REPLACE VIEW main.__msduck_is_table_constraints AS
  SELECT schema_name AS CONSTRAINT_SCHEMA,name AS CONSTRAINT_NAME,table_schema AS TABLE_SCHEMA,table_name AS TABLE_NAME,
    CASE type_code WHEN 'PK' THEN 'PRIMARY KEY' WHEN 'UQ' THEN 'UNIQUE' WHEN 'F' THEN 'FOREIGN KEY' ELSE 'CHECK' END AS CONSTRAINT_TYPE,
    'NO' AS IS_DEFERRABLE,'NO' AS INITIALLY_DEFERRED
  FROM main.__msduck_is_constraints;
CREATE OR REPLACE VIEW main.__msduck_is_key_column_usage AS
  SELECT schema_name AS CONSTRAINT_SCHEMA,name AS CONSTRAINT_NAME,table_schema AS TABLE_SCHEMA,table_name AS TABLE_NAME,
    column_name AS COLUMN_NAME,ordinal AS ORDINAL_POSITION
  FROM main.__msduck_is_key_columns;
CREATE OR REPLACE VIEW main.__msduck_is_constraint_column_usage AS
  SELECT table_schema AS TABLE_SCHEMA,table_name AS TABLE_NAME,column_name AS COLUMN_NAME,
    schema_name AS CONSTRAINT_SCHEMA,name AS CONSTRAINT_NAME
  FROM main.__msduck_is_key_columns
  UNION ALL
  SELECT c.table_schema,c.table_name,k.name,c.schema_name,c.name
  FROM main.__msduck_is_constraints c CROSS JOIN LATERAL (SELECT unnest(c.columns) AS name) k
  WHERE c.type_code='C';
CREATE OR REPLACE VIEW main.__msduck_is_referential_constraints AS
  SELECT f.schema_name AS CONSTRAINT_SCHEMA,f.name AS CONSTRAINT_NAME,k.schema_name AS UNIQUE_CONSTRAINT_SCHEMA,
    k.name AS UNIQUE_CONSTRAINT_NAME,'SIMPLE' AS MATCH_OPTION,
    CASE f.update_action WHEN 1 THEN 'CASCADE' WHEN 2 THEN 'SET NULL' WHEN 3 THEN 'SET DEFAULT' ELSE 'NO ACTION' END AS UPDATE_RULE,
    CASE f.delete_action WHEN 1 THEN 'CASCADE' WHEN 2 THEN 'SET NULL' WHEN 3 THEN 'SET DEFAULT' ELSE 'NO ACTION' END AS DELETE_RULE
  FROM main.__msduck_is_constraints f
  LEFT JOIN (SELECT c.*,mk.key_columns AS key_text FROM main.__msduck_is_constraints c
      JOIN main.__msduck_keys mk ON mk.tag=CAST(c.object_id AS BIGINT)-2000000000 WHERE c.type_code IN ('PK','UQ')) k
    ON k.parent_object_id=f.referenced_object_id
    AND list_sort(list_transform(from_json(k.key_text,'[\"VARCHAR\"]'),lambda x: lower(x)))
      =list_sort(list_transform(f.referenced_columns,lambda x: lower(x)))
  WHERE f.type_code='F';
CREATE OR REPLACE VIEW main.__msduck_is_check_constraints AS
  SELECT c.schema_name AS CONSTRAINT_SCHEMA,c.name AS CONSTRAINT_NAME,d.definition AS CHECK_CLAUSE
  FROM main.__msduck_is_constraints c LEFT JOIN main.__msduck_check_definitions d ON d.object_id=c.object_id
  WHERE c.type_code='C';
CREATE OR REPLACE VIEW main.__msduck_is_routine_parameters AS
  SELECT p.object_id,p.parameter_id,p.name,p.is_output,b.name AS data_type,p.max_length,p.precision,p.scale,
    t.is_user_defined,t.name AS type_name,ts.name AS type_schema
  FROM sys.parameters p
  LEFT JOIN sys.types t ON t.user_type_id=p.user_type_id
  LEFT JOIN sys.schemas ts ON ts.schema_id=t.schema_id
  LEFT JOIN sys.types b ON b.user_type_id=p.system_type_id;
CREATE OR REPLACE VIEW main.__msduck_is_routines AS
  SELECT s.name AS SPECIFIC_SCHEMA,m.name AS SPECIFIC_NAME,s.name AS ROUTINE_SCHEMA,m.name AS ROUTINE_NAME,
    CASE m.type_code WHEN 'P' THEN 'PROCEDURE' ELSE 'FUNCTION' END AS ROUTINE_TYPE,
    CASE WHEN m.type_code IN ('IF','TF') THEN 'TABLE' ELSE r.data_type END AS DATA_TYPE,
    f.character_maximum_length AS CHARACTER_MAXIMUM_LENGTH,f.character_octet_length AS CHARACTER_OCTET_LENGTH,
    CASE WHEN f.character THEN 'SQL_Latin1_General_CP1_CI_AS' END AS COLLATION_NAME,
    f.character_set_name AS CHARACTER_SET_NAME,f.numeric_precision AS NUMERIC_PRECISION,
    f.numeric_precision_radix AS NUMERIC_PRECISION_RADIX,f.numeric_scale AS NUMERIC_SCALE,
    f.datetime_precision AS DATETIME_PRECISION,'SQL' AS ROUTINE_BODY,left(m.definition,4000) AS ROUTINE_DEFINITION,
    'NO' AS IS_DETERMINISTIC,CASE m.type_code WHEN 'P' THEN 'MODIFIES' ELSE 'READS' END AS SQL_DATA_ACCESS,
    CASE WHEN m.type_code='P' THEN NULL ELSE 'NO' END AS IS_NULL_CALL,'YES' AS SCHEMA_LEVEL_ROUTINE,
    CAST(CASE m.type_code WHEN 'P' THEN -1 ELSE 0 END AS SMALLINT) AS MAX_DYNAMIC_RESULT_SETS,
    'NO' AS IS_USER_DEFINED_CAST,'NO' AS IS_IMPLICITLY_INVOCABLE,m.create_date AS CREATED,m.modify_date AS LAST_ALTERED
  FROM main.__msduck_modules m JOIN main.__msduck_schemas s USING(schema_id)
  LEFT JOIN main.__msduck_is_routine_parameters r ON r.object_id=m.object_id AND r.parameter_id=0
  CROSS JOIN LATERAL (SELECT main.__msduck_is_type(r.data_type,r.max_length,r.precision,r.scale) AS f)
  WHERE m.type_code IN ('P','FN','IF','TF');
CREATE OR REPLACE VIEW main.__msduck_is_parameters AS
  SELECT s.name AS SPECIFIC_SCHEMA,m.name AS SPECIFIC_NAME,p.parameter_id AS ORDINAL_POSITION,
    CASE WHEN p.parameter_id=0 THEN 'OUT' WHEN p.is_output THEN 'INOUT' ELSE 'IN' END AS PARAMETER_MODE,
    CASE WHEN p.parameter_id=0 THEN 'YES' ELSE 'NO' END AS IS_RESULT,'NO' AS AS_LOCATOR,p.name AS PARAMETER_NAME,
    p.data_type AS DATA_TYPE,f.character_maximum_length AS CHARACTER_MAXIMUM_LENGTH,
    f.character_octet_length AS CHARACTER_OCTET_LENGTH,
    CASE WHEN f.character THEN 'SQL_Latin1_General_CP1_CI_AS' END AS COLLATION_NAME,
    f.character_set_name AS CHARACTER_SET_NAME,f.numeric_precision AS NUMERIC_PRECISION,
    f.numeric_precision_radix AS NUMERIC_PRECISION_RADIX,f.numeric_scale AS NUMERIC_SCALE,
    f.datetime_precision AS DATETIME_PRECISION,
    CASE WHEN p.is_user_defined THEN p.type_schema END AS USER_DEFINED_TYPE_SCHEMA,
    CASE WHEN p.is_user_defined THEN p.type_name END AS USER_DEFINED_TYPE_NAME
  FROM main.__msduck_is_routine_parameters p JOIN main.__msduck_modules m ON m.object_id=p.object_id
  JOIN main.__msduck_schemas s ON s.schema_id=m.schema_id
  CROSS JOIN LATERAL (SELECT main.__msduck_is_type(p.data_type,p.max_length,p.precision,p.scale) AS f);
";

pub(super) fn bootstrap(db: &duckdb::Connection) -> Result<()> {
    db.execute_batch(BASE)
        .map_err(|error| anyhow::anyhow!("INFORMATION_SCHEMA bootstrap: {error}"))
}

/// How an INFORMATION_SCHEMA column is produced.
#[derive(Clone, Copy)]
enum Source {
    /// The current database's name.
    Catalog,
    /// The database's name when the given base column is not NULL.
    CatalogIf(&'static str),
    /// The base view's column of the same name.
    Column,
    Null,
}

use Source::{Catalog, CatalogIf, Column, Null};

const SYSNAME: &str = "NVARCHAR(128)";

/// A view column: name, declared type and source.
type Declared = (&'static str, &'static str, Source);

/// SQL Server's columns of each view, and the base view providing them.
fn columns(view: &str) -> Option<(&'static str, Vec<Declared>)> {
    let name = |column: &'static str| (column, SYSNAME, Column);
    let catalog = |column: &'static str| (column, SYSNAME, Catalog);
    let null = |column: &'static str, kind: &'static str| (column, kind, Null);
    Some(match view.to_ascii_uppercase().as_str() {
        "TABLES" => (
            "__msduck_is_tables",
            vec![
                catalog("TABLE_CATALOG"),
                name("TABLE_SCHEMA"),
                name("TABLE_NAME"),
                ("TABLE_TYPE", "VARCHAR(10)", Column),
            ],
        ),
        "COLUMNS" => (
            "__msduck_is_columns",
            vec![
                catalog("TABLE_CATALOG"),
                name("TABLE_SCHEMA"),
                name("TABLE_NAME"),
                name("COLUMN_NAME"),
                ("ORDINAL_POSITION", "INT", Column),
                ("COLUMN_DEFAULT", "NVARCHAR(4000)", Column),
                ("IS_NULLABLE", "VARCHAR(3)", Column),
                name("DATA_TYPE"),
                ("CHARACTER_MAXIMUM_LENGTH", "INT", Column),
                ("CHARACTER_OCTET_LENGTH", "INT", Column),
                ("NUMERIC_PRECISION", "TINYINT", Column),
                ("NUMERIC_PRECISION_RADIX", "SMALLINT", Column),
                ("NUMERIC_SCALE", "INT", Column),
                ("DATETIME_PRECISION", "SMALLINT", Column),
                null("CHARACTER_SET_CATALOG", SYSNAME),
                null("CHARACTER_SET_SCHEMA", SYSNAME),
                name("CHARACTER_SET_NAME"),
                null("COLLATION_CATALOG", SYSNAME),
                null("COLLATION_SCHEMA", SYSNAME),
                name("COLLATION_NAME"),
                ("DOMAIN_CATALOG", SYSNAME, CatalogIf("DOMAIN_NAME")),
                name("DOMAIN_SCHEMA"),
                name("DOMAIN_NAME"),
            ],
        ),
        "VIEWS" => (
            "__msduck_is_views",
            vec![
                catalog("TABLE_CATALOG"),
                name("TABLE_SCHEMA"),
                name("TABLE_NAME"),
                ("VIEW_DEFINITION", "NVARCHAR(4000)", Column),
                ("CHECK_OPTION", "VARCHAR(7)", Column),
                ("IS_UPDATABLE", "VARCHAR(2)", Column),
            ],
        ),
        "SCHEMATA" => (
            "__msduck_is_schemata",
            vec![
                catalog("CATALOG_NAME"),
                name("SCHEMA_NAME"),
                name("SCHEMA_OWNER"),
                null("DEFAULT_CHARACTER_SET_CATALOG", SYSNAME),
                null("DEFAULT_CHARACTER_SET_SCHEMA", SYSNAME),
                name("DEFAULT_CHARACTER_SET_NAME"),
            ],
        ),
        "TABLE_CONSTRAINTS" => (
            "__msduck_is_table_constraints",
            vec![
                catalog("CONSTRAINT_CATALOG"),
                name("CONSTRAINT_SCHEMA"),
                name("CONSTRAINT_NAME"),
                catalog("TABLE_CATALOG"),
                name("TABLE_SCHEMA"),
                name("TABLE_NAME"),
                ("CONSTRAINT_TYPE", "VARCHAR(11)", Column),
                ("IS_DEFERRABLE", "VARCHAR(2)", Column),
                ("INITIALLY_DEFERRED", "VARCHAR(2)", Column),
            ],
        ),
        "KEY_COLUMN_USAGE" => (
            "__msduck_is_key_column_usage",
            vec![
                catalog("CONSTRAINT_CATALOG"),
                name("CONSTRAINT_SCHEMA"),
                name("CONSTRAINT_NAME"),
                catalog("TABLE_CATALOG"),
                name("TABLE_SCHEMA"),
                name("TABLE_NAME"),
                name("COLUMN_NAME"),
                ("ORDINAL_POSITION", "INT", Column),
            ],
        ),
        "CONSTRAINT_COLUMN_USAGE" => (
            "__msduck_is_constraint_column_usage",
            vec![
                catalog("TABLE_CATALOG"),
                name("TABLE_SCHEMA"),
                name("TABLE_NAME"),
                name("COLUMN_NAME"),
                catalog("CONSTRAINT_CATALOG"),
                name("CONSTRAINT_SCHEMA"),
                name("CONSTRAINT_NAME"),
            ],
        ),
        "REFERENTIAL_CONSTRAINTS" => (
            "__msduck_is_referential_constraints",
            vec![
                catalog("CONSTRAINT_CATALOG"),
                name("CONSTRAINT_SCHEMA"),
                name("CONSTRAINT_NAME"),
                (
                    "UNIQUE_CONSTRAINT_CATALOG",
                    SYSNAME,
                    CatalogIf("UNIQUE_CONSTRAINT_NAME"),
                ),
                name("UNIQUE_CONSTRAINT_SCHEMA"),
                name("UNIQUE_CONSTRAINT_NAME"),
                ("MATCH_OPTION", "VARCHAR(7)", Column),
                ("UPDATE_RULE", "VARCHAR(11)", Column),
                ("DELETE_RULE", "VARCHAR(11)", Column),
            ],
        ),
        "CHECK_CONSTRAINTS" => (
            "__msduck_is_check_constraints",
            vec![
                catalog("CONSTRAINT_CATALOG"),
                name("CONSTRAINT_SCHEMA"),
                name("CONSTRAINT_NAME"),
                ("CHECK_CLAUSE", "NVARCHAR(4000)", Column),
            ],
        ),
        "ROUTINES" => (
            "__msduck_is_routines",
            vec![
                catalog("SPECIFIC_CATALOG"),
                ("SPECIFIC_SCHEMA", SYSNAME, Source::Column),
                ("SPECIFIC_NAME", SYSNAME, Source::Column),
                catalog("ROUTINE_CATALOG"),
                name("ROUTINE_SCHEMA"),
                name("ROUTINE_NAME"),
                ("ROUTINE_TYPE", "NVARCHAR(20)", Column),
                null("MODULE_CATALOG", SYSNAME),
                null("MODULE_SCHEMA", SYSNAME),
                null("MODULE_NAME", SYSNAME),
                null("UDT_CATALOG", SYSNAME),
                null("UDT_SCHEMA", SYSNAME),
                null("UDT_NAME", SYSNAME),
                name("DATA_TYPE"),
                ("CHARACTER_MAXIMUM_LENGTH", "INT", Column),
                ("CHARACTER_OCTET_LENGTH", "INT", Column),
                null("COLLATION_CATALOG", SYSNAME),
                null("COLLATION_SCHEMA", SYSNAME),
                name("COLLATION_NAME"),
                null("CHARACTER_SET_CATALOG", SYSNAME),
                null("CHARACTER_SET_SCHEMA", SYSNAME),
                name("CHARACTER_SET_NAME"),
                ("NUMERIC_PRECISION", "TINYINT", Column),
                ("NUMERIC_PRECISION_RADIX", "SMALLINT", Column),
                ("NUMERIC_SCALE", "INT", Column),
                ("DATETIME_PRECISION", "SMALLINT", Column),
                null("INTERVAL_TYPE", "NVARCHAR(30)"),
                null("INTERVAL_PRECISION", "SMALLINT"),
                null("TYPE_UDT_CATALOG", SYSNAME),
                null("TYPE_UDT_SCHEMA", SYSNAME),
                null("TYPE_UDT_NAME", SYSNAME),
                null("SCOPE_CATALOG", SYSNAME),
                null("SCOPE_SCHEMA", SYSNAME),
                null("SCOPE_NAME", SYSNAME),
                null("MAXIMUM_CARDINALITY", "BIGINT"),
                null("DTD_IDENTIFIER", SYSNAME),
                ("ROUTINE_BODY", "NVARCHAR(30)", Column),
                ("ROUTINE_DEFINITION", "NVARCHAR(4000)", Column),
                null("EXTERNAL_NAME", SYSNAME),
                null("EXTERNAL_LANGUAGE", "NVARCHAR(30)"),
                null("PARAMETER_STYLE", "NVARCHAR(30)"),
                ("IS_DETERMINISTIC", "NVARCHAR(10)", Column),
                ("SQL_DATA_ACCESS", "NVARCHAR(30)", Column),
                ("IS_NULL_CALL", "NVARCHAR(10)", Column),
                null("SQL_PATH", SYSNAME),
                ("SCHEMA_LEVEL_ROUTINE", "NVARCHAR(10)", Column),
                ("MAX_DYNAMIC_RESULT_SETS", "SMALLINT", Column),
                ("IS_USER_DEFINED_CAST", "NVARCHAR(10)", Column),
                ("IS_IMPLICITLY_INVOCABLE", "NVARCHAR(10)", Column),
                ("CREATED", "DATETIME", Column),
                ("LAST_ALTERED", "DATETIME", Column),
            ],
        ),
        "PARAMETERS" => (
            "__msduck_is_parameters",
            vec![
                catalog("SPECIFIC_CATALOG"),
                name("SPECIFIC_SCHEMA"),
                name("SPECIFIC_NAME"),
                ("ORDINAL_POSITION", "INT", Column),
                ("PARAMETER_MODE", "NVARCHAR(10)", Column),
                ("IS_RESULT", "NVARCHAR(10)", Column),
                ("AS_LOCATOR", "NVARCHAR(10)", Column),
                name("PARAMETER_NAME"),
                name("DATA_TYPE"),
                ("CHARACTER_MAXIMUM_LENGTH", "INT", Column),
                ("CHARACTER_OCTET_LENGTH", "INT", Column),
                null("COLLATION_CATALOG", SYSNAME),
                null("COLLATION_SCHEMA", SYSNAME),
                name("COLLATION_NAME"),
                null("CHARACTER_SET_CATALOG", SYSNAME),
                null("CHARACTER_SET_SCHEMA", SYSNAME),
                name("CHARACTER_SET_NAME"),
                ("NUMERIC_PRECISION", "TINYINT", Column),
                ("NUMERIC_PRECISION_RADIX", "SMALLINT", Column),
                ("NUMERIC_SCALE", "INT", Column),
                ("DATETIME_PRECISION", "SMALLINT", Column),
                null("INTERVAL_TYPE", "NVARCHAR(30)"),
                null("INTERVAL_PRECISION", "SMALLINT"),
                (
                    "USER_DEFINED_TYPE_CATALOG",
                    SYSNAME,
                    CatalogIf("USER_DEFINED_TYPE_NAME"),
                ),
                name("USER_DEFINED_TYPE_SCHEMA"),
                name("USER_DEFINED_TYPE_NAME"),
                null("SCOPE_CATALOG", SYSNAME),
                null("SCOPE_SCHEMA", SYSNAME),
                null("SCOPE_NAME", SYSNAME),
            ],
        ),
        _ => return None,
    })
}

fn bracket(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

fn literal(text: &str) -> String {
    format!("N'{}'", text.replace('\'', "''"))
}

/// The derived table that replaces `INFORMATION_SCHEMA.<view>`.
fn information_schema(
    view: &str,
    database: &str,
    alias: Option<TableAlias>,
) -> Option<TableFactor> {
    let (base, columns) = columns(view)?;
    let items = columns
        .iter()
        .map(|(name, kind, source)| {
            let value = match source {
                Catalog => literal(database),
                CatalogIf(column) => format!(
                    "CASE WHEN {} IS NOT NULL THEN {} END",
                    bracket(column),
                    literal(database)
                ),
                Column => bracket(name),
                Null => "NULL".to_owned(),
            };
            format!("CAST({value} AS {kind}) AS {}", bracket(name))
        })
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT {items} FROM main.{base}");
    let mut statements = msduck_sql::batch::parse(&sql).ok()?;
    let Statement::Query(query) = statements.pop()? else {
        return None;
    };
    Some(TableFactor::Derived {
        lateral: false,
        subquery: query,
        alias: Some(alias.unwrap_or_else(|| TableAlias {
            explicit: true,
            name: Ident::new(view.to_ascii_uppercase()),
            columns: vec![],
            at: None,
        })),
        sample: None,
    })
}

fn temporary(column: &str) -> String {
    format!(
        "({column} LIKE '\\_\\_msduck\\_temp\\_%' ESCAPE '\\' OR {column} LIKE '\\_\\_msduck\\_tv\\_%' ESCAPE '\\' OR {column} LIKE '\\_\\_msduck\\_global\\_%' ESCAPE '\\')"
    )
}

/// The rows of `sys.objects`, `sys.all_objects` or `sys.tables` (as
/// `qualifier`) that SQL Server shows outside tempdb: not temporary
/// objects, nor their constraints and triggers.
fn visible_rows(qualifier: &str) -> Option<Expr> {
    let qualifier = bracket(qualifier);
    let sql = format!(
        "SELECT 1 WHERE NOT {} AND {qualifier}.[parent_object_id] NOT IN (SELECT object_id FROM sys.objects AS {FILTERED} WHERE {})",
        temporary(&format!("{qualifier}.[name]")),
        temporary("name")
    );
    let mut statements = msduck_sql::batch::parse(&sql).ok()?;
    let Statement::Query(query) = statements.pop()? else {
        return None;
    };
    let SetExpr::Select(select) = *query.body else {
        return None;
    };
    select.selection
}

/// The derived table that replaces `sys.objects`, `sys.all_objects` or
/// `sys.tables` where a condition cannot be added (the preserved side of
/// an outer join, APPLY, joins without ON): the system view without
/// temporary objects.
fn filtered(view: &str, alias: Option<TableAlias>) -> Option<TableFactor> {
    let sql = format!(
        "SELECT * FROM sys.{view} AS {FILTERED} WHERE NOT {} AND parent_object_id NOT IN (SELECT object_id FROM sys.objects AS {FILTERED} WHERE {})",
        temporary("name"),
        temporary("name")
    );
    let mut statements = msduck_sql::batch::parse(&sql).ok()?;
    let Statement::Query(query) = statements.pop()? else {
        return None;
    };
    Some(TableFactor::Derived {
        lateral: false,
        subquery: query,
        alias: Some(alias.unwrap_or_else(|| TableAlias {
            explicit: true,
            name: Ident::new(view),
            columns: vec![],
            at: None,
        })),
        sample: None,
    })
}

/// The system view a table factor names (`objects`, `all_objects` or
/// `tables`), and the qualifier of its columns.
fn system_view(factor: &TableFactor) -> Option<(String, String)> {
    let TableFactor::Table {
        name,
        alias,
        args: None,
        version: None,
        ..
    } = factor
    else {
        return None;
    };
    if alias
        .as_ref()
        .is_some_and(|alias| alias.name.value == FILTERED)
    {
        return None;
    }
    let parts: Vec<&str> = name
        .0
        .iter()
        .filter_map(|part| part.as_ident().map(|ident| ident.value.as_str()))
        .collect();
    if parts.len() != name.0.len() || parts.len() < 2 {
        return None;
    }
    let view = parts[parts.len() - 1];
    if !parts[parts.len() - 2].eq_ignore_ascii_case("sys")
        || !["objects", "all_objects", "tables"]
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(view))
    {
        return None;
    }
    let qualifier = alias
        .as_ref()
        .map_or_else(|| view.to_owned(), |alias| alias.name.value.clone());
    Some((view.to_ascii_lowercase(), qualifier))
}

/// `left AND right`, unless `left` already holds `right`.
fn conjoin(left: Option<Expr>, right: Expr) -> Expr {
    match left {
        Some(left) if left.to_string().contains(&right.to_string()) => left,
        Some(left) => Expr::BinaryOp {
            left: Box::new(Expr::Nested(Box::new(left))),
            op: BinaryOperator::And,
            right: Box::new(right),
        },
        None => right,
    }
}

/// Whether a join keeps the rows of the items before it (RIGHT or FULL),
/// so that a condition on them in WHERE would drop the rows it preserves.
fn preserves_right(join: &Join) -> bool {
    matches!(
        join.join_operator,
        JoinOperator::Right(_)
            | JoinOperator::RightOuter(_)
            | JoinOperator::FullOuter(_)
            | JoinOperator::RightSemi(_)
            | JoinOperator::RightAnti(_)
    )
}

/// Replace a system view by the filtered derived table, keeping its alias;
/// an unaliased view is recorded in `renamed` for `Qualified`.
fn derive(factor: &mut TableFactor, view: &str, renamed: &mut Vec<(String, String)>) {
    let alias = match factor {
        TableFactor::Table { alias, .. } => alias.clone(),
        _ => None,
    };
    if let Some(derived) = filtered(view, alias.clone()) {
        if alias.is_none() {
            renamed.push(("sys".to_owned(), view.to_owned()));
        }
        *factor = derived;
    }
}

/// Whether a factor's alias lists column names, which a condition on the
/// view's own column names cannot use.
fn renames_columns(factor: &TableFactor) -> bool {
    matches!(factor, TableFactor::Table { alias: Some(alias), .. } if !alias.columns.is_empty())
}

/// Filter the system views of one SELECT: a condition in WHERE for a FROM
/// item, in ON for an inner or left-joined one, and a derived table where
/// neither applies (a FROM item that a RIGHT or FULL join preserves, the
/// preserved side of other joins, APPLY, joins without ON). Conditions keep
/// the result's metadata, which a derived table changes.
fn filter_select(select: &mut Select, renamed: &mut Vec<(String, String)>) {
    let mut conditions = Vec::new();
    for item in &mut select.from {
        if let Some((view, qualifier)) = system_view(&item.relation) {
            let condition = (!item.joins.iter().any(preserves_right)
                && !renames_columns(&item.relation))
            .then(|| visible_rows(&qualifier))
            .flatten();
            match condition {
                Some(condition) => conditions.push(condition),
                None => derive(&mut item.relation, &view, renamed),
            }
        }
        for join in &mut item.joins {
            let Some((view, qualifier)) = system_view(&join.relation) else {
                continue;
            };
            let columns = renames_columns(&join.relation);
            let constraint = match &mut join.join_operator {
                JoinOperator::Join(constraint)
                | JoinOperator::Inner(constraint)
                | JoinOperator::Left(constraint)
                | JoinOperator::LeftOuter(constraint) => Some(constraint),
                _ => None,
            };
            match (constraint, visible_rows(&qualifier)) {
                (Some(JoinConstraint::On(on)), Some(condition)) if !columns => {
                    *on = conjoin(Some(on.clone()), condition);
                }
                _ => derive(&mut join.relation, &view, renamed),
            }
        }
    }
    for condition in conditions {
        select.selection = Some(conjoin(select.selection.take(), condition));
    }
}

/// Rewrite a user statement's references to the views above.
pub(super) fn rewrite(session: &Session, statement: &mut Statement) {
    struct Rewrite<'a> {
        database: &'a str,
        /// Views whose unaliased references became derived tables, so that
        /// `sys.objects.name` becomes `objects.name`.
        renamed: Vec<(String, String)>,
    }
    impl VisitorMut for Rewrite<'_> {
        type Break = ();
        fn post_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<()> {
            let TableFactor::Table {
                name,
                alias,
                args: None,
                with_hints,
                version: None,
                partitions,
                ..
            } = factor
            else {
                return ControlFlow::Continue(());
            };
            if !with_hints.is_empty()
                || !partitions.is_empty()
                || alias
                    .as_ref()
                    .is_some_and(|alias| alias.name.value == FILTERED)
            {
                return ControlFlow::Continue(());
            }
            let parts: Vec<&str> = name
                .0
                .iter()
                .filter_map(|part| part.as_ident().map(|ident| ident.value.as_str()))
                .collect();
            if parts.len() != name.0.len() || parts.len() < 2 {
                return ControlFlow::Continue(());
            }
            let schema = parts[parts.len() - 2];
            let view = parts[parts.len() - 1];
            let replacement = if schema.eq_ignore_ascii_case("INFORMATION_SCHEMA") {
                information_schema(view, self.database, alias.clone())
            } else {
                None
            };
            if let Some(replacement) = replacement {
                if alias.is_none() {
                    self.renamed.push((schema.to_owned(), view.to_owned()));
                }
                *factor = replacement;
            }
            ControlFlow::Continue(())
        }
    }
    // A batch that reads tempdb's catalog sees the temporary objects that
    // the temporary tables feature redirected there; it keeps the backend
    // views.
    if session.ext.catalog.tempdb() {
        return;
    }
    struct Selects(Vec<(String, String)>);
    impl VisitorMut for Selects {
        type Break = ();
        fn pre_visit_query(&mut self, query: &mut Query) -> ControlFlow<()> {
            fn body(set: &mut SetExpr, renamed: &mut Vec<(String, String)>) {
                match set {
                    SetExpr::Select(select) => filter_select(select, renamed),
                    SetExpr::SetOperation { left, right, .. } => {
                        body(left, renamed);
                        body(right, renamed);
                    }
                    _ => {}
                }
            }
            body(&mut query.body, &mut self.0);
            ControlFlow::Continue(())
        }
    }
    let mut selects = Selects(Vec::new());
    let _ = statement.visit(&mut selects);
    let mut rewrite = Rewrite {
        database: &session.database.name,
        renamed: selects.0,
    };
    let _ = statement.visit(&mut rewrite);
    if rewrite.renamed.is_empty() {
        return;
    }
    // `sys.objects.name` and `db.sys.objects.name` refer to the derived
    // table by its alias.
    struct Qualified(Vec<(String, String)>);
    impl VisitorMut for Qualified {
        type Break = ();
        fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<()> {
            if let Expr::CompoundIdentifier(parts) = expr
                && parts.len() >= 3
            {
                let at = parts.len() - 3;
                if self.0.iter().any(|(schema, view)| {
                    parts[at].value.eq_ignore_ascii_case(schema)
                        && parts[at + 1].value.eq_ignore_ascii_case(view)
                }) {
                    parts.drain(..=at);
                }
            }
            ControlFlow::Continue(())
        }
    }
    let _ = statement.visit(&mut Qualified(rewrite.renamed));
}
