# Typed compiler and result metadata contract

Source review baseline: `80476f54d68dcf31297c648f1b6da2c71febe60f`.
This is the bounded design deliverable for task `compiler-contract-plan-v1`,
[issue #9](https://github.com/mirek/msduck/issues/9), under workstream
[#3](https://github.com/mirek/msduck/issues/3). The implementation tasks below are
proposals for owner approval, not newly authorized or claimable work. No Rust
behavior changes or fresh SQL Server equivalence claims are made here.

The next extraction should establish one ordered logical result contract and one
validated physical-to-wire adaptation path within the existing crates. A new
crate would not resolve the current repeated inference or descriptor divergence.
The full server objective still includes the language, transaction, catalog,
protocol and Unicode gaps in [ROADMAP.md](../ROADMAP.md).

## Current paths and their boundaries

| Responsibility | Current source and entry point | What still crosses or duplicates the boundary |
| --- | --- | --- |
| Logical scalar declarations and values | [core catalog metadata](../crates/msduck-core/src/catalog.rs), `TypeMetadata::logical_type`; [logical types](../crates/msduck-core/src/types.rs) | Catalog identity and incomplete metadata differ intentionally from validated value/storage declarations. A zero-capacity expression result cannot be forced through a positive-width storage declaration. |
| Row and lexical scope | [binding_scope](../crates/msduck-sql/src/binding_scope.rs), `Field`, `Scope`, `resolve` | `Field` already combines type metadata, properties, collation and JSON provenance, but execution also carries an independent vector of type overrides. |
| Explicit catalog input | [catalog_snapshot](../crates/msduck-sql/src/catalog_snapshot.rs), `CatalogSnapshot` | Tables, types and default collation are pure inputs. Root callers reacquire them for several passes. |
| Catalog acquisition | [query_catalog](../src/query_catalog.rs), `snapshot`, `bind_query_with_parameters`, `projection_with_parameters` | Reads `sys.types` and `sys.columns`; parameter scope construction is repeated. Snapshot helpers called separately do not establish one shared statement snapshot. |
| Operand annotation | [root aggregate catalog adapter](../src/aggregate_columns/catalog.rs), `acquire`, `annotate`; [pure operand binder](../crates/msduck-sql/src/aggregate_columns.rs) | Uses another snapshot shape, physical `information_schema` declarations and additional declared-target/catalog reads. This cannot be replaced with only a list of logical result columns. |
| Result declarations | [SQL result_types](../crates/msduck-sql/src/result_types.rs), `expression_type`, `projection`; [projection](../crates/msduck-sql/src/projection.rs), `query_fields`, `member_expression` | Syntax-only `ResultType` covers character/time/money; catalog-aware inference returns partial `TypeMetadata`. These overlap but have different coverage and unknown/NULL behavior. |
| Properties and constant selection | [result_properties](../crates/msduck-sql/src/result_properties.rs), `expression_with`; [constant_case](../crates/msduck-sql/src/projection/constant_case.rs), `properties`; [core result](../crates/msduck-core/src/result.rs) | Nullability/origin are separate from type. Integer constant CASE selection is bounded, does not inspect parameter values and must not become an executor. |
| Collation validation and lowering | [collation_validation](../crates/msduck-sql/src/projection/collation_validation.rs), `comparison_plan`, `lower_bin2_comparisons` | Validation builds a temporary postorder plan before rewriting. Its traversal-local node pointers do not escape; a persistent compiler plan needs stable identities. |
| Wire override acquisition | [root result_types](../src/result_types.rs), `bound_projection`, `fill_fields` | Clones the AST, replaces selected parameter references with typed NULL casts, runs syntax inference, then acquires another projection snapshot. Already inferred overrides take precedence over field metadata. |
| Normal and failed result descriptions | [engine](../src/engine.rs), `encode_batches`, `wire_type`; [query_error](../src/query_error.rs), `describe`, `describe_fields`, `wire` | Success uses Arrow types; ordinary errors use prepared DuckDB types; preparation-time overflow can use logical fields alone. Each currently assembles descriptors separately. |
| Preparation and runtime | [engine](../src/engine.rs), `validate_prepared_statements`, `execute_inner`, `Translator` | Separate preparation/execution pipelines use overlapping passes. `Translator` also receives values, transaction counters, row count, login and caught errors. |
| DML images and output destinations | [output_bind](../crates/msduck-sql/src/output_bind.rs), `Context`, `rebind`; [joined output](../src/engine/joined_output.rs) | Logical private-image declarations must survive independently of native storage. Materialization, transactions and sink writes remain root effects. |

For an ordinary query, `execute_inner` currently calls
`bind_query_with_parameters` to retain fields and lower BIN2 comparisons. Later it
calls `bound_projection` on the statement after intervening transformations,
then operand annotation, nested JSON lowering and `Translator`. These are not
merely two views of one immutable bound result. OUTPUT has a separate metadata
projection and materialized-image path, so it needs its own explicit contract
instead of reusing whichever mutated statement happens to be available.

There is a concrete alignment asymmetry to cover first: `encode_batches` and
`query_error::describe` gate logical **names** on matching column counts, and
`wire_collation` does likewise for collations. Their property lookup is positional
without the same count guard; type overrides also have positional selection.
`describe_fields` has a stricter completeness/alignment gate. This source review
identifies an inconsistency, not a demonstrated public SQL reproducer. Test a
mismatched logical/physical shape directly before choosing a compatibility policy.

## Contract to establish

One binding result should retain the original output order and distinguish an
unknown output shape from a known shape containing columns with unknown facts.
An empty vector must not conflate “no result,” “cannot infer star expansion,” and
“known result columns.” Each known column carries:

- SQL label, including duplicate and empty labels;
- declaration identity and result capacity, preserving alias user type, family,
  precision/scale, MAX and valid zero-capacity expression results;
- nullable/unknown state and stored/identity/derived/expression origin;
- collation label or conflict, and JSON fragment provenance.

Evolve `Field` and existing result declaration rules rather than introduce a
parallel general-purpose type system. Retain partial facts explicitly. Where the
validated core `Type` cannot represent a result declaration, retain that fact in
the result domain; do not weaken storage validation to make it fit. In particular,
`TypeMetadata::logical_type` intentionally rejects zero character capacity, while
`result_types::ResultType` can represent it. Unknown and untyped NULL must remain
distinct during type resolution even if an outward API currently uses `Option`.

The root obtains a statement catalog snapshot and supplies logical parameter
**declarations**, name-resolution context and compilation-affecting options.
Binding and semantic planning consume only these explicit inputs. Acquiring data,
resolving the selected database, reading clocks, allocating identities/sequences,
maintaining transactions and reading parameter values stay in the shell.

Separate a function's declaration from its evaluation timing. A plan can contain
an explicit runtime counter, parameter, clock or sequence operation without
sampling it during compilation. Represent evaluation multiplicity and conversion
boundaries so lowering cannot duplicate volatile operands. Prepared validation
must not execute DML or evaluate a sequence to obtain metadata. Rebinding values
must not silently change a prepared result declaration.

For serialization, normalize prepared-DuckDB and Arrow schema observations into
one root-owned physical shape adapter. Combine that shape with the ordered logical
contract once to produce TDS columns and validated value adapters. Use the same
result for successful rows and errors where SQL Server requires metadata first.
Logical declarations are authoritative where proven; physical observations check
representation compatibility, not SQL semantics. An incompatible shape needs an
explicit error/fallback policy; never attach unrelated NOT NULL properties or emit
a partially fabricated descriptor. Failure phase, error identity, continuation
and DONE command belong alongside this contract, not inside a guessed wire type.

## Ordered implementation tasks for owner approval

Each row is one proposed PR. Shared files mean these tasks are primarily serial;
parallel sessions can independently prepare reference fixtures after their scopes
are approved. Do not publish these rows as Ready without owner approval.

| Step | Bounded change and expected files | Dependencies | Acceptance evidence |
| --- | --- | --- | --- |
| 1. Align every descriptor input | Add one root alignment/shape decision used by `src/engine.rs` and `src/query_error.rs`; tests beside those adapters. Keep declaration inference unchanged. | None | Direct tests for shorter/longer logical fields and overrides, duplicate labels, unknown shapes, fixed non-null types and failed preparation. Prove no unrelated property is attached by index. Preserve existing full-width success/error captures. |
| 2. Bind one result contract | Evolve `binding_scope::Field`/result representation in `msduck-sql`; combine syntax-only result capacities with catalog-bound fields before lowering. Replace `src/result_types.rs::bound_projection`'s second inference pass at the query execution call site. | 1 | Character/time/money, alias identity, zero/MAX capacities, untyped NULL, parameters, CTEs, stars and sets resolve from the same original AST and snapshot. No dummy parameter-value AST is needed for result inference; immutable input tests pass. Preserve unknown facts instead of substituting physical defaults. |
| 3. Share statement catalog acquisition | Introduce explicit root compilation inputs for `query_catalog`, `aggregate_columns/catalog` and callers. Adapt existing pure binders to supplied snapshots, including private OUTPUT images. | 2 | Instrumented integration tests show repeated logical projection passes do not reread the same catalog within one compilation. Preserve physical-storage facts needed by operand lowering. DDL, temp objects, rollback and later statements get fresh inputs; do not cache snapshots across statements without invalidation. |
| 4. Normalize physical result adaptation | Add a root result adapter consumed by normal Arrow encoding, prepared error description and logical-only failure description; remove duplicate descriptor assembly. | 1–3 | Same supported logical declaration yields equal names/types/lengths/flags/collation through success, empty results and metadata-before-error paths. Reject incompatible carriers, widths or NULLs before partial token output. Retain exact error/DONE differences where SQL Server differs by failure phase. |
| 5. Bind character operations explicitly | Extend SQL planning to represent concat operands, result capacity, input encoding and collation; lower through a root native adapter using existing core concat rules. Replace heuristic concat dispatch for the covered forms. | 2; integrate through 4 | Replay all character concat captures, BIN2 edge cases and prepared/column inputs. Check each intermediate ANSI/Unicode cap, MAX, NULL, raw surrogate units, and single evaluation across native vector chunks. Exercise downstream functions as well as direct results; a STRUCT carrier must not silently break ordinary text consumers. |
| 6. Reuse semantic planning in prepare and execute | Extract the common supported query pipeline from `validate_prepared_statements` and `execute_inner` into `msduck-sql`, returning typed operations and diagnostics. Keep backend preparation/runtime in root. | 3–5 | Prepare/direct execution use the same declaration and diagnostic decisions for the covered query family. Runtime parameter changes, counters, clocks and sequences preserve timing; preparation has zero DML effects. Stable node identities survive lowering; invalid plans do not partially mutate caller ASTs. Expand to DML/OUTPUT only with its own atomicity tests. |

Do not require a whole-server compiler rewrite before fixing a proven descriptor
bug. Conversely, adding one more override vector or borrowing backend result
values for inference would extend the split contract rather than implement it.
Complete typed execution planning remains larger than these initial query tasks.

## Evidence to retain and tests to add

Existing tests and captures are regression inputs, not proof of the proposed
contract. This review inspected their source; it did not rerun the full suites.

- Pure scope/property coverage includes
  `unknown_and_ambiguous_local_names_block_outer_type_fallback`,
  `parameter_declarations_survive_cte_boundaries_without_runtime_values`,
  `declarations_survive_aliases_and_null_extension_without_mutating_catalog`,
  and the bounded constant CASE tests. Keep scope barriers and SQL NULL distinct.
- `bin2_comparison_plans_match_reference_diagnostics_and_are_atomic` covers
  validation before rewriting. Extend this invariant to new typed operations;
  shared snapshots must not change error selection through hash iteration.
- [Character concat declarations](../reference/character-concat-declarations.json)
  and [full captures](../reference/character-concat.json) separate type binding
  from values. The latter contains isolated UTF-16 surrogates; preserve its raw
  JSON through a reader that supports those code units instead of replacing them
  to accommodate a Rust JSON decoder.
- [Constant CASE](../reference/case-constant-properties.json),
  [integer overflow](../reference/integer-overflow.json) and
  [BIN2 edges](../reference/bin2-constant-edges.json) cover declaration/failure
  boundaries. Compare complete descriptors and decoded DONE fields, not rows
  alone; metadata-only differences remain failures of that comparison.
- [Independent client tests](../tests/tedious.test.mjs) already include conditional
  parameter declarations, failed decimal metadata, OUTPUT/prepared images,
  raw Unicode and integer overflow. Add paired empty/nonempty, typed-NULL/non-NULL,
  prepare/execute and success/error probes for each migrated declaration.

Use pure SQL/core tests for deterministic planning, root integration tests for
physical-schema adaptation and acquisition counts, and tedious plus pinned live
SQL Server captures for observable compatibility. Run the AGENTS.md workspace,
Clippy, client and audit gates for implementation changes. Record exact revisions;
a diagnostic audit completing is not a SQL Server equivalence pass.

Measure any compile-time claim with separate cold pure-crate, incremental pure
edit and full-server builds. No build speedup is established by this document.
Snapshot-query counts can establish fewer acquisition calls, but not a runtime
speedup without measurement. A separate DuckDB adapter crate is a later decision
once these dependencies stop crossing the boundary.

## Findings reused from mirek/mssqlite

Rechecked the local upstream checkout at
`7f71f2081602f8e3051998f5c11f058e65fe24ec`; see the broader
[reference review](reference-review.md) and retained licenses.

- [`packages/engine/src/metadata.ts`](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/engine/src/metadata.ts)
  centralizes catalog-to-wire mapping and carries a column's declared metadata.
  Reuse that separation of declaration and encoding. Its `typeInfoOfValues`
  fallback infers from observed rows; do not carry that fallback into a compiler
  contract that must describe empty and NULL-only results.
- [`packages/transpile/src/infer.ts`](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/transpile/src/infer.ts)
  explicitly offers approximate text/number/unknown classification for `+`.
  It is useful for identifying operation families, not a sufficient result type
  or SQL precedence contract. The Rust concat planner needs exact declarations.
- [`packages/transpile/src/collation.ts`](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/transpile/src/collation.ts)
  separates collation lookup and backend normalization. Its first-available
  operand/branch label selection must not replace msduck's explicit label and
  conflict rules. Wire collation, comparison encoding and expression-label
  precedence are related but distinct decisions.

These are source-level reuse decisions, not evidence that either server has
complete SQL Server behavior. The existing core/SQL/TDS dependency direction
remains the constraint for every proposed task.
