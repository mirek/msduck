# SQL Server numeric-literal result descriptors

[`reference/numeric-literal-metadata.json`](../reference/numeric-literal-metadata.json)
retains 21 batch observations and four executions of one prepared statement from
each of two independent SQL Server 2025 containers. Both used the pinned image
digest in the fixture and reported product version `17.0.4065.4`. Their canonical
captures are byte-identical (SHA-256
`d656079e91e42f2e14b1fdb1f86c0030abfc31878f29ed42e0645ae10a18c6fa`).
The fixture retains full rows, descriptors, errors, informational messages,
completion events and prepared parameter values. No client-side type or error
normalization was applied.

| Expression | First result descriptor | Exact `VARCHAR(100)` rendering |
| --- | --- | --- |
| `2147483649` | `NumericN(10,0)` | `2147483649` |
| `00012.3400` | `NumericN(6,4)` | `12.3400` |
| `2147483649/2` | `NumericN(16,6)` | `1073741824.500000` |
| `2147483649/3` | `NumericN(16,6)` | `715827883.000000` |
| `CAST(2147483649 AS DECIMAL(10,0))` | `DecimalN(10,0)` | `2147483649` |
| `CAST(2147483649 AS NUMERIC(10,0))` | `NumericN(10,0)` | `2147483649` |
| `CAST(2147483649 AS DECIMAL(10,0))/CAST(2 AS INT)` | `DecimalN(21,11)` | `1073741824.50000000000` |
| `CAST(2147483649 AS NUMERIC(10,0))/CAST(2 AS INT)` | `NumericN(21,11)` | `1073741824.50000000000` |
| Prepared `2147483649/@divisor`, `@divisor INT` | `NumericN(21,11)` | `1073741824.50000000000` for `2` |

The bare-literal quotient keeps `NumericN(16,6)` in an empty result and after a
CTE boundary. The prepared result declaration is unchanged for divisors `2`,
`3`, `NULL` and `2` again; the NULL execution returns two typed NULL cells. A
failed numeric text conversion reports error 8114, state 5, class 16, and the
next batch succeeds. These observations distinguish literal binding from
explicit declarations and bound parameters. They do not, by themselves,
establish which SQL Server compiler rewrite produces the difference.

The focused live comparison from PR #621 found six msduck discrepancies in
`SELECT 2147483648,0.00,00012.3400,2147483649/2`: all four msduck columns used
`DecimalN` instead of `NumericN`, and the quotient was `(21,11)` instead of
`(16,6)`. The raw comparison is retained locally at
`artifacts/remote/linux.local/focused-compatibility-audit-v1/selected-comparison-5f20d7f09000.json`.
The deterministic SQL crate currently gives untyped numeric literals a
`DECIMAL` AST declaration, and the TDS result codec has one decimal descriptor.
Those are implementation gaps; this reference task changes no runtime behavior.

Run `node scripts/capture-numeric-literal-metadata.mjs --check` to verify the
retained plan, fixture checksum and both runs without starting SQL Server. On a
Docker-capable host, running the script without flags starts two new isolated
containers, compares them with each other and the fixture, and writes a new
capture under `artifacts/compatibility/numeric-literal-metadata/` without
overwriting an existing file. The fixture can only be created when absent with
`--write-fixture`. The script rejects symlink or hard-link aliases to the
fixture and an unpinned image override.
