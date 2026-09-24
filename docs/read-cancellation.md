# Session active-read cancellation

`Session::batch_response_with_read_cancel` is an opt-in worker entry point for
materialized reads with describable result metadata. It uses the owned Rust pending
read adapter; the live transport does not call it yet. An atomic request flag is
scoped to the invocation and restored before the method returns.

A drained read cancellation carries its metadata as a distinct internal outcome,
bypasses T-SQL CATCH and stops every later batch statement. The session applies
XACT_ABORT rollback outside native execution, then returns the original response
and a separate Attention ACK. Cleanup failure is distinct and requires connection
closure; it does not return an ACK. Native errors remain ordinary errors.

The deterministic `attention_completion::ActiveRead` plan takes explicit RPC,
transaction, XACT_ABORT and TRY inputs. Its completion/rollback ordering and ACK
are checked against both raw runs of all 24 computation captures. Only the session
executes rollback. Response EOM, ACK EOM and subsequent request admission remain
owned by the future transport/lifecycle integration.

The matrix regression triggers cancellation from an actual native callback in a
computing aggregate and compares the complete first response and separate ACK
against retained SQL Server token observations. Transaction descriptors are bound
to the session's actual identity. It also checks retained writes, transaction count,
DATEFIRST and connection reuse. It does not claim an exact comparison of the full
combined XACT_STATE follow-up; the reference's statement-specific XACT_STATE 1
observation remains separate from existing session state emulation.

Unsupported cancellation classes remain explicit: write execution, OUTPUT/image
work, variable assignment, JSON materialization, undescribed results, preparation,
external side effects, streaming and backpressure, idle/repeated Attention,
pre-EOM IGNORE, completion races and disconnect. The opt-in entry point must not
be treated as general batch cancellation or enabled on the live wire until those
admission and transport policies are defined. A bound plan must exclude unsupported
side effects; backend SELECT/read-only flags alone do not prove this property.
