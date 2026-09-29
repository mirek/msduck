# Scoped TVP materialization

The root API `tvp_binding::materialize::with_relation` takes an explicit
connection, binding, private relation identifier, codec limits, transaction
mode and trusted operation. It validates all caller-constructible cells and
declarations before writes, creates a new temporary table, binds one row at a
time and removes the table before returning. It never replaces an existing
relation. Values are bound and identifiers quoted; temporary references are
qualified with `temp.main` to avoid search-path ambiguity.

INT cells use INTEGER, binary cells BLOB and Unicode cells the existing
`STRUCT(__msduck_utf16le BLOB)` carrier. NULL carriers remain SQL NULL rather
than a non-NULL struct with NULL payload. Empty cells and unpaired surrogate
units remain intact. Declared widths, nullability and collation remain available
through the borrowed logical binding; DuckDB storage types do not establish
SQL Server result descriptors or collation semantics.

Owned mode begins a transaction, cleans up and commits on success, and rolls
back on error. It commits or rolls back the operation's other writes as part
of that same transaction. Caller mode does neither: the executor supplies an
existing transaction spanning catalog binding, materialization and operation.
On error cleanup is attempted; if DuckDB has aborted the transaction, cleanup
failure is reported along with the original error and the caller must roll
back. Database commit/rollback failures are reported. Connection-local tables
are invisible to other connections and vanish when their connection closes.

The operation must not change transaction boundaries or rename, drop, replace
or modify its input relation. Cleanup checks the temporary catalog's relation
identity and refuses to drop a replacement. This backend interface does not
enforce SQL Server READONLY semantics; the future procedure binder must prevent
writes to TVP parameters, supply declarations and maintain its transaction.
Procedure dispatch, AST substitution, constraints and wire completion are
still required before live TVP support can be claimed.

Native tests replay the twelve successful retained wire-fixture shapes across
four runs, test raw surrogate/NUL units and all empty supply origins, quoted
names, collision, connection isolation, caller rollback, owned commit/rollback,
malformed shapes, widths, NULL constraints and resource limits. This is
backend materialization evidence, not procedure wire interoperability.
