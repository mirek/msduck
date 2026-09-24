# SQL Server view binding evidence

Three first-party suites preserve 81 sequential cases: 41 metadata cases,
27 execution cases and 13 invalid-dependency cases. Each suite was captured
identically in two fresh, isolated SQL Server databases using the image digest
recorded in its fixture. The records retain rows, column descriptors, errors,
information messages and completion data. They are SQL Server reference evidence,
not a claim that msduck passes these cases.

After `npm ci`, run this command on a host with Docker available:

```sh
node scripts/capture-view-binding.mjs
```

An optional first argument selects the output directory. The command uses the
fixtures' pinned image and SQL programs, runs each suite twice in fresh databases,
and preserves both raw runs before checking equality. It then compares each
capture to the retained fixture. Changed output fails the command; inspect the
raw evidence rather than normalizing differences. Reference files are never
replaced by the default command. Cases within a suite must stay in order because
later statements inspect earlier DDL, transaction changes and session settings.
The shared reference helper removes its own container and isolated databases.

## Findings that constrain implementation

- View descriptors depend on the logical projection. Direct stored and identity
  columns, direct computed expressions, aggregates, CTEs and derived tables have
  different provenance flags. Nested real views retain computed provenance.
- Altering a source view changes nested result properties immediately; rollback
  restores them. Caching properties only when CREATE VIEW executes is insufficient.
- Caller CTEs cannot replace the base relations inside a stored view. A caller CTE
  can shadow an unqualified view name, while a qualified view reference still
  binds the catalog view. Quoted names containing dots retain identifier boundaries.
- Aliased, nested, qualified, correlated and assignment/INSERT view consumers
  produce captured NULL-elimination warnings. An unused view under `WHERE 1=0`
  does not. ANSI_WARNINGS OFF suppresses the warning.
- Two schema-qualified sources with the same exposed short name fail with error
  1013 even when their selected columns are fully qualified. Generated aliases
  must not silently make such an invalid original query valid.
- Dropping a column used by a non-schema-bound view succeeds, but reading that
  view emits errors 207 then 4413. Same-named outer columns do not repair it.
  Re-adding the source column restores execution without ALTER VIEW. Renaming a
  dependency view's output similarly invalidates its nested consumer.

Preserve the captured error line numbers, including the differing 4413 line
numbers across sequential cases. The fixtures record observations without
asserting an explanation for every diagnostic detail.

Execution-time expansion therefore needs independent binding of stored bodies,
original public result metadata, scope-aware qualifier handling, and a shared
statement diagnostic context. Naive derived-table substitution can accidentally
capture caller columns through implicit lateral binding. Metadata rebinding alone
does not implement view-body observation or establish full view compatibility.
