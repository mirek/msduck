# Session control declarations

`@@OPTIONS` has a nonnullable INT declaration in the original logical AST.
The root supplies its live supported option bits when executing; declaration
inference never reads those bits, parameter values or result rows. Losing this
declaration previously left the complete mixed control-query metadata list
unavailable.

The captured `@@OPTIONS & 16384` projection also has nonnullable INT metadata.
The shared syntax proof recognizes unquoted options and bounded INT literal
masks, parentheses and successive AND masks. Nullable parameters, NULLs,
out-of-range literals and other bitwise/arithmetic shapes remain outside that
proof. They must not acquire a guessed nonnullable declaration.

Direct SESSIONPROPERTY projections retain nullable, computed sql_variant
properties. Its captured casts to INT also retain the computed flag; ordinary
sql_variant casts and other session functions keep their existing separate
rules. Quoted column names such as `[@@OPTIONS]` resolve against their explicit
catalog fields rather than borrowing a session-counter declaration or label.
This is a logical metadata boundary; wider runtime handling of quoted names
still needs independent server verification.

Two fresh `reference/session-property-context.json` runs agree, including the
login query and seven option-change queries. SHA256:
`33450a4d0d89328b979e31552da642487fbe8e03ca4a021634adc1387c297307`.
The two agreeing `reference/guid-assignment.json` runs additionally capture
the mixed transaction/options/property query; SHA256:
`c02022f008d61b11e550e5d17a3f13db55c4fa7836ff6fec00ce6534ef6bed77`.
The pure regression checks every column's name, declaration type and flags
across these nine query profiles in both runs. Additional tests cover empty,
derived and CTE sources, casing, parentheses, parameters and quoted names.
These tests establish declaration behavior, not full wire compatibility.

Complete current-revision Rust/client checks and raw public replay are required
before merging. The diagnostic audit records evidence; it does not establish
SQL Server compatibility. Missing ORDER framing and remaining unproven type,
source and preparation paths are separate work.
