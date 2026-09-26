# FROMPARTS result properties

The owner-generated `reference/temporal-parts.json` captures SQL Server 2025
descriptors for `TIMEFROMPARTS`, `DATETIME2FROMPARTS` and
`DATETIMEOFFSETFROMPARTS`. The captured constructor results are computed:
flags 32 when their arguments are proven non-null literal integers, and flags
33 when an argument makes the result metadata nullable. This holds for rows,
empty result sets and described results preceding runtime errors. A constant
bitwise scale such as `7&3` has flags 32; constant arithmetic (`1+2`, `8/2`),
`CAST(3 AS INT)` and a bound parameter have flags 33. Negative numeric literals
also have flags 32. These are observations from the retained probes, not a
general rule for every SQL Server expression.

The SQL crate now marks valid public and lowered FROMPARTS calls as computed.
It omits nullable only when every argument is syntactically an integer literal,
possibly under parentheses, unary sign or constant bitwise operators. Other
arguments remain nullable without inspecting runtime values, even when a
parameter happens to carry a non-NULL value. Decimal and scientific lexical
literals remain nullable until they have their own reference probes. The
projection rule preserves the
constructor's computed flag while retaining the existing rule that removes it
for ordinary temporal casts. The TDS encoder already converts these logical
properties to flags; no wire or native-vector change is needed.

The root engine binds result fields from the public query before its translator
lowers FROMPARTS and removes the scale argument. The lowered-name property rule
supports direct metadata inference of those internal forms; it does not carry
the removed scale's provenance. The public CAST-scale descriptor therefore
depends on the pre-lowering result-field path and must be checked with #359's
scale-parser change in an end-to-end replay.

The deterministic test checks 27 exact retained descriptors across the three
families, including CAST scale metadata, plus lowered function names and an
ordinary TIME cast regression. The standalone Tedious test checks 24 public
descriptors, including empty results and bound arguments. The public
`CAST(3 AS INT)` scale query requires task #359's separate scale-parser change;
its property is covered deterministically here, while a combined public replay
must run after #359 integrates. Type/scale inference, diagnostics, and temporal
value construction remain owned by their existing paths. Uncaptured argument
forms are treated conservatively as nullable and need further SQL Server
comparison before claiming full descriptor parity.
