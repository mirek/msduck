# Live idle Attention and ignored batches

The synchronous connection loop now emits the captured command-253 DONE_ATTN for
idle Attention. Each incoming Attention message receives its own response EOM.
A fragmented batch terminated with IGNORE/EOM returns command-0 DONE_ERROR,
without executing the partial SQL or generating an Attention ACK. This response
flag does not cause transaction rollback: the batch was discarded before execution.

The wire regression matrix consumes `reference/attention-boundaries.json` from the
independently verified SQL Server capture. It constructs packets separately from
the capture generator and compares complete control response payloads and message
boundaries. All twelve idle/IGNORE cases run over plain TCP and TLS, checking
transaction count, DATEFIRST, earlier writes, connection reuse and explicit rollback.
Registration through the existing tedious suite includes these twenty-four tests
in normal client discovery and CI sharding.

The previous immutable Linux server build at 78e2ed7 reproduced both defects:
idle Attention returned command 0, and IGNORE returned success status 0. Its binary
hash remained unchanged during the failing regression run. The new tests were
run from 1c12f8c, whose server source matches that retained build exactly.

This changes two completion fields. The live loop is still synchronous: active
query cancellation and read progress during response backpressure are not enabled.
The combined XACT_STATE expression difference remains separately recorded by the
reference capture; this boundary regression does not claim to resolve it. General
malformed-fragment rules are also separate from the full-packet IGNORE cases here.
