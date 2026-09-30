# Logical ORDER metadata

`msduck_sql::projection::order::infer` accepts an original query AST, an explicit
catalog snapshot and declaration scope. It returns `NoToken`, ordered one-based
projection ordinals in `Token`, or an explicit `Unknown` barrier. Zero preserves
a nonprojected sort key. Descending direction is not encoded in this token.
The caller still validates and binds the statement before emitting any result.
An unknown plan must never be replaced with guessed bytes.

The planner reads neither parameter values nor backend rows. Empty predicates,
TOP(0) and prepared execution values do not change the captured plans. Source
identity comes from fields borrowed from the same caller-supplied snapshot;
equally named columns from different sources remain distinct. The planner does
not modify the query AST. Integer conversion markers inserted by `batch::parse`
are unwrapped when recognizing the captured typed-NULL sort expression.

## Retained evidence

`reference/order-token.json` retains two matching fresh pinned SQL Server runs.
The replay resolves 65 query records; the two unresolved records are binding
errors for a missing projected column. Tests compare preparation and all three
executions of each prepared query with the same declaration-only plan, including
the execution producing no rows. Unpreparation has no ORDER token.

Supported retained shapes include projected keys, aliases and numeric ordinals,
multiple keys, hidden keys, INT-column-plus-one expressions, empty results,
TOP/OFFSET, DISTINCT and grouped projected keys, derived outputs, UNION outputs,
and an outer order on ROW_NUMBER. Internal window or derived ordering alone
produces no outer token. A sort on a projected `CAST(NULL AS INT)` alias has no
ORDER token, including a scalar query or a projection with additional columns.

A supplemental capture on 2026-09-30 used the same pinned image and trusted raw
ORDER/DONE capture helpers from checkpoint
`bd2ffbe48edae503485bb4b8c523557bc4250628`. Two fresh containers agreed on 18
queries in both batch and RPC modes. The complete rows, descriptors, errors and
raw ORDER events are retained locally in
`artifacts/order-plan-probes/capture.json`, SHA-256
`d948cab4148b3c8c17ac12f1d09bafb06b85835e17395f3961dd52ae5cbc2bf2`.
The supplemental driver is retained beside it as `harness.mjs`; these artifacts
are ignored and do not replace the committed reference fixture. Regression SQL
and exact ORDER expectations are present in `tests/order_plan.rs`.

The supplemental observations establish these concrete optimizer cases:

- Identical constant UNION and UNION ALL branches, typed-NULL branches and mixed
  NULL/integer branches retain ordinal 1.
- Equality joins do not substitute another source's projected column: ordering
  by the unprojected joined column retains zero; projecting both columns uses
  ordinal 2. Inequality and LEFT joins retain zero in the corresponding probe.
- A fixed-key predicate and TOP(1) retain ordinal 1.
- Typed-NULL alias ordering has no token with an empty predicate, TOP(0), no
  FROM clause, or an additional ordinary projected column.
- Partitioned ROW_NUMBER and its empty-input variant retain ordinal 2.
- Duplicate bare projected names are a SQL Server binding error, not a token.

## Remaining boundaries

Wildcard expansion, ambiguous names, unknown declarations, arbitrary arithmetic,
parameter-valued sort expressions and unproven folded expressions remain
explicit barriers. Aggregate result sorting and wider expression profiles need
further ground truth. The supported profiles are not a general optimizer or a
complete SQL Server ORDER-emission specification.

This module is not wired into the server. Root-side result framing, successful
binding/error precedence, prepared phases and ORDER placement still require a
separate adapter task. `engine.rs` is outside this task's scope.
