-- Object extensions for the currently supported ordinary tables and views.
-- Physical LOB placement and temporal retention have no emulation yet.
CREATE OR REPLACE VIEW sys.tables AS
SELECT o.*,
    CAST(NULL AS INTEGER) AS lob_data_space_id,
    CAST(NULL AS INTEGER) AS filestream_data_space_id,
    c.max_column_id AS max_column_id_used,
    false AS lock_on_bulk_load,
    true AS uses_ansi_nulls,
    false AS is_replicated,
    false AS has_replication_filter,
    false AS is_merge_published,
    false AS is_sync_tran_subscribed,
    false AS has_unchecked_assembly_data,
    CAST(0 AS INTEGER) AS text_in_row_limit,
    false AS large_value_types_out_of_row,
    false AS is_tracked_by_cdc,
    CAST(0 AS UTINYINT) AS lock_escalation,
    'TABLE' AS lock_escalation_desc,
    false AS is_filetable,
    false AS is_memory_optimized,
    CAST(0 AS UTINYINT) AS durability,
    'SCHEMA_AND_DATA' AS durability_desc,
    CAST(0 AS UTINYINT) AS temporal_type,
    'NON_TEMPORAL_TABLE' AS temporal_type_desc,
    CAST(NULL AS INTEGER) AS history_table_id,
    false AS is_remote_data_archive_enabled,
    false AS is_external,
    CAST(NULL AS INTEGER) AS history_retention_period,
    CAST(NULL AS INTEGER) AS history_retention_period_unit,
    CAST(NULL AS VARCHAR) AS history_retention_period_unit_desc,
    false AS is_node,
    false AS is_edge,
    CAST(0 AS UTINYINT) AS ledger_type,
    'NON_LEDGER_TABLE' AS ledger_type_desc,
    CAST(NULL AS INTEGER) AS ledger_view_id,
    false AS is_dropped_ledger_table
FROM sys.objects o
JOIN main.__msduck_column_counters c USING(object_id)
WHERE o.type='U';

CREATE OR REPLACE VIEW sys.views AS
SELECT o.*,
    false AS is_replicated,
    false AS has_replication_filter,
    false AS has_opaque_metadata,
    false AS has_unchecked_assembly_data,
    false AS with_check_option,
    false AS is_date_correlation_view,
    CAST(0 AS UTINYINT) AS ledger_view_type,
    'NON_LEDGER_VIEW' AS ledger_view_type_desc,
    false AS is_dropped_ledger_view
FROM sys.objects o WHERE o.type='V';
