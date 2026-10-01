# Constraint and module catalogs

Task #728 is in progress. `sys.procedures` now projects the shared persistent
module store, and `OBJECT_DEFINITION` reads original stored module text. Module
rename preserves that text; alter, drop and transaction rollback are observed
without a separate cache. Procedures are isolated by database and survive
restart. Startup execution and replication are unsupported and their flags are
false. Procedure creation/execution belongs to its separate extension, which
is still a stub on this branch; tests exercise the shared store directly.
Named DEFAULT constraints now project their existing object/column IDs through
`sys.default_constraints`. Integer literal definitions use the captured
`((1))` form and are also returned by `OBJECT_DEFINITION`. Source expressions
are retained separately from SQL Server catalog text. Definitions for other
expression families and preexisting rows whose source was not stored remain
NULL until verified serialization/backfill is implemented. Additive storage
migration preserves existing constraint identities. View/check definitions,
unnamed DEFAULT constraints and complete wire descriptor parity remain
unimplemented. This checkpoint does not establish full catalog compatibility.

`reference/gaps-catalog.json` retains all46 responses from each of two fresh
SQL Server17.0.4065.4 containers, including descriptors, rows, diagnostics,
return status, completion commands/status and token order. The image is pinned
explicitly and the helper-reported image is checked before work begins.
Fixture SHA-256: `e7210ca707919bae43093f4207a76256a86873e9350b5b6a621c291797bcdc8e`.

All ten complete empty catalog responses agree between the runs:

| View | Columns |
| --- | ---: |
| sys.foreign_keys | 22 |
| sys.foreign_key_columns | 6 |
| sys.key_constraints | 15 |
| sys.default_constraints | 15 |
| sys.check_constraints | 19 |
| sys.computed_columns | 44 |
| sys.triggers | 13 |
| sys.sql_modules | 12 |
| sys.procedures | 16 |
| sys.database_files | 30 |

The fixture covers named composite primary/foreign keys, unique/default/check
constraints, persisted computed columns, view/procedure/trigger definitions,
named and positional sp_pkeys/sp_fkeys and file declarations. It includes
successful table, plain-column, index, constraint and module renames and their
caution message. Renaming a column with enforced computed/check dependencies
fails with15336; the subsequent foreign-column lookup retains the original
column name. Missing procedure arguments, missing objects and invalid rename
types retain their complete errors. No successful control may contain an error;
no failure control may silently succeed.

IDs, clock values and file observations remain verbatim in both raw runs.
Only complete empty schemas are asserted equal; variable values are not erased
or replaced to make the runs agree. Consumers must verify relationships and
controlled values separately while preserving the complete captures.

The shape follows the upstream catalog skill and primary SQL Server contracts:
[sys.key_constraints](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-key-constraints-transact-sql),
[sys.foreign_keys](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-foreign-keys-transact-sql),
[sp_pkeys](https://learn.microsoft.com/en-us/sql/relational-databases/system-stored-procedures/sp-pkeys-transact-sql),
[sp_fkeys](https://learn.microsoft.com/en-us/sql/relational-databases/system-stored-procedures/sp-fkeys-transact-sql),
[sp_rename](https://learn.microsoft.com/en-us/sql/relational-databases/system-stored-procedures/sp-rename-transact-sql),
and[OBJECT_DEFINITION](https://learn.microsoft.com/en-us/sql/t-sql/functions/object-definition-transact-sql).
The pinned capture supplies exact schemas and error framing where the copied
skill or documentation describes a different version or omits a field.

To capture into a new immutable path, use
`node scripts/capture-gaps-catalog.mjs artifacts/new-catalog-reference.json`.
The output must not exist; containers are owned and removed by that invocation.
On the shared Linux builder, hold its runner lock through the capture.

Remaining work: implement persistent transactional catalog bookkeeping and
views, system procedure contracts, rename effects and failure atomicity; add
Rust/tedious regressions; complete exact-head workspace/client/audit/CI/review
verification; merge and publish task completion. engine.rs and other workers'
extension modules remain outside this task's scope.
