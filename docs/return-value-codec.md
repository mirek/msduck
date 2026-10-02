# RETURNVALUE wire codec

`msduck-tds::return_value` encodes a TDS 7.2–7.4 `RETURNVALUE` token without
session or transport effects. The layout follows the local
[MS-TDS token reference](../.agents/skills/tds-protocol/tokens.md): token `AC`,
USHORT ordinal, B_VARCHAR parameter name (UTF-16 unit count and raw units),
OUTPUT/UDF status, ULONG user type, USHORT flags, TYPE_INFO, then the typed
TYPE_VARBYTE value. There is no outer token-length field. The owner-controlled
`mirek/mssqlite` reference uses the same field order in
`packages/tds/src/token/return-value.ts` and delegates TYPE_INFO/value encoding
to its typed codecs. The prepared-handle helper retains the wire name's `@`
prefix and emits flags 0. Named `@handle` and unnamed integer vectors cover the helper and root
RPC adapter. SQL Server's named handle was captured in two matching fresh runs
and two independent runs in the owner-authored [PR #303 checkpoint](https://github.com/mirek/msduck/blob/f0824b5271b7d551462567e2625d96b7aa58a60e/reference/rpc-output-wire.json).
The capture SHA-256 is
`f4c304f84b0d76295b90bfdf74b7aae99e5b9a1a25f428fd58326061cb3c3c15`.
Its exact handle-1 token is
`ac0000074000680061006e0064006c0065000100000000000026040401000000`.
The unnamed vector checks the same header fields with an empty name; the
reference capture establishes the named token, not an unnamed SQL Server run.

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
