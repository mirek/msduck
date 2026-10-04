# BulkLoad wire capacity reference

Task #950 retains 64 bounded modern character controls in four fresh databases
on two independently owned containers running pinned SQL Server17.0.4065.4.
Each case supplies one isolated row: NULL, empty, one or two ASCII units.
CHAR/VARCHAR/NCHAR/NVARCHAR source declarations1/8 and advertised wire widths0/1
are independent; all targets are bounded variable width8. Unicode TYPE_INFO
lengths are bytes while declared capacities are UTF16 units.

All four original runs report56 successful loads and8 failures. Every two-unit
value with a one-unit source declaration fails4815/state1/class17. All other
controls succeed, including payloads beyond advertised wire width0 or1 that fit
an eight-unit declaration. Wire width alone therefore does not establish ROW
capacity for these measured shapes. NULL remains NULL. Fixed CHAR/NCHAR source
values are padded to the source declaration before storage in variable targets;
variable sources retain empty/one/two-unit values unchanged. Actual native bytes,
SQL Unicode units, diagnostics, callback counts, DONE packets and original
session counters are retained rather than inferred from this description.

The raw adapters override TYPE_INFO to the intended0/1 width and pass original
Buffer payloads without truncation or fixed padding. An independent request
validator reconstructs complete original TYPE_INFO, ROW/NULL lengths and bytes,
DONE and INSERT BULK declaration. A separate ordinary SQL sample of the wire
family at width1 acquires the collation descriptor; that sample is not evidence
that the BulkLoad TYPE_INFO width is1. Incoming and outgoing raw packets are
pinned separately from full semantic observations and packet fragmentation.
Checked database/server identity projections are only digest inputs; the original
artifacts and recursive comparison sidecars preserve every identity difference.

The unchanged retained fixture SHA256 is
`af37e1b010334c45cff9591f4e9e81fc3bcf494421cd15d70f41e8da4da8a628`
(2,442,442bytes). Acquisition collector SHA256 is
`adfa90c4b61bb3de66cc10ae920af4f86186afbb4398d382171705a6a456c6f8`.
It saved the complete four-run raw evidence before its expected unpinned-oracle
validation failure. Independently fixed original semantic/request/response/frame
pins and explicit declaration validation were added afterwards. Final frozen
collector SHA256 is
`d46668691002bce75439fc60e4fd625d005a22255e1e793dc44fed50765fbcd6`.
Its independent four-database reproduction is in progress.

The collector inherits reviewed helpers without modifying their source or older
fixtures. It bounds retained JSON to48MiB,500000nodes/depth64, each exchange to
2MiB/1024packets, connect timeout2seconds and bulk callback15seconds. Exclusive
output and sidecar guards run before Docker. Raw evidence survives capture,
comparison or validation failures; omission sidecars label comparison exhaustion.
Owned containers and isolated databases are cleaned through awaited lifecycle
helpers. Credentials and login packets are excluded from evidence.

These finite ASCII controls do not establish behavior for legacy types, arbitrary
collations, nonASCII/malformed values, MAX framing, target overflow or arbitrary
wire shapes. The current runtime may still return4804 for wire-only overflow;
changing that codec/runtime behavior is a separate task. This reference does not
claim endpoint compatibility. Final-source reproduction, focused checks on both
hosts and exact-head review/CI remain required before merge.
