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
equally named columns from different sources remain distinct. Wildcards expand
in declared source/field order, including qualified, joined, derived and CTE
sources. Modified wildcards and unresolved sources remain barriers. Expression
matching canonicalizes borrowed field identity through cloned trees, including
CAST, COLLATE and aggregate arguments. It never changes the caller's query or
catalog. Integer conversion markers inserted by `batch::parse` are unwrapped.

## Retained evidence

`reference/order-token.json` retains two matching fresh pinned SQL Server runs.
The replay resolves 65 query records; the two unresolved records are binding
errors for a missing projected column. Tests compare preparation and all three
executions of each prepared query with the same declaration-only plan, including
the execution producing no rows. Unpreparation has no ORDER token.

`reference/order-token-expanded.json` retains another two matching fresh runs.
All 86 successful nonprepared query records resolve and match exact ORDER
presence and ordinal sequence. The six unresolved records are the batch/RPC
copies of binding errors 209 (duplicate names), 408 (literal sort expression)
and 1008 (parameter-only sort expression). The two prepared arithmetic profiles
also resolve from an explicit INT parameter declaration: preparation and all
three executions retain one plan, with row counts 0,4,0,3. Both transport modes,
both fresh runs, and raw/batch-normalized ASTs are checked. A missing declaration
remains unknown, rather than inferred from captured rows or values.

Supported profiles include projected columns, aliases and numeric ordinals,
multiple keys, hidden keys, wildcard expansion, INT-column plus/multiply INT
operands, CAST of an INT column to BIGINT, COUNT(*), SUM of an INT column,
Latin1_General_100_BIN2 collation of a declared column, and captured ROW_NUMBER
window shapes. Prepared arithmetic takes only the operand's INT declaration.
TOP/OFFSET, DISTINCT, grouping, derived/CTE outputs and UNION output ordinals
are covered. Internal window or derived ordering alone produces no outer token.

The captured typed-NULL, literal 1 and literal arithmetic 1+2 projected keys
are removed from ordering. With remaining column keys, their sequence/ordinals
are preserved; with no remaining keys, no ORDER is emitted. A CASE over an INT
column predicate with identical literal branches retains its own ordinal. This
is an explicitly captured distinction, not a general constant-folding engine.
Constant UNION branches retain their projected ordinal instead of taking the
single-SELECT folding rule.

## Supplemental expression identity evidence

Review found that raw AST comparison could mistake qualified/unqualified
references inside CAST, COLLATE or SUM for different expressions. Two fresh
pinned captures on 2026-09-30 establish ordinal 1 for equivalent CAST/COLLATE
references and ordinal 2 for SUM. Complete rows/descriptors/errors/ORDER/DONE
records and the driver are retained locally under
`artifacts/order-plan-identity/`; capture SHA-256 is
`b1d165a76154c177f721e40a913749252a763f364b020831664d0684938d509a`.

Two further fresh runs show a+0, a*0 and a*1 retain ordinal 1 when projected,
and zero when used as a hidden key even when a itself is projected. Mixed
literal 1 or arithmetic 1+2 keys disappear, retaining the column's ordinal 1.
Complete records and driver are retained under `artifacts/order-plan-folding/`;
capture SHA-256 is
`4870f453216683e4a84d811ee1babab27abb4b1c178b4a64c48014d8ea0ab121`.
These ignored supplemental artifacts do not overwrite either committed fixture;
regression SQL and exact expectations are committed in `tests/order_plan.rs`.

Two fresh case/spelling runs confirmed SUM/collation case differences,
parenthesized literal operands and leading-zero integer operands retain the
projected ordinal. Cloned comparison trees normalize only the supported builtin
and collation identities, INT literal spellings and redundant parentheses.
Complete records and driver are retained under
`artifacts/order-plan-case-identity/`; capture SHA-256 is
`02572bf3b363a69a36d0ab8b0ae53cfa2b95f73a64500645d460d889c7841e43`.
The original AST remains unchanged.

## Remaining boundaries

Ambiguous names, unknown declarations, modified wildcard shapes, arbitrary
functions/expressions, parameter-only keys and unproven folded expressions remain
explicit barriers. Wider expression profiles need further ground truth. The
supported profiles are not a general optimizer or complete SQL Server
ORDER-emission specification. The planner checks the USHORT token count bound
before projection work; the bound does not establish SQL Server acceptance of
arbitrary large or duplicate key lists. Binding remains the caller's job.

This module is not wired into the server. Root-side result framing, successful
binding/error precedence, prepared phases and ORDER placement still require a
separate adapter task. `engine.rs` is outside this task's scope. Fixture agreement
proves these logical plans, not server row/descriptor/token compatibility.
