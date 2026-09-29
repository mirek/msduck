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
ran on linux.local. No floating aggregate runtime correction is included in
this evidence task. Current-server comparison and follow-up findings are added
below after the clean-checkout replay.
