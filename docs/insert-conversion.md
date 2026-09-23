# Integer INSERT targets

INSERT now applies the shared integer-conversion path to TINYINT, SMALLINT,
INT and BIGINT destinations for VALUES and SELECT sources. Numeric fractions
truncate before range checks; numeric overflow reports 8115. Character inputs
use the integer syntax rules documented in [integer conversion](integer-conversion.md).
Known money expressions retain rounding when the source column consists of
money, integral values and NULL, or a directly recognized SELECT expression.

The engine reads destination columns from the backend catalog, describes the
source without executing its rows, and wraps it in a typed projection. This
converts values after source-column type resolution and preserves parameter
bindings. Arity is checked before projection so extra source columns cannot be
silently discarded. Defaults in VALUES and DEFAULT VALUES are resolved from
stored expressions; omitted integer defaults are added to the projection.
Other omitted defaults remain native, including per-row NEWID expressions.

CTE-prefixed INSERT is normalized from sqlparser's query-shaped node into an
INSERT with a WITH source. This also prevents INSERT completion counts from
being emitted as a query result. The normalization visits nested statements.
CTE with DEFAULT VALUES and nested INSERT WITH clauses are explicitly rejected.

The copied mssqlite conversion and DML references supplied integer conversion,
source typing and failure-atomicity requirements. Tedious tests cover all four
integer widths, BIGINT boundaries, reordered/partial target lists, TOP/ORDER,
CTEs, defaults, UUID generation, prepared recovery, money input, invalid text,
excess source columns, overflow and transaction rollback. Failed multi-row
inserts leave no rows from that statement. Tiberius independently verifies
CTE completion and converted values/defaults.

Remaining work includes SQL Server's full VALUES/SELECT common-type inference,
money provenance from stored columns and complex expressions, generated and
identity columns, exact conversion messages/states, non-integer widths,
MERGE assignment conversions and full UPDATE target resolution and full statement rollback semantics in
explicit transactions. The metadata lookup and source description add planning
overhead and are not yet cached. Live SQL Server differential validation
remains outstanding.
