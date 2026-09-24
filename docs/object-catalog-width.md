# Object catalog type width

SQL Server exposes `sys.objects.type` as CHAR(2). The retained
`reference/object-catalog-width.json` captures table/view values, LEN,
DATALENGTH, raw binary payload and type predicates twice against the pinned
SQL Server container. Reproduce with
`node scripts/capture-object-catalog-width.mjs`.

Public values are `U ` and `V `: LEN is 1, DATALENGTH is 2 and the second byte
is a space. The catalog view pads these codes before consumers see them.
Private identity/storage codes remain unpadded. Direct DuckDB catalog
predicates use RTRIM explicitly because DuckDB does not apply SQL Server's
trailing-space equality. OUTPUT sink binding already trims the acquired code.
The wire encoder continues to reject CHAR values with the wrong width.

Native fixture observers use the same explicit predicate adjustment when
querying DuckDB directly; expected captured rows are unchanged. Client tests
exercise unmodified T-SQL predicates and exact sys.objects responses, including
binary values and descriptors. Captured sys.all_columns declarations and empty
SELECT * responses cover all 48 sys.tables and 23 sys.views columns exposed by
the pinned reference image. Their common object fields share one declaration
source; extension fields retain captured widths, nullability, computed/stored
origins and resource collation. Client tests compare complete responses for
both views, without the earlier descriptor exception.

The ordinary non-LOB table capture establishes lob_data_space_id = 0 and the
three default data-retention values (-1, -1, INFINITE). The ordinary view adds
the captured is_tracked_by_cdc and has_snapshot columns. Physical LOB placement,
configured retention and unsupported table/view kinds are not established by
these default-object observations.

Nine additional LOB lifecycle scenarios capture VARCHAR(MAX), NVARCHAR(MAX),
VARBINARY(MAX), column add/drop, table recreation, ALTER between bounded and MAX
types, and transaction rollback.
The logical default LOB data-space ID becomes 1 when a MAX column is declared
and remains 1 after that column is dropped or altered back to a bounded type.
Recreating the table starts at 0; rolling back either ADD or ALTER of a MAX
declaration restores the prior value. A private per-object
property records this history in the DDL transaction and survives restart.
This is the captured logical default-filegroup identity, not DuckDB physical
LOB placement. Older databases can recover current MAX declarations, but the
history of already dropped or altered-away MAX columns was not retained by
earlier versions and remains a migration gap.

The retained IN/NOT IN cases also exercise lists containing NULL. ANSI list
membership uses the same trailing-space equality as a scalar comparison, while
retaining SQL three-valued logic. The deterministic binder trims each operand
once and preserves the membership operator; mixed-type, unresolved and BIN2
comparisons remain on their existing paths.

Verification is revision-specific in the integration PR. Reference captures
establish SQL Server behavior; they do not establish a passing server replay.
