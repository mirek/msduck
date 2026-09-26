# RETURNVALUE wire codec

`msduck-tds::return_value` encodes a TDS 7.2–7.4 `RETURNVALUE` token without
session or transport effects. The layout follows the local
[MS-TDS token reference](../.agents/skills/tds-protocol/tokens.md): token `AC`,
USHORT ordinal, B_VARCHAR parameter name (UTF-16 unit count and raw units),
OUTPUT/UDF status, ULONG user type, USHORT flags, TYPE_INFO, then the typed
TYPE_VARBYTE value. There is no outer token-length field. The owner-controlled
`mirek/mssqlite` reference uses the same field order in
`packages/tds/src/token/return-value.ts` and delegates TYPE_INFO/value encoding
to its typed codecs. The existing prepared-handle token's exact byte vector is
preserved through the new encoder.

The codec accepts nullable INTN (1/2/4/8 bytes), BITN, NVARCHAR/NCHAR,
VARCHAR/CHAR, VARBINARY/BINARY and DECIMALN. It handles raw UTF-16 units,
pre-encoded ANSI bytes, NULL markers, PLP MAX payloads and exact decimal
coefficients. Each declaration and value pair is checked before its staged token
is appended; failure leaves the caller's output unchanged. One token and the
resulting response buffer must fit the server's 16 MiB message bound. Encrypted
RETURNVALUE metadata, unsupported types and implicit conversions are rejected.
DECIMAL's TYPE_INFO storage length is explicit (5, 9, 13 or 17 bytes): compact
parameter declarations and the 17-byte result form can be represented without
guessing which form a future RPC path should emit.

The new codec does **not** mean application OUTPUT parameters work. `src/rpc.rs`
currently accepts only the integer OUTPUT handle for prepare RPCs; it still
needs declaration-aware binding, execution updates, and correct RETURNVALUE /
RETURNSTATUS / DONEPROC ordering. That root integration needs SQL Server captures
of NULL, character/MAX, decimal, errors, repeated execution and Tedious output
events. The pure vectors here prove the byte layout for supported declarations;
they do not prove those application semantics or full TDS compatibility.
