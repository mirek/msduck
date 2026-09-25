# DATEPART/DATENAME startup measurement

`datepart::register` used to create 77 related DuckDB macros: two DATEFIRST wrappers, 30 typed input wrappers, 15 DATENAME value wrappers, and 30 public dispatch variants. `src/datepart.rs` now inlines each input wrapper's existing expression into its dispatch variant. All 30 DATEPART/DATENAME dispatch names, native scalar functions, DATEFIRST lookup, typed TIME/DATE checks, diagnostics, and DATENAME result expressions remain. The three root tests that called the private input wrappers now call the public dispatch wrappers and still check single evaluation over 6,000 rows. There are 47 macro definitions after the change.

The ignored `datepart::tests::startup_benchmark` records both `datepart::register` and full fresh in-memory `Server::open` durations in one process. Run `node scripts/bench-datepart-startup.mjs 0,1` on Linux to execute 20 fresh-server samples with affinity restricted to exactly two specified cores. The script reports raw milliseconds, median p50, and nearest-rank p95 (19th sorted sample). Cargo compilation occurs before the timed test. `MSDUCK_BENCH_REVISION` labels the output; use the same host, toolchain, CPU pair, and source base for comparison. The benchmark is ignored by ordinary tests.

The controlled comparison used `linux.local`, Linux cores 0 and 1, the same `cargo test --lib` harness and isolated target, Rust 1.98.1 on both sides, and merged-main `2a8d235` as the baseline. The baseline added only the identical benchmark hook/test. Candidate measurements used the changed registration code. All 20 samples finished successfully. Compatibility checks use CI's pinned Rust 1.95.0 toolchain; host-default 1.98.1 Clippy introduces an unrelated lint in `msduck-core`.

| Measurement | Main p50 / p95 | Candidate p50 / p95 | Median change |
| --- | ---: | ---: | ---: |
| `datepart::register` | 99.846 / 106.581 ms | 65.981 / 70.096 ms | −33.9% |
| Fresh `Server::open` | 484.244 / 508.067 ms | 449.632 / 470.592 ms | −7.1% |

Raw registration milliseconds, in sample order:

```text
main:      99.765590,101.980319,98.530742,109.526923,98.041788,98.386221,100.518207,99.414924,103.922168,99.545588,99.671434,101.686420,100.078074,101.327519,102.699353,99.803541,99.757615,99.343331,99.889130,106.581157
candidate: 102.724579,101.733758,97.795137,102.857828,97.532166,99.398704,105.311956,100.297664,105.018507,98.646078,99.318835,98.255809,98.260998,99.890944,96.627946,97.296024,102.325233,98.045936,98.863474,96.715028
inlined:   69.640059,66.178339,63.829569,66.505561,63.515132,63.577489,64.974089,66.596861,70.675855,67.981059,70.096372,68.083099,64.663137,64.190434,64.958820,64.208438,65.301350,65.782750,66.799380,66.292091
```

The `candidate` row above is an intermediate **batch-only** version that still defined all 77 macros. It measured 99.091 ms p50 and 105.019 ms p95 for registration, and 484.186 ms p50 and 512.094 ms p95 for fresh-server startup. Its lack of material improvement rules out reducing `execute_batch` calls alone as the gain. The final `inlined` row is the candidate in the table.

Raw fresh-server milliseconds, in sample order:

```text
main:      470.123704,481.158927,508.066584,493.687752,478.119697,478.704841,504.859240,474.662806,482.978398,498.273112,490.030527,470.282410,477.099771,510.989787,485.269450,486.794229,483.218516,492.779715,478.938137,497.307587
candidate: 500.698444,487.450255,493.949902,472.803322,484.872327,487.422443,493.921088,516.712122,483.499101,469.430769,512.093640,505.445797,478.326223,479.544471,478.880880,473.565606,473.541191,490.757216,471.881428,480.260919
inlined:   445.640828,453.677218,448.727296,466.132054,443.348935,439.611440,466.199360,470.592399,494.095413,441.703690,465.398693,442.134825,440.620906,444.430907,447.762363,464.349893,450.036934,449.226910,451.217209,456.399235
```

The prior owner-run GDB timestamps (94–96 ms in `datepart::register`) were directional profiling evidence, not used as the benchmark baseline. PR #181's built-in catalog seed experiment changed its two-core client duration from PR #174's 37m53s to 37m28s, which was insufficient to justify that complexity. The current optimization's full-client duration and compatibility audit must be measured on its own exact revision before merge; startup samples alone do not establish end-to-end benefit or SQL Server compatibility.

On the candidate source, `cargo fmt --all --check`, strict workspace Clippy, and `cargo test --workspace --locked` passed with Rust 1.95.0 on the isolated Linux workspace. The root crate reported 271 passed and one ignored benchmark; the workspace integration, deterministic crate, and doc tests also completed without failures. The full independent client suite and diagnostic audit are separate gates for the immutable PR head.
