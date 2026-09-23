# Initial compatibility baseline — 2026-09-20

Commands verified on this worktree:

- `cargo fmt --check`: passed.
- `cargo test`: 11 unit/protocol tests and 7 integration/storage tests passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `npm test`: independent tedious client test passed.
- `npm run audit:local`: captured all 17 upstream corpus cases.

The capture is `artifacts/compatibility/local.json` (generated, ignored by git).
It includes result metadata/rows, DONE-family events, errors, and reuse probes.
No real SQL Server endpoint was compared in this run. The eight cases that
execute without a query error are **not eight compatibility passes**: some
require errors that msduck fails to raise or have incorrect output/metadata.

| Case | Current local evidence |
| --- | --- |
| Scalar metadata | Values execute; varchar widths/families and nullability are approximate |
| Session scalar metadata | Values execute; exact SQL Server descriptors still need comparison |
| Implicit type conversions | `'1' + 2` fails in DuckDB binding |
| String comparison padding | `'a' = 'a '` returns false; SQL Server padding semantics missing |
| Table value constructor | Values execute; NULL ordering corrected after initial audit |
| Ordered result tokens | Rows execute; ORDER-token fidelity still missing |
| Derived APPLY | Executes after lateral-join translation; CROSS/OUTER regression tests pass |
| FOR XML PATH | Unsupported serialization |
| ALTER COLUMN | T-SQL parser coverage gap |
| Identity seed/increment | Unsupported identity declaration |
| Explicit identity insert error | Setup fails before the target behavior is exercised |
| Character width values | Missing ISNULL and character-width semantics |
| Character width assignment error | Incorrectly accepts an overlong value |
| MERGE terminator validation | Statement unsupported; correct error contract not implemented |
| OPENJSON strict path | Missing table function |
| SELECT INTO type preservation | Missing SELECT INTO lowering and metadata handling |
| Unique NULL semantics | Incorrectly permits multiple NULL unique values |

The next semantic work should retain SQL declarations in a compatibility
catalog before lowering them to DuckDB types. Widths, nullability, collation,
identity, constraint semantics, and exact result metadata cannot be recovered
reliably from Arrow result types alone. Separately, transport work remains for
TLS/authentication, transaction-manager requests, bulk load, prepared RPCs,
MARS, reset, cancellation, streaming and bounded connection lifecycle.
