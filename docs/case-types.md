# Known CASE result types

For CASE and IIF result branches whose types are known integers, character
values or NULL, the engine chooses the highest integer type when any branch
is integral: BIGINT, INT, SMALLINT, then TINYINT. Character branches convert to
that type through the existing integer conversion path. The result type is
chosen across all branches, independent of which branch is selected.

Type evidence currently comes from declared variables/RPC inputs, explicit
casts, integer-range literals, string/NULL constants and nested CASE/IIF
results. Parameters remain bound. Conversion rejects nonintegral character
syntax with 245 and preserves exact BIGINT values without floating-point
round trips. NULL branches retain the chosen integer result type.

Tests exercise searched/simple CASE and IIF, prepared character inputs,
unselected invalid text, selected conversion errors and reuse, spaces, NULL,
BIGINT boundaries, TINYINT, nested expressions and empty results.

Simple CASE also applies known integer/character precedence to the input and
WHEN comparison values, independently of THEN/ELSE result typing. It retains
the simple CASE node and first-match behavior. Prepared tests check mixed types,
fractional-text rejection, NULL non-matches, spaces, BIGINT and reuse after errors.

Both CASE forms reject all-untyped-NULL results with 8133 during batch preflight,
including CASE with no ELSE. Typed NULL result expressions remain valid. Tests
check preparation and that validation occurs before earlier batch writes.

This implements part of SQL Server's [CASE result-type precedence](https://learn.microsoft.com/en-us/sql/t-sql/language-elements/case-transact-sql).
Unknown column/function types and other type families still use backend CASE
binding. Complete catalog-based inference, decimal precision/scale, floating
and temporal promotion, character widths/collations, full comparison coercion
and exact overflow diagnostics remain unfinished. Live differential
validation remains required.
