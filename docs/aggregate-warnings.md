# Aggregate NULL-elimination diagnostics

`reference/aggregate-warnings.json` preserves 126 programs captured by
`scripts/capture-aggregate-warnings.mjs`. Two fresh pinned SQL Server containers
returned byte-identical captures on version 17.0.4065.4. The fixture retains
rows, descriptors, informational messages, errors, completion counts, and a
follow-up `@@ERROR`/`@@ROWCOUNT` read. It does not record token interleaving.

The owner-run character-extrema replay on earlier revisions matches result
values and descriptors but omits informational warning 8153. Capturing that
warning's semantics is groundwork; this change does not implement emission.

## Observed behavior

With `ANSI_WARNINGS ON`, MIN, MAX, SUM, AVG, COUNT(expression), COUNT_BIG,
STDEV and VAR emit one warning when their evaluated inputs include NULL.
All-NULL inputs warn too. Non-NULL inputs, empty inputs and predicates removing
NULL inputs do not warn. COUNT(*) and COUNT(1) do not warn just because another
source column contains NULL.

The message is number 8153, state 1, severity 0:
“Warning: Null value is eliminated by an aggregate or other SET operation.”
These single-line programs report line 1. Warning emission does not set
`@@ERROR` or enter CATCH. Ordinary SELECT row counts remain intact. The TRY/CATCH
program's following row-count read is zero; do not infer that from warning
emission alone because control-flow completion also affects session state.

Several aggregates or nullable columns in one statement produce one warning.
Two separate aggregate statements produce two. Grouped, correlated and windowed
programs each produce one warning even when multiple groups or frames consume
NULL. DISTINCT aggregates retain the warning. Ordinary DISTINCT rows and UNION
in these probes produce none.

HAVING can remove every output row while the statement still warns. TOP(0)
does not warn in the captured program. COALESCE replacing NULL before aggregation
suppresses the warning; CASE creating a NULL aggregate input causes it. Both
default-collation and BIN2 character extrema warn. All 63 programs with
`ANSI_WARNINGS OFF` suppress the warning and remain successful.

## Execution requirements

Track NULL elimination from evaluated aggregate inputs, not from schema
nullability, source rows alone, result NULLs, or surviving output rows. Preserve
the distinction between an all-NULL group and an empty source. Statement-level
diagnostic state must survive HAVING removing all results and must reset between
statements. Group and frame state must combine into one statement warning.

Do not execute aggregate operands twice to detect warnings: volatile expressions,
conversion errors and correlated sources make a second query observably wrong.
Keep warning bookkeeping in an explicit execution context owned by the root
adapter, without mutable process-global state. Deterministic crates can describe
diagnostic requirements but must not emit wire tokens or read session settings.
Preserve existing result types, ordering and exact payloads independently.

Before declaring the warning implemented, replay this fixture and the original
character-extrema fixture without dropping informational messages. Additional
ground truth is needed for wire ordering, cancellation, partial failures,
prepared execution, DML consumers and parallel native execution. Full linguistic
collation weights and transaction recovery remain separate compatibility gaps.
