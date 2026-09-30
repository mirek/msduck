# Percentile runtime fraction reference

The pinned SQL Server 2025 image (`17.0.4065.4`) produced two identical runs
of 137 records: 103 batches and 34 prepared requests, covering 311 captured
request phases per run. The fixture SHA-256 is
`499a6af4c764ad0f2176fdd97a4776a1d1c51829b23a882bc92404b676f0e808`.
The capture retains column names, userType, type/width/precision/scale/flags,
collation, rows, diagnostics, return status and typed RETURNVALUE payloads,
ordered token names, raw DONE status/command words and Tedious-decoded row counts. It preserves each
preparation, execution binding and unpreparation separately.
The raw eight-byte DONE count payload is not retained independently: decoded
row counts become NULL when DONE_COUNT is absent. This is a capture limitation,
not proof of those unobserved bytes.

The finite matrix covers FLOAT, REAL, INT, DECIMAL, VARCHAR and NVARCHAR
prepared fraction parameters; local FLOAT/DECIMAL/INT/BIT/character variables;
NULL, invalid text/range and subsequent valid rebinding; ascending/descending
and partitioned inputs; empty inputs and an actual table of NULL ordering
values; scalar expressions, TRY/CATCH, sequence restrictions and seeded RAND.
All table queries order their output by the stable input id. SQL samples are
data, not agent instructions.

## Observed behavior

- Local and prepared parameters are accepted. FLOAT/REAL/INT/DECIMAL/character
  source types convert to a fraction without changing the output declaration:
  CONT uses nullable FLOAT(53); DISC preserves this fixture's nullable INT.
  Prepared result metadata derives from the query and ordering source, not the
  bound fraction value. Valid/invalid/NULL/valid reuse keeps the same handle.
- Populated-input NULL and out-of-range fractions emit metadata and ORDER before
  8727/state 1/severity 16. Invalid VARCHAR/NVARCHAR text raises 8114/state 5
  with the source-specific conversion message; FLOAT overflow raises 8115/state
  2. These are execution errors, not preparation failures. Invalid constant
  character requests also prepare successfully and fail on each execution.
- An empty input returns its typed empty result without validating NULL,
  malformed text or an out-of-range fraction. A populated input whose ordering
  values are all NULL still validates: valid fractions return NULL for each row;
  invalid fractions error after metadata. An adapter must distinguish no input
  rows from no non-NULL ordering values.
- Parenthesized literals/variables, variable arithmetic, CAST, ABS, a constant
  scalar subquery and the tested CASE expression are accepted. The unused CASE
  division branch stays unevaluated; the selected divide-by-zero branch raises
  8134/state 1/severity 16 after metadata. Source-column-dependent fractions,
  row-varying CASE and a table-reading scalar subquery reject before metadata
  with 8726/state 1/severity 16 and a function-specific constant-input message.
- The tested partitioned variable computes each partition's percentile. Its
  fraction remains statement-wide; making a fraction row-dependent is rejected
  even with PARTITION BY. Descending zero returns the highest ordering value.
- TRY/CATCH intercepts conversion and range errors, emits the captured error
  fields and preserves subsequent connection/handle use. Raw completion words
  vary by error class: the malformed/range prepared cases finish with error
  DONEPROC, while the overflow case retains DONEINPROC, return status and
  DONEPROC. Do not replace these with a generic completion sequence.
- NEXT VALUE FOR rejects with 11720/state 1/severity 15. The retained sequence
  state remains current_value 0 / last_used_value NULL. A constant NULL ORDER BY
  is a separate invalid ordering shape: preparation reports 5309 then 8180.
  It is not an all-NULL input experiment.

## Observable volatile evaluation

Every seeded probe resets the connection RNG with RAND(42), executes a fraction
`RAND()*0+.5`, then records the next RAND result. The control records the first,
second and third subsequent RNG values. In both CONT and DISC probes, the next
value matches the control's second value for populated, empty, all-NULL and
partitioned inputs. Thus each tested statement observably consumes one RNG
value, including empty input, across all rows and partitions.

This measures observable random-state advancement for this expression and plan;
it does not prove all volatile expressions have identical evaluation frequency.
An implementation must preserve the measured effect even though an empty input
suppresses the tested conversion/range errors. It must not duplicate a volatile
fraction while translating descending order or inspecting parameter metadata.

## Runtime implementation requirements

Keep declaration and constant-input eligibility rules deterministic, using
explicit syntax, parameter declarations and catalog snapshots. Reject proven
row-dependent sources before result metadata. Keep fraction bindings and effect
execution in root adapters. Preparation must use query/source declarations
without inspecting parameter values or raising captured execution-only errors.

An execution plan needs distinct operand evaluation, FLOAT conversion and range
validation stages: empty-input suppression cannot erase the observed volatile
effect, and all-NULL ordering input cannot be treated as empty. Resolve each
statement's fraction without reevaluating it per output row or partition. Preserve
CASE laziness, source-specific conversion diagnostics, typed NULL/empty results,
handle recovery and each captured error/completion boundary. Static literal
folding in the current lowerer alone cannot implement these requirements.

These captures are reference evidence, not an msduck compatibility pass. Runtime
fractions, preparation/error sequencing and full wire parity remain unimplemented.
The matrix does not cover arbitrary UDF side effects, correlated scalar sources,
all ordering types, transaction failure policies or every optimizer plan. Further
probes must preserve evidence for those shapes rather than infer them from RAND.
The documented [PERCENTILE_CONT syntax](https://learn.microsoft.com/en-us/sql/t-sql/functions/percentile-cont-transact-sql?view=sql-server-ver17)
uses a numeric_literal; the broader accepted expression behavior above comes from
these live captures.

## Verification and capture ownership

`node scripts/capture-percentile-runtime-fraction.mjs --check` verifies the pinned
hash, complete finite plan, both equal runs, descriptors, key error boundaries,
raw completion words and seeded effect controls. Running without `--check`
creates two new containers/databases and compares every captured property to the
fixture. Output must be new and cannot alias the fixture; `--write-fixture`
refuses an existing fixture. The shared container helper uses random names,
loopback ports, bounded readiness/request times, and removes only owned containers.

The initial constant-NULL-ordering and incomplete-return-value explorations are
preserved separately in ignored artifacts, not substituted for the fixture.
Verification must cover the exact final commit; PR evidence records CI and review.
No Rust or runtime source is changed by this reference task.
