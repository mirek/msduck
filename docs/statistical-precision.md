# Statistical aggregate reference replay

`node scripts/compare-statistical-reference.mjs --revision FULL_HEAD_SHA` builds no
code. Run it from a checkout with a matching `target/debug/msduck` and tedious
installed. It starts msduck on an ephemeral localhost port and writes the full
reference and local observations, exact structural differences, and IEEE-754
hex bits to `artifacts/compatibility/statistical-precision/comparison.json`.
The output records the executable SHA-256 as well as the stated source revision.
The script checks the pinned reference checksum and agreement between the two
independent SQL Server runs. It compares all 34 behavioral cases, retaining
rows, typed descriptors, errors, information messages, event order, and raw
DONE status and command words. It excludes only the product-specific server
version case. Differences are never rounded or hidden behind tolerances.

On `be1cf48a616d42b0b81fb272ade3b0435f0732a9` (after PR #602), replay on
`linux.local` against SQL Server image
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a` from the pinned fixture
found 20 exact matches and 14 differing cases: 28 floating-point cells and
26 other structural differences. The raw comparison is saved separately under
`artifacts/remote/linux.local/statistical-precision/comparison.json`; this is
local evidence for that executable, not a compatibility verdict.

| Reference case | Float cells | Other fields | Exact difference |
| --- | ---: | ---: | --- |
| `STDEV BIT`, `STDEV no argument`, `STDEVP BIT`, `STDEVP no argument` | 0 | 1 each | DONE command 0 locally, 253 in SQL Server |
| `VAR aggregate`, `VARP aggregate` | 1 each | 0 | Variance low bits |
| `VAR BIT`, `VAR no argument`, `VARP BIT`, `VARP no argument` | 0 | 1 each | DONE command 0 locally, 253 in SQL Server |
| `partitioned windows` | 16 | 4 | Float bits and ORDER/NBCROW token kinds |
| `ordered frames` | 10 | 5 | Float bits and ORDER/NBCROW token kinds |
| `empty preceding frame` | 0 | 8 | ORDER/NBCROW token kinds |
| `DISTINCT window rejection` | 0 | 1 | DONE command 0 locally, 253 in SQL Server |

The six distinct numerical mismatches are below. Window rows repeat four of
these pairs, which accounts for the 28 cell differences. The table shows raw
binary64 bits, not rounded decimal comparisons.

| Input and function | msduck bits | SQL Server bits |
| --- | --- | --- |
| `[1,2,2,9]` `VAR` | `402b555555555554` | `402b555555555555` |
| `[1,2,2,9]` `VARP` | `40247fffffffffff` | `4024800000000000` |
| `[1,2,2]` `STDEV` | `3fe279a74590331c` | `3fe279a74590331a` |
| `[1,2,2]` `STDEVP` | `3fde2b7dddfefa66` | `3fde2b7dddfefa62` |
| `[1,2,2]` `VAR` | `3fd5555555555555` | `3fd5555555555550` |
| `[1,2,2]` `VARP` | `3fcc71c71c71c71c` | `3fcc71c71c71c715` |

For the captured small integers, SQL Server's result equals the binary64
evaluation of `(sum(x*x) - sum(x)*sum(x)/count)/(count-1)` for sample variance
and the same numerator divided by `count` for population variance. For
`[1,2,2]`, `(9 - 25/3)/2` rounds to the captured `VAR` bits; square roots
of those variance results match the captured standard deviations. For
`[1,2,2,9]`, `(90 - 196/4)/3` matches `VAR`, and division by 4 matches
`VARP`. This is an inference from these inputs, not proof of SQL Server's
general algorithm or accumulation order. The formula is vulnerable to
cancellation for large nearby values, so replacing DuckDB's functions without
more reference cases would risk numerical and overflow regressions.

The 26 other differences have different owners. All nine invalid-call cases
retain the captured error numbers, messages, and ERROR/DONE event order, but
the raw DONE command word differs. The three window cases return the same
logical rows and descriptors apart from the float bits, while SQL Server emits
ORDER and NBCROW tokens that msduck does not. The artifact retains each token
sequence and field. These wire differences must not be treated as numeric
variance failures or normalized away.

The next numerical task should first capture independent SQL Server results
for large nearby values, mixed signs and magnitudes, DECIMAL/float inputs,
NULLs, grouped and ordered windows, and volatile single-evaluation cases.
Then implement a bounded statistical state in the root DuckDB adapter if the
formula and rounding order hold, with typed DISTINCT deduplication before
floating conversion. Avoid expanding an expression into repeated uses of its
operand. A separate TDS task should address DONE command and ORDER/NBCROW
emission with raw token fixtures; those changes do not belong in the numeric
aggregate state.
