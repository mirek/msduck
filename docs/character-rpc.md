# Varchar RPC decoding

BIGVARCHAR (0xA7) inputs now decode Windows-1252 for the exact collation
advertised at login. The CP1252 extension range includes the euro sign, smart
quotes, em/en dashes and accented letters, rather than treating these bytes as
Latin-1 control characters. Undefined CP1252 slots retain their C1 code points.
The mapping was adapted from the pinned mssqlite `packages/bytes/src/cp1252.ts`;
see docs/reference-review.md for provenance.

Ordinary length-prefixed values and PLP values share the existing bounded
string/binary reader. NULL, empty strings, fragmented input and unknown-total
PLP are supported. The decoder rejects unsupported varchar collations,
truncation, declared-length violations and PLP total-length mismatches.

Verification includes real tedious VarChar calls with punctuation/accented
characters, large values spanning packets, table writes, prepared execution,
NULL and empty values. Unit tests cover CP1252 boundaries, C1 preservation,
multi-chunk PLP, unknown-total PLP and malformed/unsupported input.

Remaining work: other code pages and UTF-8 collations, fixed CHAR/NCHAR inputs,
SQL character width/padding/coercion semantics and original result type
metadata. DuckDB stores decoded UTF-8 text, and current results still advertise
nvarchar(max). Supporting the input encoding does not establish SQL Server
string comparison or collation equivalence.
