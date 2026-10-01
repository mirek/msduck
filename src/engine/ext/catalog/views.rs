//! Catalog views over the stores of other features (see docs/gaps-catalog.md).
//!
//! The constraints feature defines `sys.check_constraints`,
//! `sys.foreign_keys`, `sys.foreign_key_columns` and `sys.key_constraints`
//! first; this feature bootstraps after it and redefines the first, second
//! and fourth with SQL Server's definitions and index IDs, keeping their
//! column sets. Rows of temporary objects' backend tables, which SQL Server
//! keeps in tempdb, are left out of every view defined here.
use anyhow::{Context, Result};
use duckdb::Connection;

/// Whether a backend table name belongs to a temporary object (see
/// `temp_tables::storage`).
const TEMPORARY: &str = "CREATE OR REPLACE MACRO main.__msduck_temporary(name) AS
  starts_with(name,'__msduck_temp_') OR starts_with(name,'__msduck_tv_') OR starts_with(name,'__msduck_global_')";

/// View text, and the expressions of named CHECK constraints as written
/// (the constraints feature keeps them normalized for enforcement).
const STORES: &str = "CREATE TABLE IF NOT EXISTS main.__msduck_view_sources(object_id INTEGER NOT NULL,definition VARCHAR NOT NULL);
  CREATE TABLE IF NOT EXISTS main.__msduck_check_sources(object_id INTEGER NOT NULL,source VARCHAR NOT NULL);
  CREATE OR REPLACE VIEW main.__msduck_check_definitions AS
    SELECT c.object_id,main.__msduck_catalog_definition(coalesce(
      (SELECT max(s.source) FROM main.__msduck_check_sources s WHERE s.object_id=c.object_id),c.definition)) AS definition
    FROM main.__msduck_constraints c WHERE c.type_code='C'";

/// The user tables that are not temporary objects.
const TABLES: &str = "CREATE OR REPLACE VIEW main.__msduck_catalog_tables AS
  SELECT object_id,schema_id,name FROM main.__msduck_objects
  WHERE type_code='U' AND NOT main.__msduck_temporary(name)";

const CHECK_CONSTRAINTS: &str = "CREATE OR REPLACE VIEW sys.check_constraints AS
  SELECT c.name,c.object_id,CAST(NULL AS INTEGER) AS principal_id,c.schema_id,c.parent_object_id,
    CAST('C ' AS VARCHAR) AS type,c.type_desc,c.create_date,c.modify_date,false AS is_ms_shipped,
    false AS is_published,false AS is_schema_published,c.is_disabled,false AS is_not_for_replication,
    c.is_not_trusted,
    CAST(coalesce((SELECT k.column_id FROM main.__msduck_column_info k WHERE k.object_id=c.parent_object_id AND lower(k.name)=lower(c.parent_column)),0) AS INTEGER) AS parent_column_id,
    d.definition,true AS uses_database_collation,c.is_system_named
  FROM main.__msduck_live_constraints c
  JOIN main.__msduck_catalog_tables t ON t.object_id=c.parent_object_id
  LEFT JOIN main.__msduck_check_definitions d ON d.object_id=c.object_id
  WHERE c.type_code='C'";

/// The index of a PRIMARY KEY or UNIQUE constraint, by its tag (object IDs
/// of key constraints are `2000000000 + tag`; see constraints::catalog).
const KEY_CONSTRAINTS: &str = "CREATE OR REPLACE VIEW sys.key_constraints AS
  SELECT c.name,c.object_id,CAST(NULL AS INTEGER) AS principal_id,c.schema_id,c.parent_object_id,
    CAST(rpad(c.type_code,2,' ') AS VARCHAR) AS type,c.type_desc,c.create_date,c.modify_date,
    false AS is_ms_shipped,false AS is_published,false AS is_schema_published,
    (SELECT max(e.index_id) FROM main.__msduck_index_entries e
      WHERE e.object_id=c.parent_object_id AND e.tag=CAST(c.object_id AS BIGINT)-2000000000) AS unique_index_id,
    c.is_system_named,true AS is_enforced
  FROM main.__msduck_live_constraints c
  JOIN main.__msduck_catalog_tables t ON t.object_id=c.parent_object_id
  WHERE c.type_code IN ('PK','UQ')";

/// `key_index_id` is the referenced table's key over the referenced
/// columns, preferring its primary key.
const FOREIGN_KEYS: &str = "CREATE OR REPLACE VIEW sys.foreign_keys AS
  SELECT c.name,c.object_id,CAST(NULL AS INTEGER) AS principal_id,c.schema_id,c.parent_object_id,
    CAST('F ' AS VARCHAR) AS type,c.type_desc,c.create_date,c.modify_date,false AS is_ms_shipped,
    false AS is_published,false AS is_schema_published,c.referenced_object_id,
    (SELECT e.index_id FROM main.__msduck_index_entries e
      WHERE e.object_id=c.referenced_object_id AND (e.is_primary_key OR e.is_unique_constraint)
        AND list_sort(list_transform(e.key_columns,lambda x: lower(x)))
          =list_sort(list_transform(c.referenced_columns,lambda x: lower(x)))
      ORDER BY e.is_primary_key DESC,e.index_id LIMIT 1) AS key_index_id,
    c.is_disabled,false AS is_not_for_replication,c.is_not_trusted,
    CAST(c.delete_action AS TINYINT) AS delete_referential_action,
    CAST(CASE c.delete_action WHEN 1 THEN 'CASCADE' WHEN 2 THEN 'SET_NULL' WHEN 3 THEN 'SET_DEFAULT' ELSE 'NO_ACTION' END AS VARCHAR) AS delete_referential_action_desc,
    CAST(c.update_action AS TINYINT) AS update_referential_action,
    CAST(CASE c.update_action WHEN 1 THEN 'CASCADE' WHEN 2 THEN 'SET_NULL' WHEN 3 THEN 'SET_DEFAULT' ELSE 'NO_ACTION' END AS VARCHAR) AS update_referential_action_desc,
    c.is_system_named
  FROM main.__msduck_live_constraints c
  JOIN main.__msduck_catalog_tables t ON t.object_id=c.parent_object_id
  WHERE c.type_code='F'";

const DEFAULT_CONSTRAINTS: &str = "CREATE OR REPLACE VIEW sys.default_constraints AS
  SELECT d.name,d.object_id,CAST(NULL AS INTEGER) AS principal_id,t.schema_id,d.parent_object_id,
    CAST('D ' AS VARCHAR) AS type,CAST('DEFAULT_CONSTRAINT' AS VARCHAR) AS type_desc,
    d.create_date,d.modify_date,false AS is_ms_shipped,false AS is_published,false AS is_schema_published,
    d.column_id AS parent_column_id,
    main.__msduck_catalog_definition(s.source) AS definition,
    coalesce(s.is_system_named,false) AS is_system_named
  FROM main.__msduck_default_constraints d
  JOIN main.__msduck_catalog_tables t ON t.object_id=d.parent_object_id
  LEFT JOIN (SELECT object_id,max(source) AS source,bool_or(is_system_named) AS is_system_named
    FROM main.__msduck_default_sources GROUP BY object_id) s ON s.object_id=d.object_id";

const COMPUTED_COLUMNS: &str = "CREATE OR REPLACE VIEW sys.computed_columns AS
  SELECT c.object_id,c.name,c.column_id,c.system_type_id,c.user_type_id,c.max_length,
    c.precision,c.scale,c.collation_name,coalesce(d.is_nullable,c.is_nullable) AS is_nullable,c.is_ansi_padded,
    c.is_rowguidcol,c.is_identity,c.is_filestream,c.is_replicated,
    c.is_non_sql_subscribed,c.is_merge_published,c.is_dts_replicated,
    c.is_xml_document,c.xml_collection_id,c.default_object_id,c.rule_object_id,
    d.definition,d.definition IS NOT NULL AS uses_database_collation,
    k.is_persisted,c.is_computed,c.is_sparse,c.is_column_set,
    c.generated_always_type,c.generated_always_type_desc,c.encryption_type,
    c.encryption_type_desc,c.encryption_algorithm_name,c.column_encryption_key_id,
    c.column_encryption_key_database_name,c.is_hidden,c.is_masked,
    c.graph_type,c.graph_type_desc,c.is_data_deletion_filter_column,
    c.ledger_view_column_type,c.ledger_view_column_type_desc,
    c.is_dropped_ledger_column,false AS is_index_column_expression
  FROM sys.columns c
  JOIN main.__msduck_catalog_tables t ON t.object_id=c.object_id
  JOIN main.__msduck_computed_columns k ON k.object_id=c.object_id AND k.name_key=lower(c.name)
  LEFT JOIN (SELECT object_id,column_id,main.__msduck_catalog_definition(max(source)) AS definition,
      bool_and(is_nullable) AS is_nullable
    FROM main.__msduck_computed_sources GROUP BY object_id,column_id) d
    ON d.object_id=c.object_id AND d.column_id=c.column_id";

const TRIGGERS: &str = "CREATE OR REPLACE VIEW sys.triggers AS
  SELECT m.name,m.object_id,CAST(1 AS UTINYINT) AS parent_class,
    CAST('OBJECT_OR_COLUMN' AS VARCHAR) AS parent_class_desc,m.parent_object_id AS parent_id,
    CAST('TR' AS VARCHAR) AS type,CAST('SQL_TRIGGER' AS VARCHAR) AS type_desc,
    m.create_date,m.modify_date,false AS is_ms_shipped,m.is_disabled,false AS is_not_for_replication,
    coalesce(TRY_CAST(json_extract(m.properties,'$.instead_of') AS BOOLEAN),false) AS is_instead_of_trigger
  FROM main.__msduck_modules m
  JOIN main.__msduck_catalog_tables t ON t.object_id=m.parent_object_id
  WHERE m.type_code='TR'";

/// SET options are those every msduck session uses (ANSI_NULLS and
/// QUOTED_IDENTIFIER on). A scalar function is reported inlineable when
/// inlining is on; SQL Server also checks its body.
const SQL_MODULES: &str = "CREATE OR REPLACE VIEW sys.sql_modules AS
  SELECT m.object_id,m.definition,true AS uses_ansi_nulls,true AS uses_quoted_identifier,
    coalesce(TRY_CAST(json_extract(m.properties,'$.options.schemabinding') AS BOOLEAN),false) AS is_schema_bound,
    false AS uses_database_collation,false AS is_recompiled,
    coalesce(TRY_CAST(json_extract(m.properties,'$.options.returns_null_on_null_input') AS BOOLEAN),false) AS null_on_null_input,
    CAST(NULL AS INTEGER) AS execute_as_principal_id,
    coalesce(TRY_CAST(json_extract(m.properties,'$.options.native_compilation') AS BOOLEAN),false) AS uses_native_compilation,
    CASE m.type_code WHEN 'IF' THEN true
      WHEN 'FN' THEN coalesce(TRY_CAST(json_extract(m.properties,'$.options.inline') AS BOOLEAN),true)
      ELSE false END AS inline_type,
    CASE m.type_code WHEN 'IF' THEN true
      WHEN 'FN' THEN coalesce(TRY_CAST(json_extract(m.properties,'$.options.inline') AS BOOLEAN),true)
      ELSE false END AS is_inlineable
  FROM main.__msduck_modules m
  WHERE m.type_code<>'TR' OR m.parent_object_id IN (SELECT object_id FROM main.__msduck_catalog_tables)
  UNION ALL
  SELECT o.object_id,v.definition,true,true,false,false,false,false,CAST(NULL AS INTEGER),false,false,false
  FROM main.__msduck_objects o
  LEFT JOIN (SELECT object_id,max(definition) AS definition FROM main.__msduck_view_sources GROUP BY object_id) v
    ON v.object_id=o.object_id
  WHERE o.type_code='V'";

const PROCEDURES: &str = "CREATE OR REPLACE VIEW sys.procedures AS
  SELECT o.*,false AS is_auto_executed,false AS is_execution_replicated,
    false AS is_repl_serializable_only,false AS skips_repl_constraints
  FROM sys.objects o WHERE o.type='P '";

/// Parameters of procedures and functions from the module store's
/// `properties` (see procedures::define and functions), and the return
/// value of scalar functions as parameter 0.
const PARAMETERS: &str = "CREATE OR REPLACE VIEW main.__msduck_module_parameters AS
  SELECT m.object_id,CAST(p.value->>'name' AS VARCHAR) AS name,CAST(p.ordinal AS INTEGER) AS parameter_id,
    CAST(p.value->>'type' AS VARCHAR) AS type_text,
    coalesce(TRY_CAST(p.value->>'output' AS BOOLEAN),false) AS is_output,
    coalesce(TRY_CAST(p.value->>'readonly' AS BOOLEAN),false) AS is_readonly
  FROM main.__msduck_modules m
  CROSS JOIN LATERAL (SELECT unnest(CAST(json_extract(m.properties,'$.parameters') AS JSON[])) AS value,
    unnest(generate_series(1,CAST(json_array_length(json_extract(m.properties,'$.parameters')) AS BIGINT))) AS ordinal) p
  WHERE m.type_code IN ('P','FN','IF','TF') AND json_array_length(json_extract(m.properties,'$.parameters'))>0
  UNION ALL
  SELECT m.object_id,'',0,CAST(json_extract_string(m.properties,'$.returns') AS VARCHAR),true,false
  FROM main.__msduck_modules m WHERE m.type_code='FN';
  CREATE OR REPLACE VIEW sys.parameters AS
  SELECT p.object_id,p.name,p.parameter_id,t.system_type_id,t.user_type_id,
    CAST(coalesce(TRY_CAST(nullif(split_part(p.shape,'|',2),'') AS SMALLINT),t.max_length) AS SMALLINT) AS max_length,
    CAST(coalesce(TRY_CAST(nullif(split_part(p.shape,'|',3),'') AS UTINYINT),t.precision) AS UTINYINT) AS precision,
    CAST(coalesce(TRY_CAST(nullif(split_part(p.shape,'|',4),'') AS UTINYINT),t.scale) AS UTINYINT) AS scale,
    p.is_output,false AS is_cursor_ref,false AS has_default_value,false AS is_xml_document,
    CAST(NULL AS VARCHAR) AS default_value,CAST(0 AS INTEGER) AS xml_collection_id,p.is_readonly,
    true AS is_nullable,CAST(NULL AS INTEGER) AS encryption_type,CAST(NULL AS VARCHAR) AS encryption_type_desc,
    CAST(NULL AS VARCHAR) AS encryption_algorithm_name,CAST(NULL AS INTEGER) AS column_encryption_key_id,
    CAST(NULL AS VARCHAR) AS column_encryption_key_database_name,CAST(NULL AS INTEGER) AS vector_dimensions,
    CAST(NULL AS UTINYINT) AS vector_base_type,CAST(NULL AS VARCHAR) AS vector_base_type_desc
  FROM (SELECT *,main.__msduck_type_shape(type_text) AS shape FROM main.__msduck_module_parameters) p
  LEFT JOIN (SELECT name,system_type_id,user_type_id,max_length,precision,scale,
      row_number() OVER (PARTITION BY lower(name) ORDER BY schema_id<>4,user_type_id) AS rank
    FROM sys.types) t ON lower(t.name)=lower(split_part(p.shape,'|',1)) AND t.rank=1";

/// OBJECT_DEFINITION: module and view text, and the definitions of
/// DEFAULT and CHECK constraints.
const DEFINITIONS: &str = "CREATE OR REPLACE VIEW main.__msduck_object_definitions AS
  SELECT object_id,definition FROM main.__msduck_modules
  UNION ALL SELECT object_id,definition FROM sys.sql_modules WHERE object_id IN (SELECT object_id FROM main.__msduck_objects WHERE type_code='V')
  UNION ALL SELECT object_id,definition FROM sys.default_constraints
  UNION ALL SELECT object_id,definition FROM sys.check_constraints;
  CREATE OR REPLACE MACRO main.__msduck_object_definition(value) AS
    map_extract_value((SELECT map(list(object_id),list(definition)) FROM main.__msduck_object_definitions),value)";

pub(super) fn bootstrap(db: &Connection) -> Result<()> {
    for (name, sql) in [
        ("temporary", TEMPORARY),
        ("stores", STORES),
        ("tables", TABLES),
        ("check_constraints", CHECK_CONSTRAINTS),
        ("key_constraints", KEY_CONSTRAINTS),
        ("foreign_keys", FOREIGN_KEYS),
        ("default_constraints", DEFAULT_CONSTRAINTS),
        ("computed_columns", COMPUTED_COLUMNS),
        ("triggers", TRIGGERS),
        ("sql_modules", SQL_MODULES),
        ("procedures", PROCEDURES),
        ("parameters", PARAMETERS),
        ("definitions", DEFINITIONS),
    ] {
        db.execute_batch(sql)
            .with_context(|| format!("catalog bootstrap: {name}"))?;
    }
    super::visible::bootstrap(db)
}
