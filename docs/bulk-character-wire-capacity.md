# BulkLoad declared source row capacity

SQL Server's bounded modern character TYPE_INFO width is an advertised property,
not the capacity of the actual ROW payload. The unchanged [reference](bulk-character-wire-capacity-reference.md)
contains four original runs of 64 controls. A second four-database capture from
the frozen collector independently reproduces these observations. Source widths
1/8, advertised widths0/1 and actual NULL/empty/one/two-unit values are independent.
The original fixture SHA256 is
`af37e1b010334c45cff9591f4e9e81fc3bcf494421cd15d70f41e8da4da8a628`.
Full raw reproduction and all3403 raw comparison differences remain retained;
this implementation does not edit the oracle or collector.

Before this correction, real Session replay of all256 original observations
produced64 mismatches: otherwise admissible values were rejected with4804 when
ROW length exceeded advertised metadata width. The regression replays original
SQL and BulkLoad packets, compares original completion bytes, diagnostic number,
state, severity, message, procedure and line, original session counters and native
stored bytes. The existing server-name diagnostic difference remains explicit:
server identity is excluded only from that diagnostic comparison. This test does
not establish complete error-token identity or full SQL Server compatibility.

The root adapter derives per-column byte limits solely from the validated INSERT
BULK source declarations. Bounded CHAR/VARCHAR use declared width in bytes;
NCHAR/NVARCHAR use twice the declared UTF16-unit width. Noncharacter and MAX
source declarations provide no bounded character limit. The pure TDS codec
receives these explicit inputs and knows no SQL declarations, catalogs, sessions
or SQL diagnostics. A length prefix beyond an explicit source limit yields typed
column/byte/capacity facts before taking or copying its payload. Root metadata
admission precedes conversion of those facts to4815/state1/class17. Existing
family/nullability/MAX diagnostics retain precedence.

Without explicit source limits, modern bounded character framing follows the
physical u16 ROW length; it does not infer capacity from TYPE_INFO width. NULL
uses the reserved65535 prefix. Odd Unicode byte lengths remain malformed.
Binary, legacy, numeric and PLP validation retain their existing policies. Pending
token and packet/resource bounds remain in force. Supplied limit vectors must
match the metadata column count before decoding a ROW. Fixed-source padding and
allocation accounting remain root responsibilities.

Codec regressions cover all four modern families, default and explicit limits,
NULL/empty, widths0/1, exact source boundaries and one unit above, every split of
admitted rows, overflow without a payload, poison handling, Unicode scaling,
binary/legacy guards and limit-vector mismatch. The Session test additionally
stages501 admitted rows with advertised width0, then rejects a late source
length8001 against declared8000. It checks whole-load rollback, preservation of
the caller's earlier transaction row, staging cleanup, and a subsequent write and
commit. Existing bulk-admission regressions run unchanged.

The measured reference domain is bounded modern character data with original
CP1252/Unicode collations and ASCII control values. It does not prove arbitrary
collations, nonASCII conversion, legacy ROW admission, MAX capacity or target
overflow. Operational character carrier/catalog integration and other documented
compatibility work remain separate requirements.
