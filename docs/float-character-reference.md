# SQL Server FLOAT/REAL character conversion reference

`scripts/capture-float-character.mjs --write-fixture` ran against two fresh,
isolated containers from the pinned SQL Server 2025 image
`mcr.microsoft.com/mssql/server:2025-latest@sha256:86cc6144ef39bb0fbed2329e1ad79b13ee82e7b2e4739213a0db0800e668a74a`.
Each container used a fresh database. All 216 probes agreed across runs. The
unmodified rows, column descriptors, errors, information messages, completion
tokens and RPC return status are in `reference/float-character.json`; the script
checks both runs for exact equality and refuses to overwrite a retained fixture.
Running the script without `--write-fixture` checks a new capture against that
fixture. This is SQL Server reference evidence, **not** an msduck compatibility
result.

## Observed formatting

The 28 literal probes compare `CAST` with style-free `CONVERT` to both
`VARCHAR(23)` and `NVARCHAR(23)`. All four forms agreed for each probed REAL and
FLOAT value, including NULL. The observed style-0 values are consistent with
six significant decimal digits, fixed notation for rounded decimal exponents
-4 through 5, and otherwise a lowercase `e`, signed exponent and three
exponent digits; the complete boundary rule is an inference. For example,
`CAST(1 AS FLOAT)` converts to `1`, `CAST(1000000 AS FLOAT)` to `1e+006`, and
`CAST(0.00001 AS FLOAT)` to `1e-005`. Negative zero converts to `0`.

The physical source type matters. `CAST(1.234565 AS REAL)` converts to
`1.23457`, whereas FLOAT converts to `1.23456`. The REAL value produced from
`0.00009999995` converts to `9.99999e-005`; the FLOAT value converts to
`0.0001`. Typed table columns retain those distinctions. The typed RPC probes
preserve the same default formatting, NULL values and `doneInProc`/`doneProc`
completion sequence.

For the tested values on the pinned version, styles 1, 2 and 3 use scientific
notation with, respectively, 8, 16 and 17 significant digits, retaining trailing zeros. The
REAL value of 1.23456789 converts with style 1 to `1.2345679e+000`, style 2 to
`1.234567880630493e+000`, and style 3 to
`1.2345678806304932e+000`. FLOAT style 3 gives
`1.2345678899999999e+000`. Styled zero, negative zero, one, negative one,
small numbers, one million and NULL are captured separately. The tested styles
-1, 4 and 99 produced style-0 text for the probed value; that observation
does not establish behavior for all out-of-range styles or SQL Server versions.

## Width, family, and error observations

Omitted conversion widths were 30. `VARCHAR(MAX)` and `NVARCHAR(MAX)` retained
their MAX descriptors; CHAR and NCHAR padded to their declared width with
spaces. Successful bounded and MAX results, NULL results and TRY results retained
the declared wire descriptor. For example, `NVARCHAR(23)` reports `NVarChar`
length 46 bytes, while `VARCHAR(23)` and `CHAR(23)` report length 23 bytes.
The captured flags and collation are retained per column; these statements
describe the observed cases only.

When 1.23456789 FLOAT cannot fit in `VARCHAR(1)` or `VARCHAR(4)`, both CAST and
CONVERT report error 232, state 2, class 16:
`Arithmetic overflow error for type varchar, value = 1.234568.` CHAR uses
the same error number and wording. NVARCHAR and NCHAR report 8115, state 2,
class 16: `Arithmetic overflow error converting expression to data type
nvarchar.` The failing result still carries a typed descriptor, has no rows,
and ends with a DONE token without a row count. TRY_CAST and TRY_CONVERT instead
return a typed NULL row. These forms are separate probes so an earlier failing
projection cannot hide a later TRY result. Narrow styled conversions show the
same family-specific error distinction; style 1 fits width 16, while styles 2
and 3 do not for the probed fraction.

## Implementation boundary

These captures give a successor task a concrete differential oracle for the
existing explicit conversion paths in `src/varchar.rs` and `src/nvarchar.rs`.
Their current macros cast FLOAT/REAL through DuckDB VARCHAR before applying
width rules; the REPLICATE-only correction in PR #355 does not change them.
A shared formatter and a typed path will need to preserve source precision,
evaluate volatile operands once, and apply the correct family-specific width
error after formatting. This is an implementation inference from the source and
reference observations. Implicit conversions outside explicit CAST/CONVERT,
arbitrary style expressions, non-finite backend values and complete SQL Server
conversion behavior remain unverified here.
