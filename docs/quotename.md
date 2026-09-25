# QUOTENAME value rules

`reference/quotename.json` retains 47 cases from each of two fresh databases on
the pinned SQL Server 2025 image. A second, independent container and two more
fresh databases reproduced the fixture byte for byte. Run
`node scripts/capture-quotename.mjs` on a Docker-capable host to repeat that
comparison; output goes under ignored `artifacts/compatibility/quotename/`.
The generator preserves the SQL, full TDS descriptors, rows, errors and DONE
events. It represents returned strings as exact UTF-16 little-endian bytes so
isolated surrogate units remain valid JSON and checks them against SQL Server's
separate `VARBINARY` conversion. No credentials enter the fixture.

Microsoft [documents QUOTENAME](https://learn.microsoft.com/en-us/sql/t-sql/functions/quotename-transact-sql)
as accepting a `sysname` input of at most 128 characters and returning
`nvarchar(258)`. The retained server uses a **128 UTF-16-unit** limit under its
captured non-SC collation: 64 supplementary pairs succeed and 65 return NULL.
It preserves isolated high and low surrogates. Doubling 128 closing brackets
produces the 258-unit maximum. Every captured direct, column and empty-result
descriptor is `NVarChar` with byte length 516, flags 33, and the database
collation (LCID 1033, flags 13, sort ID 52, CP1252); its precision and scale
are null.

The server accepts `[`, `]`, `(`, `)`, `<`, `>`, `{`, `}`, single quote, double
quote and backtick. Either side of a pair selects its opening and closing
characters; each occurrence of the closing character inside the input is
doubled. It uses **only the first UTF-16 unit** of a nonempty delimiter argument:
`[]`, `[x` and `]x` all choose brackets, while `x[` returns NULL. An omitted or
empty delimiter selects brackets. An explicit SQL NULL delimiter returns NULL.
These multi-character and empty-argument observations refine the documented
one-character description; they are claims about this pinned image, not all
versions.

The captured NUL boundaries are unusual and intentional. With an ordinary
delimiter, a NUL anywhere in the input returns NULL. An explicit delimiter
whose first unit is NUL instead returns nonempty input unchanged, including
embedded NUL, with no wrappers or escaping. Empty input returns NULL in that
case. The 128-unit input limit still applies. Other invalid delimiter units
return NULL. Empty input with ordinary brackets returns `[]`.

`crates/msduck-core/src/quotename.rs` implements these deterministic value
rules over explicit UTF-16 units. The module is compiled by its dedicated
integration test through a path include while `crates/msduck-core/src/lib.rs`
is owned by another task. It performs no SQL binding, type conversion, catalog
lookup, DuckDB call or I/O. A later export owner can add
`pub mod quotename;`; a later runtime owner must bind SQL arguments, preserve
raw UTF-16, return the captured `nvarchar(258)` descriptor even for NULL or
empty results, and compare real client values and completion tokens. No
`QUOTENAME` execution support is claimed from this pure module alone.
