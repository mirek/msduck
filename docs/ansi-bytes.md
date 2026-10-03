# Native ANSI byte carrier

Task #892 implements the deterministic data boundary in
[the BulkLoad adapter plan](bulk-character-adapter-plan.md). `AnsiBytes` owns
immutable native bytes; `AnsiView` borrows them without copying. Both preserve an
explicit `EncodingIdentity` supplied by the caller. Named CP1252/CP1251/UTF8
identities and opaque caller tags are distinct, including an opaque tag1252.
The carrier does not interpret tags as code pages or validate SQL support.

`AnsiBytes::from_vec` checks active payload length before taking the existing
allocation. `from_slice` and `AnsiView::try_to_owned` check the current explicit
byte limit before copying. There is no unchecked allocating Clone/ToOwned API.
The payload limit does not measure process heap overhead or spare capacity in
a caller-owned Vec. `checked_byte_total` permits checked aggregate accounting
without mutating the caller's counter. Debug/error formatting stays bounded.

`AnsiView::nullable` retains None as NULL and Some(empty) as an empty value,
including at a zero-byte limit. Declaration/encoding facts for a NULL stay in the
caller's plan. Encoding/profile validation and SQL diagnostics must precede NULL
shortcuts in a conversion adapter; this byte holder does not perform them.

The module preserves all256 bytes and the raw malformed UTF8 controls from
[reference #884](bulk-character-encoding-reference.md), rather than decoding
them. That reference's fixed SHA256 is
`0d2cff08bc7f59d271955311ac540f95a37f31b456bc008f739117c1a09d3a83`.
Some controls fail SQL insertion, others are accepted, and native bytes differ
from SQL Unicode conversion and client display. Carrier construction does not
claim any of those inputs is valid SQL character data.

The separately claimed `ansi-byte-export-v1` companion exports the module as
`msduck_core::ansi_bytes`; integration tests exercise this public library API.
The old stalled export reservation was revoked through the protected registry,
retaining its claim and branch. Its unfinished datetime export remains a
dependency-correct successor, not completed work. No common Value/Parameter
variant, conversion algorithm, backend storage or server output path changes
here. Those steps remain required before the lossless carrier can satisfy
runtime character compatibility.
