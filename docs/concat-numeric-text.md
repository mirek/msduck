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

REAL/FLOAT preserve signed zero as `-0`. The formatter first converts the
absolute IEEE value to 17 significant decimal digits, then rounds those guarded
digits to six using decimal arithmetic. It handles carry before placing the
point or selecting fixed/scientific notation. Fixed notation uses rounded
exponents -4 through 5; scientific notation strips trailing mantissa zeros and
pads signed exponents to at least three digits. No floating scaling factors or
reparsed rounded floating values choose the final digits.

This is an implementation inference matching retained observations, not a claim
about SQL Server’s internal algorithm. Ordinary Rust `.5e` formatting fails the
captured FLOAT nearest 1.234575 (`1.23457` versus SQL `1.23458`). Binary scaling
passed the initial reference but failed the broader owner-run task830 grid:
FLOAT bits `bee671526d3d6e16` produced `1.23457e-200` versus SQL `1.23456e-200`.
The predecessor fails 1,632 of 52,032 non-NULL ordinary text comparisons across
four raw runs (68 distinct inputs). The guarded decimal candidate matches all
52,032. Original raw contexts/rows/descriptors and private before-failure logs
are preserved; wider finite behavior remains unproved.

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

The second path-included replay consumes merged reference #831
(`reference/float-default-grid.json`, SHA-256
`1a86ed8f35876029c94b317c3520917cfce4d3f180c286e5612494cbeaeddba9`).
All 2,170 ordinary input programs and eight prepared control programs are
covered across four complete runs. Each run supplies 13,092 exact string and
UTF-16 comparisons, including typed NULL and all 48 prepared executions.
Native storage controls remain separate source evidence. Explicit style-0,
TRANSLATE and CONCAT_WS controls remain independently labelled expectations;
no agreement among them is assumed by the reader. The actual ec80 predecessor
passes the original replay and fails this grid regression on the exact
`bee671526d3d6e16` input. The corrected guarded conversion passes both complete
replays with 61,528 comparisons overall.

This module establishes no runtime compatibility pass. The fixture samples IEEE
rounding boundaries and extremes but does not exhaust finite bit patterns.
Uncaptured rounding boundaries, styles, source coercion, arbitrary session rules,
metadata allocation and runtime/wire integration require further evidence.
The tests also reject range, source-kind, decimal-identity, style and nonfinite
payload errors; unknown cases must not be coerced into character declarations.
