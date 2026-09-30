# Percentile character fractions

Two fresh pinned SQL Server 2025 containers returned identical captures of
109 records, including every prepared phase and final connection reuse.
`reference/percentile-character-fraction.json` has SHA-256
`2d345861c98702b60c1ba69de35e65436b4c760bc7785706b6f9cdee60bf938e`.
The capture keeps rows, typed descriptors, warnings/errors, token events and
raw DONE statuses and commands. `--check` validates both retained runs and the
fixed request plan. Fresh capture refuses an existing output or fixture alias;
`--write-fixture` refuses any existing fixture. It does not overwrite evidence.

Character fractions convert to binary64 before range checking. Thus
`'1.00000000000000000001'` rounds to one, tiny positive or negative magnitudes
underflow to zero, and empty or ASCII-space-only text is zero. Signs and
`e`/`E`/`d`/`D` exponents are accepted. Non-finite conversion raises
8115/state 2; an out-of-range finite value raises 8727/state 1. Malformed text,
including NaN/Infinity spellings, has 8114/state 5 with source-specific
`varchar` or `nvarchar` text. These errors have severity 16.

Leading whitespace is accepted, including the historical U+180E whitespace
character. Only trailing ASCII spaces are accepted; trailing controls and
Unicode spaces fail. Whitespace-only controls/Unicode spaces are invalid.
Conversion stops at the first NUL. Two further independently repeated live
probe sets retain 28 lexical/NUL and 45 whitespace/exponent observations in
ignored `artifacts/remote/linux.local/percentile-character-fraction-v1-*`
JSON captures. These supplementary captures were not substituted for or
appended to the immutable fixture.

The deterministic SQL lowerer folds only explicit character constants and
known constant VARCHAR/NVARCHAR casts, respecting declared widths through the
core character rules. It preserves the ordering operand once and leaves
unknown/dynamic producers unknown. Descending zero uses the existing maximum
path. A separately claimed core diagnostic change recognizes only the exact
captured VARCHAR/NVARCHAR-to-FLOAT messages as 8114/state 5; explicit
application errors retain their supplied number/state.

Focused pure tests cover 102 applicable percentile batch shapes, malformed
AST preservation, cast truncation, descending zero, lexical edge cases and
single operand evaluation. The independent client replay executes all 108
non-version records and saves every raw difference, with exact FLOAT-value
comparisons for supported successful requests and captured error identities.
It checks seven valid prepared source bindings and preserves the other
prepared-phase differences as evidence. Additional client checks cover cast
widths, supplementary lexical cases, catchability and application identity.

## Remaining gaps

The original character-only task retained two numeric endpoint differences.
The separately captured numeric-literal conversion now fixes those endpoints:
SQL Server rounds numeric `1.00000000000000000001` to one, and the lowerer
matches it. Both numeric controls now participate in the pure/default/client
comparisons without exclusion. The numeric source matrix and its remaining
source/diagnostic timing gaps are documented in `percentile-numeric-rounding.md`.

SQL Server prepares invalid/range character constants successfully, then
reports conversion/range errors on execution; the current constant lowerer
rejects them during preparation. SQL Server also accepts the captured dynamic
NVARCHAR fraction parameter, while this lowerer still reports an explicit
unsupported-literal error. Runtime fraction binding, arbitrary scalar fraction
expressions, error timing/descriptor emission, character DISC metadata and
ORDER/NBCROW/DONE compatibility remain unfinished. The two partitioned VALUES
parser failures documented in `percentile-execution.md` are unchanged.

Commands for exact-revision validation:

```sh
node scripts/capture-percentile-character-fraction.mjs --check
cargo test -p msduck-core -p msduck-sql -p msduck-tds --locked
cargo build --workspace --all-targets --locked
node --test tests/percentile_character_fraction.test.mjs
```

The focused file is not yet in the shared default client inventory; run it
explicitly alongside the required workspace/client/corpus checks. Exact head,
full verification, review/fallback and merge evidence are recorded in the PR.
This finite matrix does not establish complete percentile compatibility.
