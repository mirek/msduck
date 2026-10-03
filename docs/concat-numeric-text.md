# Deterministic numeric implicit text

`crates/msduck-sql/src/numeric_text.rs` is an unregistered pure module. Its
`text(source, value, style)` accepts the original logical declaration and an
already typed integer, validated core Decimal, exact scale-four money coefficient,
or original REAL/FLOAT IEEE bits. It returns optional text, without changing the
source declaration or choosing result metadata. Binding, source coercion,
CONCAT_WS/TRANSLATE evaluation and runtime adapters remain separate work.

Integer ranges, decimal declaration identity and SMALLMONEY bounds are checked.
Money uses the existing exact coefficient formatter; Decimal uses its validated
scale-preserving display. Neither passes through a float. NULL retains the
caller’s declaration and still validates the source and style. Unsupported
sources/styles, mismatched payloads and nonfinite IEEE inputs return distinct
pure errors, without inventing SQL diagnostics whose source context is absent.
Only implicit/default style and explicit style 0 are admitted.

REAL/FLOAT preserve signed zero as `-0`. The formatter scales the binary value,
rounds to six significant digits, then places the decimal point using strings.
Extreme powers are split to avoid overflowing scale factors for subnormals.
Fixed notation uses rounded exponents -4 through 5; scientific notation strips
trailing mantissa zeros and pads signed exponents to at least three digits.
Binary scaling is an implementation inference that matches the retained adjacent
IEEE probes, not a claim about SQL Server’s internal algorithm. Rust’s ordinary
`.5e` rounding is insufficient: the captured FLOAT nearest 1.234575 produces
`1.23458`, while the initial implementation produced `1.23457`. The predecessor
failure is retained in the worker’s private verification log.

The path-included integration test reads every applicable observation of merged
reference #827 (`reference/concat-numeric-format.json`, SHA-256
`313ce8e6cd3254493f17aed4bfef54a4bdeb990f887e7c8f7178d184a0b8ad1c`).
Each of four complete runs supplies 2,290 exact string and UTF-16 comparisons,
including all ANSI/Unicode first/last/separator/NULL-companion function contexts,
separately identified explicit style-0 controls and all prepared rebindings.
Typed scalar text supplies exact coefficients; IEEE RPC and prepared scalar
payload bytes supply exact values. Decoded imprecise JS decimal/money numbers
are never formatting inputs. Native source requests remain source evidence,
and all 77 source-overflow observations per run remain separate diagnostics,
rather than being mistaken for formatter output.

This module establishes no runtime compatibility pass. The fixture samples IEEE
rounding boundaries and extremes but does not exhaust finite bit patterns.
Uncaptured rounding boundaries, styles, source coercion, arbitrary session rules,
metadata allocation and runtime/wire integration require further evidence.
The tests also reject range, source-kind, decimal-identity, style and nonfinite
payload errors; unknown cases must not be coerced into character declarations.
