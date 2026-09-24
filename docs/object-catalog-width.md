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
binary values and descriptors. Tests also retain rows from sys.tables and
sys.views, whose complete descriptor compatibility remains unfinished.

Verification is revision-specific in the integration PR. Reference captures
establish SQL Server behavior; they do not establish a passing server replay.
