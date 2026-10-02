# GUID storage assignment reference

`reference/guid-assignment.json` retains two complete, independently agreeing
36-request runs against fresh containers of the pinned SQL Server 2025 image
from `scripts/lib/reference-container.mjs`. Capture revision: `b0c3d8e`.
Fixture SHA-256: `c02022f008d61b11e550e5d17a3f13db55c4fa7836ff6fec00ce6534ef6bed77`.
Run `node scripts/capture-guid-assignment.mjs NEW-OUTPUT.json` to produce a new
capture; existing captures cannot be overwritten. The observer retains rows,
descriptors, errors, INFO, ORDER, decoded/raw DONE details and event order.
Typed parameter requests use Tedious RPC with explicit declarations.

After `cargo build --workspace --all-targets`, run
`node scripts/capture-guid-assignment.mjs --replay NEW-LOCAL-OUTPUT.json` for an
isolated local server replay of the same requests. It validates the retained
reference controls, then preserves every local result and raw comparison path
in a new artifact. A successful replay command means observations were retained;
it does not mean compatibility passed. No supplementary setup or excluded
requests hide unsupported behavior.

The complete local replay at `4e8c97c` retained 36 requests and 339 raw
differences. Artifact SHA-256:
`e512778991822022ef4fd41bc88a2f985e31dd0ae4dfdece9c24a48918677338`.
Canonical and braced assignments succeed, but suffixed INSERT and UPDATE
assignments fail with native error 245/state 1 instead of succeeding. Malformed
assignments likewise emit native 245/state 1 instead of the captured
8169/state 2. `@@OPTIONS` is explicitly unsupported (40515), so the original
post-BEGIN options probe produces no result. The later transaction-state and
TRY/CATCH probes encounter an aborted DuckDB transaction; subsequent parameter
and ALTER requests inherit that failure. They were retained rather than repaired
or skipped, and do not independently establish local parameter/ALTER behavior.
Server integration must repair both conversion and session lifecycle, then
repeat the complete replay. No compatibility pass is claimed.

The capture observes canonical, braced and suffixed VARCHAR/NVARCHAR assignments,
typed GUID passthrough and typed NULL. Malformed single-row INSERT, multirow
INSERT and multirow UPDATE report 8169/state 2/severity 16. Readbacks retain
the original rows after failed statements. Successful ALTER COLUMN converts
braced/suffixed stored text; failed ALTER retains the original VARCHAR values.
Parameters exercise VARCHAR, NVARCHAR, UNIQUEIDENTIFIER, NULL and malformed
NVARCHAR, with exact readbacks and errors.

Transaction behavior must follow the captured evidence. With XACT_ABORT OFF
(`@@OPTIONS & 16384 = 0`), BEGIN and a successful prior write leave
`@@TRANCOUNT=1, XACT_STATE()=1`. A malformed GUID assignment aborts that
transaction: the next request observes both values as zero, the prior write is
absent, and COMMIT reports 3902. In the single-batch TRY/CATCH control, CATCH
observes 8169/state 2 with `@@TRANCOUNT=1, XACT_STATE()=-1`. Attempting COMMIT
reports 3930; the subsequent SELECT still reads the prior row 30 inside that
transaction. Batch completion reports 3998 and rolls the transaction back.
A subsequent readback confirms both transaction state and prior writes were
cleared. These results do not establish behavior for other conversion classes.

The first capture attempt stopped at an incorrectly assumed successful COMMIT.
The retained diagnostic capture at `9c8718b` then completed both fresh runs and
exposed the rollback and uncommittable-transaction controls. No successful
transaction continuation is claimed for those requests. The final fixture
preserves the complete observations without normalizing them.

## Runtime assignment integration

The deterministic parser in `msduck-core::types::uniqueidentifier` supplies
character conversion and mixed-endian GUID bytes. The root
`src/guid_assignment.rs` adapter emits canonical text from those bytes and
lets DuckDB own native UUID slot layout. Registration now runs through
`src/scalar.rs`; `src/assignment.rs` recognizes UUID storage targets.

GUID-containing ordinary INSERT and unjoined UPDATE plans materialize all
operands once before mutation. `__msduck_guid_assignment_check(input)` returns
fixed VARCHAR fields `value` (canonical GUID text or NULL) and `error` (marked
conversion diagnostic or NULL). The shell checks every error before changing
rows, then casts successful values to native UUID. An error's NULL value is
never accepted as a successful NULL assignment. Mixed character targets use
the existing checked storage adapters within the same stage.

Expected malformed character conversion is returned as data, preserving the
native transaction for reads. The session owns logical transaction dooming,
commit rejection and rollback. With XACT_ABORT OFF, the captured uncaught8169
failure rolls back prior explicit writes. CATCH observes1/-1 transaction state;
COMMIT fails with3930 but subsequent reads still see prior writes, followed by
batch-end3998 and rollback. The supported live SET state supplies@@OPTIONS
for the XACT_ABORT capture controls; extension translators receive that same
explicit snapshot.

ALTER COLUMN checks the current column's conversion before DDL effects and
applies successful UUID conversion inside the existing validated, atomic ALTER
executor. The ALTER operand is the existing column, with no caller-supplied
volatile USING expression. Native malformed carriers and uncaptured source
types remain explicit adapter failures rather than invented8169 conversions.
`guid_assignment::diagnostic` accepts only the adapter's exact marked native
error envelope; checked error-cell decoding is restricted to producing plans
controlled by this adapter. Unrelated messages retain their own identities.

At revision7dfeee89115d2c4264e1c98c1ceef9f3150e3001, seven runtime tests passed,
covering suffixes, typed Unicode parameters/NULLs, mixed character targets,
failed multirow INSERT/UPDATE atomicity, uncaught rollback, CATCH reads,
ALTER source preservation, live SET masks and single evaluation across6000
rows. Nine existing storage tests, four transaction tests and seven native
GUID tests also passed. Strict Clippy then reported a manual-inspect style
finding; subsequent fixes use `inspect_err` and name the runtime test row type
rather than suppressing strict Clippy findings.
Current-head complete replay, independent client comparison, full workspace,
client suite and diagnostic audit are still required before merge.

The independent GUID client regression compares captured assignment rows,
errors, information messages and transaction effects. The complete local
replay separately retains all raw descriptors, token events and completion
fields; those differences must not be normalized or presented as a full wire
compatibility pass. ProductVersion reports the emulated server version rather
than the reference installation's build version.

Checked mutation currently covers ordinary INSERT SELECT and unjoined UPDATE
without OUTPUT and with an unshadowed physical row ID. Joined/CTE/OUTPUT write
paths need further integration; the checked transaction contract is not claimed
for those paths. Scalar CAST/TRY_CONVERT, GUID ordering and uncaptured source
types remain separate compatibility work. Claims771/777 and bounded
companions779/780 authorize the root integration in PR772. The earlier
backlog773 and stale scalar reservation35 were superseded under explicit
owner authorization. Restore the deferred, unclaimed identity ALTER preflight
reservation321 after companion780 completes.
