CREATE OR REPLACE VIEW sys.columns AS
SELECT c.object_id,c.name,c.column_id,
    coalesce(d.system_type_id,t.system_type_id) AS system_type_id,
    coalesce(d.user_type_id,t.user_type_id) AS user_type_id,
    coalesce(d.max_length,t.max_length) AS max_length,
    coalesce(d.precision,t.precision) AS precision,
    coalesce(d.scale,t.scale) AS scale,
    d.collation_name,c.is_nullable,
    coalesce(d.system_type_id,t.system_type_id) IN (165,167,173,175,231,239,98) AS is_ansi_padded,
    false AS is_rowguidcol,c.is_identity,false AS is_computed,false AS is_filestream,
    false AS is_replicated,false AS is_non_sql_subscribed,false AS is_merge_published,
    false AS is_dts_replicated,false AS is_xml_document,
    CAST(0 AS INTEGER) AS xml_collection_id,
    CASE WHEN ic.column_default IS NULL OR c.is_identity THEN CAST(0 AS INTEGER) ELSE CAST(NULL AS INTEGER) END AS default_object_id,
    CAST(0 AS INTEGER) AS rule_object_id,false AS is_sparse,false AS is_column_set,
    CAST(0 AS UTINYINT) AS generated_always_type,'NOT_APPLICABLE' AS generated_always_type_desc,
    CAST(NULL AS INTEGER) AS encryption_type,CAST(NULL AS VARCHAR) AS encryption_type_desc,
    CAST(NULL AS VARCHAR) AS encryption_algorithm_name,CAST(NULL AS INTEGER) AS column_encryption_key_id,
    CAST(NULL AS VARCHAR) AS column_encryption_key_database_name,
    false AS is_hidden,false AS is_masked
FROM main.__msduck_column_info c
JOIN main.__msduck_objects o USING(object_id)
JOIN main.__msduck_schemas s USING(schema_id)
JOIN information_schema.columns ic ON ic.table_catalog=current_database() AND lower(ic.table_schema)=lower(s.name) AND lower(ic.table_name)=lower(o.name) AND lower(ic.column_name)=lower(c.name)
LEFT JOIN main.__msduck_declared_columns d ON d.object_id=c.object_id AND d.column_id=c.column_id
LEFT JOIN sys.types t ON t.name=CASE ic.data_type
    WHEN 'INTEGER' THEN 'int' WHEN 'SMALLINT' THEN 'smallint' WHEN 'BIGINT' THEN 'bigint'
    WHEN 'UTINYINT' THEN 'tinyint' WHEN 'BOOLEAN' THEN 'bit' WHEN 'FLOAT' THEN 'real'
    WHEN 'DOUBLE' THEN 'float' WHEN 'DATE' THEN 'date' WHEN 'UUID' THEN 'uniqueidentifier'
    ELSE NULL END;

-- Precision means declared character/binary length for those families, rather
-- than the numeric-only precision field stored in sys.columns.
CREATE OR REPLACE MACRO main.__msduck_columnproperty(obj,col,prop) AS
map_extract_value(
    (SELECT map(list(CAST(c.object_id AS VARCHAR)||chr(0)||lower(c.name)||chr(0)||p.property),list(p.value))
     FROM sys.columns c CROSS JOIN LATERAL (VALUES
        ('columnid',c.column_id),
        ('allowsnull',CAST(c.is_nullable AS INTEGER)),
        ('isidentity',CAST(c.is_identity AS INTEGER)),
        ('precision',CAST(CASE
            WHEN c.max_length=-1 THEN -1
            WHEN c.system_type_id IN (231,239) THEN c.max_length/2
            WHEN c.system_type_id IN (165,167,173,175) THEN c.max_length
            WHEN c.system_type_id IN (34,35,99) THEN NULL
            ELSE c.precision END AS INTEGER)),
        ('scale',CAST(c.scale AS INTEGER)),
        ('usesansitrim',CASE WHEN c.system_type_id IN (167,175) THEN CAST(c.is_ansi_padded AS INTEGER) ELSE NULL END),
        ('iscomputed',CAST(c.is_computed AS INTEGER)),
        ('generatedalwaystype',CAST(c.generated_always_type AS INTEGER)),
        ('ishidden',CAST(c.is_hidden AS INTEGER)),
        ('issparse',CAST(c.is_sparse AS INTEGER)),
        ('iscolumnset',CAST(c.is_column_set AS INTEGER)),
        ('isrowguidcol',CAST(c.is_rowguidcol AS INTEGER))
     ) p(property,value)),
    CAST(obj AS VARCHAR)||chr(0)||lower(CAST(col AS VARCHAR))||chr(0)||lower(CAST(prop AS VARCHAR)));


CREATE OR REPLACE MACRO main.__msduck_identity_variant(type_id,value) AS
    CASE WHEN value IS NULL THEN NULL ELSE struct_pack(
        __msduck_variant_type := CAST(type_id AS UTINYINT),
        __msduck_variant_integer := CAST(value AS BIGINT)) END;

CREATE OR REPLACE VIEW sys.identity_columns AS
SELECT c.*,
    __msduck_identity_variant(c.system_type_id,d.seed) AS seed_value,
    __msduck_identity_variant(c.system_type_id,d.increment_value) AS increment_value,
    __msduck_identity_variant(c.system_type_id,seq.last_value) AS last_value,
    false AS is_not_for_replication
FROM sys.columns c
JOIN main.__msduck_objects o USING(object_id)
JOIN main.__msduck_schemas s USING(schema_id)
JOIN information_schema.columns ic ON ic.table_catalog=current_database() AND lower(ic.table_schema)=lower(s.name) AND lower(ic.table_name)=lower(o.name) AND lower(ic.column_name)=lower(c.name)
JOIN main.__msduck_identity_definitions d ON d.sequence_name=__msduck_identity_sequence(ic.column_default)
JOIN duckdb_sequences() seq ON seq.database_name=current_database() AND seq.schema_name||'.'||seq.sequence_name=d.sequence_name
WHERE c.is_identity;


CREATE OR REPLACE MACRO main.__msduck_variant_property(value,property) AS
map_extract_value((SELECT map(list(CAST(t.type_id AS VARCHAR)||chr(0)||p.property),
    list(struct_pack(__msduck_variant_type := CAST(CASE WHEN p.property='basetype' THEN 231 ELSE 56 END AS UTINYINT),
        __msduck_variant_integer := CAST(p.value AS BIGINT),
        __msduck_variant_sysname := CAST(CASE WHEN p.property='basetype' THEN t.name ELSE NULL END AS VARCHAR))))
    FROM (VALUES (48,'tinyint',1,3),(52,'smallint',2,5),(56,'int',4,10),(104,'bit',1,1),(127,'bigint',8,19)) t(type_id,name,width,precision_value)
    CROSS JOIN LATERAL (VALUES ('basetype',NULL),('precision',t.precision_value),('scale',0),('maxlength',t.width),('totalbytes',t.width+2)) p(property,value)),
    CAST(__msduck_variant_tag(value) AS VARCHAR)||chr(0)||lower(CAST(property AS VARCHAR)));
