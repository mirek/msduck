-- Built-in definitions adapted from the reviewed mssqlite sys catalog seed.
CREATE OR REPLACE VIEW sys.types AS
SELECT name,CAST(system_id AS UTINYINT) AS system_type_id,
    CAST(user_id AS INTEGER) AS user_type_id,CAST(4 AS INTEGER) AS schema_id,
    CAST(NULL AS INTEGER) AS principal_id,CAST(length AS SMALLINT) AS max_length,
    CAST(type_precision AS UTINYINT) AS precision,CAST(type_scale AS UTINYINT) AS scale,
    type_collation AS collation_name,name NOT IN ('sysname','timestamp') AS is_nullable,
    false AS is_user_defined,system_id=240 AS is_assembly_type,
    CAST(0 AS INTEGER) AS default_object_id,CAST(0 AS INTEGER) AS rule_object_id,
    false AS is_table_type
FROM (VALUES
    ('image',34,34,16,0,0,NULL),
    ('text',35,35,16,0,0,'SQL_Latin1_General_CP1_CI_AS'),
    ('uniqueidentifier',36,36,16,0,0,NULL),
    ('date',40,40,3,10,0,NULL),
    ('time',41,41,5,16,7,NULL),
    ('datetime2',42,42,8,27,7,NULL),
    ('datetimeoffset',43,43,10,34,7,NULL),
    ('tinyint',48,48,1,3,0,NULL),
    ('smallint',52,52,2,5,0,NULL),
    ('int',56,56,4,10,0,NULL),
    ('smalldatetime',58,58,4,16,0,NULL),
    ('real',59,59,4,24,0,NULL),
    ('money',60,60,8,19,4,NULL),
    ('datetime',61,61,8,23,3,NULL),
    ('float',62,62,8,53,0,NULL),
    ('sql_variant',98,98,8016,0,0,NULL),
    ('ntext',99,99,16,0,0,'SQL_Latin1_General_CP1_CI_AS'),
    ('bit',104,104,1,1,0,NULL),
    ('decimal',106,106,17,38,38,NULL),
    ('numeric',108,108,17,38,38,NULL),
    ('smallmoney',122,122,4,10,4,NULL),
    ('bigint',127,127,8,19,0,NULL),
    ('hierarchyid',240,128,892,0,0,NULL),
    ('geometry',240,129,-1,0,0,NULL),
    ('geography',240,130,-1,0,0,NULL),
    ('varbinary',165,165,8000,0,0,NULL),
    ('varchar',167,167,8000,0,0,'SQL_Latin1_General_CP1_CI_AS'),
    ('binary',173,173,8000,0,0,NULL),
    ('char',175,175,8000,0,0,'SQL_Latin1_General_CP1_CI_AS'),
    ('timestamp',189,189,8,0,0,NULL),
    ('nvarchar',231,231,8000,0,0,'SQL_Latin1_General_CP1_CI_AS'),
    ('nchar',239,239,8000,0,0,'SQL_Latin1_General_CP1_CI_AS'),
    ('xml',241,241,-1,0,0,NULL),
    ('sysname',231,256,256,0,0,'SQL_Latin1_General_CP1_CI_AS')
) t(name,system_id,user_id,length,type_precision,type_scale,type_collation);
CREATE OR REPLACE MACRO main.__msduck_type_id(value) AS
    map_extract_value((SELECT map(list('sys'||chr(0)||name),list(user_type_id)) FROM sys.types),__msduck_type_key(CAST(value AS VARCHAR)));
CREATE OR REPLACE MACRO main.__msduck_type_name(value) AS
    map_extract_value((SELECT map(list(user_type_id),list(name)) FROM sys.types),value);
