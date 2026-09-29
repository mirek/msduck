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

Execution results and remaining implementation priorities will be recorded
here after the first complete replay.
