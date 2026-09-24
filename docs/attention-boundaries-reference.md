# Idle Attention, repeated Attention, and IGNORE

Task #169 captures eighteen post-login TLS scenarios from the pinned SQL Server
container: idle or active Attention sent once or twice in one client write, and a
fragmented SQL batch terminated with IGNORE/EOM with an empty or nonempty final
payload. Each runs in autocommit and explicit transactions with XACT_ABORT OFF/ON.
Two fresh captures and an independent two-run recapture agree: 72 successful
scenario executions. The generator is `scripts/capture-attention-boundaries.mjs`;
`reference/attention-boundaries.json` retains both original raw runs.

## Observations

Idle Attention returns a separate thirteen-byte DONE_ATTN message:

```
FD 20 00 FD 00 00 00 00 00 00 00 00 00
```

Its command is 253, including when no request is active. Two Attention packets in
one client write produce two separate ACK messages in the captured idle and active
cases. Active cancellation first completes its original response, then sends both
ACKs. A client burst does not prove that both packets reached server cancellation
logic before worker completion; these captures establish the resulting message
order, not every internal scheduling interleaving.

IGNORE terminates the incomplete batch without executing its insert. Its one
response is DONE_ERROR, command 0, without an ERROR token or Attention ACK:

```
FD 02 00 00 00 00 00 00 00 00 00 00 00
```

Both empty and nonempty final payloads produce that result. Idle Attention and
IGNORE preserve the earlier row and explicit transaction even with XACT_ABORT ON.
DATEFIRST stays 2. This distinguishes discarding an incomplete message from
cancelling an executing statement. The current live server's IGNORE success bit
and idle Attention command 0 differ from these observations and need correction.

For active cases, row 1 precedes the request, row 2 precedes WAITFOR, and row 3
would follow it. A separate session confirms WAITFOR in `sys.dm_exec_requests`
before Attention is sent. Autocommit and explicit/XACT_ABORT OFF retain rows 1
and 2; explicit/XACT_ABORT ON rolls back both. Row 3 never appears. The raw original
response's DONE ordering/commands remain in the fixture and must not be inferred
from the different computing-SELECT or IF/TRY reference batches.

Two successive probe batches establish transaction count, DATEFIRST, retained
writes and reuse. Their combined expression again observes XACT_STATE 1 even with
transaction count zero. This is retained exactly and is not a general rule for
XACT_STATE evaluated in other statement shapes.

## Capture boundaries and comparison

The harness uses tedious for TLS/login/setup, then owns raw post-login request
packets and consumes complete response messages through its installed transport.
It decodes tokens with the installed tedious parser. The parser's rollback handler
updates the transaction descriptor used by subsequent raw batch headers. It does
not cancel a tedious Request or clear driver cancellation flags. Package upgrades
must revalidate these private transport/parser interfaces.

Authentication packets and credentials are excluded. Each case retains packet
order, complete bytes, response EOM boundaries, decoded column/row/error/DONE/ORDER
observations, and the initial transaction descriptor. Incoming captures are bounded
to 4 MiB and sixteen response messages; a response deadline closes the connection
and fails the capture rather than abandoning a read and reusing it later. Every
observed incoming response must reassemble to exactly one consumed raw payload.

Comparison includes complete response payloads, not just selected decoded fields.
Only a rollback ENVCHANGE's old eight-byte descriptor is bound: it must equal the
current connection descriptor, appear in exactly one complete expected ENVCHANGE
encoding, and have the captured rollback shape. Packet-header SPIDs and packet
splitting are retained in raw traces but do not determine payload equality. No
rows, completion flags, ordering, or unknown tokens are discarded for comparison.

The first IGNORE fragment fills the negotiated 4096-byte packet, using UTF-16
spaces after the insert text; its EOM bit is clear. The final packet has ID 2 and
status IGNORE|EOM. An earlier development probe with a shorter non-final packet
ended the message stream without a response. That trace was retained separately;
it does not establish a general malformed-fragment policy. Do not confuse that
probe with the successful full-packet IGNORE cases.

Idle/IGNORE probes follow their packets with an ordered SQL marker rather than
using a short sleep to infer that no response exists. Active cases wait for the
first observed ACK before submitting their marker; the two captured ACKs precede
that marker's response. The second marker checks subsequent reuse. This does not
prove absence of arbitrarily delayed events beyond the observation window.

## Remaining implementation

The deterministic lifecycle still rejects idle/repeated Attention. It needs an
explicit ACK count and ordered write/retirement behavior consistent with this
capture, retaining worker quiescence and stale-request protections. Correct the
live server's idle ACK command and IGNORE completion separately with wire
regressions. Responsive transport, completion races, Attention during output
backpressure, disconnect, arbitrary writes, plain TCP, MARS and generalized
malformed framing remain separate work. Passing these captures is not implemented
server cancellation.

Run `node scripts/capture-attention-boundaries.mjs OUTPUT_DIRECTORY` with Docker
and the pinned image available. The helper creates/removes only its own isolated
containers and databases. A native msduck build is not required for this capture.
