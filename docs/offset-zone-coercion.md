# Offset zone coercion

The retained owner-run SQL Server capture in `reference/offset-functions.json`
is the reference for these rules. The runtime now accepts exact six-byte ANSI
text `±HH:MM` and the corresponding six UTF-16 code units in the native Unicode
carrier. Surrounding whitespace, a numeric text value such as `90`, malformed
minutes and values beyond `±14:00` raise the function's 9812 diagnostic.

Integer and BIT operands mean minutes. DECIMAL/NUMERIC and FLOAT/REAL operands
truncate fractional minutes toward zero: the captured `±90.5` cases become
`±90`. A syntactically identifiable MONEY operand uses MONEY's integer rounding,
so the captured `CAST(90.5 AS MONEY)` becomes 91. Native decimals are read by
their declared width and scale; float inputs must be finite and within the
bounded minute range before conversion. NULL source and zone values continue to
produce a typed NULL, and existing UTC/local overflow handling remains in place.

DuckDB represents MONEY as DECIMAL(19,4) and SMALLMONEY as DECIMAL(10,4).
Ordinary DECIMAL values may have the same physical types. The lowering step
selects MONEY rounding when expression or parameter metadata identifies the
declared MONEY type. A bare column whose MONEY declaration is available only in
the root catalog is still ambiguous at this adapter boundary. Applying MONEY
rounding to every DECIMAL(19,4) would make ordinary decimal columns wrong; a
catalog-aware successor should pass the declared column type explicitly.

The retained fixture also records result descriptors, error states, DONE tokens
and prepared RPC status. The new regressions cover selected value and error
paths, not a byte-for-byte comparison of all those wire observations. The
remaining descriptor, completion, and precedence differences need a separate
capture comparison against a fixed server revision.
