# Explicit VARCHAR and CHAR conversions

CAST, CONVERT, TRY_CAST and TRY_CONVERT to VARCHAR now handle representable
Windows-1252 text. Bounded lengths count bytes and truncate strings. Omitted
explicit conversion lengths become 30 at parsing, preserving the distinction
from internal formatting casts and DDL defaults. VARCHAR(MAX) preserves the full
value and uses VARCHAR PLP wire encoding for nonempty, empty and NULL results.
Known result expressions expose VARCHAR descriptors, including empty result sets.

Tiny/small/INT values too wide for the output use an asterisk. Decimal, BIGINT
and floating inputs report arithmetic overflow when their formatted text does
not fit; TRY forms return NULL in that case. Numeric formatting otherwise follows
the backend and still needs complete SQL Server emulation, especially floating,
money and date/time inputs. Style arguments remain unsupported. This conversion
work does not add column width enforcement or all implicit cast
rules. Some dynamically inferred result descriptors remain backend-derived.

The code-page encoder currently requires exact representability. Lossy replacement,
best-fit mappings, supplementary Unicode and other collations remain unfinished;
unrepresentable values fail explicitly instead of being sent in invalid VARCHAR
wire bytes. This limitation also applies to TRY forms. No live SQL Server endpoint
was used to establish full conversion parity.

Native tests exercise 6,000-row byte truncation, NULLs, volatile argument evaluation
and exact MAX wire bytes. Client tests cover CP1252 text, widths/defaults, overflow,
prepared values over 8,000 bytes, NULL/empty descriptors and persisted views.

References: [CAST and CONVERT](https://learn.microsoft.com/en-us/sql/t-sql/functions/cast-and-convert-transact-sql),
[integer conversions](https://learn.microsoft.com/en-us/sql/t-sql/data-types/int-bigint-smallint-and-tinyint-transact-sql).


CHAR and CHARACTER casts and no-style CHAR CONVERT/TRY forms use the same
conversion checks, followed by space padding to the declared byte length.
Lengths are 1–8000 and default to 30 in explicit conversions; CHAR(MAX) is invalid.
Known results expose CHAR descriptors and fixed-length wire values. NULL remains
NULL; TRY overflow remains NULL rather than padded empty text. View and SELECT
INTO catalogs retain the CHAR declaration, and the generated padding survives
materialization. This does not implement CHAR table storage enforcement, full
comparison/collation rules, or full result-type inference for columns and arbitrary expressions.

The fixed-width native test checks padding/truncation and once-per-row evaluation
across 6,000 rows, plus exact CP1252 wire bytes. Client coverage includes CHAR
metadata for empty/NULL results, prepared values, defaults, TRY behavior, invalid
widths, ISNULL padding and view/SELECT INTO propagation.


Known CHAR expressions now use their widest fixed width across CASE, COALESCE,
IIF and CHOOSE. Result branches are padded without changing their conditions or
indices. UNION, UNION ALL, INTERSECT and EXCEPT pad differing known CHAR widths
before comparing/deduplicating rows, including nested set operations. Empty and
NULL results preserve the common CHAR descriptor. The same shared machinery
continues to handle NCHAR independently.

Native tests exercise both CHAR and NCHAR with 6,000-row set operands and a
volatile conditional branch, checking that only selected branches allocate.
Client tests cover all four set operators, nested sets, conditional forms,
prepared inputs and empty results. Mixed type families, collation-sensitive
comparisons, arbitrary expression inference and catalog propagation of these
new common types remain unfinished.
