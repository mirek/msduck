# BulkLoadBCP token codec

`crates/msduck-tds/src/bulk_load.rs` is a pure decoder for a TDS 7.2–7.4
BulkLoadBCP payload, after packet type `0x07` has been framed by a transport.
The message shape is `COLMETADATA ROW* DONE`. The decoder accepts arbitrary
fragments, emits every complete row on each `push`, and retains only the
unfinished metadata, ROW or DONE token. One retained token is limited to
16 MiB, including prefixes and PLP chunk headers. The column count is limited
to 1,024. An error poisons the decoder; the caller must discard it and roll back
any partially applied engine work.

`Decoder::new(EomMode::RequireDone)` is the default wire rule. The explicit
`AllowRowBoundary` option accepts end-of-message after complete rows without a
DONE token, matching the observed FreeTDS/freebcp convention. It never accepts
an incomplete token at EOM. A DONE token must be final and have no trailing
bytes. The codec does not interpret packet IGNORE, Attention, transport EOF, or
transaction state.

`Column` retains user type, flags, raw UTF-16 name units and typed TYPE_INFO.
`Value` retains bytes exactly, including isolated Unicode surrogate units and
the distinction between NULL and an empty non-NULL value. Fixed and nullable
integer, bit, float, money, datetime, date/time, GUID, decimal/numeric,
character/binary, PLP MAX, `sql_variant`, and legacy text/image wire families
are accepted with bounded lengths. Unsupported TYPE_INFO, encrypted metadata,
NBCROW, invalid scalar widths, malformed PLP totals and Unicode byte counts
fail explicitly. The codec does not convert wire values to SQL types or claim
INSERT BULK execution, target-column validation, defaults, identity behavior,
constraints, or atomicity. Those require a separate server adapter and
end-to-end client tests.

The wire grammar and type widths follow [Microsoft MS-TDS Bulk Load BCP](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/ab4a7d62-cd1f-4db1-b67d-ecae58f493e3),
[COLMETADATA](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tds/0dfc5367-a388-4c92-9ba4-4d28e775acbc),
and the repository's copied `.agents/skills/tds-protocol/` reference.
The owner-authored [mssqlite bulk-load implementation](https://github.com/mirek/mssqlite/blob/7f71f2081602f8e3051998f5c11f058e65fe24ec/packages/tds/src/bulk-load.ts)
informed the fragment, DONE and FreeTDS boundary inventory (MIT license,
Copyright (c) Mirek Rusin). The Rust implementation and test wire fixtures
were written independently; no upstream code or fixture bytes were copied.

The reference replay in `crates/msduck-tds/tests/bulk_load_reference.rs` uses the
unchanged four-run `reference/bulk-load-wire.json` fixture (SHA-256
`f842d48d442697181072410b114294eb781882c3b9ead63e1a9d7b746ee0d849`).
It checks each actual type-7 packet header, length, payload and final EOM, then
feeds the retained payload into the decoder. The twelve successful observations
preserve original input metadata: user type zero, writable flags 4/5, INTN(4),
NVARCHAR byte width32 and its exact five collation bytes, and VARBINARY width16.
These request flags are distinct from the captured SELECT readback flags8/9.
Values retain original integer bytes, UTF-16 units, binary bytes, typed NULLs
and empty non-NULL binary values. The client DONE has zero status, command and
count even when the server callback reports one or three inserted rows.

Every split point and a one-byte feed additionally exercise the same twelve
retained successful payloads. These are decoder fragment tests; the retained
captures each contain one type-7 packet, so this does not claim new multi-packet
SQL Server ground truth. All four DONE-only zero-row observations preserve the
captured SQL Server error4804/state2/class16; both decoder EOM policies reject
that stream as malformed and remain poisoned, including every fragment split.
The pure error is not presented as a server diagnostic mapping.

This adds reference evidence without changing the decoder, fixtures, root
transport, target binding, transaction handling or execution. The reference
observer's retained response-header SPID/server-name differences remain raw and
outside this request-token replay; no server BulkLoad compatibility claim is
made by successful deterministic tests.
