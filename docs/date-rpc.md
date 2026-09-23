# DATE RPC parameters

TDS DATE (0x28) uses a zero value length for NULL or a three-byte unsigned
little-endian day count from 0001-01-01. The decoder validates the length and
SQL Server's maximum day count, 3652058, before converting to DuckDB Date32
by subtracting 719162 days. Bound values retain their DATE declaration.
No timezone or floating-point conversion occurs in the server.

Unit vectors cover both calendar limits, the Unix epoch, typed NULLs, every
truncated prefix, invalid lengths and dates beyond year 9999. Independent
tedious tests cover year 1, year 9999, leap days, table insertion and ordered
selection, prepared reuse, and DATE metadata for NULL and empty results.

Other temporal RPC inputs (DATETIME2, DATETIMEOFFSET and legacy DATETIME)
remain unsupported. Date string coercion, date arithmetic, timezone handling
and SQL Server error-number equivalence require further work and differential
validation against SQL Server.
