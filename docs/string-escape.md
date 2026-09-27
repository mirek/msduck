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

The implementation accepts ASCII case variants of `json` followed by ordinary
U+0020 spaces, including CHAR/NCHAR padding. Leading spaces and trailing tabs,
newlines, nonbreaking spaces or other characters return 13622/state 1. Format
validation precedes NULL-text propagation: a NULL source with `xml` still raises
13622, while a NULL source with `json ` returns NULL. Wrong arity reports 174.
An untyped literal NULL format reports 8116/state 1; a typed NULL character
format, including an RPC parameter, reports 8116/state 8. SQL Server's error
message uses uppercase `STRING_ESCAPE` only in the typed case. Noncharacter
coercion and collation behavior still need live comparison. Isolated UTF-16
surrogate units remain outside the server's string representation.

Pure tests cover the special/control character matrix, Unicode, decoding round
trips and large expansion. Native 6000-row tests cover chunk boundaries, NULLs
and volatile arguments. Client tests cover MAX metadata, prepared recovery after
invalid formats, long outputs, TRY/CATCH, views/CTEs, defaults and empty results.
The pinned SQL Server 2025 capture in
`reference/string-escape-format.json` records 27 literal, typed and RPC format
cases twice in fresh databases, including exact rows, descriptors, diagnostics
and DONE-family events. Reproduce it with
`node scripts/capture-string-escape-format.mjs`. The standalone public client
replay is `node --test tests/string_escape_format.test.mjs`; it is not yet in
the serial npm test inventory. All captured values, errors, type/length,
collation and completion events currently match. SQL Server sets result column
flags to 33, while msduck reports 1: the computed bit (32) is missing from
the shared SQL result-properties inference. The fixture replay asserts this
raw difference explicitly; a dependent metadata task will make it exact once
the current result-properties worker finishes. The audit corpus records further
results and diagnostics for comparison against SQL Server.

References:

- [Microsoft STRING_ESCAPE reference](https://learn.microsoft.com/en-us/sql/t-sql/functions/string-escape-transact-sql)
- [Microsoft JSON error messages, including 13622](https://learn.microsoft.com/en-us/sql/relational-databases/errors-events/database-engine-events-and-errors-13000-to-13999)
