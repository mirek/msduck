# DATALENGTH

DATALENGTH counts Windows-1252 bytes for VARCHAR/CHAR, UTF-16 bytes for
NVARCHAR/NCHAR, and raw bytes for BINARY/VARBINARY. It includes trailing spaces,
returns NULL for NULL and zero for empty variable-width values. Fixed-width
padding is counted from the stored or converted value. INT is returned for
bounded types and BIGINT for MAX types, including empty result sets.

Supported source forms include literals, declared parameters, explicit casts,
and catalog-resolved columns through tables, views, CTEs, derived queries and
OPENJSON WITH schemas. UPPER, LOWER, LTRIM, RTRIM and TRIM retain the input
character family and capacity through nesting and persisted view descriptors.
LEN shares this inference, fixing its INT result for catalog-backed MAX inputs.
BIT, integer, DECIMAL/NUMERIC, MONEY/SMALLMONEY and REAL/FLOAT families report their declared storage
width without evaluating their input more than once. Native character/binary
functions use exact vector signatures and process NULLs across chunk boundaries.

The upstream mssqlite `Character.dataLength` implementation and tests were
reviewed. This implementation uses declared SQL types rather than inferring a
fallback width from a runtime JavaScript value or DuckDB's UTF-8 storage.

Numeric literals use SQL precision/scale inference; decimal widths follow the
5/9/13/17-byte precision bands. Numeric declarations are restored from catalog
metadata, keeping money types distinct from their decimal backing storage.

DATE, DATETIME and SMALLDATETIME report 3, 8 and 4 bytes respectively. TIME,
DATETIME2 and DATETIMEOFFSET use the documented scale-dependent widths (3–5,
6–8 and 8–10 bytes). These widths are distinct from binary casts that include a
precision byte. Tests cover all scales, stored declarations, CAST/CONVERT,
constructors, views, prepared parameters, NULLs and chunked execution. Plain
CONVERT to legacy datetime types now follows the existing CAST path; style-specific
format conversion remains separate work.

Fixed scalar byte widths now come from `msduck_core::types::Type`. Known
UNIQUEIDENTIFIER declarations and casts report 16 bytes, with NULL propagation;
the declaration client test and local audit cover both. See Microsoft's
[UNIQUEIDENTIFIER storage definition](https://learn.microsoft.com/en-us/sql/t-sql/data-types/uniqueidentifier-transact-sql).

Remaining work includes SQL_VARIANT
representations; result-type inference for other character-producing functions,
CASE, concatenation and mixed set operations; additional collations/code pages;
and live SQL Server comparison. Unknown source types are explicitly unsupported.
General binary storage padding/cast behavior remains a separate gap.

Reference: [Microsoft DATALENGTH](https://learn.microsoft.com/en-us/sql/t-sql/functions/datalength-transact-sql).

Function-family references: [UPPER](https://learn.microsoft.com/en-us/sql/t-sql/functions/upper-transact-sql), [RTRIM](https://learn.microsoft.com/en-us/sql/t-sql/functions/rtrim-transact-sql). Case conversion still uses the backend implementation; exact collation-specific case mappings remain open.

Numeric storage references: [decimal/numeric](https://learn.microsoft.com/en-us/sql/t-sql/data-types/decimal-and-numeric-transact-sql), [float/real](https://learn.microsoft.com/en-us/sql/t-sql/data-types/float-and-real-transact-sql), [money/smallmoney](https://learn.microsoft.com/en-us/sql/t-sql/data-types/money-and-smallmoney-transact-sql). General arithmetic-result inference is still incomplete.

Temporal storage references: [DATE](https://learn.microsoft.com/en-us/sql/t-sql/data-types/date-transact-sql), [date/time types](https://learn.microsoft.com/en-us/sql/t-sql/functions/date-and-time-data-types-and-functions-transact-sql), [DATETIMEOFFSET](https://learn.microsoft.com/en-us/sql/t-sql/data-types/datetimeoffset-transact-sql). Live DATALENGTH comparison across server versions remains outstanding.
