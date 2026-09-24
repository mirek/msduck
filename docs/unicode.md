# UNICODE

UNICODE returns the first UTF-16 code unit under the server's current non-SC
character semantics. For example, UNICODE(N'Å') is 197 and UNICODE(N'🦆') is
55358 (the leading surrogate). NULL and empty strings return NULL. The result
uses INT metadata even for NULL or empty result sets.

The native VARCHAR-to-INT function reads one input per row and handles inline
and heap strings across DuckDB vector chunks. It applies to literals, bound
parameters, source columns, stored defaults and views. Existing integer inference
recognizes UNICODE results in arithmetic, comparisons and CASE expressions.
Argument count and aggregate/window modifiers are validated during translation.

Microsoft documents the distinction between non-SC and SC behavior in its
[UNICODE reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/unicode-transact-sql?view=sql-server-ver17).
Tests cover BMP and supplementary characters, NUL, empty and NULL values,
Windows-1252 RPC input, prepared long strings, columns/defaults, empty metadata,
argument errors and 6,000 rows across native chunks.

SC collations, unpaired-surrogate storage, full input-conversion semantics and
exact SQL Server diagnostics remain unfinished. This does not implement ASCII,
CHAR or NCHAR parity. Live SQL Server differential validation remains outstanding.

## UTF-16 storage assignments

The core's `CharacterType::store_utf16` applies storage overflow and padding
rules directly to UTF-16 units. Unlike an expression CAST, storage rejects
non-space overflow. It retains isolated surrogates and NULs and discards only
excess trailing spaces. Native adapters validate carrier payloads, propagate
NULLs and enforce output allocation bounds.

INSERT and UPDATE retain the target's physical character layout separately from
its logical declaration. Columns already materialized as the named UTF-16
carrier now use these rules, including old/new images in joined UPDATE OUTPUT.
A backend macro selects conversion using the input's bound physical type, so
carrier values never stringify through VARCHAR. Numeric inputs still use backend
formatting before packing. Backend table definitions and capture casts are parsed
with the DuckDB dialect, which recognizes the carrier's STRUCT declaration.

This does not yet change CREATE TABLE NVARCHAR/NCHAR storage. Those declarations
still use VARCHAR, and the four programs in
`reference/unicode-character-storage.json` remain failing compatibility cases.
The new `reference/unicode-materialized-storage.json` independently captures SQL
Server behavior for SELECT INTO followed by assignments and OUTPUT. General
Unicode operators, constraints, ALTER conversion and migration of existing
VARCHAR-backed columns remain required before adopting the carrier for all
Unicode declarations.

Verification of this assignment stage: formatting, strict Clippy and all 444
workspace Rust tests passed. The four live materialized-storage programs match
OUTPUT values, column metadata and raw DONE tokens exactly. Ordered readback
values also match; the literal `id` column retains the existing SELECT INTO
nullability mismatch (local nullable INT versus SQL Server non-null INT).
The client regression preserves that exact difference explicitly. Full client
and audit runs are pending in `artifacts/compatibility/unicode-storage-verification.json`.

## Reading carrier code units and indexed-storage prerequisite

UNICODE lowering now normalizes the bound input with the carrier conversion
macro and reads its first UTF-16 unit directly. It no longer casts a carrier to
its STRUCT display text. Empty strings and outer NULL return NULL; malformed
payloads are rejected. The native tests exercise stored high/low surrogates,
numeric input, NULL and empty input, and 6,000 volatile rows evaluated once.
Four live SQL Server captures are in `reference/unicode-first-unit.json`.
Full wire regression verification for this follow-up is pending.

A probe against the bundled DuckDB 1.5.5 confirms that STRUCT columns cannot be
primary, unique or ordinary index keys. An expression index on their BLOB payload
is accepted, as is a BLOB primary key. The complete native output is preserved in
`artifacts/compatibility/unicode-carrier-index-probe.log`, with the Rust source
beside it. Consequently, switching all declared Unicode columns to STRUCT also
requires a separate collation-aware key representation and constraint handling.
Indexing raw payload bytes alone would not implement SQL Server text equality.

The first-unit follow-up passes all 397 library tests and strict Clippy. A fresh,
frozen Linux `verify` run now covers the complete workspace, client regressions
and audit; its pending status is recorded in
`artifacts/compatibility/unicode-first-unit-verification.json`. The older joined
OUTPUT snapshot completed with 440 Rust tests, 391 client tests and 322 audit
captures. Its 320 preceding cases differ only in the raw order of two rows in an
unordered APPLY query; two newly added probes include the still-failing ordinary
NVARCHAR storage case. No compatibility differences were normalized away.

The computed-expression metadata correction is now verified through a direct
wire test and three current client tests, including prepared raw surrogate input.
All four first-unit reference programs match result sets and raw DONE tokens;
readback still records only the known SELECT INTO literal-id metadata difference.
The current workspace passes all 450 Rust tests, formatting and strict Clippy.
Full current client and audit runs remain pending. The earlier frozen Linux run
completed 445 Rust tests and 392 of 393 client tests; its sole failure was the
now-corrected computed flag, and its audit phase therefore did not run.

The corrected snapshot also passes all 450 Rust tests, formatting, strict Clippy
and the three focused Unicode client tests on Linux. This replaces the earlier
known-failing wire snapshot as current verification evidence. The complete
current local client suite and audit are still running; their handles and logs
are retained in `artifacts/compatibility/unicode-bin2-verification.json`.
