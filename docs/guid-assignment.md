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
reports 3930; batch completion reports 3998 and rolls the transaction back.
A subsequent readback confirms both transaction state and prior writes were
cleared. These results do not establish behavior for other conversion classes.

The first capture attempt stopped at an incorrectly assumed successful COMMIT.
The retained diagnostic capture at `9c8718b` then completed both fresh runs and
exposed the rollback and uncommittable-transaction controls. No successful
transaction continuation is claimed for those requests. The final fixture
preserves the complete observations without normalizing them.

## Runtime integration remains pending

The existing core parser in `msduck-core::types::uniqueidentifier` provides
deterministic character conversion and mixed-endian GUID bytes.
`src/guid_assignment.rs` now provides an explicitly registered native adapter
for character/Unicode conversion, NULL and UUID passthrough. It emits canonical
text from the core's mixed-endian bytes and lets DuckDB own UUID slot layout.
Four native regressions cover byte layout, raw UTF-16 suffixes, single
evaluation across 6,000 rows and native diagnostic translation.
`guid_assignment::diagnostic` recognizes only this adapter's canonical marked
DuckDB error envelope and returns the core error identity (8169/state 2/severity
16). Application text, unrelated backend errors, malformed carriers and appended
error text are not reclassified. Root assignment still needs wiring, diagnostics
and the captured transaction effects. These native tests do not establish server
assignment support.

Source inspection found native scalar registration in `src/scalar.rs`, rather
than `src/lib.rs`. Error translation and transaction lifecycle are owned by the
engine shell. The current task does not authorize editing those files; any
necessary registration companion must be exclusively claimed, and integration
with the owner's separately reserved `engine.rs` must be coordinated before
edits. Backlog issue #773 / `guid-assignment-shell-integration-v1` records that
coordination and reserves no files. Scalar CAST/TRY_CONVERT, GUID ordering and
uncaptured source types remain
separate compatibility gaps.
