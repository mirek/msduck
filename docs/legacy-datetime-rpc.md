# Legacy datetime RPC inputs

RPC decoding accepts nullable DATETIMNTYPE (0x6f) with a 4- or 8-byte
metadata width and either NULL or a matching value width. Fixed DATETIM4TYPE
(0x3a) and DATETIMETYPE (0x3d) use the same bounded decoders.

SmallDateTime reads unsigned 16-bit days since 1900 and unsigned minutes,
rejecting minutes at or above 1440. DateTime reads signed 32-bit days since
1900 and unsigned 1/300-second ticks. Days must be in -53690..2958463
(1753-01-01 through 9999-12-31), and ticks must be below 25920000.
Neither negative dates nor the upper year boundary pass through floating point.
NULL parameters retain timestamp metadata. Declarations for DATETIME and
SMALLDATETIME translate to DuckDB TIMESTAMP, including prepared RPCs and DDL.

This is an input interoperability step, not full legacy temporal compatibility.
DuckDB stores these values as microsecond timestamps. A datetime tick rounds to
the nearest microsecond: tick 1 becomes .003333 and tick 2 becomes .006667.
Results currently advertise datetime2(7), not the original legacy type. This
can differ from SQL Server casts and from a client's legacy datetime rounding.
The original tick count/type must eventually survive storage and expression
processing. String casts and assignments do not yet enforce legacy ranges or
rounding; only the incoming wire value receives the validation above.

Datetime2 RPC inputs now use a separate exact representation, preserving the
full year range, 100ns fraction and declared scale. See
[DATETIME2 support and remaining integration](datetime2.md).

Unit tests cover both wire forms, NULLs, dates on both sides of 1900, every
truncated prefix, invalid widths and out-of-range dates/times. Tedious covers
calendar boundaries, fractional mapping, NULL metadata, table storage and
prepared execution. These tests have not been compared with a live SQL Server.

References: [MS-TDS date/time layouts](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/786f5b8a-f87d-4980-9070-b9b7274c681d),
[SQL Server temporal types](https://learn.microsoft.com/en-us/sql/t-sql/functions/date-and-time-data-types-and-functions-transact-sql).
