# Column catalog and lookup foundation

COL_NAME(object_id,column_id) returns the column name as NVARCHAR(128), including
correct descriptors for empty and NULL results. COLUMNPROPERTY(object_id,name,
property) returns INT for ColumnId, AllowsNull, IsIdentity, Precision, Scale,
UsesAnsiTrim, IsComputed, GeneratedAlwaysType, IsHidden, IsSparse, IsColumnSet
and IsRowGuidCol. Name and property
matching is currently case insensitive. Invalid IDs, missing names and NULL
arguments return NULL.

Table column IDs are persisted independently from DuckDB ordinal positions.
Dropping a column leaves a gap; later additions use the next ID above the highest
committed ID ever assigned to that table. ALTER COLUMN retains its ID while
nullability is read from the live definition. IDENTITY is recognized from the
private allocator default. Registry changes and counters share the DDL transaction,
so rollback restores both. Drop/recreate starts a new object with IDs beginning
at 1. Startup retains counters; older tables without registry entries are
backfilled from their current ordinal positions, without reconstructing past gaps.
View columns use the current view projection order after ALTER VIEW.

Prepared lookups observe later DDL. Native tests verify restart after dropping the
highest column and once-per-row evaluation of volatile lookup arguments over
6,000 rows. Client tests cover gaps, identity removal/readdition, nullability,
rollback, view reordering, recreation and empty result descriptors.

Precision reports character/binary length (Unicode characters rather than bytes),
-1 for MAX, and numeric/temporal precision from the declared type. Scale uses the
declared scale. UsesAnsiTrim applies to CHAR/VARCHAR; other types return NULL.
Legacy TEXT/NTEXT/IMAGE precision and other unimplemented properties return NULL.
Feature flags reflect the current sys.columns defaults; full computed, sparse,
ROWGUIDCOL and ANSI_PADDING option tracking are unfinished. Lookups observe ALTER,
rollback and persisted view declarations. Prepared client tests cover these changes,
NULL arguments and INT descriptors on empty results.

This remains partial COLUMNPROPERTY support. Full column metadata, permissions,
full collation rules, column rename and cross-database catalogs remain unfinished.
View nullability/identity inference currently follows backend metadata.

References: [COL_NAME](https://learn.microsoft.com/en-us/sql/t-sql/functions/col-name-transact-sql),
[COLUMNPROPERTY](https://learn.microsoft.com/en-us/sql/t-sql/functions/columnproperty-transact-sql).


sys.columns now exposes all registered table/view columns, joining sys.objects
and sys.types. Newly created or altered table columns persist their original
SQL Server type before DuckDB lowering. This preserves VARCHAR versus NVARCHAR,
NUMERIC versus DECIMAL, declared byte lengths (including MAX=-1), decimal storage
sizes and temporal scale/precision. Nullability and identity flags remain live.
ALTER replaces the declaration within the DDL transaction; rollback restores it,
drop cleans it up, and restart retains it. The identity allocator's backend
DEFAULT does not become a user default constraint.

Primitive backend types with unambiguous SQL Server equivalents can be inferred
for older tables and unresolved query outputs. Ambiguous types (including text widths,
legacy timestamp kinds and decimal aliases) return NULL metadata until their
provenance is implemented. This preserves rows in sys.columns without inventing
a declaration. Unsupported view expressions and historical backfill remain
open. An ALTER declaration can supply previously missing metadata.

The view exposes the core columns through encryption/hidden/masked metadata.
Unsupported advanced features have disabled flags; later-version extensions,
ROWGUIDCOL/computed details and full option tracking remain unfinished. A column
with a user DEFAULT returns NULL default_object_id until constraint identities
are implemented; columns without defaults and identity columns return 0. Catalog
text widths, nullability descriptors, permission filtering and full ANSI padding/
collation behavior still need exact emulation. Declaration metadata does not by
itself add missing storage enforcement for character widths or advanced types.

Native tests cover decimal storage boundaries, temporal scales, persistence,
rollback and cleanup. Client tests cover differing type aliases/widths, ALTER
success and failure, dropped-column gaps, view fallback and empty wire metadata.
The local audit captures values without asserting live SQL Server parity.

Reference: [sys.columns](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-columns-transact-sql).


View CREATE/ALTER and SELECT INTO now preserve direct source column types through
aliases, joins, wildcards, derived tables and non-recursive CTEs. Explicit CAST
and CONVERT declarations are preserved, using their 30-character default length
when omitted. Output positions are checked against the created object's columns;
explicit view column lists and renamed projections retain the right type metadata.

View metadata is a persisted snapshot: unrelated DDL and changes to source type
widths do not silently rewrite it. ALTER VIEW replaces the snapshot transactionally;
rollback restores the previous one. Unsupported projections clear old declarations
instead of retaining stale metadata. SELECT INTO records declarations during table
creation, before row population, so a failed population retains both the empty
table and its known metadata in autocommit mode. Explicit rollback removes both.

Expression inference beyond direct columns and explicit casts, set-operation
common types, recursive CTEs, parameter type propagation, identity inheritance,
sp_refreshview, and full view binding/nullability semantics remain unfinished.
Catalog propagation does not yet update all query-result wire descriptors or
change backend storage enforcement. Older persisted views are not reconstructed
from lowered SQL; ALTER VIEW can supply their original declarations again.

VARCHAR CAST/CONVERT execution now supports the no-style conversion path; see
[VARCHAR conversion limits](varchar.md) for code-page and formatting gaps.

Identity-specific catalog values are available in [sys.identity_columns](identity.md).
