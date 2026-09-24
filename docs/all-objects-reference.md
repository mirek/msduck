# SQL Server all_objects reference

Task #163 captures `sys.all_objects`, `sys.objects`, and `sys.system_objects`
from the pinned SQL Server container. The generator is
`scripts/capture-all-objects.mjs`; `reference/all-objects.json` retains both raw
fresh-database runs, with 47 observations each. A separate invocation creates a
new container and two more databases and compares every observation with the
retained runs. No msduck runtime catalog behavior is changed by this task.

Each observation retains SQL, rows, column descriptors, errors, informational
messages, row counts, and tedious completion events. Descriptors are captured
both through an empty `SELECT *` and through `sys.all_columns` declarations.
This is decoded client evidence; it does not retain complete raw TDS packets.

## Membership and metadata

For this pinned build, a fresh database has 2,742 `all_objects` rows, 118 `objects`
rows, and 2,624 `system_objects` rows. ID-based set differences are empty, the two
source sets are disjoint, and their union exactly matches `all_objects`. These are
observations of this build, not universal row-count constants for an emulator.

The 118 fresh `objects` rows are already marked `is_ms_shipped`: 72 system tables
and 43 internal tables in `sys`, plus three service queues in `dbo`. All captured
`system_objects` IDs are negative. Neither schema names, the shipped flag, nor the
observed ID sign should substitute for explicit catalog membership.

All three views expose the same twelve column names in the same order, but their
TDS descriptors differ. In particular, `objects` returns `Bit` for the three
publication/shipped flags, whereas `all_objects` and `system_objects` return
`BitN(1)`. `system_objects` also returns `IntN(4)` for `parent_object_id`, compared
with `Int` in the other two views. The fixture retains the remaining flag,
collation, width, and declaration differences; do not infer one view's metadata
by copying another's or by inspecting current values.

The created catalog contains fourteen objects: two tables, primary-key/default/
check/foreign-key constraints, a view, a procedure, scalar/inline/table-valued
functions, a trigger, a sequence, and a synonym. They appear in `objects` and
`all_objects`, not `system_objects`. The full snapshot preserves CHAR(2) type
padding and parent identities. A schema transfer moves the view without changing
its object ID. Dropping the child table also removes its foreign-key object.

## Transaction observations

The owner session observes transactional creation, schema transfer, and drop.
A second ordinary session with `LOCK_TIMEOUT 1000` receives error 1222, state 51,
class 16 when reading this catalog snapshot during each pending DDL stage. Its
empty result must not be interpreted as an empty catalog. After rollback, both
sessions see the original fourteen objects. The capture verifier checks that the
owner's complete catalog rows are restored, including raw IDs and dates.

A committed synonym drop leaves thirteen created objects. ALTER TABLE and the
committed drop are also observed from the second connection. This does not
establish snapshot isolation, dirty-read catalog semantics, or arbitrary metadata
locking behavior.

## Identity and clock binding

Both raw runs retain actual server-generated object IDs, schema IDs, parent IDs,
and create/modify dates. Cross-database comparison binds object IDs to the same
row's captured schema/name/type, schema IDs to schema names, and nonzero parent
IDs to the captured parent's qualified name. It checks unique identities and IDs,
parent-row relationships, and stable IDs across the captured lifecycle.
Create/modify dates are validated as dates but their clock values are excluded
from cross-run equality. No raw fixture values or metadata are rewritten. These
bindings are comparison rules, not permission to ignore runtime identity,
parentage, or timestamp semantics.

## Runtime work still needed

`src/object_catalog.rs` currently exposes a table/view-oriented `sys.objects`
backed by `main.__msduck_objects`; it does not expose either missing view.
Implementation needs explicit supported system-object membership, a union with
transactionally maintained object rows, and independent declaration/result
metadata for each public view. Catalog identity allocation and DDL hooks must
preserve parentage, transfer, cascading removal, rollback, and observer behavior.

A seeded count, a `sys.all_objects` alias containing only user tables, or rows
invented solely to make a cross join expensive would not reproduce this evidence.
The captured built-in inventory does not itself implement those built-ins. Full
system membership, procedures/functions/constraints, permissions-based metadata
visibility, and catalog locking require their own implementation and verification.
This task does not claim that the original cancellation query using `all_objects`
can yet execute unchanged against msduck.

Run `node scripts/capture-all-objects.mjs OUTPUT_DIRECTORY` on a machine with the
pinned SQL Server image available to Docker. The helper creates and removes only
its own fresh containers/databases and keeps credentials out of arguments and
artifacts. Server execution tests are not replaced by this reference capture.
