# Percentile execution comparison

`node scripts/compare-percentile-reference.mjs --check` validates the pinned
reference checksum and two independent retained runs. Running without flags
builds the clean checkout with the workspace/all-targets feature graph, starts
an isolated ephemeral-port server and replays all 36 non-version records:
33 batches and three prepared requests, including each prepare, rebound
execution and unprepare phase. A final reuse request must succeed.

The output defaults to
`artifacts/compatibility/percentile-execution/comparison.json`; an optional
positional path selects another new file outside the checkout or under a
Git-ignored directory. Unignored in-checkout output is refused before building,
so the script cannot invalidate its own clean-source check. Existing paths, including fixture
aliases, are refused before building. Failures leave an incomplete marker;
only a completed replay contains results. The revision, Git tree, executable
hash, build command, fixture checksum and reference image identify the evidence.
Use the shared runner lock with the Linux cache; no local native rebuild is
needed when replaying from a clean isolated Linux checkout.

Every descriptor, row, warning, error, phase, decoded event and raw DONE status
and command is retained. FLOAT cells are also encoded as exact binary32/64
bits. Comparison neither sorts rows nor rounds values nor excludes known gaps.
A failed prepare retains its diagnostics and the absence of executions. The
reference JSON predates a separate signed-zero bit field; its serialized zero
cannot prove the original wire sign. Differences are an inventory, not a
passing compatibility test. ProductVersion alone is excluded because it is
product-specific.

## First complete replay

The Linux replay of `d51b2bc2715a88de6bc11165621b8ede9593a544` completed all
36 records and successful final connection reuse. Two records matched in full:
empty continuous input and session reuse. The other 34 retained 245 raw
structural/value/event differences. No corresponding returned FLOAT values
differed in their bits. Eleven bit-layout differences reflect absent result
sets or preparation metadata, rather than low-bit interpolation differences.
The summary now counts those separately as `floatShapeDifferences`.

The ignored artifact
`artifacts/remote/linux.local/percentile-execution-comparison-v1/initial.json`
retains all local/reference observations and differences with exact source,
executable and reference identities. Final-head replay evidence is recorded
in the linked PR. No normalization turned these observations into a pass.

Verified implementation priorities:

- Both partitioned VALUES queries fail parsing with error 102. Keep their
  original SQL and repair the grammar; changing the sample query would hide it.
- The character fraction `'0.5'` fails with error 50000 instead of returning
  the captured continuous median 2.5.
- Continuous VARCHAR/DATE order inputs and empty VARCHAR input return 50000
  and a DuckDB binder message instead of captured 402 and source-type text.
  Preparing an NVARCHAR continuous input also omits the subsequent 8180.
- Out-of-range fractions have the correct captured 8727 identity, but reject
  before the result descriptor and ORDER token that SQL Server emits.
- Successful prepared INT-continuous and NVARCHAR-discrete requests retain
  execution values, but omit preparation result metadata and differ in token
  events and completion words. The NULL execution also differs in row token
  encoding. These phases remain independent comparisons.
- DISC integer nullability and character descriptor/collation differences,
  and missing ORDER events remain visible in successful batch requests.

The available interpolation cases agree numerically, including FLOAT median
`0.15000000000000002`; this finite matrix does not establish arbitrary input,
precision, plan, signed-zero or complete percentile compatibility.
