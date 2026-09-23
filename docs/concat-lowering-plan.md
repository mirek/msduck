# Typed character concatenation execution

The current declaration and execution paths disagree. This plan covers character
`+` lowering; it does not claim that the execution fix is implemented. Base source
is `8d9f9233fbc82335ded210f67ff7e504d65cc1ae`. Task
`concat-lowering-plan-v1` owns only this document.

## Evidence and failure

[Core shape and prefix rules](../crates/msduck-core/src/concat.rs) compute a
bounded ANSI capacity of at most 8000 bytes or Unicode capacity of at most 4000
UTF-16 units. `shape` also preserves fixed/variable families and MAX; `ansi` and
`utf16` construct bounded prefixes. These execution helpers are not invoked by
the translator's character-plus lowering.

[Result type inference](../crates/msduck-sql/src/result_types.rs), through
`operand` and `concatenate`, and [catalog expression metadata](../crates/msduck-sql/src/expression_metadata/character.rs),
through `concat_info`, use that shape rule. [Projection inference](../crates/msduck-sql/src/projection.rs)
applies `concat_info` to character operands, including typed and untyped NULLs.
The two inference entrypoints should ultimately consume one bound operation.

In [Translator](../src/engine.rs), the binary-expression branch rewrites `Plus`
to DuckDB `StringConcat` when both operands satisfy `string_expr`. That rewrite
has no bounded prefix operation. The [TDS encoder](../crates/msduck-tds/src/lib.rs)
rejects oversized NVARCHAR values with `NVARCHAR value exceeds declared width`.
Increasing the descriptor or suppressing that check would hide the mismatch.

The [SPACE client test](../tests/tedious.test.mjs), named
`SPACE applies integer conversion, negative NULLs and the 8000-character cap`,
prepares `SELECT SPACE(@n),N'a'+SPACE(@n)+N'b'`. For the large positive input it
expects an 8000-space first column and an 8002-character second column. The
second expectation conflicts with bounded Unicode concatenation semantics.
Microsoft documents [bounded truncation and intermediate-result behavior](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/string-concatenation-transact-sql?view=sql-server-ver17).
A MAX operand in a later node cannot restore text already truncated in a child.

Observed evidence: Linux revision `58ab166` passed 472 Rust tests but only 396 of
397 clients; the named test raised the width error. A focused run on `a9ba5e7`,
which lacks the result-alignment change, reproduces it. Logs are retained locally
at `/tmp/msduck-result-alignment-remote.log` and
`/tmp/msduck-smp-space-client.log`; PRs #13 and #16 record the failed validation.
These runs establish an inherited failure, not a new alignment regression.
Source inspection explains the uncapped execution/capped descriptor mismatch;
there is no instrumented capture of each intermediate native value yet.

The retained [26 SQL Server captures](../reference/character-concat.json) cover
all 16 fixed/variable ANSI/Unicode family pairs, bounded declaration caps, MAX,
typed/untyped NULL and raw surrogate composition. The cases named `ANSI cap`,
`Unicode cap` and `Unicode mixed cap` concatenate short values: they verify
capacities, **not runtime overflow truncation**. No retained case is the exact
prepared SPACE query. Inference predicts `a` followed by 3999 spaces, with the
final `b` discarded; capture the exact prepared query before changing its test.
Read these fixtures with a raw UTF-16-capable reader such as Node: the isolated
surrogate in `raw Unicode unit` must not be replaced to satisfy a JSON library.

## Bound operation and effects boundary

Bind an explicit character-concat node in `msduck-sql` before backend rewriting.
It needs child declarations, result family/capacity, collation and conversion
requirements, NULL behavior, and an operation identity surviving AST traversal.
Inputs are the expression, parameter declarations and explicit catalog snapshot;
parameter values, DuckDB queries and observed rows cannot determine the type.
Keep unknown operands unknown until resolved or diagnosed. Numeric precedence,
alias-type precedence, binary concatenation and the distinct CONCAT function
must not be silently classified as this character operation.

Lower each bound binary node into a native operation taking each child once.
Convert children according to the bound collation/encoding, preserve fixed-input
padding, propagate NULL, then apply that node's capacity using the core prefix
helper. Parent nodes consume that result, including any prior truncation. Do not
flatten/reassociate a chain or apply just one final truncation. Enforce a separate
resource limit for MAX and checked byte/unit arithmetic before allocation.

The root [Unicode carrier](../src/unicode_carrier.rs) already represents raw
UTF-16 as `STRUCT(__msduck_utf16le BLOB)`. Its `Pack`, `Cast`, `Slice`, `Length`,
`FirstUnit`, `Bin2Compare`, `read`, `units` and `encode` provide reusable adapter
patterns. `register` supplies `__msduck_carrier_input`, which selects conversion
by physical type without stringifying an existing carrier. Reuse its vector
validity and single-evaluation tests, but independently test each new adapter.
ANSI prefixing must occur in the resolved code page, not UTF-8 bytes.

A blanket replacement of Unicode `+` by a STRUCT-returning function is incomplete:
LOWER, UPPER, trimming, replacement, CASE/COALESCE, comparisons, sorting, assignment,
casts and storage must accept the resulting physical representation. Audit each
consumer. Keep lossless carriers through supported operations; never stringify
STRUCT or replace isolated surrogates as an implicit fallback. Extend missing
consumers with explicit semantic adapters before claiming full composition.
Output metadata remains the bound logical type even for empty and NULL-only sets.

## Ordered implementation tasks

1. **Capture overflow reference behavior.** Scope a new reference fixture and its
   generator. Capture the exact prepared SPACE query at NULL, -1, 0, 2, 3999,
   4000, 8000 and INT_MAX; preserve complete descriptors and completion/errors.
   Include left-associated and explicitly grouped chains with an early versus
   late MAX operand, ANSI overflow, mixed encodings and surrogate boundaries.
   Use the pinned SQL Server image recorded in the existing captures.
2. **Bind typed operations.** Scope a new SQL planning module, its crate export
   and pure tests. Share existing core shape rules; plan from catalog/parameter
   declarations and collation labels. Test nested operations, scope barriers,
   unknown and NULL operands, mixed numeric precedence and aliases. Validate
   the whole plan before mutating the input AST. No backend calls.
3. **Implement native execution and integrate consumers.** Scope a new root
   concat adapter, root registration and translator integration, plus the exact
   consumer modules identified by the audit. Share the core byte/unit prefix
   operations; include vector NULL, selected/dictionary input, multi-chunk,
   overflow-limit and volatile-child counters. Coordinate this scope with the
   still-claimed result-alignment task before publishing it as ready.
4. **Verify public execution and prepared metadata.** Scope the client test and
   differential fixture runner after the reference and adapter tasks land.
   Correct the SPACE expectation from the captured result, not from the current
   backend. Compare all retained cases and new overflow probes, including empty
   results and post-error connection reuse. Do not discard descriptor or DONE
   differences to obtain a passing comparison.

These are proposed successor scopes, not currently claimable tasks. Publish each
with concrete disjoint paths and dependencies in the protected registry. The
current task authorizes neither engine edits nor changes to test expectations.

Acceptance for the implementation includes formatting, strict workspace Clippy,
workspace tests, the independent client suite and a diagnostic corpus audit.
Compare raw audit changes to the retained baseline; completion of that audit is
not proof of SQL Server equivalence. Run native/client work in an isolated build
or the locked Linux runner and record the tested revision.

## Upstream reuse

Inspected `mirek/mssqlite` at
`7f71f2081602f8e3051998f5c11f058e65fe24ec`, specifically
[`packages/transpile/src/infer.ts`](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/transpile/src/infer.ts).
Its `infer` returns text/number/unknown and uses function-name sets to guide `+`.
Reuse that inventory for coverage, not its approximate classification as the
result contract: it does not bind character capacity, code page or intermediate
truncation. The copied [T-SQL skill](../.agents/skills/t-sql/SKILL.md) remains a
reference guide; retained SQL Server captures and exact public probes determine
compatibility acceptance.
