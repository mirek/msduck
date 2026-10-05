# Deterministic STRING_SPLIT binding rules

`crates/msduck-sql/src/string_split.rs` is a pure, currently unexported rule
module based on the owner-retained `reference/string-split.json`. The fixture
contains 49 programs repeated in two fresh databases in each of two pinned
SQL Server 2025 containers; all four captures agree exactly. The module is
path-imported by its focused tests while `msduck-sql/src/lib.rs` and root
integration files are reserved by other workers. It does not make
`STRING_SPLIT` available through the msduck server.

`bind` recognizes an unqualified `STRING_SPLIT(...)` table factor, checks
positional argument count and captured type/ordinal errors, and returns
`value` metadata from explicit declared source and separator types. It never
uses a row value to choose result width, family, nullability or collation.
Unknown declarations stay unknown. The optional `ordinal` is non-null BIGINT
only for the captured constant 1 and `CAST(1 AS BIT)` forms; captured constant
0 or NULL omits it. The captured `@ordinal` variable returns error 8748.
Captured invalid integers 2 and -1 return 4199, decimal 1.0 returns 8116, and
wrong arity returns 313/8144. SQL Server can emit a later 207 for a SELECT
that still names the absent `ordinal` column; that projection diagnostic is
outside this module. Other integer spellings, casts, expressions and column
references return explicit unsupported results until first-party captures
establish their conversion and error rules.

`split_units` operates on already evaluated UTF-16 units, preserving isolated
surrogates and empty edge/interior tokens. An empty input produces one empty
token (ordinal 1 when enabled); NULL input produces no rows, while the bound
result descriptor still exists. A NULL, empty, multi-unit or supplementary
separator produces captured error 214/state 11 after result metadata. This
empty-input result follows the retained SQL Server fixture; the copied upstream
T-SQL skill's older mssqlite note says something different and is not msduck
ground truth. ANSI bytes must be converted by a root adapter using their
declared code page before this rule is called. To avoid eager allocation for
unbounded MAX data, this adapter explicitly rejects input over 1,048,576
UTF-16 units as unsupported; streaming large values remains successor work.

An end-to-end successor must export the module after the current `lib.rs`
claim clears, attach result fields to projection and aggregate-column binding,
and wire table-factor lowering/execution in the root translator after its
claim clears. It must preserve source declarations for columns and RPC
parameters, evaluate each expression once per source row, carry typed metadata
through empty/NULL results, and emit the captured ERROR/DONE sequence. Public
Tedious SELECT, APPLY and prepared/RPC fixture replays are required before
claiming compatibility. In particular, no row order is promised without
`ORDER BY ordinal`.
