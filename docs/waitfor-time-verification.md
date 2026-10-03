# Bounded WAITFOR TIME verification

The real-clock integration probe in `tests/gaps_transactions.rs` formerly chose
a target one second after GETDATE() and expected completion within three seconds.
If query work crosses that target, WAITFOR TIME correctly waits until its next
occurrence the following day. Canonical core verification at b3a0dae stalled in
this test for over ten minutes; its own verified process group was stopped and
log retained. That partial run does not count as full Rust/client/audit evidence.

The successful probe now allows a five-second future margin and supplies an
Attention watchdog with a ten-second deadline. It requires successful completion
and elapsed time between three and ten seconds. Cancellation or an ignored wait
cannot become a pass. Dropping the watchdog wakes and joins its receiver thread
promptly, including during unwinding, rather than sleeping through the deadline.

A second real integration probe deliberately passes its one-second target with
a two-second DELAY. It requires cancellation after four seconds, prevents a
later THROW from running, and verifies the session remains usable. The existing
pure `waitfor_time_reaches_the_next_occurrence` clock tests preserve exact
next-day arithmetic; no clock or engine behavior is changed, and no test is
skipped. Existing type/diagnostic/transaction and cancellation probes remain.

This bounds the request wait through the existing cancellation path. It is not
a process-level watchdog for unrelated native deadlocks and does not establish
SQL Server compatibility. Focused and full verification evidence belongs to the
PR's exact revision, not the interrupted predecessor.
