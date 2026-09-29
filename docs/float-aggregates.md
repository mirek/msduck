# FLOAT and REAL aggregate reference evidence

The pinned SQL Server 2025 image in `scripts/lib/reference-container.mjs`
returned product version **17.0.4065.4**. Two fresh containers each executed
46 identical requests through independent tedious connections. The complete
observations are retained in `reference/float-aggregates.json`; its SHA-256 is
`e19bac07a0d08caf68fc94101a3ee1dee891faf6598002af44d8d93800492ce2`.

The matrix covers SUM, AVG, MIN and MAX over REAL, FLOAT(24) and FLOAT(53):
fractional, DISTINCT, singleton, NULL, empty, signed-zero, grouped, ordered,
bounded and empty window frames, and two cancellation arrangements. Additional
requests expose loss of low-order additions, REAL sums beyond the REAL range,
and finite FLOAT intermediate overflow followed by successful reuse.

SUM and AVG return FLOAT(53) for all three source declarations. MIN/MAX retain
four-byte REAL/FLOAT(24) or eight-byte FLOAT(53). For REAL values 0.1, 0.2, 0.3,
NULL, 0.3 the reference SUM is 0.9000000283122063: source rounding happens before
widened addition. Two REAL values near 3e38 produce a finite sum near 6e38.

The FLOAT sequence 9007199254740992, 1, 1, 1 produces SUM 9007199254740992 and
AVG 2251799813685248, demonstrating rounded binary64 accumulation in these
plans. The two cancellation arrangements produce different answers. These
small captures do not establish an order contract for arbitrary parallel plans.

SUM and AVG of two finite FLOAT values 1e308 report error 8115, state 2,
severity 16, with the canonical float-overflow message. A later negative operand
does not erase an overflowing SUM transition. All reuse probes succeed. Signed
zero in MIN/MAX is preserved in separate hexadecimal bit fields, because JSON
row serialization loses its sign. The fixture retains every descriptor, NULL,
warning, error, raw DONE status/command and observed token event; nothing is
normalized to claim whole-case agreement.

Commands:

```sh
node scripts/capture-float-aggregate-reference.mjs --check
node scripts/capture-float-aggregate-reference.mjs artifacts/compatibility/float-aggregates/fresh.json
node scripts/capture-float-aggregate-reference.mjs --compare
```

Fresh capture uses two new reference containers and compares them with the
retained fixture. `--write-fixture` is for the initial capture only and refuses
to replace a fixture. Output files use exclusive creation and cannot alias the
fixture. `--compare` requires a clean checkout, builds its exact revision with
`cargo build --workspace --all-targets --locked`, hashes the executable and
retains full local/reference results plus separate floating-bit differences.
Use the shared runner lock when using the Linux target cache.

The local emulated container exited before readiness; the successful captures
ran on linux.local. No floating aggregate runtime correction is included in this evidence task.

## Current-server replay

Clean-checkout replay of `dff2bffaff6070891327d3a14d9196b7384d2d0c`
retained 45 comparisons (ProductVersion excluded). **22** matched completely;
there were **5** floating-bit differences and **86** result/event differences.
The executable hash and full observations are in the ignored artifact
`artifacts/compatibility/float-aggregates/comparison-sign.json`.

Three FLOAT overflow requests returned Infinity instead of the captured 8115
error. This is a runtime defect, not an acceptable approximate answer. One
FLOAT(53) DISTINCT request also changed SUM and AVG low bits. The earlier replay
at `b19405741fcd3cc6bc2ec39b41d5fffebc79cf1b`, with identical Rust sources,
matched that DISTINCT request: its backend deduplication order can change.
Both observations are retained; no sorting or tolerance hides that variation.

The remaining differences include absent ORDER/NBCROW tokens and diagnostic
placement. Descriptor widths and ordinary precision probes agreed. JSON rows
lose the zero sign; comparison reconstructs only that sign from the separate
retained bit field before comparing rows. Actual bit differences remain exact.

Next runtime work must reject non-finite FLOAT SUM/AVG state/results with the
captured error identity and preserve source widening, DISTINCT membership,
one input evaluation, NULL/empty behavior and warning/reuse semantics. Ordered
and bounded-frame evidence must remain bit-exact. A physical-order claim for
unordered DISTINCT or parallel aggregation requires additional plan evidence;
the recorded low-bit variation must remain visible.
