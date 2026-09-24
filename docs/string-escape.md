# STRING_ESCAPE

STRING_ESCAPE(text, 'json') returns escaped string contents as NVARCHAR(MAX),
without adding surrounding quotation marks. It escapes quotes, backslashes,
forward slashes, backspace, form feed, newline, carriage return and tab. Remaining
control characters U+0000 through U+001F use lowercase `\u00xx` escapes. Other
Unicode characters remain intact. Empty input stays empty; output may expand
beyond bounded character widths.

`msduck_core::json_escape` owns the deterministic rules and borrows the input when
no escaping is needed. The adapter evaluates both arguments once, preserves
NULLs, and exposes the MAX result through direct queries, prepared calls,
views/CTEs and stored defaults. LEN and DATALENGTH recognize the result as
NVARCHAR(MAX), so they report BIGINT character/byte counts. The protocol uses
its existing `Type::Text` descriptor for NVARCHAR(MAX) and PLP framing.

The implementation accepts ASCII case variants of `json`, rejects other format
values with 13622, and preserves that error through TRY/CATCH. Wrong arity reports
174; an untyped literal NULL format argument reports 8116. NULL text and typed
NULL format values propagate NULL. These argument-policy details, padding and
collation behavior, error states/precedence, and noncharacter coercion still need
live SQL Server comparison; local tests are not proof of exact compatibility.
Isolated UTF-16 surrogate units remain outside the server's string representation.

Pure tests cover the special/control character matrix, Unicode, decoding round
trips and large expansion. Native 6000-row tests cover chunk boundaries, NULLs
and volatile arguments. Client tests cover MAX metadata, prepared recovery after
invalid formats, long outputs, TRY/CATCH, views/CTEs, defaults and empty results.
The audit corpus records results and diagnostics for comparison against SQL Server.

References:

- [Microsoft STRING_ESCAPE reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/string-escape-transact-sql)
- [Microsoft JSON error messages, including 13622](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-13000-to-13999)
