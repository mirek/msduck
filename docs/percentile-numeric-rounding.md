# Percentile numeric source conversion

Two independent pinned SQL Server 2025 containers produced identical 213-record
runs. The fixture SHA-256 is
`c649224a9bd1215b8ab071590884dd5c9aee00c8f03bc6bdd425c4036a1e7a31`.
It retains 144 percentile queries, 66 standalone source controls, setup/version
and final reuse. Raw columns include userType, declared type/width/precision/
scale/flags and collation. Rows, diagnostics, token order, raw DONE status/command
words and decoded counts remain unchanged. Raw count payload bytes are not
captured independently of Tedious's decoder.

Decimal literal fractions convert to binary64 before the range check. Thus
`1.00000000000000000001` and `1.0000000000000001` round to one and are accepted;
`1.0000000000000002` rejects with 8727/state 1/severity 16. The adjacent retained
38-digit values around the half-ULP boundary have distinct results. A decoded
standalone decimal row is not a reliable proxy for its converted fraction:
Tedious's decimal-to-JavaScript representation may round differently. The
percentile result and exact FLOAT bits are the checked observations.

Scientific numeric literals are FLOAT sources. Values below normal binary64
range flush to zero, including negative `1e-308`, `5e-324` and extreme negative
exponents. Decimal literals retain their source precision and reject above 38
with 1007/state 1/severity 15 before metadata. Scientific overflow rejects with
168/state 1/severity 15 before metadata. These lexical errors differ from range
validation and character-to-FLOAT conversion. Zero with a huge positive exponent
remains zero. The lowerer preserves those distinctions rather than interpreting
all numeric syntax as arbitrary-precision decimal or applying character rules.

Known constant CAST sources retain their conversion: FLOAT/REAL widths, exact
DECIMAL scale rounding, MONEY/SMALLMONEY conversion, integer truncation and BIT
truth conversion. REAL conversion can retain subnormals; scientific literal
flushing must not be applied again after it. Unknown/effectful sources remain
unknown, unsupported casts stay explicit, and parameters are not inspected to
infer compile metadata. Only supported constant shapes are folded. Descending
zero uses the existing maximum path and ordering operands occur once.

The root folds these proven constant sources before child casts turn into native
adapters. It recognizes only complete canonical literal diagnostics and preserves
already structured application THROW number/state/severity. The default
signature test and existing pure/character replay now include the previously
excluded numeric endpoints directly. A focused numeric client replay preserves
all raw differences and compares 144 queries' diagnostic identities and exact
rows/FLOAT bits. Successful result families and widths are checked independently.

## Remaining scope

The 66 standalone source controls are evidence about SQL Server; this task does
not implement general SQL numeric-literal parsing outside percentile fractions.
Prepared error timing, empty-input suppression, dynamic fractions, expression
effects, additional source cast shapes and complete descriptor/token parity still
need the root runtime adapter described in `percentile-runtime-fraction.md`.
Range errors in the current static lowerer still occur too early. Captured
syntax diagnostics and runtime range diagnostics must not be conflated in later
integration. Further precision/effectful shapes require their own raw evidence.
This finite replay does not establish complete percentile or server compatibility.

## Verification

Run `node scripts/capture-percentile-numeric-rounding.mjs --check` to verify the
hash, full plan and both retained runs; a new output path captures two independent
containers and compares the complete result without normalization. Existing
outputs/fixture aliases are refused and only owned containers/databases are
removed. Run the pure numeric and existing character tests, all required workspace
and default client checks, the focused numeric/character clients, and corpus audit
on the final head. PR665 records exact revision, CI, review/fallback and preserved
raw differences. The focused client file must still run explicitly.
