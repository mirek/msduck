# Constraint and module catalogs

Task #728 is in progress. `sys.procedures` now projects the shared persistent
module store, and `OBJECT_DEFINITION` reads original stored module text. Module
rename preserves that text; alter, drop and transaction rollback are observed
without a separate cache. Procedures are isolated by database and survive
restart. Startup execution and replication are unsupported and their flags are
false. Procedure creation/execution belongs to its separate extension, which
is still a stub on this branch; tests exercise the shared store directly.
Named DEFAULT constraints now project their existing object/column IDs through
`sys.default_constraints`. Definition serialization covers the retained numeric,
negative, decimal/scientific, escaped string/Unicode, NULL, binary, arithmetic,
function and integer CAST/CONVERT controls, and is also used by
`OBJECT_DEFINITION`. Source expressions are retained separately from SQL Server
catalog text. Unsupported syntax and preexisting rows whose source was not
stored remain NULL until serialization/backfill is implemented. Additive storage
migration preserves existing constraint identities. View definitions,
unnamed DEFAULT constraints and complete wire descriptor parity remain
unimplemented. This checkpoint does not establish full catalog compatibility.

PK/UQ constraints now have persistent object IDs allocated by the keys DDL
lifecycle under companion #756. They appear in `sys.objects`, `OBJECT_ID` and
`sys.key_constraints`; reads allocate no IDs. Table and constraint identities
are distinct; rollback restores old IDs and committed drop/recreate allocates
new ones. New keys retain explicit/generated name provenance. Bootstrap fills
missing legacy identities without changing parents, retaining unknown name
provenance as NULL. The key view has the captured 15-column order;
`unique_index_id` remains unknown until logical index/clustering provenance is
wired in. No index-ID or full wire parity is claimed. Explicit namespace
conflicts are checked before native table creation, including duplicate names
in one declaration.

`reference/gaps-catalog.json` retains all46 responses from each of two fresh
SQL Server17.0.4065.4 containers, including descriptors, rows, diagnostics,
return status, completion commands/status and token order. The image is pinned
explicitly and the helper-reported image is checked before work begins.
The original envelope (excluding the additive `definitionProfile`) retains
SHA-256 `e7210ca707919bae43093f4207a76256a86873e9350b5b6a621c291797bcdc8e`.
Aggregate fixture SHA-256:
`6d763f84f5b96a36d11f820dfb0c9ed11213e289a7de3ffb98fb2fcf617f786c`.

The additive profile retains eight complete responses from each of two fresh
pinned containers at capture revision fc4bb84. Both full runs agree, covering
15 DEFAULT and nine computed expressions, including their complete metadata,
diagnostics and completion streams. The deterministic formatter is checked
against every expression's retained definition. Computed catalog rows are now implemented for the retained controls;
the remaining expression syntax still needs implementation; passing the
formatter test does not establish computed column metadata compatibility.

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
The broader expression profile uses
`node scripts/capture-gaps-catalog.mjs --definitions artifacts/new-definitions.json`.

Remaining work: implement persistent transactional catalog bookkeeping and
views, system procedure contracts, rename effects and failure atomicity; add
Rust/tedious regressions; complete exact-head workspace/client/audit/CI/review
verification; merge and publish task completion. engine.rs and other workers'
extension modules remain outside this task's scope.

Computed columns now retain original source expressions and supported SQL Server
catalog definitions in a table/column-owned transactional store. The view uses
the captured 44-column order, with persisted state from the computed-column
adapter and cleanup through column/table drop and rollback. Legacy definitions
remain NULL. The controlled persisted arithmetic row is tested against the raw
reference; logical nullability reuses deterministic expression properties over explicit
source-column declarations, including the retained ISNULL/CASE controls.
Legacy definition/property backfill and full wire descriptor parity remain
outstanding. No stored operand is evaluated during inference.

Named CHECKs now retain their own schema-scoped object identities and original
source expressions in the DDL transaction. `sys.check_constraints` has the
captured 19-column order; the controlled single-column table CHECK matches its
retained parent column, definition and enforcement flags. `OBJECT_DEFINITION`
reads the same stored text. Namespace conflicts are rejected before native DDL;
drop/rollback follow table identity. Unnamed CHECK identities, legacy backfill,
constraint alteration/trust controls, wider expression relationships and complete
wire descriptors remain unfinished.
